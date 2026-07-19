//! Background task that periodically refreshes every device's status into the
//! fleet and notifies subscribers (e.g. the SSE stream) of the update.

use std::time::Duration;

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
    for (id, host) in targets {
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
        if let Ok((id, result)) = joined {
            updates.push((id, result));
        }
    }

    {
        let mut fleet = state.inner.fleet.write().await;
        for (id, result) in updates {
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
    }
    state.notify();
}
