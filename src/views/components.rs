use std::fmt::Display;

use maud::{Markup, html};
use switchkit::RelayState;

use crate::fleet::DeviceView;

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

/// Renders a Wi-Fi signal indicator from Tasmota's 0-100 signal-quality
/// percentage: four strength bars plus the percentage value. `None` (device
/// offline or not yet polled) renders the muted `n/a`, never a fabricated bar.
pub fn signal_indicator(pct: Option<i64>) -> Markup {
    let Some(pct) = pct else {
        return na::<i64>(None);
    };
    let pct = pct.clamp(0, 100);
    let filled: i64 = match pct {
        0 => 0,
        1..=25 => 1,
        26..=50 => 2,
        51..=75 => 3,
        _ => 4,
    };
    let label = format!("Wi-Fi signal {pct}%");
    html! {
        span.signal title=(label) aria-label=(label) {
            span.signal-bars aria-hidden="true" {
                @for i in 1..=4_i64 {
                    span.bar.on[i <= filled] {}
                }
            }
            span.signal-pct { (pct) "%" }
        }
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

#[cfg(test)]
mod tests {
    use super::signal_indicator;

    #[test]
    fn signal_indicator_shows_percentage_bars_and_label() {
        let html = signal_indicator(Some(68)).into_string();
        assert!(html.contains("68%"), "shows the quality as a percentage");
        assert!(
            html.contains("Wi-Fi signal 68%"),
            "carries an accessible label"
        );
        // 51..=75 fills three of the four bars.
        assert_eq!(html.matches("bar on").count(), 3);
    }

    #[test]
    fn signal_indicator_absent_renders_na_not_zero() {
        let html = signal_indicator(None).into_string();
        assert!(html.contains("n/a"), "an offline/unpolled device shows n/a");
        assert!(!html.contains('%'), "never a fabricated 0%");
        assert!(
            !html.contains("bar on"),
            "no filled bars for an absent reading"
        );
    }

    #[test]
    fn signal_indicator_scales_and_clamps_bars() {
        assert_eq!(
            signal_indicator(Some(100))
                .into_string()
                .matches("bar on")
                .count(),
            4
        );
        assert_eq!(
            signal_indicator(Some(0))
                .into_string()
                .matches("bar on")
                .count(),
            0
        );
        // A dBm-like out-of-range value clamps into 0..=100 (no panic, no bogus bar count).
        assert_eq!(
            signal_indicator(Some(-55))
                .into_string()
                .matches("bar on")
                .count(),
            0
        );
    }
}
