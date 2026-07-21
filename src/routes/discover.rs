//! Device discovery routes (Task 9, mixed-vendor per Plan C Task 3):
//! `GET /discover` (the CIDR scan form), `POST /discover/scan` (runs
//! `switchkit::discover` against EVERY vendor client this app has wired up,
//! fully async), and `POST /discover/add` (persists a found device into the
//! config and fleet).
//!
//! SECURITY: `add` never trusts a caller-supplied vendor. Discovery is the
//! only source of truth for which vendor a host is: `add` always re-probes
//! the single host over every wired client (see `probe_host`) and persists
//! ONLY the vendor that probe confirms. A host that no client confirms is
//! rejected, never guessed or defaulted.

use std::sync::Arc;

use axum::Form;
use axum::extract::State;
use maud::{Markup, html};
use serde::Deserialize;
use switchkit::{SmartDevice, Vendor};

use crate::auth::Csrf;
use crate::config::DeviceConfig;
use crate::error::AppError;
use crate::fleet::{DeviceView, device_id};
use crate::redact::scrub_credentials;
use crate::state::AppState;
use crate::views::{discover, layout};

/// Every vendor client this app currently has wired up (see
/// `AppState::client`), collected as owned `Arc`s so the borrowed
/// `&dyn SmartDevice` slice handed to `switchkit::discover` can outlive the
/// async call. `Vendor` is `#[non_exhaustive]`; listing known variants
/// explicitly here (rather than iterating some enum-wide set) means a future
/// vendor this app has not wired a client for is simply absent from probing,
/// never guessed.
fn wired_clients(state: &AppState) -> Vec<Arc<dyn SmartDevice + Send + Sync>> {
    [Vendor::Tasmota, Vendor::Shelly]
        .into_iter()
        .filter_map(|v| state.client(v))
        .collect()
}

/// Re-confirms a single host's vendor server-side by probing it against
/// EVERY wired client, exactly like `scan` does for a whole range. Returns
/// the vendor `switchkit::discover` actually confirmed, or `None` if no
/// client confirmed it - the caller must never fall back to a guessed or
/// caller-supplied vendor in that case.
///
/// No credentials are passed to the probe: at this point the host is not yet
/// in the config (that's what `add` is about to do), so there is no stored
/// credential for it to look up in the first place - unlike `target_for`,
/// which only serves already-configured hosts.
async fn probe_host(state: &AppState, host: &str) -> Option<Vendor> {
    let clients = wired_clients(state);
    let refs: Vec<&dyn SmartDevice> = clients
        .iter()
        .map(|c| c.as_ref() as &dyn SmartDevice)
        .collect();
    let hosts = vec![host.to_string()];
    let found = switchkit::discover(&refs, &hosts, 1, None).await;
    found.into_iter().next().map(|d| d.vendor)
}

/// A documentation-range placeholder shown when `detect_local_cidr()` cannot
/// determine the host's own subnet (e.g. a sandboxed or offline environment).
/// Never a real/LAN default: the user must supply their own range in that case.
const RANGE_PLACEHOLDER: &str = "192.0.2.0/24";

/// `GET /discover` - the CIDR scan form, pre-filled with the host's detected
/// local subnet, or `RANGE_PLACEHOLDER` when detection fails.
pub async fn index(State(state): State<AppState>, csrf: Csrf) -> Markup {
    let chrome = layout::Chrome {
        active: layout::Nav::Discover,
        show_logout: state.builtin_auth().await,
    };
    let default_range = switchkit::detect_local_cidr().unwrap_or_else(|| RANGE_PLACEHOLDER.into());
    layout::page("Discover", &csrf.0, chrome, discover::page(&default_range))
}

#[derive(Deserialize)]
pub struct ScanForm {
    range: String,
}

/// `POST /discover/scan` - expand the CIDR (rejecting a malformed or
/// too-large range with 400 before any network I/O, via `hosts_in_cidr`'s own
/// scan-size guard), then probe every host against EVERY vendor client wired
/// up in `AppState` (`wired_clients` - a future vendor with a wired client
/// joins this list for free). `switchkit::discover` is itself async (a
/// bounded, concurrent fan-out), so this runs directly on the async runtime -
/// no `spawn_blocking`, unlike the old `tasmota_core` blocking scan this
/// replaces. A host that no client confirms is simply absent from the
/// result - never guessed at, never defaulted to a vendor. An empty result
/// (no device confirmed by any client in the range) renders
/// `discover::results`' own hint, never an error.
pub async fn scan(
    State(state): State<AppState>,
    Form(form): Form<ScanForm>,
) -> Result<Markup, AppError> {
    // `hosts_in_cidr` returns a `switchkit::Error` too; scrub it like every
    // other one even though this particular path runs before any network I/O
    // and cannot realistically carry a credential.
    let hosts = switchkit::hosts_in_cidr(&form.range)
        .map_err(|e| AppError::BadRequest(scrub_credentials(&e.to_string())))?;
    let clients = wired_clients(&state);
    if clients.is_empty() {
        return Err(AppError::Internal("no vendor clients configured".into()));
    }
    let refs: Vec<&dyn SmartDevice> = clients
        .iter()
        .map(|c| c.as_ref() as &dyn SmartDevice)
        .collect();
    let found = switchkit::discover(&refs, &hosts, 64, None).await;
    let triples: Vec<(String, String, Vendor)> = found
        .iter()
        .map(|d| {
            (
                d.snapshot.display_name().to_string(),
                d.snapshot.host.clone(),
                d.vendor,
            )
        })
        .collect();
    Ok(discover::results(&triples))
}

#[derive(Deserialize)]
pub struct AddForm {
    name: String,
    host: String,
}

/// `POST /discover/add` - persist a found device into the config and fleet.
///
/// SECURITY: this form is deliberately NOT given a `vendor` field. Even if a
/// caller appends one to the POST body (e.g. `vendor=shelly` copied from a
/// scan result, honest or forged), `AddForm` has nowhere to deserialize it
/// into, so it can never be read, let alone trusted. The persisted vendor
/// comes ONLY from `probe_host` re-confirming `form.host` server-side against
/// every wired vendor client; a host that no client confirms is rejected with
/// 400 and never added - a vendor is never guessed or taken on a caller's
/// word.
///
/// The duplicate check and the config push happen under the SAME write-lock
/// hold, so two concurrent adds of the same host can never both succeed; a
/// duplicate host is rejected with 400 and never appended twice.
///
/// If `save_config().await` fails (disk full, permission error, bad path),
/// the just-pushed device is rolled back out of the in-memory config before
/// the error is returned - otherwise it would linger as a "ghost" entry that
/// was never persisted or added to the fleet, yet still fails every future
/// duplicate check for that host until the process restarts. The config lock
/// is never held across the `save_config().await`; on failure it is
/// re-acquired only to remove the device.
pub async fn add(
    State(state): State<AppState>,
    Form(form): Form<AddForm>,
) -> Result<Markup, AppError> {
    let Some(vendor) = probe_host(&state, &form.host).await else {
        return Err(AppError::BadRequest(format!(
            "{} did not confirm as a known vendor; refusing to add an unverified device",
            form.host
        )));
    };
    let device = DeviceConfig {
        name: form.name,
        host: form.host,
        password: None,
        protected: false,
        group: None,
        // The ONLY source for this field: the server-side re-probe above.
        vendor,
    };
    {
        let mut cfg = state.inner.config.write().await;
        if cfg.devices.iter().any(|d| d.host == device.host) {
            return Err(AppError::BadRequest(format!(
                "{} is already in the fleet",
                device.host
            )));
        }
        cfg.devices.push(device.clone());
    }
    if let Err(e) = state.save_config().await {
        let mut cfg = state.inner.config.write().await;
        cfg.devices.retain(|d| d.host != device.host);
        return Err(AppError::Internal(e.to_string()));
    }
    {
        let mut fleet = state.inner.fleet.write().await;
        fleet.devices.push(DeviceView::from_config(&device));
    }
    state.notify();
    let id = device_id(&device.host);
    Ok(html! {
        li.discover-added id=(format!("discover-row-{id}")) {
            span.discover-check aria-hidden="true" { "\u{2713}" }
            span { strong { (device.name) } " added to the fleet" }
            a href="/" { "View on dashboard" }
        }
    })
}
