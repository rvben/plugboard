//! Prometheus `/metrics` exporter.
//!
//! `tasmota-web` already polls every configured device over HTTP, so exposing
//! that data as Prometheus metrics turns it into a drop-in exporter for a
//! Tasmota fleet, no MQTT broker and no separate exporter process needed.
//!
//! The guiding rule is the same one the rest of the app follows: absent data
//! must not become a plausible value. A metric series is emitted ONLY when its
//! value is actually known. An offline device emits none of its telemetry
//! series at all (Prometheus marks them stale rather than reading a fabricated
//! `0`), and a relay in `Unknown` state emits no `relay_state` series rather
//! than a guessed on/off. The one series that is ALWAYS emitted per configured
//! device is `tasmota_web_device_reachable`, because reachability is always
//! known: the last poll either succeeded or it did not.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Mutex, PoisonError};

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use switchkit::{DeviceSnapshot, RelayState, Vendor};

use crate::fleet::{DeviceView, Fleet};
use crate::state::AppState;

/// Accumulating poll-outcome counters for one device, keyed by device id in
/// `MetricsState` (not by position in the fleet), so a settings rename does
/// not touch them and a device removed then re-added under the same host
/// picks its counters back up rather than resetting to zero.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeviceMetrics {
    pub poll_success: u64,
    pub poll_error: u64,
    /// Unix timestamp of the last successful poll. `None` until this device
    /// has ever had one, so `render` can omit the timestamp series entirely
    /// rather than claim a success time that never happened.
    pub last_success_unix: Option<u64>,
}

/// Poll-outcome counters for every device, surviving fleet rebuilds (a
/// settings change replaces `AppState::Inner::fleet`'s `Fleet`, but this map
/// lives alongside it in `Inner` and is never cleared by that rebuild).
pub type MetricsState = Mutex<HashMap<String, DeviceMetrics>>;

/// Escapes a label value per the Prometheus text exposition format: a
/// backslash must come first (otherwise the escapes it introduces would
/// themselves be escaped), then the quote, then the newline. Device names and
/// hosts are user/device-controlled, so an unescaped `"` in a name would
/// otherwise break the format for every series after it.
pub fn escape_label(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

fn device_labels(host: &str, name: &str) -> String {
    format!(
        r#"host="{}",name="{}""#,
        escape_label(host),
        escape_label(name)
    )
}

/// Cumulative energy in kWh, guarded exactly like `DeviceView::power_w` /
/// `today_kwh`: `None` while offline (never a stale reading) or when the
/// device has no energy sensor (never a fabricated `0`).
fn total_kwh(dev: &DeviceView) -> Option<f64> {
    if !dev.reachable {
        return None;
    }
    dev.status
        .as_ref()
        .and_then(|s| s.energy.as_ref())
        .and_then(|e| e.total_kwh)
}

/// Each relay's `(index, 1|0)` pair, from the last successful status ONLY.
/// Offline devices and a device with no status yield no relays at all; a
/// relay whose state could not be confidently mapped to on/off
/// (`RelayState::Unknown`) is skipped rather than guessed.
fn relay_states(dev: &DeviceView) -> Vec<(u8, u8)> {
    if !dev.reachable {
        return Vec::new();
    }
    let Some(status) = dev.status.as_ref() else {
        return Vec::new();
    };
    status
        .relays
        .iter()
        .filter_map(|r| match r.state {
            RelayState::On => Some((r.index, 1)),
            RelayState::Off => Some((r.index, 0)),
            RelayState::Unknown(_) => None,
        })
        .collect()
}

/// Records the outcome of one poll tick into `metrics_state`, joined by device
/// id. Every target produces exactly one outcome: a target present in
/// `updates` records its `Ok`/`Err` result; a target missing from `updates`
/// (its poll task panicked or was otherwise lost) counts as an error too,
/// mirroring how `poller::apply_results` treats it as offline. A poll that
/// produced no result is not silently invisible in the metrics either.
///
/// `now_unix` is `None` only when the system clock read before the Unix
/// epoch; a success still increments `poll_success`, but `last_success_unix`
/// is left as it was rather than fabricated as epoch-0.
///
/// `targets` is the full current set of polled device ids for this tick (one
/// entry per device configured in the fleet), so afterward any `metrics_state`
/// entry whose id is not in `targets` belongs to a device removed from the
/// fleet (e.g. via Settings) and is pruned. This is the only place counters
/// are ever dropped, keeping the map from growing unboundedly across repeated
/// add/remove churn; every tick polls all current devices, so a removed
/// device's counters are gone within one poll interval.
///
/// Pure and synchronous (the mutex is held only for the duration of the
/// updates, never across an `.await`), so it is trivially unit-testable
/// without any device I/O.
pub fn record_poll_outcomes(
    metrics_state: &MetricsState,
    targets: &[(String, String, Vendor)],
    updates: &[(String, switchkit::Result<DeviceSnapshot>)],
    now_unix: Option<u64>,
) {
    let mut metrics = metrics_state.lock().unwrap_or_else(PoisonError::into_inner);
    let mut seen = std::collections::HashSet::with_capacity(updates.len());
    for (id, result) in updates {
        seen.insert(id.as_str());
        let entry = metrics.entry(id.clone()).or_default();
        if result.is_ok() {
            entry.poll_success += 1;
            if let Some(secs) = now_unix {
                entry.last_success_unix = Some(secs);
            }
        } else {
            entry.poll_error += 1;
        }
    }
    for (id, _host, _vendor) in targets {
        if seen.contains(id.as_str()) {
            continue;
        }
        metrics.entry(id.clone()).or_default().poll_error += 1;
    }

    let current_ids: std::collections::HashSet<&str> =
        targets.iter().map(|(id, _, _)| id.as_str()).collect();
    metrics.retain(|id, _| current_ids.contains(id.as_str()));
}

/// Renders the full Prometheus text exposition (format version `0.0.4`) for
/// the current fleet. Each metric family's `# HELP`/`# TYPE` lines are
/// written once, followed by that family's series for every device that
/// qualifies; a device that does not qualify for a given family (offline, no
/// sensor, unpolled) contributes no line to it at all.
///
/// `metrics_state` is joined against `fleet` by device id: a counter for a
/// device no longer in the fleet (removed via Settings) is silently dropped
/// rather than rendered as an orphaned series.
pub fn render(fleet: &Fleet, metrics_state: &MetricsState, version: &str) -> String {
    let mut out = String::new();

    let _ = writeln!(
        out,
        "# HELP tasmota_web_build_info Static build information for this tasmota-web instance."
    );
    let _ = writeln!(out, "# TYPE tasmota_web_build_info gauge");
    let _ = writeln!(
        out,
        r#"tasmota_web_build_info{{version="{}"}} 1"#,
        escape_label(version)
    );

    let _ = writeln!(
        out,
        "# HELP tasmota_web_fleet_devices Number of devices configured in the fleet."
    );
    let _ = writeln!(out, "# TYPE tasmota_web_fleet_devices gauge");
    let _ = writeln!(out, "tasmota_web_fleet_devices {}", fleet.devices.len());

    let _ = writeln!(
        out,
        "# HELP tasmota_web_device_reachable Whether the last poll of this device succeeded (1) or failed (0). Always present for every configured device."
    );
    let _ = writeln!(out, "# TYPE tasmota_web_device_reachable gauge");
    for dev in &fleet.devices {
        let labels = device_labels(&dev.host, dev.display_name());
        let _ = writeln!(
            out,
            "tasmota_web_device_reachable{{{labels}}} {}",
            dev.reachable as u8
        );
    }

    let metrics_snapshot: HashMap<String, DeviceMetrics> = {
        let guard = metrics_state.lock().unwrap_or_else(PoisonError::into_inner);
        guard.clone()
    };
    let metrics_for = |id: &str| metrics_snapshot.get(id).copied().unwrap_or_default();

    let _ = writeln!(
        out,
        "# HELP tasmota_web_device_last_poll_success_timestamp_seconds Unix timestamp of the last successful poll of this device. Absent until the device has ever had one."
    );
    let _ = writeln!(
        out,
        "# TYPE tasmota_web_device_last_poll_success_timestamp_seconds gauge"
    );
    for dev in &fleet.devices {
        if let Some(ts) = metrics_for(&dev.id).last_success_unix {
            let labels = device_labels(&dev.host, dev.display_name());
            let _ = writeln!(
                out,
                "tasmota_web_device_last_poll_success_timestamp_seconds{{{labels}}} {ts}"
            );
        }
    }

    let _ = writeln!(
        out,
        "# HELP tasmota_web_device_poll_total Total number of poll attempts against this device, by result."
    );
    let _ = writeln!(out, "# TYPE tasmota_web_device_poll_total counter");
    for dev in &fleet.devices {
        let labels = device_labels(&dev.host, dev.display_name());
        let m = metrics_for(&dev.id);
        let _ = writeln!(
            out,
            "tasmota_web_device_poll_total{{{labels},result=\"success\"}} {}",
            m.poll_success
        );
        let _ = writeln!(
            out,
            "tasmota_web_device_poll_total{{{labels},result=\"error\"}} {}",
            m.poll_error
        );
    }

    let _ = writeln!(
        out,
        "# HELP tasmota_web_device_power_watts Live power draw in watts. Absent when offline or the device has no energy sensor."
    );
    let _ = writeln!(out, "# TYPE tasmota_web_device_power_watts gauge");
    for dev in &fleet.devices {
        if let Some(w) = dev.power_w() {
            let labels = device_labels(&dev.host, dev.display_name());
            let _ = writeln!(out, "tasmota_web_device_power_watts{{{labels}}} {w}");
        }
    }

    let _ = writeln!(
        out,
        "# HELP tasmota_web_device_energy_today_kwh Energy consumed today, in kWh. Absent when offline or the device has no energy sensor."
    );
    let _ = writeln!(out, "# TYPE tasmota_web_device_energy_today_kwh gauge");
    for dev in &fleet.devices {
        if let Some(kwh) = dev.today_kwh() {
            let labels = device_labels(&dev.host, dev.display_name());
            let _ = writeln!(out, "tasmota_web_device_energy_today_kwh{{{labels}}} {kwh}");
        }
    }

    let _ = writeln!(
        out,
        "# HELP tasmota_web_device_energy_total_kwh Cumulative energy consumed, in kWh. Absent when offline or the device has no energy sensor."
    );
    let _ = writeln!(out, "# TYPE tasmota_web_device_energy_total_kwh gauge");
    for dev in &fleet.devices {
        if let Some(kwh) = total_kwh(dev) {
            let labels = device_labels(&dev.host, dev.display_name());
            let _ = writeln!(out, "tasmota_web_device_energy_total_kwh{{{labels}}} {kwh}");
        }
    }

    let _ = writeln!(
        out,
        "# HELP tasmota_web_device_wifi_signal_percent Wi-Fi signal quality, 0-100. Absent when offline or not yet reported."
    );
    let _ = writeln!(out, "# TYPE tasmota_web_device_wifi_signal_percent gauge");
    for dev in &fleet.devices {
        if let Some(rssi) = dev.rssi() {
            let labels = device_labels(&dev.host, dev.display_name());
            let _ = writeln!(
                out,
                "tasmota_web_device_wifi_signal_percent{{{labels}}} {rssi}"
            );
        }
    }

    let _ = writeln!(
        out,
        "# HELP tasmota_web_device_relay_state Relay state, 1 = on, 0 = off. A relay in an unrecognized state emits no series."
    );
    let _ = writeln!(out, "# TYPE tasmota_web_device_relay_state gauge");
    for dev in &fleet.devices {
        let labels = device_labels(&dev.host, dev.display_name());
        for (index, value) in relay_states(dev) {
            let _ = writeln!(
                out,
                "tasmota_web_device_relay_state{{{labels},relay=\"{index}\"}} {value}"
            );
        }
    }

    out
}

/// `GET /metrics`. Registered outside `require_auth`/CSRF/session (see
/// `routes::router`) so a Prometheus scraper can reach it directly without a
/// login, in both `proxy` and `builtin` auth modes. Returns `404` when
/// `Config::metrics_enabled` is false.
pub async fn handler(State(state): State<AppState>) -> Response {
    let enabled = state.inner.config.read().await.metrics_enabled;
    if !enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Short read lock, clone what's needed, drop the lock before rendering:
    // never hold an async lock across the (synchronous, but non-trivial)
    // render work, and never across an `.await`.
    let fleet_snapshot = {
        let fleet = state.inner.fleet.read().await;
        fleet.clone()
    };
    let body = render(
        &fleet_snapshot,
        &state.inner.metrics,
        env!("CARGO_PKG_VERSION"),
    );
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use switchkit::{Energy, Relay, RelayState, Signal};

    use super::*;
    use crate::fleet::DeviceView;

    fn energy(power_w: Option<f64>, today_kwh: Option<f64>, total_kwh: Option<f64>) -> Energy {
        Energy {
            power_w,
            voltage_v: None,
            current_a: None,
            today_kwh,
            total_kwh,
        }
    }

    fn status(
        energy: Option<Energy>,
        signal_percent: Option<i64>,
        relays: Vec<Relay>,
    ) -> DeviceSnapshot {
        DeviceSnapshot {
            host: "192.0.2.1".into(),
            name: Some("Plug".into()),
            relays,
            energy,
            signal: signal_percent.map(Signal::from_quality_percent),
            uptime: Some("1T00:00:00".into()),
            ..Default::default()
        }
    }

    fn device(
        id: &str,
        host: &str,
        name: &str,
        reachable: bool,
        status: Option<DeviceSnapshot>,
    ) -> DeviceView {
        DeviceView {
            id: id.into(),
            name: name.into(),
            host: host.into(),
            protected: false,
            vendor: Vendor::Tasmota,
            reachable,
            status,
            error: None,
        }
    }

    #[test]
    fn escape_label_escapes_backslash_quote_and_newline() {
        let input = "back\\slash \"quote\" new\nline";
        let escaped = escape_label(input);
        assert_eq!(escaped, "back\\\\slash \\\"quote\\\" new\\nline");
    }

    #[test]
    fn escape_label_leaves_plain_text_untouched() {
        assert_eq!(escape_label("Living Room Lamp"), "Living Room Lamp");
    }

    /// (a) An OFFLINE device: `device_reachable` is emitted as `0`, and NONE
    /// of its telemetry series appear, even though its (stale, carried-over)
    /// `status` field is still populated - `render` must guard on `reachable`
    /// itself, not merely assume the caller already cleared `status`.
    #[test]
    fn offline_device_emits_reachable_zero_and_no_telemetry() {
        let dev = device(
            "d-1",
            "192.0.2.10",
            "Freezer",
            false,
            Some(status(
                Some(energy(Some(42.0), Some(1.5), Some(99.0))),
                Some(80),
                vec![Relay {
                    index: 0,
                    state: RelayState::On,
                    raw: "ON".into(),
                }],
            )),
        );
        let fleet = Fleet { devices: vec![dev] };
        let metrics_state: MetricsState = Mutex::new(HashMap::new());
        let text = render(&fleet, &metrics_state, "0.0.0-test");

        assert!(
            text.contains(r#"tasmota_web_device_reachable{host="192.0.2.10",name="Freezer"} 0"#),
            "text was:\n{text}"
        );
        assert!(
            !text.contains("tasmota_web_device_power_watts{host=\"192.0.2.10\""),
            "an offline device must never emit power_watts, real or fabricated; text was:\n{text}"
        );
        assert!(
            !text.contains("tasmota_web_device_energy_today_kwh{host=\"192.0.2.10\""),
            "text was:\n{text}"
        );
        assert!(
            !text.contains("tasmota_web_device_energy_total_kwh{host=\"192.0.2.10\""),
            "text was:\n{text}"
        );
        assert!(
            !text.contains("tasmota_web_device_wifi_signal_percent{host=\"192.0.2.10\""),
            "text was:\n{text}"
        );
        assert!(
            !text.contains("tasmota_web_device_relay_state{host=\"192.0.2.10\""),
            "text was:\n{text}"
        );
    }

    /// (b) A REACHABLE device with no energy sensor (`energy: None`): no
    /// `power_watts` (or `energy_*`) line, but `device_reachable` is `1`.
    #[test]
    fn reachable_device_without_energy_emits_no_power_line() {
        let dev = device(
            "d-2",
            "192.0.2.11",
            "Sensor",
            true,
            Some(status(None, Some(70), Vec::new())),
        );
        let fleet = Fleet { devices: vec![dev] };
        let metrics_state: MetricsState = Mutex::new(HashMap::new());
        let text = render(&fleet, &metrics_state, "0.0.0-test");

        assert!(
            text.contains(r#"tasmota_web_device_reachable{host="192.0.2.11",name="Sensor"} 1"#),
            "text was:\n{text}"
        );
        assert!(
            !text.contains("tasmota_web_device_power_watts{host=\"192.0.2.11\""),
            "a device with no energy sensor must never emit a fabricated power_watts 0; text was:\n{text}"
        );
        assert!(
            !text.contains("tasmota_web_device_energy_today_kwh{host=\"192.0.2.11\""),
            "text was:\n{text}"
        );
        assert!(
            !text.contains("tasmota_web_device_energy_total_kwh{host=\"192.0.2.11\""),
            "text was:\n{text}"
        );
    }

    /// (c) A REACHABLE device with a relay in `Unknown` state: no
    /// `relay_state` series for that relay index, never a guessed 0/1.
    #[test]
    fn unknown_relay_state_emits_no_relay_line() {
        let dev = device(
            "d-3",
            "192.0.2.12",
            "Switch",
            true,
            Some(status(
                None,
                None,
                vec![Relay {
                    index: 0,
                    state: RelayState::Unknown("Blink".into()),
                    raw: "Blink".into(),
                }],
            )),
        );
        let fleet = Fleet { devices: vec![dev] };
        let metrics_state: MetricsState = Mutex::new(HashMap::new());
        let text = render(&fleet, &metrics_state, "0.0.0-test");

        assert!(
            !text.contains("tasmota_web_device_relay_state{host=\"192.0.2.12\""),
            "an Unknown relay state must never be guessed as on(1) or off(0); text was:\n{text}"
        );
    }

    /// 2b. A REACHABLE device WITH energy present: `power_watts` (and the kWh
    /// series) carry the real value, proving the guard isn't simply "always
    /// absent" - it must actually pass the reading through when one exists.
    #[test]
    fn reachable_device_with_energy_emits_real_values() {
        let dev = device(
            "d-4",
            "192.0.2.13",
            "Lamp",
            true,
            Some(status(
                Some(energy(Some(42.5), Some(1.25), Some(87.0))),
                Some(90),
                vec![
                    Relay {
                        index: 0,
                        state: RelayState::On,
                        raw: "ON".into(),
                    },
                    Relay {
                        index: 1,
                        state: RelayState::Off,
                        raw: "OFF".into(),
                    },
                ],
            )),
        );
        let fleet = Fleet { devices: vec![dev] };
        let metrics_state: MetricsState = Mutex::new(HashMap::new());
        let text = render(&fleet, &metrics_state, "0.0.0-test");

        assert!(
            text.contains(r#"tasmota_web_device_power_watts{host="192.0.2.13",name="Lamp"} 42.5"#),
            "text was:\n{text}"
        );
        assert!(
            text.contains(
                r#"tasmota_web_device_energy_today_kwh{host="192.0.2.13",name="Lamp"} 1.25"#
            ),
            "text was:\n{text}"
        );
        assert!(
            text.contains(
                r#"tasmota_web_device_energy_total_kwh{host="192.0.2.13",name="Lamp"} 87"#
            ),
            "text was:\n{text}"
        );
        assert!(
            text.contains(
                r#"tasmota_web_device_wifi_signal_percent{host="192.0.2.13",name="Lamp"} 90"#
            ),
            "text was:\n{text}"
        );
        assert!(
            text.contains(
                r#"tasmota_web_device_relay_state{host="192.0.2.13",name="Lamp",relay="0"} 1"#
            ),
            "text was:\n{text}"
        );
        assert!(
            text.contains(
                r#"tasmota_web_device_relay_state{host="192.0.2.13",name="Lamp",relay="1"} 0"#
            ),
            "text was:\n{text}"
        );
    }

    /// Label escaping end to end: a device named with an embedded quote
    /// renders a validly-escaped `name="Kit\"chen"`, not a broken exposition.
    #[test]
    fn device_name_with_quote_is_escaped_in_rendered_labels() {
        let dev = device("d-5", "192.0.2.14", "Kit\"chen", true, None);
        let fleet = Fleet { devices: vec![dev] };
        let metrics_state: MetricsState = Mutex::new(HashMap::new());
        let text = render(&fleet, &metrics_state, "0.0.0-test");

        assert!(text.contains(r#"name="Kit\"chen""#), "text was:\n{text}");
        // The unescaped raw quote pattern must never appear on its own.
        assert!(!text.contains(r#"name="Kit"chen""#), "text was:\n{text}");
    }

    /// Counters accumulate across ticks and survive a target that produced no
    /// result at all (its poll task panicked/was lost); `render` reflects the
    /// running totals and only emits the last-success timestamp once a
    /// success has actually happened.
    #[test]
    fn record_poll_outcomes_counts_success_and_error_and_render_reflects_them() {
        let metrics_state: MetricsState = Mutex::new(HashMap::new());
        let targets = vec![
            ("d-1".to_string(), "192.0.2.20".to_string(), Vendor::Tasmota),
            ("d-2".to_string(), "192.0.2.21".to_string(), Vendor::Tasmota),
        ];

        // Tick 1: d-1 succeeds, d-2 errors.
        let updates: Vec<(String, switchkit::Result<DeviceSnapshot>)> = vec![
            ("d-1".to_string(), Ok(status(None, None, Vec::new()))),
            (
                "d-2".to_string(),
                Err(switchkit::Error::Network {
                    host: "192.0.2.21".into(),
                    message: "connection refused".into(),
                }),
            ),
        ];
        record_poll_outcomes(&metrics_state, &targets, &updates, Some(1_000));

        // Tick 2: d-1 succeeds again; d-2's poll task is presumed lost (absent
        // from `updates` entirely, not merely an `Err`).
        let updates2: Vec<(String, switchkit::Result<DeviceSnapshot>)> =
            vec![("d-1".to_string(), Ok(status(None, None, Vec::new())))];
        record_poll_outcomes(&metrics_state, &targets, &updates2, Some(2_000));

        let fleet = Fleet {
            devices: vec![
                device("d-1", "192.0.2.20", "A", true, None),
                device("d-2", "192.0.2.21", "B", false, None),
            ],
        };
        let text = render(&fleet, &metrics_state, "0.0.0-test");

        assert!(
            text.contains(
                r#"tasmota_web_device_poll_total{host="192.0.2.20",name="A",result="success"} 2"#
            ),
            "text was:\n{text}"
        );
        assert!(
            text.contains(
                r#"tasmota_web_device_poll_total{host="192.0.2.20",name="A",result="error"} 0"#
            ),
            "text was:\n{text}"
        );
        assert!(
            text.contains(
                r#"tasmota_web_device_poll_total{host="192.0.2.21",name="B",result="error"} 2"#
            ),
            "the lost-task tick must count as an error too, text was:\n{text}"
        );
        assert!(
            text.contains(
                r#"tasmota_web_device_last_poll_success_timestamp_seconds{host="192.0.2.20",name="A"} 2000"#
            ),
            "text was:\n{text}"
        );
        assert!(
            !text.contains(
                "tasmota_web_device_last_poll_success_timestamp_seconds{host=\"192.0.2.21\""
            ),
            "a device with zero successes must never get a fabricated last-success timestamp; text was:\n{text}"
        );
    }

    /// Removing a device from the fleet (e.g. via Settings) must drop its
    /// counters from the render, not leak them as an orphaned series with no
    /// corresponding `device_reachable` line.
    #[test]
    fn render_drops_counters_for_devices_no_longer_in_the_fleet() {
        let metrics_state: MetricsState = Mutex::new(HashMap::new());
        let targets = vec![("d-1".to_string(), "192.0.2.22".to_string(), Vendor::Tasmota)];
        let updates: Vec<(String, switchkit::Result<DeviceSnapshot>)> =
            vec![("d-1".to_string(), Ok(status(None, None, Vec::new())))];
        record_poll_outcomes(&metrics_state, &targets, &updates, Some(1_000));

        // The fleet no longer contains d-1 (removed via Settings).
        let fleet = Fleet { devices: vec![] };
        let text = render(&fleet, &metrics_state, "0.0.0-test");

        assert!(
            !text.contains("192.0.2.22"),
            "an orphaned counter must not appear once its device leaves the fleet; text was:\n{text}"
        );
    }

    /// `record_poll_outcomes` itself prunes `metrics_state`, not just
    /// `render`: an entry for a device id that is no longer part of the
    /// current tick's `targets` (removed via Settings) must be gone from the
    /// map after the call, while a real device's counters survive untouched.
    /// This fails if the `retain` at the end of `record_poll_outcomes` is
    /// removed: the seeded "orphan" entry would still be present afterward.
    #[test]
    fn record_poll_outcomes_prunes_orphaned_device_ids() {
        let metrics_state: MetricsState = Mutex::new(HashMap::new());
        {
            let mut guard = metrics_state.lock().unwrap();
            guard.insert(
                "orphan".to_string(),
                DeviceMetrics {
                    poll_success: 5,
                    poll_error: 1,
                    last_success_unix: Some(500),
                },
            );
        }

        let targets = vec![("d-1".to_string(), "192.0.2.40".to_string(), Vendor::Tasmota)];
        let updates: Vec<(String, switchkit::Result<DeviceSnapshot>)> =
            vec![("d-1".to_string(), Ok(status(None, None, Vec::new())))];
        record_poll_outcomes(&metrics_state, &targets, &updates, Some(1_000));

        let guard = metrics_state.lock().unwrap();
        assert!(
            !guard.contains_key("orphan"),
            "a device id no longer among the current targets must be pruned from metrics_state"
        );
        let d1 = guard.get("d-1").expect("d-1 counters present");
        assert_eq!(d1.poll_success, 1);
        assert_eq!(d1.poll_error, 0);
        assert_eq!(d1.last_success_unix, Some(1_000));
    }

    #[test]
    fn build_info_and_fleet_devices_are_always_present() {
        let fleet = Fleet {
            devices: vec![device("d-1", "192.0.2.23", "A", true, None)],
        };
        let metrics_state: MetricsState = Mutex::new(HashMap::new());
        let text = render(&fleet, &metrics_state, "1.2.3");

        assert!(text.contains(r#"tasmota_web_build_info{version="1.2.3"} 1"#));
        assert!(text.contains("tasmota_web_fleet_devices 1"));
    }
}
