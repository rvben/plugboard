use maud::{Markup, html};
use switchkit::{Capabilities, DeviceSnapshot, RelayState, Vendor};

use crate::fleet::DeviceView;
use crate::views::components::{
    ToggleTarget, na, relay_control, signal_indicator, state_badge, vendor_tag,
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

fn relay_badge(state: &RelayState) -> Markup {
    match state {
        RelayState::On => html! { span.badge.on { "on" } },
        RelayState::Off => html! { span.badge.off { "off" } },
        // A relay string we could not confidently map renders "unknown", never a guess.
        RelayState::Unknown(_) => html! { span.badge.unknown { "unknown" } },
    }
}

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
                            (relay_badge(&relay.state))
                        }
                    }
                }
            }
        }
    }
}

/// Rendered only when the device confirms `capabilities.metering` - a
/// non-metering device has no energy data model at all, so showing this
/// section (even full of `n/a`) would imply a capability the device does
/// not have. Leads with the live draw as a meter readout.
fn energy_section(dev: &DeviceView) -> Markup {
    if !capabilities(dev).metering {
        return html! {};
    }
    let energy = live_status(dev).and_then(|s| s.energy.as_ref());
    html! {
        section.panel.energy {
            h2 { "Energy" }
            div.energy-hero {
                span.value { (na(energy.and_then(|e| e.power_w))) }
                span.unit { "W" }
            }
            dl {
                dt { "Voltage" } dd { (na(energy.and_then(|e| e.voltage_v))) " V" }
                dt { "Current" } dd { (na(energy.and_then(|e| e.current_a))) " A" }
                dt { "Today" } dd { (na(energy.and_then(|e| e.today_kwh))) " kWh" }
                // `switchkit`'s vendor-neutral `Energy` model carries no
                // yesterday-kWh field for any vendor (Tasmota's own status
                // response has one, but the async `SmartDevice` trait this app
                // now runs on does not surface it), so this row is permanently
                // `n/a` rather than removed - a genuine, unavoidable behavior
                // change from the old sync `tasmota-core` path, not a bug.
                dt { "Yesterday" } dd { (na::<f64>(None)) " kWh" }
                dt { "Total" } dd { (na(energy.and_then(|e| e.total_kwh))) " kWh" }
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

/// Uptime always; the firmware row only when the device confirms
/// `capabilities.firmware_ota` (mirrors `energy_section`'s reasoning - the
/// word "Firmware" must not appear for a device that hasn't confirmed it).
fn system_section(dev: &DeviceView) -> Markup {
    let uptime = live_status(dev).and_then(|s| s.uptime.clone());
    let firmware = live_status(dev)
        .and_then(|s| s.firmware.as_ref())
        .and_then(|f| f.version.clone());
    html! {
        section.panel.system {
            h2 { "System" }
            dl {
                @if capabilities(dev).firmware_ota {
                    dt { "Firmware" } dd { (na(firmware)) }
                }
                dt { "Uptime" } dd { (na(uptime)) }
            }
        }
    }
}

/// Wraps admin-panel output in its single shared `#admin-result` region.
/// Every admin route (`routes::admin`) response - a rendered result, an
/// empty gated placeholder, or nothing at all - is wrapped here, so every
/// `hx-target="#admin-result" hx-swap="outerHTML"` form always gets back an
/// element it can swap itself with.
pub fn admin_result(content: Markup) -> Markup {
    html! { div id="admin-result" { (content) } }
}

/// The per-device admin panel: console, config get/set, firmware
/// check/update, a config backup download link, and a restore pointer.
/// Every form targets `#admin-result` with an `outerHTML` swap; the handler
/// behind each destructive action (`routes::admin`) decides whether to
/// execute directly or return a confirm modal (an OOB swap into `#modal`)
/// instead, classifying every command through the SAME shared
/// `switchkit::guardrail::classify(dev.vendor, ..)` regardless of vendor - a
/// Tasmota console command and a Shelly RPC method go through identical
/// gating, never a vendor-specific bypass. `restore` has no route for any
/// vendor: its upload endpoint is unverified against a live device (see
/// `tasmota-cli`'s own `restore` refusal), so the panel points at the
/// device's own web UI instead of offering a control that could report a
/// false success.
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
///
/// The console subsection's copy differs per vendor (Tasmota: bare command
/// words like `Status 8`; Shelly: raw JSON-RPC method names like
/// `Shelly.GetStatus`), but both post the SAME `command` field to the SAME
/// `/device/:id/console` route, which dispatches via `SmartDevice::console`
/// and classifies via the shared guardrail either way - there is no
/// vendor-specific route or handler.
fn admin_panel(dev: &DeviceView) -> Markup {
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
                    form hx-post=(format!("/device/{id}/console")) hx-target="#admin-result" hx-swap="outerHTML" {
                        div.field {
                            label for=(format!("console-{id}")) { "Command" }
                            input.mono type="text" id=(format!("console-{id}")) name="command" placeholder=(console_placeholder) required;
                        }
                        button type="submit" { "Run" }
                    }
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
                div.admin-section.admin-firmware {
                    h3 { "Firmware" }
                    p.hint { "Check the running version, or flash new firmware over the air." }
                    form hx-post=(format!("/device/{id}/firmware/check")) hx-target="#admin-result" hx-swap="outerHTML" {
                        button type="submit" { "Check version" }
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

/// The page header: name, host, vendor - and the live state badge plus the
/// relay control, both derived from the last successful STATUS read only.
/// The toggle discards its card-fragment response (`ToggleTarget::Discard`);
/// `app.js` re-triggers the live region instead.
fn device_header(dev: &DeviceView) -> Markup {
    html! {
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
    }
}

/// Renders the full device detail page: header (with the relay control),
/// live status panels, and the admin panel (console/config/firmware/backup).
/// Every live field goes through `na()` (or the offline branch above it), so
/// an offline device or a device with a sparse status never renders a
/// coerced value. Energy, firmware, and the admin subsections are
/// capability-gated (see `capabilities`) and simply absent when the device
/// hasn't confirmed the matching capability - there is no MQTT section:
/// `switchkit`'s vendor-neutral `DeviceSnapshot` has no MQTT data model at
/// all, so a permanent-`n/a` MQTT section would only ever imply a capability
/// that doesn't exist.
///
/// The header + status panels live in `#device-live`, which re-fetches
/// itself every `poll_secs` (and on the `refresh-live` event a toggle
/// fires): `hx-select` picks the same region out of the full page response,
/// so the detail page tracks the poller without a manual reload. The admin
/// panel deliberately sits OUTSIDE the live region - a refresh must never
/// wipe console output mid-read.
pub fn device_page(dev: &DeviceView, poll_secs: u64) -> Markup {
    html! {
        div.device-detail {
            div.device-live id="device-live"
                hx-get=(format!("/device/{}", dev.id))
                hx-select="#device-live"
                hx-target="this"
                hx-swap="outerHTML"
                hx-trigger=(format!("every {poll_secs}s, refresh-live from:body")) {
                (device_header(dev))
                div.device-panels {
                    (relays_section(dev))
                    (energy_section(dev))
                    (network_section(dev))
                    (system_section(dev))
                }
            }
            div id="admin-panel" { (admin_panel(dev)) }
        }
    }
}
