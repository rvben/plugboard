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
use crate::views::components::{bulk_toast, close_modal, confirm_modal, note_toast, undo_toast};
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
            .ok_or_else(|| AppError::NotFound(format!("Device {id} is not configured.")))?;
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
            .ok_or_else(|| AppError::NotFound(format!("Device {id} is not configured.")))?;
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
        .ok_or_else(|| AppError::NotFound(format!("Device {id} is not configured.")))?;
    let toast = undo_toast(&id, form.relay, relay.state.as_str());
    // close_modal() OOB-clears #modal (a no-op when no modal was open, e.g. a normal
    // card toggle); the toast appends to #toasts.
    Ok(html! { (device_card(dev, &series, &upds)) (close_modal()) (toast) }.into_response())
}

#[derive(Deserialize)]
pub struct ApplyAllForm {
    confirmed: Option<String>,
}

/// `POST /updates/apply-all` - command a firmware update for every device
/// with a CONFIRMED available version. Fleet-wide and firmware-flashing, so
/// it ALWAYS confirms first (one human confirmation covers the batch,
/// protected devices included, exactly like bulk power); each accepted
/// command enters the same observed `Applying` lifecycle as a single
/// update, so the cards and callouts follow every device individually.
pub async fn updates_apply_all(
    State(state): State<AppState>,
    Form(form): Form<ApplyAllForm>,
) -> Result<axum::response::Response, AppError> {
    let confirmed = form.confirmed.as_deref() == Some("true");
    if !confirmed {
        let series = history::snapshot(&state.inner.history);
        let upds = crate::updates::snapshot(&state.inner.updates);
        let fleet = state.inner.fleet.read().await;
        let count = fleet
            .devices
            .iter()
            .filter(|d| upds.get(&d.id).is_some_and(|u| u.available().is_some()))
            .count();
        // The button's count is a page-load snapshot; if everything was
        // updated meanwhile, say so instead of asking to confirm nothing.
        if count == 0 {
            return Ok(html! {
                (dashboard::grid(&fleet, &series, &upds))
                (note_toast("Everything is up to date."))
            }
            .into_response());
        }
        let modal = confirm_modal(
            &format!(
                "Update {count} device{} to newer firmware? Each installs it and reboots.",
                if count == 1 { "" } else { "s" }
            ),
            "/updates/apply-all",
            &[],
            "#grid",
            "outerHTML",
        );
        return Ok(html! { (dashboard::grid(&fleet, &series, &upds)) (modal) }.into_response());
    }

    let (started, failed) = crate::updates::apply_available(&state, true).await;
    let message = if failed == 0 {
        format!(
            "Updating {started} device{}",
            if started == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "Updating {started} device{}, {failed} failed to start",
            if started == 1 { "" } else { "s" }
        )
    };
    let series = history::snapshot(&state.inner.history);
    let upds = crate::updates::snapshot(&state.inner.updates);
    let fleet = state.inner.fleet.read().await;
    Ok(html! {
        (dashboard::grid(&fleet, &series, &upds))
        (close_modal())
        (note_toast(&message))
    }
    .into_response())
}

#[derive(Deserialize)]
pub struct BulkForm {
    /// "on" or "off"; anything else is a 400, never silently ignored.
    action: String,
    confirmed: Option<String>,
    /// Restrict the bulk action to one group's members (the per-group Off/On
    /// controls); absent means the whole fleet, exactly as before groups.
    group: Option<String>,
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
    // A whitespace-only group means "no group filter", matching how the
    // grid treats blank group names as ungrouped.
    let group = form
        .group
        .as_deref()
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(str::to_string);
    let confirmed = form.confirmed.as_deref() == Some("true");
    if !confirmed {
        let series = history::snapshot(&state.inner.history);
        let upds = crate::updates::snapshot(&state.inner.updates);
        let fleet = state.inner.fleet.read().await;
        let title = match group.as_deref() {
            Some(g) => format!("Switch everything in {g} {}?", form.action),
            None => format!("Switch all devices {}?", form.action),
        };
        let mut hidden: Vec<(&str, &str)> = vec![("action", &form.action)];
        if let Some(g) = group.as_deref() {
            hidden.push(("group", g));
        }
        let modal = confirm_modal(&title, "/devices/power", &hidden, "#grid", "outerHTML");
        return Ok(html! { (dashboard::grid(&fleet, &series, &upds)) (modal) }.into_response());
    }

    // Snapshot (id, host, vendor) without holding the fleet lock across device
    // I/O, exactly like `poller::refresh_once`; a group filter narrows the
    // targets to that group's members.
    let targets: Vec<(String, String, Vendor)> = {
        let fleet = state.inner.fleet.read().await;
        fleet
            .devices
            .iter()
            .filter(|d| match group.as_deref() {
                Some(g) => d.group.as_deref().map(str::trim) == Some(g),
                None => true,
            })
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
