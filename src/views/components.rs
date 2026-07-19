use std::fmt::Display;

use maud::{Markup, html};

use crate::fleet::DeviceView;
use tasmota_core::RelayState;

/// Render `Some(v)` as-is, `None` as a muted "n/a" span. Never coerces an
/// absent value to `0` or an empty string.
pub fn na<T: Display>(v: Option<T>) -> Markup {
    match v {
        Some(v) => html! { (v) },
        None => html! { span.na { "n/a" } },
    }
}

/// Renders the device's on/off/unknown/offline badge.
pub fn state_badge(dev: &DeviceView) -> Markup {
    if !dev.is_online() {
        return html! { span.badge.offline { "offline" } };
    }
    // Reachable: the relay comes from the fresh status ONLY (never a carried-over
    // value); an empty/unconfirmable relay renders `unknown`, not a guess.
    let relay = dev.status.as_ref().and_then(|s| s.relays.first());
    match relay.map(|r| &r.state) {
        Some(RelayState::On) => html! { span.badge.on { "on" } },
        Some(RelayState::Off) => html! { span.badge.off { "off" } },
        Some(RelayState::Unknown(_)) | None => html! { span.badge.unknown { "unknown" } },
    }
}

/// A confirmation modal, rendered as an OUT-OF-BAND swap into the layout's `#modal`
/// placeholder, so opening it never disturbs the page or the card. The confirm form
/// re-posts `action` with `confirmed=true` plus `hidden` (the original validated
/// payload) and targets `target` (the element the confirmed response replaces, e.g.
/// the card `#card-{id}` or the admin panel `#admin-result`). Values are auto-escaped
/// by maud. NEVER pass credentials through here.
pub fn confirm_modal(title: &str, action: &str, hidden: &[(&str, &str)], target: &str) -> Markup {
    html! {
        div id="modal" hx-swap-oob="true" {
            div.modal-backdrop {
                div.modal role="dialog" aria-modal="true" {
                    h2 { (title) }
                    form hx-post=(action) hx-target=(target) hx-swap="outerHTML" {
                        input type="hidden" name="confirmed" value="true";
                        @for (k, v) in hidden {
                            input type="hidden" name=(k) value=(v);
                        }
                        div.modal-actions {
                            button type="button" class="btn-cancel" hx-get="/modal/close" hx-target="#modal" hx-swap="outerHTML" { "Cancel" }
                            button type="submit" class="btn-danger" { "Confirm" }
                        }
                    }
                }
            }
        }
    }
}

/// Clear the modal region as an OOB swap (returned alongside a confirmed action's
/// primary response). `GET /modal/close` returns the plain `html! { div id="modal" {} }`
/// for the Cancel button's direct-target swap.
pub fn close_modal() -> Markup {
    html! { div id="modal" hx-swap-oob="true" {} }
}

/// Out-of-band summary toast for a bulk power action ("3 switched", or "3
/// switched, 1 failed" when some devices errored). No undo: a bulk action
/// touches every device, so "undo" would itself need to be a confirmed bulk
/// write; the summary is purely informational.
pub fn bulk_toast(switched: usize, failed: usize) -> Markup {
    let message = if failed == 0 {
        format!("{switched} switched")
    } else {
        format!("{switched} switched, {failed} failed")
    };
    html! {
        div id="toasts" hx-swap-oob="beforeend:#toasts" {
            div.toast { span { (message) } }
        }
    }
}

/// Out-of-band toast with an Undo action (a toggle is its own inverse, so this
/// switches back). `confirmed=true` via hx-vals so undo also works on protected
/// devices without another modal. `hx-swap-oob` injects it into `#toasts`.
pub fn undo_toast(id: &str, new_state: &str) -> Markup {
    html! {
        div id="toasts" hx-swap-oob="beforeend:#toasts" {
            div.toast {
                span { "Switched to " (new_state) }
                button.undo
                    hx-post=(format!("/device/{id}/toggle"))
                    hx-vals=r#"{"confirmed":"true"}"#
                    hx-target=(format!("#card-{id}"))
                    hx-swap="outerHTML" { "Undo" }
            }
        }
    }
}
