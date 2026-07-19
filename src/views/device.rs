use maud::{Markup, html};
use tasmota_core::{DeviceStatus, RelayState};

use crate::fleet::DeviceView;
use crate::views::components::{na, signal_indicator, state_badge};

/// The device's live status, or `None` when offline. Centralizes the same
/// `reachable` guard `DeviceView::power_w`/`today_kwh`/`rssi` already apply,
/// so every section below reads live data through one place: an offline
/// device never leaks a stale `status` as though it were current.
fn live_status(dev: &DeviceView) -> Option<&DeviceStatus> {
    if dev.is_online() {
        dev.status.as_ref()
    } else {
        None
    }
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
        section.relays {
            h2 { "Relays" }
            @if !dev.is_online() {
                p { span.badge.offline { "offline" } }
            } @else if relays.is_empty() {
                p { "No relays reported." }
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

fn energy_section(dev: &DeviceView) -> Markup {
    let energy = live_status(dev).and_then(|s| s.energy.as_ref());
    html! {
        section.energy {
            h2 { "Energy" }
            dl {
                dt { "Power" } dd { (na(energy.and_then(|e| e.power_w))) " W" }
                dt { "Voltage" } dd { (na(energy.and_then(|e| e.voltage_v))) " V" }
                dt { "Current" } dd { (na(energy.and_then(|e| e.current_a))) " A" }
                dt { "Today" } dd { (na(energy.and_then(|e| e.today_kwh))) " kWh" }
                dt { "Yesterday" } dd { (na(energy.and_then(|e| e.yesterday_kwh))) " kWh" }
                dt { "Total" } dd { (na(energy.and_then(|e| e.total_kwh))) " kWh" }
            }
        }
    }
}

fn firmware_section(dev: &DeviceView) -> Markup {
    let firmware = live_status(dev).and_then(|s| s.firmware.clone());
    html! {
        section.firmware {
            h2 { "Firmware" }
            p { (na(firmware)) }
        }
    }
}

fn network_section(dev: &DeviceView) -> Markup {
    let net = live_status(dev).map(|s| &s.net);
    html! {
        section.network {
            h2 { "Network" }
            dl {
                dt { "IP" } dd { (na(net.and_then(|n| n.ip.clone()))) }
                dt { "MAC" } dd { (na(net.and_then(|n| n.mac.clone()))) }
                dt { "Hostname" } dd { (na(net.and_then(|n| n.hostname.clone()))) }
                dt { "Wi-Fi signal" } dd { (signal_indicator(dev.rssi())) }
            }
        }
    }
}

fn uptime_section(dev: &DeviceView) -> Markup {
    let uptime = live_status(dev).and_then(|s| s.uptime.clone());
    html! {
        section.uptime {
            h2 { "Uptime" }
            p { (na(uptime)) }
        }
    }
}

fn mqtt_section(dev: &DeviceView) -> Markup {
    let mqtt = live_status(dev).and_then(|s| s.mqtt.as_ref());
    html! {
        section.mqtt {
            h2 { "MQTT" }
            dl {
                dt { "Host" } dd { (na(mqtt.and_then(|m| m.host.clone()))) }
                dt { "Port" } dd { (na(mqtt.and_then(|m| m.port))) }
                dt { "Client" } dd { (na(mqtt.and_then(|m| m.client.clone()))) }
                dt { "Reconnects" } dd { (na(mqtt.and_then(|m| m.reconnect_count))) }
                // Tasmota exposes no reliable live MQTT connected/disconnected flag over
                // HTTP. This is hard-coded to `n/a` regardless of what `mqtt.connected`
                // reports (even `Some(true)`): it must never render a guessed bool.
                dt { "Connected" } dd { (na::<bool>(None)) }
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

/// The per-device admin panel (Task 8): console, config get/set, firmware
/// check/update, a config backup download link, and a disabled restore
/// control. Every form targets `#admin-result` with an `outerHTML` swap; the
/// handler behind each destructive action (`routes::admin`) decides whether
/// to execute directly or return a confirm modal (an OOB swap into `#modal`)
/// instead, reusing `tasmota_core::guardrail::classify` exactly like the CLI.
/// `restore` has no route: its upload endpoint is unverified against a live
/// device (see `tasmota-cli`'s own `restore` refusal), so the control here is
/// permanently disabled with an explanatory tooltip rather than wired to a
/// handler that could report a false success.
fn admin_panel(dev: &DeviceView) -> Markup {
    let id = &dev.id;
    html! {
        section.admin-panel {
            h2 { "Admin" }
            div.admin-section.admin-console {
                h3 { "Console" }
                form hx-post=(format!("/device/{id}/console")) hx-target="#admin-result" hx-swap="outerHTML" {
                    input type="text" name="command" placeholder="e.g. Status 8" required;
                    button type="submit" { "Run" }
                }
            }
            div.admin-section.admin-config {
                h3 { "Config" }
                form hx-post=(format!("/device/{id}/config/get")) hx-target="#admin-result" hx-swap="outerHTML" {
                    input type="text" name="setting" placeholder="Setting name" required;
                    button type="submit" { "Get" }
                }
                form hx-post=(format!("/device/{id}/config/set")) hx-target="#admin-result" hx-swap="outerHTML" {
                    input type="text" name="setting" placeholder="Setting name" required;
                    input type="text" name="value" placeholder="Value" required;
                    button type="submit" class="btn-danger" { "Set" }
                }
            }
            div.admin-section.admin-firmware {
                h3 { "Firmware" }
                form hx-post=(format!("/device/{id}/firmware/check")) hx-target="#admin-result" hx-swap="outerHTML" {
                    button type="submit" { "Check version" }
                }
                form hx-post=(format!("/device/{id}/firmware/update")) hx-target="#admin-result" hx-swap="outerHTML" {
                    input type="text" name="url" placeholder="OTA URL (optional)";
                    button type="submit" class="btn-danger" { "Flash firmware" }
                }
            }
            div.admin-section.admin-backup {
                h3 { "Backup" }
                a.backup-link href=(format!("/device/{id}/backup")) { "Download config backup (.dmp)" }
                button type="button" disabled title="Restore is disabled pending endpoint verification against a live device. Use the device web UI (Configuration > Backup/Restore) instead." {
                    "Restore (unavailable)"
                }
            }
            (admin_result(html! {}))
        }
    }
}

/// Renders the full device detail page: relays, energy, firmware, network,
/// uptime, MQTT, and the admin panel (console/config/firmware/backup).
/// Every live field goes through `na()` (or the offline branch above it), so
/// an offline device or a device with a sparse status never renders a
/// coerced value.
pub fn device_page(dev: &DeviceView) -> Markup {
    html! {
        div.device-detail {
            header.device-header {
                h1 { (dev.display_name()) }
                span.host { (dev.host) }
                (state_badge(dev))
            }
            (relays_section(dev))
            (energy_section(dev))
            (firmware_section(dev))
            (network_section(dev))
            (uptime_section(dev))
            (mqtt_section(dev))
            div id="admin-panel" { (admin_panel(dev)) }
        }
    }
}
