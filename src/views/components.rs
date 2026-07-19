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
