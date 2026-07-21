//! Settings page: the device list with per-device rename/remove/
//! credential/protected controls, the poll interval, and a READ-ONLY
//! auth-mode section.
//!
//! Device credentials are write-only: `DeviceConfig.password` is never
//! rendered into any field's `value` (only whether one is set, as a badge).
//! The login `password_hash` is never rendered at all - only whether a
//! built-in username + hash are BOTH configured. See `tests/settings.rs` for
//! the non-vacuous proof of both. Auth mode and the built-in credential are
//! not editable from this page (config-file + restart only, per the design's
//! lockout-hazard rationale), so this section carries no form.

use maud::{Markup, html};

use crate::config::{AuthConfig, AuthMode, Config, DeviceConfig, UpdatesConfig};
use crate::fleet::device_id;
use crate::views::components::vendor_tag;

fn updates_section(updates: &UpdatesConfig) -> Markup {
    html! {
        section.panel.settings-updates {
            h2 { "Firmware updates" }
            form hx-post="/settings/updates" hx-target="#settings-page" hx-swap="outerHTML" {
                div.settings-toggles {
                    label {
                        input type="checkbox" name="enabled" value="true" checked[updates.enabled];
                        "Check for updates automatically"
                    }
                    label {
                        input type="checkbox" name="auto_apply" value="true" checked[updates.auto_apply];
                        "Install updates automatically"
                    }
                }
                button type="submit" { "Save" }
            }
            p.hint {
                "Checks ask Shelly devices directly and compare Tasmota against the latest release. Automatic installs go through the same observed update flow as the buttons, and always skip protected devices."
            }
        }
    }
}

fn auth_section(auth: &AuthConfig) -> Markup {
    let mode_label = match auth.mode {
        AuthMode::Proxy => "proxy",
        AuthMode::Builtin => "builtin",
    };
    html! {
        section.panel.settings-auth {
            h2 { "Authentication" }
            dl {
                dt { "Mode" }
                dd { (mode_label) }
                @if auth.mode == AuthMode::Builtin {
                    dt { "Built-in credential" }
                    dd {
                        @if auth.username.is_some() && auth.password_hash.is_some() {
                            "configured"
                        } @else {
                            "NOT configured (login will fail closed)"
                        }
                    }
                }
            }
            p.hint {
                "Auth mode and the built-in credential are set in the config file and require a restart; they cannot be changed from this page."
            }
        }
    }
}

fn poll_interval_section(secs: u64) -> Markup {
    html! {
        section.panel.settings-poll-interval {
            h2 { "Poll interval" }
            form hx-post="/settings/poll-interval" hx-target="#settings-page" hx-swap="outerHTML" {
                div.field {
                    label for="secs" { "Seconds between polls" }
                    input type="number" id="secs" name="secs" min="1" value=(secs) required;
                }
                button type="submit" { "Save" }
            }
            p.hint { "How often every device is read for the dashboard, the detail pages, and metrics." }
        }
    }
}

fn device_row(d: &DeviceConfig) -> Markup {
    let id = device_id(&d.host);
    html! {
        li.settings-device id=(format!("settings-device-{id}")) {
            div.device-summary {
                span.device-name { (d.name) }
                span.device-host { (d.host) }
                (vendor_tag(d.vendor))
                @if d.password.is_some() {
                    span.badge.credential-set { "credential set" }
                }
                form.settings-remove hx-post="/settings/device/remove" hx-target="#settings-page" hx-swap="outerHTML" {
                    input type="hidden" name="host" value=(d.host);
                    button type="submit" class="btn-danger" { "Remove" }
                }
            }
            div.device-forms {
                form.settings-rename hx-post="/settings/device/rename" hx-target="#settings-page" hx-swap="outerHTML" {
                    input type="hidden" name="host" value=(d.host);
                    div.field {
                        label for=(format!("name-{id}")) { "Name" }
                        input type="text" id=(format!("name-{id}")) name="name" value=(d.name) required;
                    }
                    button type="submit" { "Rename" }
                }
                form.settings-group hx-post="/settings/device/group" hx-target="#settings-page" hx-swap="outerHTML" {
                    input type="hidden" name="host" value=(d.host);
                    div.field {
                        label for=(format!("group-{id}")) { "Group" }
                        input type="text" id=(format!("group-{id}")) name="group"
                            value=[d.group.as_deref()] placeholder="e.g. Living room" list="group-names";
                    }
                    button type="submit" { "Save" }
                }
                form.settings-credentials hx-post="/settings/device/credentials" hx-target="#settings-page" hx-swap="outerHTML" {
                    input type="hidden" name="host" value=(d.host);
                    div.field {
                        label for=(format!("password-{id}")) { "Device password" }
                        input type="password" id=(format!("password-{id}")) name="password"
                            placeholder="Blank clears it" autocomplete="new-password";
                    }
                    button type="submit" { "Save credential" }
                }
                form.settings-protected hx-post="/settings/device/protected" hx-target="#settings-page" hx-swap="outerHTML" {
                    input type="hidden" name="host" value=(d.host);
                    label {
                        input type="checkbox" name="protected" value="true" checked[d.protected];
                        "Protected (require confirmation for writes)"
                    }
                    button type="submit" { "Save" }
                }
            }
        }
    }
}

fn devices_section(devices: &[DeviceConfig]) -> Markup {
    // Existing group names as autocomplete suggestions, so a fleet's rooms
    // stay consistently spelled without restricting free-form entry.
    let mut group_names: Vec<&str> = devices
        .iter()
        .filter_map(|d| d.group.as_deref())
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .collect();
    group_names.sort_unstable();
    group_names.dedup();
    html! {
        section.settings-devices {
            h2 { "Devices" }
            datalist id="group-names" {
                @for g in &group_names {
                    option value=(g) {}
                }
            }
            @if devices.is_empty() {
                p.empty {
                    strong { "No devices configured" }
                    span { "Add one from " a href="/discover" { "Discover" } "." }
                }
            } @else {
                ul.settings-device-list {
                    @for d in devices {
                        (device_row(d))
                    }
                }
            }
        }
    }
}

/// The full `/settings` content (without the page shell -
/// `routes::settings::index` wraps it with `layout::page`). Every POST
/// handler in `routes::settings` returns this same fragment, re-derived from
/// the just-mutated config, to swap into `#settings-page` - so the rendered
/// list is always in sync with the config it was just built from.
pub fn settings_page(config: &Config) -> Markup {
    html! {
        div.settings-page id="settings-page" {
            header.page-header {
                h1 { "Settings" }
            }
            (devices_section(&config.devices))
            (updates_section(&config.updates))
            (poll_interval_section(config.poll_interval_secs))
            (auth_section(&config.auth))
        }
    }
}
