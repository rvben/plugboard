use maud::{Markup, html};
use switchkit::RelayState;

use crate::fleet::{DeviceView, Fleet};
use crate::history::Series;
use crate::updates::UpdatesMap;
use crate::views::components::{
    ToggleTarget, na, power_chart, power_mark, relay_control, signal_indicator, state_badge,
    vendor_tag,
};

/// Renders one device card. This is the SSE swap unit: it must carry a stable
/// `id="card-{id}"` and `sse-swap="device-{id}"` so a later `device-{id}` SSE
/// event can target it. `history` supplies the device's recent power samples
/// (may be empty; the chart renders nothing until a real sample exists).
/// A confirmed-newer firmware shows as an ambient accent dot beside the
/// name (the exact version rides in the tooltip/label); the wordy
/// affordance lives on the detail page, next to the actual Update action.
pub fn device_card(dev: &DeviceView, history: &Series, updates: &UpdatesMap) -> Markup {
    let series = history.device(&dev.id);
    let update = updates.get(&dev.id).and_then(|u| u.available());
    // maud 0.26 dynamic attribute values use `attr=(expr)`, never `attr={ ... }`.
    // `hx-swap="outerHTML"` so an SSE `device-{id}` event REPLACES this card rather
    // than nesting a new card inside it (htmx sse-swap defaults to innerHTML).
    html! {
        article.card.offline[!dev.is_online()] id=(format!("card-{}", dev.id)) sse-swap=(format!("device-{}", dev.id)) hx-swap="outerHTML" {
            div.card-header {
                div.card-title {
                    a.card-name href=(format!("/device/{}", dev.id)) { (dev.display_name()) }
                    @if let Some(v) = update {
                        // A link, not a bare marker: hover/focus shows an
                        // instant styled tooltip (native `title` is slow on
                        // desktop and absent on touch), and tapping it lands
                        // on the device's update callout, which repeats the
                        // same dot glyph next to its explanation.
                        a.update-dot href=(format!("/device/{}#admin-firmware", dev.id))
                            data-tip=(format!("Firmware {v} available"))
                            aria-label=(format!("Firmware {v} available, view update")) {}
                    }
                }
                div.card-state { (state_badge(dev)) }
            }
            div.card-readouts {
                div.card-power { (na(dev.power_w())) span.unit { " W" } }
                div.card-today { "today " (na(dev.today_kwh())) " kWh" }
            }
            @if series.iter().any(Option::is_some) {
                div.card-spark { (power_chart(series, &history.ticks, "recent power draw")) }
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
pub fn fleet_summary(fleet: &Fleet, history: &Series, updates: &UpdatesMap) -> Markup {
    let updates_available = fleet
        .devices
        .iter()
        .filter(|d| updates.get(&d.id).is_some_and(|u| u.available().is_some()))
        .count();
    let updating = fleet
        .devices
        .iter()
        .filter(|d| {
            updates
                .get(&d.id)
                .is_some_and(|u| matches!(u.phase, crate::updates::Phase::Applying { .. }))
        })
        .count();
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
            @if history.fleet.iter().any(Option::is_some) {
                div.summary-spark { (power_chart(&history.fleet, &history.ticks, "recent fleet load")) }
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
                @if updates_available > 0 {
                    div.stat {
                        span.stat-label { "Updates" }
                        span.stat-value.updates-count { (updates_available) }
                    }
                }
                @if updating > 0 {
                    div.stat {
                        span.stat-label { "Updating" }
                        span.stat-value.updates-count {
                            span.callout-spinner aria-hidden="true" {}
                            (updating)
                        }
                    }
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
pub fn grid(fleet: &Fleet, history: &Series, updates: &UpdatesMap) -> Markup {
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
                (fleet_summary(fleet, history, updates))
                div.grid {
                    @for dev in &fleet.devices { (device_card(dev, history, updates)) }
                }
            }
        }
    }
}

/// The bulk controls above the grid, absent for an empty fleet (nothing to
/// switch). Each form targets `#grid` (`hx-swap="outerHTML"`); every route
/// always confirms first, so the initial response is a re-rendered
/// (unchanged) grid plus an OOB confirm modal, never a direct write.
///
/// These deliberately live OUTSIDE the SSE-swapped `#grid`: a primary
/// action inside a node that is replaced every poll tick can be swapped out
/// mid-click. The Update all button's count is a page-load snapshot; the
/// confirm modal re-counts, and a fleet with nothing left to update answers
/// with an "up to date" toast.
fn bulk_controls(fleet: &Fleet, updates: &UpdatesMap) -> Markup {
    let updates_available = fleet
        .devices
        .iter()
        .filter(|d| updates.get(&d.id).is_some_and(|u| u.available().is_some()))
        .count();
    html! {
        div.grid-header {
            h1 { "Devices" }
            @if !fleet.devices.is_empty() {
                div.bulk-actions {
                    @if updates_available > 0 {
                        form hx-post="/updates/apply-all" hx-target="#grid" hx-swap="outerHTML" {
                            button type="submit" class="btn-primary" {
                                "Update all (" (updates_available) ")"
                            }
                        }
                    }
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
pub fn dashboard_page(fleet: &Fleet, history: &Series, updates: &UpdatesMap) -> Markup {
    html! {
        (bulk_controls(fleet, updates))
        (grid(fleet, history, updates))
    }
}

#[cfg(test)]
mod tests {
    use switchkit::{DeviceSnapshot, Energy, Relay, RelayState, Vendor};

    use super::{device_card, fleet_summary};
    use crate::fleet::{DeviceView, Fleet};
    use crate::history::Series;
    use crate::updates::UpdatesMap;

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
        let html = fleet_summary(&fleet, &Series::default(), &UpdatesMap::new()).into_string();
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
        let html = fleet_summary(&fleet, &Series::default(), &UpdatesMap::new()).into_string();
        assert!(
            html.contains(">n/a<"),
            "absent load must render n/a: {html}"
        );
        assert!(
            !html.contains("0.0"),
            "an absent load must never be coerced to 0: {html}"
        );
    }

    /// An available update renders as the ambient dot carrying the exact
    /// version in its accessible label; without one there is no dot at all.
    #[test]
    fn card_update_dot_carries_version_and_is_absent_when_up_to_date() {
        let dev = device("a", true, Some(10.0), Some(true));
        let mut updates = UpdatesMap::new();
        updates.insert(
            "d-a".to_string(),
            crate::updates::UpdateInfo {
                current: Some("14.2.0".into()),
                checked_unix: 1_000,
                phase: crate::updates::Phase::Available("15.5.0".into()),
            },
        );
        let with = device_card(&dev, &Series::default(), &updates).into_string();
        assert!(with.contains("update-dot"), "{with}");
        assert!(
            with.contains("Firmware 15.5.0 available"),
            "the dot must carry the exact version accessibly: {with}"
        );
        assert!(
            with.contains("#admin-firmware"),
            "the dot must link to the update action: {with}"
        );

        let without = device_card(&dev, &Series::default(), &UpdatesMap::new()).into_string();
        assert!(
            !without.contains("update-dot"),
            "no confirmed update, no dot: {without}"
        );
    }

    /// A card renders a chart only once a real sample exists; a history of
    /// pure gaps (device offline since startup) renders none.
    #[test]
    fn card_chart_requires_a_real_sample() {
        let dev = device("a", true, Some(10.0), Some(true));
        let series = |samples: Vec<Option<f64>>| Series {
            ticks: (0..samples.len() as u64).map(|i| 1_000 + i * 5).collect(),
            fleet: samples.clone(),
            devices: std::iter::once(("d-a".to_string(), samples)).collect(),
        };
        let with = device_card(
            &dev,
            &series(vec![Some(10.0), Some(12.0)]),
            &UpdatesMap::new(),
        )
        .into_string();
        assert!(with.contains("sparkline"), "{with}");
        let without =
            device_card(&dev, &series(vec![None, None]), &UpdatesMap::new()).into_string();
        assert!(
            !without.contains("sparkline"),
            "all-gap history must not fabricate a line: {without}"
        );
    }
}
