use axum::Form;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use maud::{Markup, html};
use serde::Deserialize;
use tasmota_core::ops::PowerAction;

use crate::auth::Csrf;
use crate::error::AppError;
use crate::ops;
use crate::poller;
use crate::state::AppState;
use crate::views::components::{bulk_toast, close_modal, confirm_modal, undo_toast};
use crate::views::dashboard::device_card;
use crate::views::{dashboard, layout};

pub async fn index(State(state): State<AppState>, csrf: Csrf) -> Markup {
    let fleet = state.inner.fleet.read().await;
    layout::page("Dashboard", &csrf.0, dashboard::dashboard_page(&fleet))
}

/// `GET /modal/close` - the Cancel button's direct target swap: an empty `#modal`,
/// dismissing whatever confirmation was open.
pub async fn modal_close() -> Markup {
    html! { div id="modal" {} }
}

#[derive(Deserialize)]
pub struct ToggleForm {
    confirmed: Option<String>,
}

pub async fn toggle(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<ToggleForm>,
) -> Result<axum::response::Response, AppError> {
    let (host, protected) = {
        let fleet = state.inner.fleet.read().await;
        let dev = fleet
            .get(&id)
            .ok_or_else(|| AppError::NotFound(id.clone()))?;
        (dev.host.clone(), dev.protected)
    };
    let confirmed = form.confirmed.as_deref() == Some("true");
    // Protected devices execute ONLY with confirmed=true. The modal posts back to this
    // same route with that field; there is no separate unguarded confirm route.
    if protected && !confirmed {
        // Return the UNCHANGED card (primary target #card-{id}) plus the modal as an
        // OOB swap into #modal. The modal's confirm form re-posts here with
        // confirmed=true and targets #card-{id}. The modal injects `confirmed=true`.
        let fleet = state.inner.fleet.read().await;
        let dev = fleet
            .get(&id)
            .ok_or_else(|| AppError::NotFound(id.clone()))?;
        let modal = confirm_modal(
            &format!("Switch {}?", dev.display_name()),
            &format!("/device/{id}/toggle"),
            &[],
            &format!("#card-{id}"),
        );
        return Ok(html! { (device_card(dev)) (modal) }.into_response());
    }
    let addr = state.addr_for(&host).await;
    let relay = ops::set_power(
        &state.inner.transport,
        addr.clone(),
        None,
        PowerAction::Toggle,
    )
    .await?;
    // Refresh full status so the card reflects the new relay plus fresh energy/RSSI.
    // The follow-up read decides reachability EXACTLY like the poller: a control action
    // never fabricates reachability, so a FAILED refresh renders the card offline / n/a
    // (reachable=false, status=None, error set), never a stale or half-confirmed reading.
    // The command's confirmed relay is surfaced in the undo toast (below) and the next
    // successful poll restores the live card.
    let refreshed = ops::get_status(&state.inner.transport, addr).await;
    {
        let mut fleet = state.inner.fleet.write().await;
        if let Some(dev) = fleet.get_mut(&id) {
            match refreshed {
                Ok(s) => {
                    dev.status = Some(s);
                    dev.error = None;
                    dev.reachable = true;
                }
                Err(e) => {
                    dev.error = Some(e.to_string());
                    dev.status = None;
                    dev.reachable = false;
                }
            }
        }
    }
    state.notify();
    let fleet = state.inner.fleet.read().await;
    let dev = fleet
        .get(&id)
        .ok_or_else(|| AppError::NotFound(id.clone()))?;
    let toast = undo_toast(&id, relay.state.as_str());
    // close_modal() OOB-clears #modal (a no-op when no modal was open, e.g. a normal
    // card toggle); the toast appends to #toasts.
    Ok(html! { (device_card(dev)) (close_modal()) (toast) }.into_response())
}

#[derive(Deserialize)]
pub struct BulkForm {
    /// "on" or "off"; anything else is a 400, never silently ignored.
    action: String,
    confirmed: Option<String>,
}

/// `POST /devices/power` - switch every device on or off. A bulk write is
/// destructive by nature (it touches the whole fleet), so it ALWAYS confirms:
/// without `confirmed=true` this returns the unchanged grid plus an OOB confirm
/// modal and never touches a device. With `confirmed=true` it fans the power
/// command out to every device with the same bounded `Semaphore` + `JoinSet`
/// pattern as `poller::refresh_once` (never holding the fleet lock across I/O),
/// tracks per-device success/failure for the summary toast, then calls
/// `poller::refresh_once` so the whole fleet - including any device that just
/// failed to switch - reflects fresh, honest telemetry rather than a stale or
/// half-confirmed reading. One unreachable device never aborts the others: the
/// response is always 200 with a per-device summary.
pub async fn bulk_power(
    State(state): State<AppState>,
    Form(form): Form<BulkForm>,
) -> Result<axum::response::Response, AppError> {
    let action = match form.action.as_str() {
        "on" => PowerAction::On,
        "off" => PowerAction::Off,
        other => return Err(AppError::BadRequest(format!("invalid action: {other}"))),
    };
    let confirmed = form.confirmed.as_deref() == Some("true");
    if !confirmed {
        let fleet = state.inner.fleet.read().await;
        let modal = confirm_modal(
            &format!("Switch all devices {}?", form.action),
            "/devices/power",
            &[("action", &form.action)],
            "#grid",
        );
        return Ok(html! { (dashboard::grid(&fleet)) (modal) }.into_response());
    }

    // Snapshot (id, host) without holding the fleet lock across device I/O, exactly
    // like `poller::refresh_once`.
    let targets: Vec<(String, String)> = {
        let fleet = state.inner.fleet.read().await;
        fleet
            .devices
            .iter()
            .map(|d| (d.id.clone(), d.host.clone()))
            .collect()
    };

    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(poller::MAX_CONCURRENT));
    let mut set = tokio::task::JoinSet::new();
    for (id, host) in &targets {
        let id = id.clone();
        let host = host.clone();
        let state = state.clone();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore open");
            let addr = state.addr_for(&host).await;
            let result = ops::set_power(&state.inner.transport, addr, None, action).await;
            (id, result)
        });
    }

    let mut switched = 0usize;
    let mut failed = 0usize;
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((_id, Ok(_relay))) => switched += 1,
            Ok((id, Err(e))) => {
                failed += 1;
                tracing::warn!(id = %id, error = %e, "bulk power command failed for device");
            }
            // The task panicked or was cancelled: count it as a failure rather than
            // silently dropping it from the summary.
            Err(join_err) => {
                failed += 1;
                tracing::warn!(error = %join_err, "bulk power task failed to join");
            }
        }
    }

    // Re-poll the whole fleet so every card - switched, failed, or untouched -
    // reflects fresh status rather than the command's own (unrefreshed) response.
    poller::refresh_once(&state).await;

    let fleet = state.inner.fleet.read().await;
    let toast = bulk_toast(switched, failed);
    Ok(html! { (dashboard::grid(&fleet)) (close_modal()) (toast) }.into_response())
}
