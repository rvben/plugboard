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

/// Per-device settings live on each device's own page (name, group,
/// credential, protection, removal) - everything about ONE device in one
/// place. This section just points there, and to Discover for adding.
fn devices_pointer(devices: &[DeviceConfig]) -> Markup {
    let count = devices.len();
    html! {
        section.panel.settings-devices {
            h2 { "Devices" }
            p.hint {
                @if count == 0 {
                    "No devices yet. Add one from " a href="/discover" { "Discover" } "."
                } @else {
                    (count) " device" (if count == 1 { "" } else { "s" }) " configured. Each device's name, group, credential, and protection are managed on its own page - open it from the "
                    a href="/" { "dashboard" }
                    ". Add more from "
                    a href="/discover" { "Discover" }
                    "."
                }
            }
        }
    }
}

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
            (devices_pointer(&config.devices))
            (updates_section(&config.updates))
            (poll_interval_section(config.poll_interval_secs))
            (auth_section(&config.auth))
        }
    }
}
