use maud::{Markup, html};

use crate::fleet::{DeviceView, Fleet};
use crate::views::components::{na, state_badge};

/// Renders one device card. This is the SSE swap unit: it must carry a stable
/// `id="card-{id}"` and `sse-swap="device-{id}"` so a later `device-{id}` SSE
/// event can target it.
pub fn device_card(dev: &DeviceView) -> Markup {
    // maud 0.26 dynamic attribute values use `attr=(expr)`, never `attr={ ... }`.
    // `hx-swap="outerHTML"` so an SSE `device-{id}` event REPLACES this card rather
    // than nesting a new card inside it (htmx sse-swap defaults to innerHTML).
    html! {
        article.card id=(format!("card-{}", dev.id)) sse-swap=(format!("device-{}", dev.id)) hx-swap="outerHTML" {
            a.card-name href=(format!("/device/{}", dev.id)) { (dev.display_name()) }
            div.card-state { (state_badge(dev)) }
            div.card-power { (na(dev.power_w())) " W" }
            div.card-today { "today " (na(dev.today_kwh())) " kWh" }
            div.card-meta {
                span.rssi { "rssi " (na(dev.rssi())) }
                span.online { @if dev.is_online() { "online" } @else { "offline" } }
            }
            form.toggle hx-post=(format!("/device/{}/toggle", dev.id)) hx-swap="outerHTML" hx-target=(format!("#card-{}", dev.id)) {
                button type="submit" disabled[!dev.is_online()] { "Toggle" }
            }
        }
    }
}

/// Renders the device grid: the SSE connection lives here (`sse-connect="/events"`)
/// so every card's `sse-swap` receives its named event.
pub fn dashboard_page(fleet: &Fleet) -> Markup {
    html! {
        // htmx SSE extension: `sse-connect` opens the EventSource; each card's
        // `sse-swap="device-{id}"` receives its named event and swaps in place.
        // `id="grid"` is the bulk all-on/off swap target (Task 6b).
        section.grid id="grid" sse-connect="/events" {
            @if fleet.devices.is_empty() {
                p.empty { "No devices yet. " a href="/discover" { "Discover devices" } "." }
            }
            @for dev in &fleet.devices { (device_card(dev)) }
        }
    }
}
