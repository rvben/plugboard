use maud::{Markup, html};
use switchkit::RelayState;

use crate::fleet::{DeviceView, Fleet};
use crate::history::Series;
use crate::views::components::{
    ToggleTarget, na, power_mark, relay_control, signal_indicator, sparkline, state_badge,
    vendor_tag,
};

/// Renders one device card. This is the SSE swap unit: it must carry a stable
/// `id="card-{id}"` and `sse-swap="device-{id}"` so a later `device-{id}` SSE
/// event can target it. `series` is the device's recent power samples (may be
/// empty; the sparkline renders nothing until a real sample exists).
pub fn device_card(dev: &DeviceView, series: &[Option<f64>]) -> Markup {
    // maud 0.26 dynamic attribute values use `attr=(expr)`, never `attr={ ... }`.
    // `hx-swap="outerHTML"` so an SSE `device-{id}` event REPLACES this card rather
    // than nesting a new card inside it (htmx sse-swap defaults to innerHTML).
    html! {
        article.card.offline[!dev.is_online()] id=(format!("card-{}", dev.id)) sse-swap=(format!("device-{}", dev.id)) hx-swap="outerHTML" {
            div.card-header {
                a.card-name href=(format!("/device/{}", dev.id)) { (dev.display_name()) }
                div.card-state { (state_badge(dev)) }
            }
            div.card-readouts {
                div.card-power { (na(dev.power_w())) span.unit { " W" } }
                div.card-today { "today " (na(dev.today_kwh())) " kWh" }
            }
            @if series.iter().any(Option::is_some) {
                div.card-spark { (sparkline(series, "recent power draw")) }
            }
            div.card-meta {
                (vendor_tag(dev.vendor))
                (signal_indicator(dev.signal()))
                span.host title=(dev.host) { (dev.host) }
            }
            div.card-footer {
                (relay_control(dev, ToggleTarget::Card(&dev.id)))
            }
        }
    }
}

/// The live fleet hero above the grid: the measured load as the app's
/// primary instrument (big readout + the recent fleet sparkline), with
/// relays-on and online counts beside it. Every value is honest about its
/// inputs: the load sums ONLY devices that are currently reachable AND
/// reporting a power reading (the caption says how many that is), and shows
/// the muted n/a - never 0 - when nothing reports. This is the SSE `summary`
/// swap unit.
pub fn fleet_summary(fleet: &Fleet, fleet_series: &[Option<f64>]) -> Markup {
    let total = fleet.devices.len();
    let online = fleet.devices.iter().filter(|d| d.is_online()).count();
    let on = fleet
        .devices
        .iter()
        .filter(|d| {
            d.is_online()
                && matches!(
                    d.status
                        .as_ref()
                        .and_then(|s| s.relays.first())
                        .map(|r| &r.state),
                    Some(RelayState::On)
                )
        })
        .count();
    let readings: Vec<f64> = fleet.devices.iter().filter_map(|d| d.power_w()).collect();
    let reporting = readings.len();
    let load: Option<f64> = (!readings.is_empty()).then(|| readings.iter().sum());
    html! {
        div.fleet-summary id="fleet-summary" sse-swap="summary" hx-swap="outerHTML" {
            div.stat.stat-load {
                span.stat-label { "Measured load" }
                span.stat-value.stat-hero {
                    @match load {
                        Some(w) => { (format!("{w:.1}")) span.unit { " W" } }
                        None => { (na::<f64>(None)) }
                    }
                }
                span.stat-detail { (reporting) " of " (total) " devices reporting" }
            }
            @if fleet_series.iter().any(Option::is_some) {
                div.summary-spark { (sparkline(fleet_series, "recent fleet load")) }
            }
            div.summary-counts {
                div.stat {
                    span.stat-label { "Relays on" }
                    span.stat-value { (on) span.of { " / " (total) } }
                }
                div.stat {
                    span.stat-label { "Online" }
                    span.stat-value { (online) span.of { " / " (total) } }
                }
            }
        }
    }
}

/// Renders the device grid: the SSE connection lives here (`sse-connect="/events"`)
/// so every card's `sse-swap` (and the summary strip's) receives its named
/// event. This is also the bulk all-on/off swap target -
/// `routes::dashboard::bulk_power` re-renders this exact fragment so
/// `id="grid"` and `sse-connect` survive an `outerHTML` swap and SSE keeps
/// working afterward. Factored out of `dashboard_page` so both call sites
/// always produce identical grid markup.
pub fn grid(fleet: &Fleet, history: &Series) -> Markup {
    html! {
        // htmx SSE extension: `sse-connect` opens the EventSource; each card's
        // `sse-swap="device-{id}"` receives its named event and swaps in place.
        section id="grid" sse-connect="/events" {
            @if fleet.devices.is_empty() {
                p.empty {
                    (power_mark(28))
                    strong { "No devices yet" }
                    span { "Scan your network to find Tasmota and Shelly devices." }
                    span { a href="/discover" { "Discover devices" } }
                }
            } @else {
                (fleet_summary(fleet, &history.fleet))
                div.grid {
                    @for dev in &fleet.devices { (device_card(dev, history.device(&dev.id))) }
                }
            }
        }
    }
}

/// The bulk all-on/off controls above the grid, absent for an empty fleet
/// (nothing to switch). Each form posts `/devices/power` and targets `#grid`
/// (`hx-swap="outerHTML"`); the route always confirms first, so the initial
/// response is a re-rendered (unchanged) grid plus an OOB confirm modal,
/// never a direct write.
fn bulk_controls(fleet: &Fleet) -> Markup {
    html! {
        div.grid-header {
            h1 { "Devices" }
            @if !fleet.devices.is_empty() {
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
}

/// The full dashboard body: bulk controls above the device grid.
pub fn dashboard_page(fleet: &Fleet, history: &Series) -> Markup {
    html! {
        (bulk_controls(fleet))
        (grid(fleet, history))
    }
}

#[cfg(test)]
mod tests {
    use switchkit::{DeviceSnapshot, Energy, Relay, RelayState, Vendor};

    use super::{device_card, fleet_summary};
    use crate::fleet::{DeviceView, Fleet};

    fn device(
        id: &str,
        reachable: bool,
        power_w: Option<f64>,
        relay_on: Option<bool>,
    ) -> DeviceView {
        let status = reachable.then(|| DeviceSnapshot {
            host: id.to_string(),
            energy: power_w.map(|w| Energy {
                power_w: Some(w),
                ..Default::default()
            }),
            relays: relay_on
                .map(|on| {
                    vec![Relay {
                        index: 0,
                        state: if on { RelayState::On } else { RelayState::Off },
                        raw: on.to_string(),
                    }]
                })
                .unwrap_or_default(),
            ..Default::default()
        });
        DeviceView {
            id: format!("d-{id}"),
            name: id.to_string(),
            host: id.to_string(),
            protected: false,
            vendor: Vendor::Tasmota,
            reachable,
            status,
            error: None,
        }
    }

    /// The load sums only reachable, reporting devices and says how many
    /// contributed; counts are per the live poll.
    #[test]
    fn fleet_summary_sums_only_reporting_devices() {
        let fleet = Fleet {
            devices: vec![
                device("a", true, Some(100.5), Some(true)),
                device("b", true, Some(49.5), Some(false)),
                device("c", true, None, Some(true)), // online, no meter
                device("d", false, None, None),      // offline
            ],
        };
        let html = fleet_summary(&fleet, &[]).into_string();
        assert!(html.contains("150.0"), "sums the two real readings: {html}");
        assert!(
            html.contains("2 of 4 devices reporting"),
            "captions the honest denominator: {html}"
        );
        assert!(html.contains("Relays on"), "labels the on count: {html}");
        // 2 relays on (a and c), 3 online, of 4 total.
        assert!(html.contains(">2<span class=\"of\"> / 4</span>"), "{html}");
        assert!(html.contains(">3<span class=\"of\"> / 4</span>"), "{html}");
    }

    /// No reporting devices -> the load is the muted n/a marker, NEVER a
    /// fabricated 0 that would read as "the house draws nothing".
    #[test]
    fn fleet_summary_renders_na_not_zero_when_nothing_reports() {
        let fleet = Fleet {
            devices: vec![device("a", false, None, None)],
        };
        let html = fleet_summary(&fleet, &[]).into_string();
        assert!(
            html.contains(">n/a<"),
            "absent load must render n/a: {html}"
        );
        assert!(
            !html.contains("0.0"),
            "an absent load must never be coerced to 0: {html}"
        );
    }

    /// A card renders a sparkline only once a real sample exists; a
    /// history of pure gaps (device offline since startup) renders none.
    #[test]
    fn card_sparkline_requires_a_real_sample() {
        let dev = device("a", true, Some(10.0), Some(true));
        let with = device_card(&dev, &[Some(10.0), Some(12.0)]).into_string();
        assert!(with.contains("sparkline"), "{with}");
        let without = device_card(&dev, &[None, None]).into_string();
        assert!(
            !without.contains("sparkline"),
            "all-gap history must not fabricate a line: {without}"
        );
    }
}
