use maud::{Markup, html};
use switchkit::{Capabilities, DeviceSnapshot, Vendor};

use crate::fleet::DeviceView;
use crate::history::Series;
use crate::updates::UpdateInfo;
use crate::views::components::{
    ToggleTarget, na, power_chart, relay_channel_control, relay_control, signal_indicator,
    state_badge, vendor_tag,
};

/// The device's live status, or `None` when offline. Centralizes the same
/// `reachable` guard `DeviceView::power_w`/`today_kwh`/`rssi` already apply,
/// so every section below reads live data through one place: an offline
/// device never leaks a stale `status` as though it were current.
fn live_status(dev: &DeviceView) -> Option<&DeviceSnapshot> {
    if dev.is_online() {
        dev.status.as_ref()
    } else {
        None
    }
}

/// The device's confirmed capabilities, or all-`false` (`Capabilities`'
/// `Default`) when offline/unpolled. Every capability-gated section below
/// reads through here so a capability is never claimed for a device we
/// currently have no live snapshot to confirm it from - a gated section is
/// simply absent rather than shown as a broken/disabled control.
fn capabilities(dev: &DeviceView) -> Capabilities {
    live_status(dev).map(|s| s.capabilities).unwrap_or_default()
}

/// Renders a raw vendor uptime string as a human duration, keeping the raw
/// value as a tooltip. Tasmota reports `"11T02:03:04"` (days T H:M:S);
/// Shelly reports bare seconds (`"987654"`). Anything unrecognized renders
/// verbatim - a presentation nicety must never fabricate or drop a value.
fn humanize_uptime(raw: &str) -> String {
    fn fmt(days: u64, hours: u64, minutes: u64, seconds: u64) -> String {
        if days > 0 {
            format!("{days}d {hours}h {minutes}m")
        } else if hours > 0 {
            format!("{hours}h {minutes}m")
        } else if minutes > 0 {
            format!("{minutes}m {seconds}s")
        } else {
            format!("{seconds}s")
        }
    }
    if let Some((days, clock)) = raw.split_once('T') {
        let parts: Vec<&str> = clock.split(':').collect();
        if let (Ok(d), [h, m, s]) = (days.parse::<u64>(), parts.as_slice())
            && let (Ok(h), Ok(m), Ok(s)) = (h.parse::<u64>(), m.parse::<u64>(), s.parse::<u64>())
        {
            return fmt(d, h, m, s);
        }
    }
    if let Ok(total) = raw.parse::<u64>() {
        return fmt(
            total / 86_400,
            (total % 86_400) / 3_600,
            (total % 3_600) / 60,
            total % 60,
        );
    }
    raw.to_string()
}

/// How long ago an update check ran, as a compact span ("5m", "2h 10m").
/// Falls back to "0s" if the clock reads before the check time (skew).
fn checked_ago(update: &UpdateInfo) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(update.checked_unix);
    crate::views::components::fmt_span(now.saturating_sub(update.checked_unix))
}

/// The hero: identity (name, host, vendor), live state + the primary relay
/// switch, and - for metering devices only - the energy cluster: a big live
/// draw readout, the recent power sparkline, and the meter stats. A device
/// that has not CONFIRMED `capabilities.metering` gets no energy cluster at
/// all (not even one full of `n/a`): showing it would imply a capability the
/// device does not have.
fn hero(dev: &DeviceView, history: &Series) -> Markup {
    let series = history.device(&dev.id);
    let energy = live_status(dev).and_then(|s| s.energy.as_ref());
    html! {
        section.panel.device-hero {
            header.device-header {
                div.device-title {
                    h1 { (dev.display_name()) }
                    span.host { (dev.host) }
                    (vendor_tag(dev.vendor))
                }
                div.device-actions {
                    (state_badge(dev))
                    (relay_control(dev, ToggleTarget::Discard))
                }
            }
            @if capabilities(dev).metering {
                div.energy-cluster {
                    div.energy-now {
                        h2 { "Energy" }
                        div.energy-hero {
                            span.value { (na(energy.and_then(|e| e.power_w))) }
                            span.unit { "W" }
                        }
                        @if series.iter().any(Option::is_some) {
                            div.hero-spark { (power_chart(series, &history.ticks, "recent power draw")) }
                        }
                    }
                    dl.energy-stats {
                        dt { "Voltage" } dd { (na(energy.and_then(|e| e.voltage_v))) " V" }
                        dt { "Current" } dd { (na(energy.and_then(|e| e.current_a))) " A" }
                        dt { "Today" } dd { (na(energy.and_then(|e| e.today_kwh))) " kWh" }
                        // `switchkit`'s vendor-neutral `Energy` model carries no
                        // yesterday-kWh field for any vendor (Tasmota's own status
                        // response has one, but the async `SmartDevice` trait this
                        // app runs on does not surface it), so this row is
                        // permanently `n/a` rather than removed - a genuine,
                        // unavoidable behavior change from the old sync
                        // `tasmota-core` path, not a bug.
                        dt { "Yesterday" } dd { (na::<f64>(None)) " kWh" }
                        dt { "Total" } dd { (na(energy.and_then(|e| e.total_kwh))) " kWh" }
                    }
                }
            }
        }
    }
}

/// One row per relay, each with its OWN channel switch (posting the same
/// toggle route with an explicit `relay` field). The switch position comes
/// from the live status only, exactly like everything else; offline devices
/// show the offline badge instead of controls that could not honestly
/// render a position.
fn relays_section(dev: &DeviceView) -> Markup {
    let relays = live_status(dev).map(|s| s.relays.as_slice()).unwrap_or(&[]);
    html! {
        section.panel.relays {
            h2 { "Relays" }
            @if !dev.is_online() {
                p { span.badge.offline title=[dev.error.as_deref()] { "offline" } }
            } @else if relays.is_empty() {
                p.hint { "No relays reported." }
            } @else {
                ul {
                    @for relay in relays {
                        li {
                            span.relay-index { "Relay " (relay.index) }
                            (relay_channel_control(dev, relay, ToggleTarget::Discard))
                        }
                    }
                }
            }
        }
    }
}

fn network_section(dev: &DeviceView) -> Markup {
    let net = live_status(dev).map(|s| &s.net);
    html! {
        section.panel.network {
            h2 { "Network" }
            dl {
                dt { "IP" } dd { (na(net.and_then(|n| n.ip.clone()))) }
                dt { "MAC" } dd { (na(net.and_then(|n| n.mac.clone()))) }
                dt { "Hostname" } dd { (na(net.and_then(|n| n.hostname.clone()))) }
                dt { "Wi-Fi signal" } dd { (signal_indicator(dev.signal())) }
            }
        }
    }
}

/// Model and generation rows appear only when the device actually reported
/// them; the firmware row only when the device confirms
/// `capabilities.firmware_ota` (the word "Firmware" must not appear for a
/// device that hasn't); uptime always (n/a-honest), humanized with the raw
/// vendor string as a tooltip.
fn system_section(dev: &DeviceView, update: Option<&UpdateInfo>) -> Markup {
    let available = update.and_then(|u| u.available.as_deref());
    let status = live_status(dev);
    let model = status.and_then(|s| s.model.clone());
    let generation = status.and_then(|s| s.generation.clone());
    let firmware = status
        .and_then(|s| s.firmware.as_ref())
        .and_then(|f| f.version.clone());
    let uptime = status.and_then(|s| s.uptime.clone());
    html! {
        section.panel.system {
            h2 { "System" }
            dl {
                @if let Some(model) = model {
                    dt { "Model" } dd { (model) }
                }
                @if let Some(generation) = generation {
                    dt { "Generation" } dd { (generation) }
                }
                @if capabilities(dev).firmware_ota {
                    dt { "Firmware" }
                    dd {
                        (na(firmware))
                        @if let Some(v) = available {
                            // Jumps to the admin panel's Update action below.
                            " " a.update-tag href="#admin-firmware" { (v) " available" }
                        }
                    }
                }
                dt { "Uptime" }
                dd {
                    @match uptime {
                        Some(raw) => { span title=(raw) { (humanize_uptime(&raw)) } }
                        None => { (na::<String>(None)) }
                    }
                }
            }
        }
    }
}

/// Wraps admin-panel output in its single shared `#admin-result` region
/// (config get/set and firmware check/update results). Every route response
/// targeting it - a rendered result, an empty gated placeholder, or nothing
/// at all - is wrapped here, so every `hx-target="#admin-result"
/// hx-swap="outerHTML"` form always gets back an element it can swap itself
/// with. The console does NOT use this region: it appends entries to its own
/// `#console-log` (see `admin_panel`).
pub fn admin_result(content: Markup) -> Markup {
    html! { div id="admin-result" { (content) } }
}

/// The per-device admin panel: console, config get/set, firmware
/// check/update, a config backup download link, and a restore pointer.
/// The handler behind each destructive action (`routes::admin`) decides
/// whether to execute directly or return a confirm modal (an OOB swap into
/// `#modal`) instead, classifying every command through the SAME shared
/// `switchkit::guardrail::classify(dev.vendor, ..)` regardless of vendor - a
/// Tasmota console command and a Shelly RPC method go through identical
/// gating, never a vendor-specific bypass. `restore` has no route for any
/// vendor: its upload endpoint is unverified against a live device (see
/// `tasmota-cli`'s own `restore` refusal), so the panel points at the
/// device's own web UI instead of offering a control that could report a
/// false success.
///
/// The console renders as a terminal: each run APPENDS a command + response
/// entry to `#console-log` (`hx-swap="beforeend"`), so a session's command
/// history stays visible. Config and firmware results still swap the shared
/// `#admin-result` region.
///
/// Console+Config are gated on `capabilities.console`, Firmware on
/// `capabilities.firmware_ota`, Backup on `capabilities.config_backup` - a
/// device lacking a capability simply has no subsection for it, never a
/// visible-but-disabled control implying a capability it doesn't have. When
/// NONE of the three capabilities are confirmed (offline, unpolled, or a
/// device with no admin surface at all), the whole panel - including
/// `#admin-result` - is absent. This is exactly how Shelly Gen1 (no RPC
/// console, `capabilities.console == false`) simply has no console/config
/// subsection, without any vendor special-casing in this gate.
fn admin_panel(dev: &DeviceView, update: Option<&UpdateInfo>) -> Markup {
    let caps = capabilities(dev);
    if !caps.console && !caps.firmware_ota && !caps.config_backup {
        return html! {};
    }
    let id = &dev.id;
    // `Vendor` is `#[non_exhaustive]`; a future variant this app doesn't yet
    // know the command syntax for gets a vendor-neutral placeholder rather
    // than falsely implying Tasmota or Shelly syntax.
    let (console_heading, console_hint, console_placeholder) = match dev.vendor {
        Vendor::Tasmota => (
            "Console",
            "Runs a Tasmota console command on the device. Destructive commands ask for confirmation first.",
            "e.g. Status 8",
        ),
        Vendor::Shelly => (
            "RPC console",
            "Calls a Shelly RPC method on the device. Destructive methods ask for confirmation first.",
            "e.g. Shelly.GetStatus",
        ),
        _ => (
            "Console",
            "Runs a command on the device. Destructive commands ask for confirmation first.",
            "Enter a command",
        ),
    };
    html! {
        section.panel.admin-panel {
            h2 { "Admin" }
            @if caps.console {
                div.admin-section.admin-console {
                    h3 { (console_heading) }
                    p.hint { (console_hint) }
                    form hx-post=(format!("/device/{id}/console")) hx-target="#console-log" hx-swap="beforeend" {
                        div.field {
                            label for=(format!("console-{id}")) { "Command" }
                            input.mono type="text" id=(format!("console-{id}")) name="command" placeholder=(console_placeholder) required
                                autocomplete="off" autocapitalize="off" spellcheck="false";
                        }
                        button type="submit" { "Run" }
                    }
                    div.console-log id="console-log" {}
                }
                div.admin-section.admin-config {
                    h3 { "Config" }
                    p.hint { "Read or write one setting by name. Writes always ask for confirmation." }
                    form hx-post=(format!("/device/{id}/config/get")) hx-target="#admin-result" hx-swap="outerHTML" {
                        div.field {
                            label for=(format!("config-get-{id}")) { "Setting" }
                            input.mono type="text" id=(format!("config-get-{id}")) name="setting" placeholder="Setting name" required;
                        }
                        button type="submit" { "Get" }
                    }
                    form hx-post=(format!("/device/{id}/config/set")) hx-target="#admin-result" hx-swap="outerHTML" {
                        div.field {
                            label for=(format!("config-set-{id}")) { "Setting" }
                            input.mono type="text" id=(format!("config-set-{id}")) name="setting" placeholder="Setting name" required;
                        }
                        div.field {
                            label for=(format!("config-value-{id}")) { "Value" }
                            input.mono type="text" id=(format!("config-value-{id}")) name="value" placeholder="Value" required;
                        }
                        button type="submit" class="btn-danger" { "Set" }
                    }
                }
            }
            @if caps.firmware_ota {
                div.admin-section.admin-firmware id="admin-firmware" {
                    h3 { "Firmware" }
                    p.hint { "Check the running version, or flash new firmware over the air." }
                    @if let Some(u) = update {
                        @if let Some(v) = u.available.as_deref() {
                            p.update-notice {
                                "Version " (v) " is available (running " (u.current) ", checked " (checked_ago(u)) " ago)."
                            }
                            form hx-post=(format!("/device/{id}/firmware/update")) hx-target="#admin-result" hx-swap="outerHTML" {
                                button type="submit" class="btn-primary" { "Update to " (v) }
                            }
                        } @else {
                            p.hint { "Up to date (running " (u.current) ", checked " (checked_ago(u)) " ago)." }
                        }
                    }
                    form hx-post=(format!("/device/{id}/firmware/check")) hx-target="#admin-result" hx-swap="outerHTML" {
                        button type="submit" { "Check version" }
                    }
                    form.refreshes-live hx-post="/updates/check" hx-swap="none" {
                        button type="submit" { "Check for updates" }
                    }
                    form hx-post=(format!("/device/{id}/firmware/update")) hx-target="#admin-result" hx-swap="outerHTML" {
                        div.field {
                            label for=(format!("ota-{id}")) { "OTA URL (optional)" }
                            input.mono type="text" id=(format!("ota-{id}")) name="url" placeholder="Device default when empty";
                        }
                        button type="submit" class="btn-danger" { "Flash firmware" }
                    }
                }
            }
            @if caps.config_backup {
                div.admin-section.admin-backup {
                    h3 { "Backup" }
                    a.backup-link href=(format!("/device/{id}/backup")) { "Download config backup (.dmp)" }
                    p.hint {
                        "Restore is not offered here (its endpoint is unverified against a live device); use the device's own web UI (Configuration > Backup/Restore)."
                    }
                }
            }
            (admin_result(html! {}))
        }
    }
}

/// Renders the full device detail page: the instrument hero (identity, state,
/// primary switch, and - when metering is confirmed - the live draw, recent
/// power sparkline, and meter stats), per-relay switches, network and system
/// panels, and the admin panel. Every live field goes through `na()` (or the
/// offline branch above it), so an offline device or a device with a sparse
/// status never renders a coerced value. There is no MQTT section:
/// `switchkit`'s vendor-neutral `DeviceSnapshot` has no MQTT data model at
/// all, so a permanent-`n/a` MQTT section would only ever imply a capability
/// that doesn't exist.
///
/// The hero + status panels live in `#device-live`, which re-fetches itself
/// every `poll_secs` (and on the `refresh-live` event a toggle fires):
/// `hx-select` picks the same region out of the full page response, so the
/// detail page tracks the poller without a manual reload. The admin panel
/// deliberately sits OUTSIDE the live region - a refresh must never wipe
/// console history mid-read.
pub fn device_page(
    dev: &DeviceView,
    poll_secs: u64,
    history: &Series,
    update: Option<&UpdateInfo>,
) -> Markup {
    html! {
        div.device-detail {
            div.device-live id="device-live"
                hx-get=(format!("/device/{}", dev.id))
                hx-select="#device-live"
                hx-target="this"
                hx-swap="outerHTML"
                hx-trigger=(format!("every {poll_secs}s, refresh-live from:body")) {
                (hero(dev, history))
                div.device-panels {
                    (relays_section(dev))
                    (network_section(dev))
                    (system_section(dev, update))
                }
            }
            div id="admin-panel" { (admin_panel(dev, update)) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::humanize_uptime;

    /// Tasmota's `DDTHH:MM:SS` form and Shelly's bare-seconds form both
    /// humanize; anything unrecognized passes through verbatim rather than
    /// being dropped or guessed at.
    #[test]
    fn humanize_uptime_handles_both_vendor_forms_and_falls_back() {
        assert_eq!(humanize_uptime("11T02:03:04"), "11d 2h 3m");
        assert_eq!(humanize_uptime("0T00:05:09"), "5m 9s");
        assert_eq!(humanize_uptime("987654"), "11d 10h 20m");
        assert_eq!(humanize_uptime("59"), "59s");
        assert_eq!(humanize_uptime("weird-format"), "weird-format");
        assert_eq!(humanize_uptime("1T2:3"), "1T2:3");
    }
}
