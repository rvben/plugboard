use axum::Form;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use maud::{Markup, html};
use serde::Deserialize;
use switchkit::{PowerAction, Vendor};

use crate::auth::Csrf;
use crate::error::AppError;
use crate::history;
use crate::ops;
use crate::poller;
use crate::redact::scrub_credentials;
use crate::state::AppState;
use crate::views::components::{bulk_toast, close_modal, confirm_modal, undo_toast};
use crate::views::dashboard::device_card;
use crate::views::{dashboard, layout};

pub async fn index(State(state): State<AppState>, csrf: Csrf) -> Markup {
    let chrome = layout::Chrome {
        active: layout::Nav::Devices,
        show_logout: state.builtin_auth().await,
    };
    let series = history::snapshot(&state.inner.history);
    let upds = crate::updates::snapshot(&state.inner.updates);
    let fleet = state.inner.fleet.read().await;
    layout::page(
        "Dashboard",
        &csrf.0,
        chrome,
        dashboard::dashboard_page(&fleet, &series, &upds),
    )
}

/// `GET /modal/close` - the Cancel button's direct target swap: an empty `#modal`,
/// dismissing whatever confirmation was open.
pub async fn modal_close() -> Markup {
    html! { div id="modal" {} }
}

#[derive(Deserialize)]
pub struct ToggleForm {
    confirmed: Option<String>,
    /// Optional explicit relay channel (the detail page's per-relay
    /// switches); absent means the device's default relay, exactly as
    /// before.
    relay: Option<u8>,
}

pub async fn toggle(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<ToggleForm>,
) -> Result<axum::response::Response, AppError> {
    let (host, protected, vendor) = {
        let fleet = state.inner.fleet.read().await;
        let dev = fleet
            .get(&id)
            .ok_or_else(|| AppError::NotFound(id.clone()))?;
        (dev.host.clone(), dev.protected, dev.vendor)
    };
    let confirmed = form.confirmed.as_deref() == Some("true");
    // Protected devices execute ONLY with confirmed=true. The modal posts back to this
    // same route with that field; there is no separate unguarded confirm route.
    if protected && !confirmed {
        // Return the UNCHANGED card (primary target #card-{id}) plus the modal as an
        // OOB swap into #modal. The modal's confirm form re-posts here with
        // confirmed=true and targets #card-{id}, echoing the relay channel so
        // the confirmed toggle hits the SAME relay. The modal injects
        // `confirmed=true`.
        let series = history::snapshot(&state.inner.history);
        let upds = crate::updates::snapshot(&state.inner.updates);
        let fleet = state.inner.fleet.read().await;
        let dev = fleet
            .get(&id)
            .ok_or_else(|| AppError::NotFound(id.clone()))?;
        let relay_str = form.relay.map(|r| r.to_string());
        let mut hidden: Vec<(&str, &str)> = Vec::new();
        if let Some(r) = relay_str.as_deref() {
            hidden.push(("relay", r));
        }
        let modal = confirm_modal(
            &format!("Switch {}?", dev.display_name()),
            &format!("/device/{id}/toggle"),
            &hidden,
            &format!("#card-{id}"),
            "outerHTML",
        );
        return Ok(html! { (device_card(dev, &series, &upds)) (modal) }.into_response());
    }
    let client = state
        .client(vendor)
        .ok_or_else(|| AppError::Internal(format!("no client configured for {host}'s vendor")))?;
    let target = state.target_for(&host).await;
    let relay = ops::set_power(client.as_ref(), &target, form.relay, PowerAction::Toggle).await?;
    // Refresh full status so the card reflects the new relay plus fresh energy/RSSI.
    // The follow-up read decides reachability EXACTLY like the poller: a control action
    // never fabricates reachability, so a FAILED refresh renders the card offline / n/a
    // (reachable=false, status=None, error set), never a stale or half-confirmed reading.
    // The command's confirmed relay is surfaced in the undo toast (below) and the next
    // successful poll restores the live card.
    let refreshed = ops::get_status(client.as_ref(), &target).await;
    {
        let mut fleet = state.inner.fleet.write().await;
        if let Some(dev) = fleet.get_mut(&id) {
            match refreshed {
                Ok(s) => {
                    dev.status = Some(s);
                    dev.error = None;
                    dev.reachable = true;
                }
                // `e` may embed the device's credential-bearing request URL (see
                // `crate::redact`), so it is scrubbed before being stored.
                Err(e) => {
                    dev.error = Some(scrub_credentials(&e.to_string()));
                    dev.status = None;
                    dev.reachable = false;
                }
            }
        }
    }
    state.notify();
    let series = history::snapshot(&state.inner.history);
    let upds = crate::updates::snapshot(&state.inner.updates);
    let fleet = state.inner.fleet.read().await;
    let dev = fleet
        .get(&id)
        .ok_or_else(|| AppError::NotFound(id.clone()))?;
    let toast = undo_toast(&id, form.relay, relay.state.as_str());
    // close_modal() OOB-clears #modal (a no-op when no modal was open, e.g. a normal
    // card toggle); the toast appends to #toasts.
    Ok(html! { (device_card(dev, &series, &upds)) (close_modal()) (toast) }.into_response())
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
        let series = history::snapshot(&state.inner.history);
        let upds = crate::updates::snapshot(&state.inner.updates);
        let fleet = state.inner.fleet.read().await;
        let modal = confirm_modal(
            &format!("Switch all devices {}?", form.action),
            "/devices/power",
            &[("action", &form.action)],
            "#grid",
            "outerHTML",
        );
        return Ok(html! { (dashboard::grid(&fleet, &series, &upds)) (modal) }.into_response());
    }

    // Snapshot (id, host, vendor) without holding the fleet lock across device
    // I/O, exactly like `poller::refresh_once`.
    let targets: Vec<(String, String, Vendor)> = {
        let fleet = state.inner.fleet.read().await;
        fleet
            .devices
            .iter()
            .map(|d| (d.id.clone(), d.host.clone(), d.vendor))
            .collect()
    };

    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(poller::MAX_CONCURRENT));
    let mut set = tokio::task::JoinSet::new();
    for (id, host, vendor) in &targets {
        let id = id.clone();
        let host = host.clone();
        let vendor = *vendor;
        let state = state.clone();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore open");
            let result = match state.client(vendor) {
                Some(client) => {
                    let target = state.target_for(&host).await;
                    ops::set_power(client.as_ref(), &target, None, action).await
                }
                // No client wired up for this vendor: count it exactly like any
                // other command failure below, never a panic or a silent skip.
                None => Err(switchkit::Error::Unsupported {
                    host: host.clone(),
                    message: "no client configured for this device's vendor".into(),
                }),
            };
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
                // `e` may embed the device's credential-bearing request URL (see
                // `crate::redact`), so it is scrubbed before being logged.
                tracing::warn!(
                    id = %id,
                    error = %scrub_credentials(&e.to_string()),
                    "bulk power command failed for device"
                );
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

    let series = history::snapshot(&state.inner.history);
    let upds = crate::updates::snapshot(&state.inner.updates);
    let fleet = state.inner.fleet.read().await;
    let toast = bulk_toast(switched, failed);
    Ok(html! { (dashboard::grid(&fleet, &series, &upds)) (close_modal()) (toast) }.into_response())
}
