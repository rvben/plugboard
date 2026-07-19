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
            div.card-header {
                a.card-name href=(format!("/device/{}", dev.id)) { (dev.display_name()) }
                div.card-state { (state_badge(dev)) }
            }
            div.card-readouts {
                div.card-power { (na(dev.power_w())) " W" }
                div.card-today { "today " (na(dev.today_kwh())) " kWh" }
            }
            div.card-meta {
                span.rssi { "rssi " (na(dev.rssi())) }
                span class=(if dev.is_online() { "online" } else { "online is-offline" }) {
                    @if dev.is_online() { "online" } @else { "offline" }
                }
            }
            div.card-footer {
                form.toggle hx-post=(format!("/device/{}/toggle", dev.id)) hx-swap="outerHTML" hx-target=(format!("#card-{}", dev.id)) {
                    button type="submit" disabled[!dev.is_online()] { "Toggle" }
                }
            }
        }
    }
}

/// Renders the device grid: the SSE connection lives here (`sse-connect="/events"`)
/// so every card's `sse-swap` receives its named event. This is also the bulk
/// all-on/off swap target (Task 6b) - `routes::dashboard::bulk_power` re-renders
/// this exact fragment so `id="grid"` and `sse-connect` survive an `outerHTML`
/// swap and SSE keeps working afterward. Factored out of `dashboard_page` so both
/// call sites always produce identical grid markup.
pub fn grid(fleet: &Fleet) -> Markup {
    html! {
        // htmx SSE extension: `sse-connect` opens the EventSource; each card's
        // `sse-swap="device-{id}"` receives its named event and swaps in place.
        section.grid id="grid" sse-connect="/events" {
            @if fleet.devices.is_empty() {
                p.empty { "No devices yet. " a href="/discover" { "Discover devices" } "." }
            }
            @for dev in &fleet.devices { (device_card(dev)) }
        }
    }
}

/// The bulk all-on/off controls above the grid. Each form posts `/devices/power`
/// and targets `#grid` (`hx-swap="outerHTML"`); the route always confirms first
/// (Task 6b), so the initial response is a re-rendered (unchanged) grid plus an
/// OOB confirm modal, never a direct write.
fn bulk_controls() -> Markup {
    html! {
        div.grid-header {
            h1 { "Devices" }
            div.bulk-actions {
                form hx-post="/devices/power" hx-target="#grid" hx-swap="outerHTML" {
                    input type="hidden" name="action" value="off";
                    button type="submit" { "All off" }
                }
                form hx-post="/devices/power" hx-target="#grid" hx-swap="outerHTML" {
                    input type="hidden" name="action" value="on";
                    button type="submit" { "All on" }
                }
            }
        }
    }
}

/// The full dashboard body: bulk controls above the device grid.
pub fn dashboard_page(fleet: &Fleet) -> Markup {
    html! {
        (bulk_controls())
        (grid(fleet))
    }
}
