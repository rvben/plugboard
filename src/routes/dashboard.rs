use axum::Form;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use maud::{Markup, html};
use serde::Deserialize;
use tasmota_core::ops::PowerAction;

use crate::auth::Csrf;
use crate::error::AppError;
use crate::ops;
use crate::state::AppState;
use crate::views::components::{close_modal, confirm_modal, undo_toast};
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
