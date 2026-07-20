use std::fmt::Display;

use maud::{Markup, PreEscaped, html};
use switchkit::{RelayState, Signal, Vendor};

use crate::fleet::DeviceView;

/// The IEC standby symbol as an inline SVG, the app's one brand mark (topbar,
/// login, empty states). `currentColor` so it takes the surrounding text or
/// accent color.
pub fn power_mark(size: u32) -> Markup {
    html! {
        (PreEscaped(format!(
            "<svg width=\"{size}\" height=\"{size}\" viewBox=\"0 0 24 24\" fill=\"none\" \
             stroke=\"currentColor\" stroke-width=\"2.4\" stroke-linecap=\"round\" \
             aria-hidden=\"true\"><path d=\"M12 3.5v8\"/><path d=\"M7 6.2a7.5 7.5 0 1 0 10 0\"/></svg>"
        )))
    }
}

/// Flush a run of connected sample points into path data: a polyline (plus a
/// soft area fill down to the baseline) for two or more points, a short
/// horizontal tick for an isolated point (so a lone real sample between gaps
/// is still visible without fabricating a line).
fn flush_run(
    run: &mut Vec<(f64, f64)>,
    baseline: f64,
    lines: &mut Vec<String>,
    areas: &mut Vec<String>,
) {
    match run.len() {
        0 => {}
        1 => {
            let (x, y) = run[0];
            lines.push(format!("M{:.1} {y:.1}L{:.1} {y:.1}", x - 0.8, x + 0.8));
        }
        _ => {
            let mut d = String::new();
            for (i, (x, y)) in run.iter().enumerate() {
                let cmd = if i == 0 { 'M' } else { 'L' };
                d.push_str(&format!("{cmd}{x:.1} {y:.1}"));
            }
            lines.push(d.clone());
            let (x0, _) = run[0];
            let (xn, _) = run[run.len() - 1];
            areas.push(format!(
                "M{x0:.1} {baseline:.1}{}L{xn:.1} {baseline:.1}Z",
                d.replacen('M', "L", 1)
            ));
        }
    }
    run.clear();
}

/// Server-rendered sparkline of recent power samples as a small inline SVG.
/// The series is right-anchored (newest sample at the right edge, history
/// filling leftward as ticks accumulate) and scaled against its own maximum
/// with a zero baseline, so the shape is proportional to absolute draw.
///
/// Honesty: `None` samples (offline / unpolled / non-metering ticks) BREAK
/// the line into gaps - never interpolated across, never drawn as zero (a
/// measured `0.0` IS drawn, as a line along the baseline). A series with no
/// real sample renders nothing at all rather than a fabricated flat line.
/// The numeric readouts remain the accessible data; the SVG is aria-hidden.
pub fn sparkline(series: &[Option<f64>], label: &str) -> Markup {
    const W: f64 = 100.0;
    const H: f64 = 32.0;
    const PAD: f64 = 2.5;
    if !series.iter().any(Option::is_some) {
        return html! {};
    }
    let cap = crate::history::CAPACITY.max(series.len());
    let n = series.len();
    let max = series.iter().flatten().fold(0.0_f64, |a, &b| a.max(b));
    let scale = if max > 0.0 { max } else { 1.0 };
    let x = |i: usize| (cap - n + i) as f64 / (cap - 1) as f64 * W;
    let y = |v: f64| H - PAD - (v / scale) * (H - 2.0 * PAD);
    let baseline = H - PAD;
    let mut lines: Vec<String> = Vec::new();
    let mut areas: Vec<String> = Vec::new();
    let mut run: Vec<(f64, f64)> = Vec::new();
    for (i, s) in series.iter().enumerate() {
        match s {
            Some(v) => run.push((x(i), y(*v))),
            None => flush_run(&mut run, baseline, &mut lines, &mut areas),
        }
    }
    flush_run(&mut run, baseline, &mut lines, &mut areas);
    html! {
        svg.sparkline viewBox=(format!("0 0 {W} {H}")) preserveAspectRatio="none" aria-hidden="true" {
            title { (label) }
            @for d in &areas {
                path.spark-area d=(d) fill="currentColor" fill-opacity="0.12" stroke="none" {}
            }
            @for d in &lines {
                path d=(d) fill="none" stroke="currentColor" stroke-width="1.5"
                    stroke-linejoin="round" stroke-linecap="round" vector-effect="non-scaling-stroke" {}
            }
        }
    }
}

/// Render `Some(v)` as-is, `None` as a muted "n/a" span. Never coerces an
/// absent value to `0` or an empty string.
pub fn na<T: Display>(v: Option<T>) -> Markup {
    match v {
        Some(v) => html! { (v) },
        None => html! { span.na { "n/a" } },
    }
}

/// Renders the device's on/off/unknown/offline badge. An offline badge
/// carries the scrubbed poll error as a tooltip when one is known, so the
/// failure reason is one hover away without cluttering the card.
pub fn state_badge(dev: &DeviceView) -> Markup {
    if !dev.is_online() {
        return html! { span.badge.offline title=[dev.error.as_deref()] { "offline" } };
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

/// Maps a Tasmota-style 0-100 signal-quality percentage to a 0..=4 filled-bar
/// count. Clamped so an out-of-range value never panics or produces a bogus
/// bar count.
fn quality_bars(pct: i64) -> i64 {
    match pct.clamp(0, 100) {
        0 => 0,
        1..=25 => 1,
        26..=50 => 2,
        51..=75 => 3,
        _ => 4,
    }
}

/// Maps a raw Wi-Fi RSSI dBm reading (Shelly's native unit) to a 0..=4
/// filled-bar count. dBm is negative and closer-to-zero is stronger; these
/// thresholds mirror the common "excellent/good/fair/weak" RSSI bands used
/// by most consumer Wi-Fi tooling. This is a bars mapping ONLY - it never
/// produces or implies a percentage, since dBm and quality-percent are not
/// interchangeable units and nothing here converts one into the other.
fn dbm_bars(dbm: i64) -> i64 {
    match dbm {
        d if d >= -55 => 4,
        d if d >= -65 => 3,
        d if d >= -75 => 2,
        d if d >= -85 => 1,
        _ => 0,
    }
}

/// Shared four-bar strength indicator markup, given how many of the four
/// bars are filled (0..=4).
fn signal_bars(filled: i64) -> Markup {
    html! {
        span.signal-bars aria-hidden="true" {
            @for i in 1..=4_i64 {
                span.bar.on[i <= filled] {}
            }
        }
    }
}

/// Renders a Wi-Fi/radio signal indicator from a vendor-neutral `Signal`.
/// Tasmota devices report `quality_percent` (0-100%); Shelly devices report
/// `rssi_dbm` (raw dBm). Each renders in its OWN real unit via its own bars
/// mapping - never fabricated from the other, and never converted between
/// them. `None` (device offline, not yet polled, or a signal reading with
/// neither field set) renders the muted `n/a`, never a fabricated bar.
pub fn signal_indicator(signal: Option<&Signal>) -> Markup {
    let Some(signal) = signal else {
        return na::<i64>(None);
    };
    if let Some(pct) = signal.quality_percent {
        let pct = i64::from(pct);
        let filled = quality_bars(pct);
        let label = format!("Wi-Fi signal {pct}%");
        return html! {
            span.signal title=(label) aria-label=(label) {
                (signal_bars(filled))
                span.signal-pct { (pct) "%" }
            }
        };
    }
    if let Some(dbm) = signal.rssi_dbm {
        let filled = dbm_bars(dbm);
        let label = format!("Wi-Fi signal {dbm} dBm");
        return html! {
            span.signal title=(label) aria-label=(label) {
                (signal_bars(filled))
                span.signal-dbm { (dbm) " dBm" }
            }
        };
    }
    na::<i64>(None)
}

/// A small, visually muted vendor tag ("Tasmota"/"Shelly") shown on the
/// dashboard card and the device detail header, so a mixed fleet is
/// identifiable at a glance. `Vendor` is `#[non_exhaustive]`, so an unknown
/// future variant renders "Unknown" rather than failing to compile - it
/// never falsely claims a specific known vendor.
pub fn vendor_tag(vendor: Vendor) -> Markup {
    let label = match vendor {
        Vendor::Tasmota => "Tasmota",
        Vendor::Shelly => "Shelly",
        _ => "Unknown",
    };
    html! { span.vendor-tag { (label) } }
}

/// Where a relay-toggle form should apply the card fragment the toggle route
/// returns.
pub enum ToggleTarget<'a> {
    /// Replace the dashboard card (`#card-{id}`) with the returned card.
    Card(&'a str),
    /// Discard the returned card fragment (`hx-swap="none"`): the device
    /// detail page re-renders its own live region instead. The response's
    /// OOB toast/modal swaps still apply.
    Discard,
}

/// The device's primary relay toggle control (the DEFAULT relay, no explicit
/// channel), labeled "Power": a real switch when the relay state is
/// CONFIRMED by the last successful STATUS read, a plain "Toggle" button
/// when the device is online but the relay is unknown, and a disabled button
/// when the device is unreachable. A switch never fakes a position: its
/// checked state comes from live status only, exactly like `state_badge`.
pub fn relay_control(dev: &DeviceView, target: ToggleTarget) -> Markup {
    let relay = if dev.is_online() {
        dev.status
            .as_ref()
            .and_then(|s| s.relays.first())
            .map(|r| &r.state)
    } else {
        None
    };
    toggle_control(dev, relay, None, Some("Power"), target)
}

/// A per-relay switch for a specific channel (the detail page's relay rows).
/// Posts the same `/device/:id/toggle` route with an explicit `relay` field;
/// the row already names the relay, so no extra label is rendered.
pub fn relay_channel_control(
    dev: &DeviceView,
    relay: &switchkit::Relay,
    target: ToggleTarget,
) -> Markup {
    toggle_control(dev, Some(&relay.state), Some(relay.index), None, target)
}

fn toggle_control(
    dev: &DeviceView,
    relay: Option<&RelayState>,
    channel: Option<u8>,
    label: Option<&str>,
    target: ToggleTarget,
) -> Markup {
    let name = dev.display_name();
    let subject = match channel {
        Some(idx) => format!("{name} relay {idx}"),
        None => name.to_string(),
    };
    let (hx_target, hx_swap, on_detail_page) = match target {
        ToggleTarget::Card(id) => (Some(format!("#card-{id}")), "outerHTML", false),
        ToggleTarget::Discard => (None, "none", true),
    };
    html! {
        form.control-row.device-toggle[on_detail_page]
            hx-post=(format!("/device/{}/toggle", dev.id))
            hx-target=[hx_target]
            hx-swap=(hx_swap) {
            @if let Some(idx) = channel {
                input type="hidden" name="relay" value=(idx);
            }
            @if let Some(label) = label {
                span.control-label { (label) }
            }
            @match relay {
                Some(RelayState::On) => {
                    button.switch type="submit" role="switch" aria-checked="true"
                        aria-label=(format!("Turn {subject} off")) {}
                }
                Some(RelayState::Off) => {
                    button.switch type="submit" role="switch" aria-checked="false"
                        aria-label=(format!("Turn {subject} on")) {}
                }
                Some(RelayState::Unknown(_)) => {
                    button.btn-toggle type="submit" { "Toggle" }
                }
                None => {
                    button.btn-toggle type="submit" disabled[!dev.is_online()] { "Toggle" }
                }
            }
        }
    }
}

/// A confirmation modal, rendered as an OUT-OF-BAND swap into the layout's `#modal`
/// placeholder, so opening it never disturbs the page or the card. The confirm form
/// re-posts `action` with `confirmed=true` plus `hidden` (the original validated
/// payload) and applies the response to `target` with `swap` (e.g. the card
/// `#card-{id}` with `outerHTML`, or the console log `#console-log` with
/// `beforeend`). Values are auto-escaped by maud. NEVER pass credentials
/// through here.
pub fn confirm_modal(
    title: &str,
    action: &str,
    hidden: &[(&str, &str)],
    target: &str,
    swap: &str,
) -> Markup {
    html! {
        div id="modal" hx-swap-oob="true" {
            div.modal-backdrop {
                div.modal role="dialog" aria-modal="true" aria-labelledby="modal-title" {
                    h2 id="modal-title" { (title) }
                    form hx-post=(action) hx-target=(target) hx-swap=(swap) {
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
///
/// The Undo button discards its direct response (`hx-swap="none"`) instead of
/// targeting `#card-{id}`: the toast appears on BOTH the dashboard and the
/// device detail page, and on the latter there is no card to target - htmx
/// would raise `targetError` and never send the request. The toggle route's
/// `state.notify()` pushes the updated card over SSE immediately, and the
/// `device-toggle` class makes the detail page's live region refresh itself
/// (see `app.js`), so both surfaces update without a direct swap.
///
/// `channel` is echoed back so undoing a per-relay toggle switches the SAME
/// relay, never the default one.
pub fn undo_toast(id: &str, channel: Option<u8>, new_state: &str) -> Markup {
    let vals = match channel {
        Some(idx) => format!(r#"{{"confirmed":"true","relay":"{idx}"}}"#),
        None => r#"{"confirmed":"true"}"#.to_string(),
    };
    html! {
        div id="toasts" hx-swap-oob="beforeend:#toasts" {
            div.toast {
                span { "Switched to " (new_state) }
                button.undo.device-toggle
                    hx-post=(format!("/device/{id}/toggle"))
                    hx-vals=(vals)
                    hx-swap="none" { "Undo" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use switchkit::{Signal, Vendor};

    use super::{signal_indicator, sparkline, vendor_tag};

    /// Count of stroked line paths (the data lines, as opposed to the soft
    /// area fills that accompany them).
    fn line_paths(html: &str) -> usize {
        html.matches(r#"fill="none""#).count()
    }

    /// A gap in the series must BREAK the line (two separate paths), never be
    /// interpolated across or drawn as a zero point.
    #[test]
    fn sparkline_renders_gaps_as_gaps_never_interpolated() {
        let series = [Some(1.0), Some(2.0), None, Some(3.0), Some(1.0)];
        let html = sparkline(&series, "test").into_string();
        assert_eq!(
            line_paths(&html),
            2,
            "a None sample must split the line into two paths: {html}"
        );
    }

    /// A series with no real sample renders NOTHING - a fabricated flat line
    /// would read as "measured zero the whole time".
    #[test]
    fn sparkline_renders_nothing_without_a_real_sample() {
        assert_eq!(sparkline(&[], "test").into_string(), "");
        assert_eq!(sparkline(&[None, None, None], "test").into_string(), "");
    }

    /// Real measured zeros ARE data and draw a line along the baseline.
    #[test]
    fn sparkline_draws_real_zeros() {
        let html = sparkline(&[Some(0.0), Some(0.0)], "test").into_string();
        assert_eq!(line_paths(&html), 1, "{html}");
    }

    /// An isolated sample between gaps still renders (as a short tick, with
    /// no area - there is no span to fill).
    #[test]
    fn sparkline_shows_isolated_samples() {
        let html = sparkline(&[None, Some(5.0), None], "test").into_string();
        assert_eq!(line_paths(&html), 1, "{html}");
        assert!(!html.contains("spark-area"), "{html}");
    }

    #[test]
    fn signal_indicator_shows_percentage_bars_and_label() {
        let signal = Signal::from_quality_percent(68);
        let html = signal_indicator(Some(&signal)).into_string();
        assert!(html.contains("68%"), "shows the quality as a percentage");
        assert!(
            html.contains("Wi-Fi signal 68%"),
            "carries an accessible label"
        );
        assert!(!html.contains("dBm"), "never fabricates a dBm value");
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
        let full = Signal::from_quality_percent(100);
        assert_eq!(
            signal_indicator(Some(&full))
                .into_string()
                .matches("bar on")
                .count(),
            4
        );
        let empty = Signal::from_quality_percent(0);
        assert_eq!(
            signal_indicator(Some(&empty))
                .into_string()
                .matches("bar on")
                .count(),
            0
        );
    }

    /// A Shelly-style dBm reading renders its own unit, never a fabricated
    /// percentage: this is the honesty invariant the whole rewrite exists for.
    #[test]
    fn signal_indicator_shows_dbm_bars_and_label_never_a_percentage() {
        let signal = Signal::from_dbm(-60);
        let html = signal_indicator(Some(&signal)).into_string();
        assert!(html.contains("-60 dBm"), "shows the raw dBm value");
        assert!(
            html.contains("Wi-Fi signal -60 dBm"),
            "carries an accessible label"
        );
        assert!(
            !html.contains('%'),
            "never fabricates a percentage from dBm"
        );
        // -60 falls in the -65..=-55 band: three of the four bars.
        assert_eq!(html.matches("bar on").count(), 3);
    }

    #[test]
    fn signal_indicator_dbm_scales_across_bands() {
        let strongest = Signal::from_dbm(-40);
        assert_eq!(
            signal_indicator(Some(&strongest))
                .into_string()
                .matches("bar on")
                .count(),
            4
        );
        let weakest = Signal::from_dbm(-95);
        assert_eq!(
            signal_indicator(Some(&weakest))
                .into_string()
                .matches("bar on")
                .count(),
            0
        );
    }

    #[test]
    fn vendor_tag_shows_tasmota_and_shelly_labels() {
        assert!(
            vendor_tag(Vendor::Tasmota)
                .into_string()
                .contains("Tasmota")
        );
        assert!(vendor_tag(Vendor::Shelly).into_string().contains("Shelly"));
    }
}
