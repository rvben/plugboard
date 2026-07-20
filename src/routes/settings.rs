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

/// Re-renders the `#settings-page` fragment from the current config. Every
/// POST handler below returns this, so the swapped-in markup always reflects
/// the config as it stands after that handler's mutation (or after a
/// rollback, if the save failed and the handler already returned an error
/// instead of calling this).
async fn render_fragment(state: &AppState) -> Markup {
    let config = state.inner.config.read().await;
    settings::settings_page(&config)
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
    Ok(render_fragment(&state).await)
}

#[derive(Deserialize)]
pub struct RemoveForm {
    host: String,
}

/// `POST /settings/device/remove` - drop a device from config and fleet. The
/// removed `DeviceConfig` is held so it can be reinserted if `save_config`
/// fails, exactly like `routes::discover::add`'s rollback for a failed push.
pub async fn remove(
    State(state): State<AppState>,
    Form(form): Form<RemoveForm>,
) -> Result<Markup, AppError> {
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
    Ok(render_fragment(&state).await)
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
    Ok(render_fragment(&state).await)
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
    Ok(render_fragment(&state).await)
}

#[derive(Deserialize)]
pub struct PollIntervalForm {
    secs: u64,
}

/// `POST /settings/poll-interval` - update the poll interval.
/// `poller::spawn_poller` re-reads `config.poll_interval_secs` (clamped to a
/// 1s minimum) at the top of every loop iteration, so this takes effect on
/// the poller's very next tick with no extra wiring.
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
