//! Background task that periodically refreshes every device's status into the
//! fleet and notifies subscribers (e.g. the SSE stream) of the update.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use switchkit::{DeviceSnapshot, Vendor};

use crate::fleet::Fleet;
use crate::history;
use crate::metrics;
use crate::ops;
use crate::redact::scrub_credentials;
use crate::state::AppState;

/// Bound on concurrent in-flight device polls, so a large fleet cannot flood the
/// blocking pool on a single tick. Shared with `routes::dashboard::bulk_power`,
/// which fans out a power command with the same bound.
pub(crate) const MAX_CONCURRENT: usize = 16;

/// Spawn the poller loop. Runs forever, refreshing then sleeping for
/// `poll_interval_secs` (re-read from config each iteration, minimum 1s).
pub fn spawn_poller(state: AppState) {
    tokio::spawn(async move {
        loop {
            refresh_once(&state).await;
            let secs = state.inner.config.read().await.poll_interval_secs.max(1);
            tokio::time::sleep(Duration::from_secs(secs)).await;
        }
    });
}

/// Refresh every device's status once, concurrently (bounded), then apply the
/// results to the fleet and notify subscribers.
///
/// The fleet write lock is never held across device I/O: the (id, host, vendor)
/// list is snapshotted first, all polling happens without holding any fleet
/// lock, and the write lock is taken only briefly at the end to apply the
/// collected results.
pub async fn refresh_once(state: &AppState) {
    let targets: Vec<(String, String, Vendor)> = {
        let fleet = state.inner.fleet.read().await;
        fleet
            .devices
            .iter()
            .map(|d| (d.id.clone(), d.host.clone(), d.vendor))
            .collect()
    };

    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT));
    let mut set = tokio::task::JoinSet::new();
    for (id, host, vendor) in &targets {
        let id = id.clone();
        let host = host.clone();
        let vendor = *vendor;
        let state = state.clone();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore open");
            let result = match state.client(vendor) {
                Some(client) => {
                    let target = state.target_for(&host).await;
                    ops::get_status(client.as_ref(), &target).await
                }
                // No client is wired up for this vendor: treat exactly like any
                // other unreachable device (offline, scrubbed error), never a
                // panic or a silently-skipped poll.
                None => Err(switchkit::Error::Unsupported {
                    host: host.clone(),
                    message: "no client configured for this device's vendor".into(),
                }),
            };
            (id, result)
        });
    }

    let mut updates = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((id, result)) => updates.push((id, result)),
            // The poll task panicked (or was cancelled) and produced no result. Do
            // NOT drop this silently: `apply_results` below still marks the target
            // offline below so its status can never freeze at a stale live value.
            Err(join_err) => {
                tracing::warn!(error = %join_err, "device poll task failed to join");
            }
        }
    }

    // Record poll outcomes into the accumulating `/metrics` counters BEFORE
    // `apply_results` consumes `updates` below. This is pure/synchronous
    // bookkeeping (no I/O, no lock held across an `.await`), independent of
    // the fleet write below.
    // `None` only on the near-impossible case of a system clock set before
    // the Unix epoch; recording epoch-0 in that case would fabricate a "last
    // success" time that never happened, so `record_poll_outcomes` leaves
    // `last_success_unix` untouched instead.
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .ok();
    metrics::record_poll_outcomes(&state.inner.metrics, &targets, &updates, now_unix);

    // Record this tick's power samples into the history ring buffers, in
    // fleet order. A device is `Some(watts)` ONLY when its poll succeeded AND
    // carried a power reading; an `Err`, a lost task, or a metering-less
    // snapshot records a gap (`None`), never a fabricated zero. The capture
    // time rides along so chart axes label real sample ages; on the
    // pre-epoch-clock edge case (`now_unix` is `None`) the tick is not
    // recorded at all rather than stamped with a fabricated time.
    if let Some(now) = now_unix {
        let samples: Vec<(String, Option<f64>)> = targets
            .iter()
            .map(|(id, _host, _vendor)| {
                let power = updates
                    .iter()
                    .find(|(uid, _)| uid == id)
                    .and_then(|(_, result)| result.as_ref().ok())
                    .and_then(|s| s.energy.as_ref())
                    .and_then(|e| e.power_w);
                (id.clone(), power)
            })
            .collect();
        history::record_tick(&state.inner.history, now, &samples);
    }

    {
        let mut fleet = state.inner.fleet.write().await;
        apply_results(&mut fleet, &targets, updates);
    }

    // Feed this tick's observations (reachability + running firmware
    // version) into the update lifecycle, so an in-flight update is
    // CONFIRMED by a real read-back - or honestly reported unconfirmed
    // when its window elapses.
    if let Some(now) = now_unix {
        let observations: Vec<(String, bool, Option<String>)> = {
            let fleet = state.inner.fleet.read().await;
            fleet
                .devices
                .iter()
                .map(|d| {
                    let version = d
                        .status
                        .as_ref()
                        .and_then(|s| s.firmware.as_ref())
                        .and_then(|f| f.version.clone());
                    (d.id.clone(), d.reachable, version)
                })
                .collect()
        };
        crate::updates::observe_poll(&state.inner.updates, &observations, now);
    }

    state.notify();
}

/// Apply collected poll results to the fleet, then reconcile: any target whose
/// id did NOT produce a result (its task panicked or was otherwise lost) is
/// explicitly marked offline. This guarantees every device is either updated
/// with its `Ok`/`Err` poll result or explicitly marked unreachable every
/// tick - none is ever silently skipped and left showing frozen stale data.
///
/// Pure and synchronous: takes no locks (the caller holds the fleet write
/// lock) and performs no I/O, so it is trivially unit-testable.
fn apply_results(
    fleet: &mut Fleet,
    targets: &[(String, String, Vendor)],
    updates: Vec<(String, switchkit::Result<DeviceSnapshot>)>,
) {
    let mut updated_ids = std::collections::HashSet::with_capacity(updates.len());
    for (id, result) in updates {
        updated_ids.insert(id.clone());
        if let Some(dev) = fleet.get_mut(&id) {
            match result {
                Ok(s) => {
                    dev.status = Some(s);
                    dev.error = None;
                    dev.reachable = true;
                }
                // Clear the stale status so telemetry renders n/a, never last-seen
                // readings presented as live; the device is offline. `e` may embed
                // the device's credential-bearing request URL (see `crate::redact`),
                // so it is scrubbed before being stored - a future UI that renders
                // `dev.error` can never leak it either.
                Err(e) => {
                    dev.error = Some(scrub_credentials(&e.to_string()));
                    dev.status = None;
                    dev.reachable = false;
                }
            }
        }
    }

    for (id, _host, _vendor) in targets {
        if updated_ids.contains(id) {
            continue;
        }
        if let Some(dev) = fleet.get_mut(id) {
            dev.status = None;
            dev.reachable = false;
            dev.error = Some("poll task failed".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use switchkit::{DeviceSnapshot, Energy, Signal, Vendor};

    use super::apply_results;
    use crate::fleet::{DeviceView, Fleet};

    fn sample_status() -> DeviceSnapshot {
        DeviceSnapshot {
            host: "192.0.2.20".into(),
            name: Some("Plug".into()),
            energy: Some(Energy {
                power_w: Some(42.0),
                today_kwh: Some(1.5),
                total_kwh: None,
                voltage_v: None,
                current_a: None,
            }),
            signal: Some(Signal::from_quality_percent(50)),
            uptime: Some("1T00:00:00".into()),
            ..Default::default()
        }
    }

    fn online_device(id: &str, host: &str) -> DeviceView {
        DeviceView {
            id: id.into(),
            name: "Plug".into(),
            host: host.into(),
            protected: false,
            vendor: Vendor::Tasmota,
            reachable: true,
            status: Some(sample_status()),
            error: None,
        }
    }

    /// A device whose poll task produced no result (panicked / lost from the
    /// JoinSet) must NOT keep showing its previous "reachable" status with
    /// frozen telemetry: it must be explicitly marked offline, exactly like a
    /// device whose poll returned an `Err`. This test fails if the reconcile
    /// loop in `apply_results` is removed (verified: reverting to only the
    /// apply-updates loop leaves the missing device `reachable: true` with its
    /// stale status still attached, and the assertions below fail).
    #[test]
    fn missing_result_marks_device_offline_not_frozen() {
        let mut fleet = Fleet {
            devices: vec![
                online_device("d-1", "192.0.2.10"),
                online_device("d-2", "192.0.2.20"),
            ],
        };
        let targets = vec![
            ("d-1".to_string(), "192.0.2.10".to_string(), Vendor::Tasmota),
            ("d-2".to_string(), "192.0.2.20".to_string(), Vendor::Tasmota),
        ];
        // Only "d-1" produced a result; "d-2"'s task is presumed panicked/lost.
        let updates = vec![("d-1".to_string(), Ok(sample_status()))];

        apply_results(&mut fleet, &targets, updates);

        let d1 = fleet.get("d-1").expect("d-1 present");
        assert!(d1.reachable);
        assert!(d1.status.is_some());
        assert!(d1.error.is_none());

        let d2 = fleet.get("d-2").expect("d-2 present");
        assert!(!d2.reachable);
        assert!(d2.status.is_none());
        assert_eq!(d2.error.as_deref(), Some("poll task failed"));
    }
}
