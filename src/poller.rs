//! Background task that periodically refreshes every device's status into the
//! fleet and notifies subscribers (e.g. the SSE stream) of the update.

use std::time::Duration;

use crate::fleet::Fleet;
use crate::ops;
use crate::state::AppState;

/// Bound on concurrent in-flight device polls, so a large fleet cannot flood the
/// blocking pool on a single tick.
const MAX_CONCURRENT: usize = 16;

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
/// The fleet write lock is never held across device I/O: the (id, host) list is
/// snapshotted first, all polling happens without holding any fleet lock, and the
/// write lock is taken only briefly at the end to apply the collected results.
pub async fn refresh_once(state: &AppState) {
    let targets: Vec<(String, String)> = {
        let fleet = state.inner.fleet.read().await;
        fleet
            .devices
            .iter()
            .map(|d| (d.id.clone(), d.host.clone()))
            .collect()
    };

    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT));
    let mut set = tokio::task::JoinSet::new();
    for (id, host) in &targets {
        let id = id.clone();
        let host = host.clone();
        let state = state.clone();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore open");
            let addr = state.addr_for(&host).await;
            let result = ops::get_status(&state.inner.transport, addr).await;
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

    {
        let mut fleet = state.inner.fleet.write().await;
        apply_results(&mut fleet, &targets, updates);
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
    targets: &[(String, String)],
    updates: Vec<(String, tasmota_core::Result<tasmota_core::DeviceStatus>)>,
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
                // readings presented as live; the device is offline.
                Err(e) => {
                    dev.error = Some(e.to_string());
                    dev.status = None;
                    dev.reachable = false;
                }
            }
        }
    }

    for (id, _host) in targets {
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
    use tasmota_core::{DeviceStatus, Energy, NetInfo};

    use super::apply_results;
    use crate::fleet::{DeviceView, Fleet};

    fn sample_status() -> DeviceStatus {
        DeviceStatus {
            host: "192.0.2.20".into(),
            name: Some("Plug".into()),
            friendly_names: vec!["Plug".into()],
            module: Some(1),
            relays: Vec::new(),
            firmware: Some("14.2.0".into()),
            net: NetInfo::default(),
            uptime: Some("1T00:00:00".into()),
            wifi_rssi: Some(-50),
            energy: Some(Energy {
                power_w: Some(42.0),
                voltage_v: None,
                current_a: None,
                today_kwh: Some(1.5),
                yesterday_kwh: None,
                total_kwh: None,
            }),
            mqtt: None,
        }
    }

    fn online_device(id: &str, host: &str) -> DeviceView {
        DeviceView {
            id: id.into(),
            name: "Plug".into(),
            host: host.into(),
            protected: false,
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
            ("d-1".to_string(), "192.0.2.10".to_string()),
            ("d-2".to_string(), "192.0.2.20".to_string()),
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
