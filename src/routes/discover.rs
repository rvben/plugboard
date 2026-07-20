//! Device discovery routes (Task 9): `GET /discover` (the CIDR scan form),
//! `POST /discover/scan` (runs `switchkit::discover` against the fleet's
//! wired vendor clients, fully async), and `POST /discover/add` (persists a
//! found device into the config and fleet).

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

/// A documentation-range placeholder shown when `detect_local_cidr()` cannot
/// determine the host's own subnet (e.g. a sandboxed or offline environment).
/// Never a real/LAN default: the user must supply their own range in that case.
const RANGE_PLACEHOLDER: &str = "192.0.2.0/24";

/// `GET /discover` - the CIDR scan form, pre-filled with the host's detected
/// local subnet, or `RANGE_PLACEHOLDER` when detection fails.
pub async fn index(csrf: Csrf) -> Markup {
    let default_range = switchkit::detect_local_cidr().unwrap_or_else(|| RANGE_PLACEHOLDER.into());
    layout::page("Discover", &csrf.0, discover::page(&default_range))
}

#[derive(Deserialize)]
pub struct ScanForm {
    range: String,
}

/// `POST /discover/scan` - expand the CIDR (rejecting a malformed or
/// too-large range with 400 before any network I/O, via `hosts_in_cidr`'s own
/// scan-size guard), then probe every host against every vendor client wired
/// up in `AppState` (Tasmota only for now - see `AppState::client`; a future
/// vendor with a wired client joins this list for free). `switchkit::discover`
/// is itself async (a bounded, concurrent fan-out), so this runs directly on
/// the async runtime - no `spawn_blocking`, unlike the old `tasmota_core`
/// blocking scan this replaces. An empty result (no device confirmed by any
/// client in the range) renders `discover::results`' own hint, never an error.
pub async fn scan(
    State(state): State<AppState>,
    Form(form): Form<ScanForm>,
) -> Result<Markup, AppError> {
    // `hosts_in_cidr` returns a `switchkit::Error` too; scrub it like every
    // other one even though this particular path runs before any network I/O
    // and cannot realistically carry a credential.
    let hosts = switchkit::hosts_in_cidr(&form.range)
        .map_err(|e| AppError::BadRequest(scrub_credentials(&e.to_string())))?;
    let Some(tasmota) = state.client(Vendor::Tasmota) else {
        return Err(AppError::Internal(
            "no client configured for Tasmota".into(),
        ));
    };
    let clients: [&dyn SmartDevice; 1] = [tasmota.as_ref()];
    let found = switchkit::discover(&clients, &hosts, 64, None).await;
    let pairs: Vec<(String, String)> = found
        .iter()
        .map(|d| {
            (
                d.snapshot.display_name().to_string(),
                d.snapshot.host.clone(),
            )
        })
        .collect();
    Ok(discover::results(&pairs))
}

#[derive(Deserialize)]
pub struct AddForm {
    name: String,
    host: String,
}

/// `POST /discover/add` - persist a found device into the config and fleet.
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
    let device = DeviceConfig {
        name: form.name,
        host: form.host,
        password: None,
        protected: false,
        // Discovery currently only probes Tasmota clients (see `scan` above),
        // so every device it finds is a real Tasmota device. Task 3 wires
        // per-vendor discovery and threads the matched vendor through here.
        vendor: Vendor::Tasmota,
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
    Ok(html! { li id=(format!("discover-row-{id}")) { (device.name) " added." } })
}
