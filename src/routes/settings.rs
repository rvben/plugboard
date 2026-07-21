//! Settings routes (Task 10): manage the device list (rename/remove by
//! host - add lives in `routes::discover`), per-device credentials + the
//! `protected` flag, and the poll interval; plus the read-only auth-mode
//! display rendered by `views::settings`.
//!
//! Every mutating handler here mirrors `routes::discover::add`'s
//! save-then-rollback discipline: mutate the in-memory config first (under a
//! single write-lock acquisition, released before any `.await`), call
//! `state.save_config().await` OFF that lock, and roll the mutation back if
//! the save fails - so a failed disk write never leaves a half-applied
//! in-memory change. There is deliberately no route to edit auth mode or the
//! built-in credential: see the design's lockout-hazard rationale.

use axum::Form;
use axum::extract::State;
use axum::response::IntoResponse;
use maud::Markup;
use serde::Deserialize;

use crate::auth::Csrf;
use crate::error::AppError;
use crate::fleet::device_id;
use crate::state::AppState;
use crate::views::{layout, settings};

/// `GET /settings`.
pub async fn index(State(state): State<AppState>, csrf: Csrf) -> Markup {
    let chrome = layout::Chrome {
        active: layout::Nav::Settings,
        show_logout: state.builtin_auth().await,
    };
    let config = state.inner.config.read().await;
    layout::page(
        "Settings",
        &csrf.0,
        chrome,
        settings::settings_page(&config),
    )
}

/// Re-renders the `#settings-page` fragment from the current config (the
/// app-level handlers below return this, so the swapped-in markup always
/// reflects the config as it stands after the mutation - or after a
/// rollback, if the save failed and the handler already returned an error).
async fn render_fragment(state: &AppState) -> Markup {
    let config = state.inner.config.read().await;
    settings::settings_page(&config)
}

/// Re-renders one device's settings panel (`#device-settings`) from the
/// current config + fleet - the swap target every per-device settings form
/// on the detail page uses.
async fn device_settings_fragment(state: &AppState, host: &str) -> Result<Markup, AppError> {
    let (has_credential, group_names) = {
        let config = state.inner.config.read().await;
        let has_credential = config
            .devices
            .iter()
            .find(|d| d.host == host)
            .is_some_and(|d| d.password.is_some());
        let mut group_names: Vec<String> = config
            .devices
            .iter()
            .filter_map(|d| d.group.as_deref())
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(str::to_string)
            .collect();
        group_names.sort_unstable();
        group_names.dedup();
        (has_credential, group_names)
    };
    let fleet = state.inner.fleet.read().await;
    let dev = fleet
        .get(&device_id(host))
        .ok_or_else(|| AppError::NotFound(format!("Device {host} is not configured.")))?;
    Ok(crate::views::device::device_settings_panel(
        dev,
        &crate::views::device::SettingsCtx {
            has_credential,
            group_names,
        },
    ))
}

#[derive(Deserialize)]
pub struct RenameForm {
    host: String,
    name: String,
}

/// `POST /settings/device/rename` - rename a device by host. The fleet's
/// `DeviceView` is updated too (same host, so the same `device_id` - only
/// the displayed name changes).
pub async fn rename(
    State(state): State<AppState>,
    Form(form): Form<RenameForm>,
) -> Result<Markup, AppError> {
    let previous_name = {
        let mut cfg = state.inner.config.write().await;
        let dev = cfg
            .devices
            .iter_mut()
            .find(|d| d.host == form.host)
            .ok_or_else(|| AppError::NotFound(form.host.clone()))?;
        let previous_name = dev.name.clone();
        dev.name = form.name.clone();
        previous_name
    };
    if let Err(e) = state.save_config().await {
        let mut cfg = state.inner.config.write().await;
        if let Some(dev) = cfg.devices.iter_mut().find(|d| d.host == form.host) {
            dev.name = previous_name;
        }
        return Err(AppError::Internal(e.to_string()));
    }
    {
        let mut fleet = state.inner.fleet.write().await;
        let id = device_id(&form.host);
        if let Some(dev) = fleet.get_mut(&id) {
            dev.name = form.name.clone();
        }
    }
    state.notify();
    device_settings_fragment(&state, &form.host).await
}

#[derive(Deserialize)]
pub struct RemoveForm {
    host: String,
    confirmed: Option<String>,
}

/// `POST /settings/device/remove` - drop a device from config and fleet.
/// Confirm-gated (removal forgets the stored name, group, credential, and
/// history); the confirmed response navigates back to the dashboard via
/// `hx-redirect`, since the device's own page no longer exists. The removed
/// `DeviceConfig` is held so it can be reinserted if `save_config` fails,
/// exactly like `routes::discover::add`'s rollback for a failed push.
pub async fn remove(
    State(state): State<AppState>,
    Form(form): Form<RemoveForm>,
) -> Result<axum::response::Response, AppError> {
    if form.confirmed.as_deref() != Some("true") {
        let fleet = state.inner.fleet.read().await;
        let dev = fleet.get(&device_id(&form.host)).ok_or_else(|| {
            AppError::NotFound(format!("Device {} is not configured.", form.host))
        })?;
        let modal = crate::views::components::confirm_modal(
            &format!("Remove {} from plugboard?", dev.display_name()),
            Some(
                "The device itself is not touched; its stored settings and history are forgotten. You can add it again from Discover.",
            ),
            "/settings/device/remove",
            &[("host", &form.host)],
            "#device-remove",
            "outerHTML",
        );
        return Ok(maud::html! {
            (crate::views::device::remove_panel(dev))
            (modal)
        }
        .into_response());
    }
    let removed = {
        let mut cfg = state.inner.config.write().await;
        let idx = cfg
            .devices
            .iter()
            .position(|d| d.host == form.host)
            .ok_or_else(|| AppError::NotFound(form.host.clone()))?;
        cfg.devices.remove(idx)
    };
    if let Err(e) = state.save_config().await {
        let mut cfg = state.inner.config.write().await;
        cfg.devices.push(removed);
        return Err(AppError::Internal(e.to_string()));
    }
    {
        let mut fleet = state.inner.fleet.write().await;
        let id = device_id(&form.host);
        fleet.devices.retain(|d| d.id != id);
    }
    state.notify();
    let mut response = axum::http::StatusCode::OK.into_response();
    response
        .headers_mut()
        .insert("hx-redirect", axum::http::HeaderValue::from_static("/"));
    Ok(response)
}

#[derive(Deserialize)]
pub struct CredentialsForm {
    host: String,
    password: String,
}

/// `POST /settings/device/credentials` - set (or, given an empty submission,
/// clear) a device's stored password. Write-only: `DeviceConfig.password` is
/// never rendered back into any page - see `views::settings::device_row` and
/// `tests/settings.rs`'s non-vacuous proof. The fleet needs no rebuild for
/// this: `AppState::target_for` reads `DeviceConfig.password` fresh from config
/// on every call, so a credential change takes effect on the very next
/// request without any cached copy to update.
pub async fn credentials(
    State(state): State<AppState>,
    Form(form): Form<CredentialsForm>,
) -> Result<Markup, AppError> {
    let previous_password = {
        let mut cfg = state.inner.config.write().await;
        let dev = cfg
            .devices
            .iter_mut()
            .find(|d| d.host == form.host)
            .ok_or_else(|| AppError::NotFound(form.host.clone()))?;
        let previous_password = dev.password.clone();
        dev.password = if form.password.is_empty() {
            None
        } else {
            Some(form.password.clone())
        };
        previous_password
    };
    if let Err(e) = state.save_config().await {
        let mut cfg = state.inner.config.write().await;
        if let Some(dev) = cfg.devices.iter_mut().find(|d| d.host == form.host) {
            dev.password = previous_password;
        }
        return Err(AppError::Internal(e.to_string()));
    }
    device_settings_fragment(&state, &form.host).await
}

#[derive(Deserialize)]
pub struct ProtectedForm {
    host: String,
    /// A checkbox submits `protected=true` when checked and OMITS the field
    /// entirely when unchecked (standard HTML forms), so `Option<String>`
    /// distinguishes present-and-checked from absent-and-unchecked; only
    /// `Some("true")` is treated as checked, mirroring
    /// `routes::dashboard::ToggleForm`'s `confirmed` convention.
    protected: Option<String>,
}

/// `POST /settings/device/protected` - set the protected flag. The fleet's
/// `DeviceView.protected` is updated too: it is the value
/// `routes::dashboard::toggle` and the admin routes actually gate on, so a
/// change here must take effect immediately, not just on the next fleet
/// rebuild.
pub async fn protected(
    State(state): State<AppState>,
    Form(form): Form<ProtectedForm>,
) -> Result<Markup, AppError> {
    let value = form.protected.as_deref() == Some("true");
    let previous_protected = {
        let mut cfg = state.inner.config.write().await;
        let dev = cfg
            .devices
            .iter_mut()
            .find(|d| d.host == form.host)
            .ok_or_else(|| AppError::NotFound(form.host.clone()))?;
        let previous_protected = dev.protected;
        dev.protected = value;
        previous_protected
    };
    if let Err(e) = state.save_config().await {
        let mut cfg = state.inner.config.write().await;
        if let Some(dev) = cfg.devices.iter_mut().find(|d| d.host == form.host) {
            dev.protected = previous_protected;
        }
        return Err(AppError::Internal(e.to_string()));
    }
    {
        let mut fleet = state.inner.fleet.write().await;
        let id = device_id(&form.host);
        if let Some(dev) = fleet.get_mut(&id) {
            dev.protected = value;
        }
    }
    state.notify();
    device_settings_fragment(&state, &form.host).await
}

#[derive(Deserialize)]
pub struct PollIntervalForm {
    secs: u64,
}

/// `POST /settings/poll-interval` - update the poll interval.
/// `poller::spawn_poller` re-reads `config.poll_interval_secs` (clamped to a
/// 1s minimum) at the top of every loop iteration, so this takes effect on
/// the poller's very next tick with no extra wiring.
#[derive(Deserialize)]
pub struct GroupForm {
    host: String,
    group: String,
}

/// `POST /settings/device/group` - set or clear a device's organizational
/// group by host (a blank submission clears it). The fleet's `DeviceView`
/// mirrors the change immediately, like rename does, so the next dashboard
/// render sections correctly without waiting for a restart.
pub async fn group(
    State(state): State<AppState>,
    Form(form): Form<GroupForm>,
) -> Result<Markup, AppError> {
    let new_group = {
        let trimmed = form.group.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    };
    let previous = {
        let mut cfg = state.inner.config.write().await;
        let dev = cfg
            .devices
            .iter_mut()
            .find(|d| d.host == form.host)
            .ok_or_else(|| AppError::NotFound(form.host.clone()))?;
        let previous = dev.group.clone();
        dev.group = new_group.clone();
        previous
    };
    if let Err(e) = state.save_config().await {
        let mut cfg = state.inner.config.write().await;
        if let Some(dev) = cfg.devices.iter_mut().find(|d| d.host == form.host) {
            dev.group = previous;
        }
        return Err(AppError::Internal(e.to_string()));
    }
    {
        let mut fleet = state.inner.fleet.write().await;
        if let Some(dev) = fleet.get_mut(&device_id(&form.host)) {
            dev.group = new_group;
        }
    }
    state.notify();
    device_settings_fragment(&state, &form.host).await
}

#[derive(Deserialize)]
pub struct UpdatesSettingsForm {
    /// Checkbox semantics: present ("true") when ticked, absent otherwise.
    enabled: Option<String>,
    auto_apply: Option<String>,
}

/// `POST /settings/updates` - the update checker's two behavior toggles,
/// persisted with the same mutate-save-rollback discipline as every other
/// settings write. Auto-apply always skips protected devices (enforced in
/// `updates::apply_available`, not here).
pub async fn updates_settings(
    State(state): State<AppState>,
    Form(form): Form<UpdatesSettingsForm>,
) -> Result<Markup, AppError> {
    let enabled = form.enabled.as_deref() == Some("true");
    let auto_apply = form.auto_apply.as_deref() == Some("true");
    let previous = {
        let mut cfg = state.inner.config.write().await;
        let previous = (cfg.updates.enabled, cfg.updates.auto_apply);
        cfg.updates.enabled = enabled;
        cfg.updates.auto_apply = auto_apply;
        previous
    };
    if let Err(e) = state.save_config().await {
        let mut cfg = state.inner.config.write().await;
        (cfg.updates.enabled, cfg.updates.auto_apply) = previous;
        return Err(AppError::Internal(e.to_string()));
    }
    Ok(render_fragment(&state).await)
}

pub async fn poll_interval(
    State(state): State<AppState>,
    Form(form): Form<PollIntervalForm>,
) -> Result<Markup, AppError> {
    let previous_secs = {
        let mut cfg = state.inner.config.write().await;
        let previous_secs = cfg.poll_interval_secs;
        cfg.poll_interval_secs = form.secs;
        previous_secs
    };
    if let Err(e) = state.save_config().await {
        let mut cfg = state.inner.config.write().await;
        cfg.poll_interval_secs = previous_secs;
        return Err(AppError::Internal(e.to_string()));
    }
    Ok(render_fragment(&state).await)
}
