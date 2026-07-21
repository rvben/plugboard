//! Automatic firmware update discovery.
//!
//! A background task periodically determines, per device, whether a newer
//! firmware than the RUNNING version is available:
//! - **Shelly** (Gen2/Gen3): asks the device itself via the read-only
//!   `Shelly.CheckForUpdate` RPC - the device knows its own stable channel.
//! - **Tasmota**: fetches the latest release tag once per check cycle from a
//!   configurable release feed (GitHub's `releases/latest` by default) and
//!   compares it against each device's reported version. One fetch covers
//!   the whole fleet.
//!
//! Honesty invariants:
//! - An update is claimed ONLY for a device whose running version was
//!   confirmed by a live poll AND whose candidate version parses strictly
//!   newer. Unparseable versions, failed fetches, failed RPCs, and offline
//!   devices claim nothing.
//! - Results are replaced wholesale each check: a device that is offline or
//!   gone no longer carries a stale "update available" the app can't verify.
//! - Checking is read-only. APPLYING an update always goes through the
//!   existing confirmed firmware-update flow (`routes::admin`), never from
//!   here.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use switchkit::Vendor;

use crate::ops;
use crate::redact::scrub_credentials;
use crate::state::AppState;

/// How long a commanded update may stay unconfirmed before the app honestly
/// reports that it could not verify the outcome. Vendor OTA cycles finish in
/// well under a minute; five is generous.
pub const APPLY_TIMEOUT_SECS: u64 = 300;

/// Where a device stands in its update lifecycle. Every transition is driven
/// by an OBSERVATION (a check result, a command we sent, a poll reading the
/// running version back), never by optimism.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Checked; nothing newer could be confirmed.
    UpToDate,
    /// A strictly-newer version was confirmed available.
    Available(String),
    /// We commanded an update and are watching the poller for the device to
    /// come back running it. `target` is `None` for a custom-URL flash
    /// (whose resulting version we cannot know up front); `from` is the
    /// version running when the command was sent.
    Applying {
        target: Option<String>,
        from: Option<String>,
        started_unix: u64,
    },
    /// The device came back and a live poll CONFIRMED the new version.
    Applied { version: String },
    /// The confirmation window elapsed without the device reporting the
    /// expected change. The outcome is unknown - said so, never guessed.
    Unconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    /// The last version a live observation reported the device RUNNING
    /// (`None` when no observation has confirmed one yet).
    pub current: Option<String>,
    /// When this entry last changed by check or observation (unix seconds).
    pub checked_unix: u64,
    pub phase: Phase,
}

impl UpdateInfo {
    /// The confirmed-available newer version, if that is the current phase.
    pub fn available(&self) -> Option<&str> {
        match &self.phase {
            Phase::Available(v) => Some(v),
            _ => None,
        }
    }
}

pub type UpdatesState = Mutex<HashMap<String, UpdateInfo>>;

/// Cloned, render-ready snapshot keyed by device id.
pub type UpdatesMap = HashMap<String, UpdateInfo>;

pub fn snapshot(state: &UpdatesState) -> UpdatesMap {
    state.lock().expect("updates lock").clone()
}

/// Record that an update command was ACCEPTED by the device: the entry
/// enters `Applying`, and the poller's observations decide how it ends.
pub fn mark_applying(
    state: &UpdatesState,
    id: &str,
    target: Option<String>,
    from: Option<String>,
    now: u64,
) {
    let mut map = state.lock().expect("updates lock");
    map.insert(
        id.to_string(),
        UpdateInfo {
            current: from.clone(),
            checked_unix: now,
            phase: Phase::Applying {
                target,
                from,
                started_unix: now,
            },
        },
    );
}

/// Feed one poll tick's observations (`(id, online, running version)`) into
/// the lifecycle: an `Applying` entry becomes `Applied` when the device
/// comes back CONFIRMING the target version (or, for a custom flash, any
/// version different from the one it started on), and `Unconfirmed` when
/// the window elapses first. Everything else is left alone - the periodic
/// check owns those transitions.
pub fn observe_poll(
    state: &UpdatesState,
    observations: &[(String, bool, Option<String>)],
    now: u64,
) {
    let mut map = state.lock().expect("updates lock");
    for (id, online, version) in observations {
        let Some(entry) = map.get_mut(id) else {
            continue;
        };
        let Phase::Applying {
            target,
            from,
            started_unix,
        } = &entry.phase
        else {
            continue;
        };
        if *online && let Some(v) = version {
            entry.current = Some(v.clone());
            let confirmed = match (target.as_deref(), from.as_deref()) {
                (Some(t), _) => v == t,
                (None, Some(f)) => v != f,
                // No target and no known starting version: nothing to
                // compare against, so a confirmation is impossible and the
                // timeout below will report that honestly.
                (None, None) => false,
            };
            if confirmed {
                entry.phase = Phase::Applied { version: v.clone() };
                entry.checked_unix = now;
                continue;
            }
        }
        if now.saturating_sub(*started_unix) > APPLY_TIMEOUT_SECS {
            entry.phase = Phase::Unconfirmed;
            entry.checked_unix = now;
        }
    }
}

/// Lenient numeric parse of a firmware version: strips a leading `v`, cuts
/// build/channel suffixes (`14.2.0(release-tasmota)`, `1.4.4-beta1`), and
/// yields the dotted numeric components. `None` when nothing numeric parses -
/// callers must then claim NO update rather than guessing.
fn parse_version(raw: &str) -> Option<Vec<u64>> {
    let cleaned = raw.trim().trim_start_matches(['v', 'V']);
    let cleaned = cleaned.split(['(', '-', '+', ' ']).next()?;
    let parts: Option<Vec<u64>> = cleaned.split('.').map(|p| p.parse::<u64>().ok()).collect();
    parts.filter(|p| !p.is_empty())
}

/// Whether `candidate` is STRICTLY newer than `current`. Components are
/// zero-padded to equal length so `14.2` equals `14.2.0` (a cosmetic
/// difference must never become an "update"). Unparseable on either side is
/// `false`: no claim without a comparison.
fn is_newer(candidate: &str, current: &str) -> bool {
    let (Some(mut a), Some(mut b)) = (parse_version(candidate), parse_version(current)) else {
        return false;
    };
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a > b
}

/// Extracts a confirmed-newer stable version from a Shelly
/// `Shelly.CheckForUpdate` response (`{"stable":{"version":"1.5.1"},...}`).
/// The device only lists what it considers newer, but the comparison runs
/// anyway: this function never returns the version already running.
fn shelly_available(response: &serde_json::Value, current: &str) -> Option<String> {
    let stable = response.get("stable")?.get("version")?.as_str()?;
    is_newer(stable, current).then(|| stable.to_string())
}

/// Latest Tasmota release version from the configured release feed (a
/// GitHub `releases/latest`-shaped JSON document: `{"tag_name":"v15.5.0"}`).
/// Any failure is `None`, never a guess; the checker just tries again next
/// cycle.
async fn fetch_tasmota_latest(http: &reqwest::Client, url: &str) -> Option<String> {
    let response = match http.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, "tasmota release feed unreachable");
            return None;
        }
    };
    if !response.status().is_success() {
        tracing::debug!(status = %response.status(), "tasmota release feed returned an error");
        return None;
    }
    let body: serde_json::Value = response.json().await.ok()?;
    let tag = body.get("tag_name")?.as_str()?;
    Some(tag.trim_start_matches(['v', 'V']).to_string())
}

/// Run one full check over the fleet and replace the shared results.
/// Considers only devices that are reachable, confirm `firmware_ota`, and
/// report a current version - anything else has no basis for a claim.
pub async fn check_fleet(state: &AppState) {
    let (enabled, release_url) = {
        let cfg = state.inner.config.read().await;
        (cfg.updates.enabled, cfg.updates.tasmota_release_url.clone())
    };
    if !enabled {
        return;
    }
    let Some(now) = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .ok()
    else {
        return;
    };

    // Entries mid-`Applying` are preserved verbatim and their devices are
    // NOT probed: the device is installing/rebooting, and the poller's
    // observations (not a check) decide how that phase ends.
    let applying: HashMap<String, UpdateInfo> = {
        let map = state.inner.updates.lock().expect("updates lock");
        map.iter()
            .filter(|(_, u)| matches!(u.phase, Phase::Applying { .. }))
            .map(|(id, u)| (id.clone(), u.clone()))
            .collect()
    };

    let devices: Vec<(String, String, Vendor, String)> = {
        let fleet = state.inner.fleet.read().await;
        fleet
            .devices
            .iter()
            .filter_map(|d| {
                if !d.reachable || applying.contains_key(&d.id) {
                    return None;
                }
                let s = d.status.as_ref()?;
                if !s.capabilities.firmware_ota {
                    return None;
                }
                let current = s.firmware.as_ref()?.version.clone()?;
                Some((d.id.clone(), d.host.clone(), d.vendor, current))
            })
            .collect()
    };

    let tasmota_latest = if devices.iter().any(|(_, _, v, _)| *v == Vendor::Tasmota) {
        match reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("plugboard/", env!("CARGO_PKG_VERSION")))
            .build()
        {
            Ok(http) => fetch_tasmota_latest(&http, &release_url).await,
            Err(e) => {
                tracing::warn!(error = %e, "could not build update-check http client");
                None
            }
        }
    } else {
        None
    };

    let mut results = applying;
    for (id, host, vendor, current) in devices {
        let available = match vendor {
            Vendor::Tasmota => tasmota_latest
                .as_deref()
                .filter(|latest| is_newer(latest, &current))
                .map(str::to_string),
            Vendor::Shelly => match state.client(vendor) {
                Some(client) => {
                    let target = state.target_for(&host).await;
                    match ops::console(client.as_ref(), &target, "Shelly.CheckForUpdate").await {
                        Ok(value) => shelly_available(&value, &current),
                        Err(e) => {
                            tracing::debug!(
                                id = %id,
                                error = %scrub_credentials(&e.to_string()),
                                "shelly update check failed"
                            );
                            None
                        }
                    }
                }
                None => None,
            },
            _ => None,
        };
        results.insert(
            id,
            UpdateInfo {
                current: Some(current),
                checked_unix: now,
                phase: match available {
                    Some(v) => Phase::Available(v),
                    None => Phase::UpToDate,
                },
            },
        );
    }

    *state.inner.updates.lock().expect("updates lock") = results;
    // Repaint dashboards over SSE so update chips appear without a reload.
    state.notify();

    // Opt-in auto-apply: install what this check just confirmed, through the
    // SAME observed lifecycle the UI button uses. `apply_available` always
    // skips `protected` devices on this path - no human confirmed anything.
    let auto_apply = state.inner.config.read().await.updates.auto_apply;
    if auto_apply {
        let (started, failed) = apply_available(state, false).await;
        if started > 0 || failed > 0 {
            tracing::info!(started, failed, "auto-applied firmware updates");
        }
    }
}

/// Command firmware updates for EVERY device currently in `Available`,
/// each through the same per-device flow the callout button uses: send the
/// command, mark `Applying`, and let the poller's observations decide the
/// outcome. `include_protected` is true only for the human-confirmed
/// "Update all" action; the auto path passes false, so a `protected`
/// device's confirmation contract survives. Fan-out is bounded like every
/// other fleet-wide action. Returns `(started, failed)`.
pub async fn apply_available(state: &AppState, include_protected: bool) -> (usize, usize) {
    let Some(now) = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .ok()
    else {
        return (0, 0);
    };
    let candidates: Vec<(String, String, Vendor, String, Option<String>)> = {
        let upds = snapshot(&state.inner.updates);
        let fleet = state.inner.fleet.read().await;
        fleet
            .devices
            .iter()
            .filter_map(|d| {
                if d.protected && !include_protected {
                    return None;
                }
                let u = upds.get(&d.id)?;
                let target = u.available()?.to_string();
                Some((
                    d.id.clone(),
                    d.host.clone(),
                    d.vendor,
                    target,
                    u.current.clone(),
                ))
            })
            .collect()
    };
    if candidates.is_empty() {
        return (0, 0);
    }

    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(crate::poller::MAX_CONCURRENT));
    let mut set = tokio::task::JoinSet::new();
    for (id, host, vendor, target_version, from) in candidates {
        let state = state.clone();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore open");
            let Some(client) = state.client(vendor) else {
                return (id, false);
            };
            let target = state.target_for(&host).await;
            match ops::firmware_update(client.as_ref(), &target, None).await {
                Ok(()) => {
                    mark_applying(&state.inner.updates, &id, Some(target_version), from, now);
                    (id, true)
                }
                Err(e) => {
                    tracing::warn!(
                        id = %id,
                        error = %scrub_credentials(&e.to_string()),
                        "firmware update command failed"
                    );
                    (id, false)
                }
            }
        });
    }

    let mut started = 0usize;
    let mut failed = 0usize;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((_, true)) => started += 1,
            Ok((_, false)) => failed += 1,
            Err(join_err) => {
                failed += 1;
                tracing::warn!(error = %join_err, "firmware update task failed to join");
            }
        }
    }
    if started > 0 {
        state.notify();
    }
    (started, failed)
}

/// Spawn the periodic checker: once shortly after startup (letting the first
/// poll land so current versions exist), then every configured interval.
pub fn spawn_update_checker(state: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(15)).await;
        loop {
            check_fleet(&state).await;
            let secs = state
                .inner
                .config
                .read()
                .await
                .updates
                .interval_secs
                .max(60);
            tokio::time::sleep(Duration::from_secs(secs)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{is_newer, parse_version, shelly_available};

    #[test]
    fn parse_version_handles_vendor_forms() {
        assert_eq!(parse_version("14.2.0"), Some(vec![14, 2, 0]));
        assert_eq!(parse_version("v15.5.0"), Some(vec![15, 5, 0]));
        assert_eq!(
            parse_version("14.2.0(release-tasmota)"),
            Some(vec![14, 2, 0])
        );
        assert_eq!(parse_version("1.4.4-beta1"), Some(vec![1, 4, 4]));
        assert_eq!(parse_version("garbage"), None);
        assert_eq!(parse_version(""), None);
    }

    /// Strictly newer only: equal (including cosmetically different but
    /// numerically equal) and older candidates claim nothing, and an
    /// unparseable version NEVER becomes an update claim.
    #[test]
    fn is_newer_is_strict_and_never_guesses() {
        assert!(is_newer("14.3.0", "14.2.0"));
        assert!(is_newer("15.0.0", "14.9.9"));
        assert!(!is_newer("14.2.0", "14.2.0"));
        assert!(
            !is_newer("14.2", "14.2.0"),
            "cosmetic difference is not an update"
        );
        assert!(!is_newer("14.1.9", "14.2.0"));
        assert!(!is_newer("garbage", "14.2.0"));
        assert!(!is_newer("14.3.0", "garbage"));
    }

    #[test]
    fn shelly_available_requires_a_strictly_newer_stable() {
        let newer = json!({"stable": {"version": "1.5.1"}});
        assert_eq!(shelly_available(&newer, "1.4.4"), Some("1.5.1".to_string()));
        // The version already running is not an "update", whatever the
        // device response claims.
        assert_eq!(shelly_available(&newer, "1.5.1"), None);
        // No stable entry (device up to date) or a malformed response: no claim.
        assert_eq!(shelly_available(&json!({}), "1.4.4"), None);
        assert_eq!(
            shelly_available(&json!({"stable": {"build_id": "x"}}), "1.4.4"),
            None
        );
    }
}
