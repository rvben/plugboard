//! Device discovery routes (Task 9): `GET /discover` (the CIDR scan form),
//! `POST /discover/scan` (runs `tasmota_core::discovery::scan`, blocking, off
//! the async runtime), and `POST /discover/add` (persists a found device into
//! the config and fleet).

use axum::Form;
use axum::extract::State;
use maud::{Markup, html};
use serde::Deserialize;
use tasmota_core::discovery;

use crate::auth::Csrf;
use crate::config::DeviceConfig;
use crate::error::AppError;
use crate::fleet::{DeviceView, device_id};
use crate::state::AppState;
use crate::views::{discover, layout};

/// A documentation-range placeholder shown when `detect_local_cidr()` cannot
/// determine the host's own subnet (e.g. a sandboxed or offline environment).
/// Never a real/LAN default: the user must supply their own range in that case.
const RANGE_PLACEHOLDER: &str = "192.0.2.0/24";

/// `GET /discover` - the CIDR scan form, pre-filled with the host's detected
/// local subnet, or `RANGE_PLACEHOLDER` when detection fails.
pub async fn index(csrf: Csrf) -> Markup {
    let default_range = discovery::detect_local_cidr().unwrap_or_else(|| RANGE_PLACEHOLDER.into());
    layout::page("Discover", &csrf.0, discover::page(&default_range))
}

#[derive(Deserialize)]
pub struct ScanForm {
    range: String,
}

/// `POST /discover/scan` - expand the CIDR (rejecting a malformed or
/// too-large range with 400 before any network I/O, via `hosts_in_cidr`'s own
/// scan-size guard), then scan it. `discovery::scan` is synchronous/blocking
/// (it uses OS threads internally), so it runs inside `spawn_blocking` rather
/// than on the async runtime. An empty result (no reachable Tasmota device in
/// the range) renders `discover::results`' own hint, never an error.
pub async fn scan(
    State(state): State<AppState>,
    Form(form): Form<ScanForm>,
) -> Result<Markup, AppError> {
    let hosts =
        discovery::hosts_in_cidr(&form.range).map_err(|e| AppError::BadRequest(e.to_string()))?;
    let transport = state.inner.transport.clone();
    let found = tokio::task::spawn_blocking(move || discovery::scan(&transport, &hosts, 64, None))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let pairs: Vec<(String, String)> = found
        .iter()
        .map(|d| (d.status.display_name().to_string(), d.host.clone()))
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
pub async fn add(
    State(state): State<AppState>,
    Form(form): Form<AddForm>,
) -> Result<Markup, AppError> {
    let device = DeviceConfig {
        name: form.name,
        host: form.host,
        password: None,
        protected: false,
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
    state
        .save_config()
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    {
        let mut fleet = state.inner.fleet.write().await;
        fleet.devices.push(DeviceView::from_config(&device));
    }
    state.notify();
    let id = device_id(&device.host);
    Ok(html! { li id=(format!("discover-row-{id}")) { (device.name) " added." } })
}
