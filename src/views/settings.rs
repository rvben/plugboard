//! Settings page (Task 10): the device list with per-device rename/remove/
//! credential/protected controls, the poll interval, and a READ-ONLY
//! auth-mode line.
//!
//! Device credentials are write-only: `DeviceConfig.password` is never
//! rendered into any field's `value` (only whether one is set, as a badge).
//! The login `password_hash` is never rendered at all - only whether a
//! built-in username + hash are BOTH configured. See `tests/settings.rs` for
//! the non-vacuous proof of both. Auth mode and the built-in credential are
//! not editable from this page (config-file + restart only, per the design's
//! lockout-hazard rationale), so this section carries no form.

use maud::{Markup, html};

use crate::config::{AuthConfig, AuthMode, Config, DeviceConfig};
use crate::fleet::device_id;

fn auth_section(auth: &AuthConfig) -> Markup {
    let mode_label = match auth.mode {
        AuthMode::Proxy => "proxy",
        AuthMode::Builtin => "builtin",
    };
    html! {
        section.settings-auth {
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
        section.settings-poll-interval {
            h2 { "Poll interval" }
            form hx-post="/settings/poll-interval" hx-target="#settings-page" hx-swap="outerHTML" {
                label for="secs" { "Seconds between polls" }
                input type="number" id="secs" name="secs" min="1" value=(secs) required;
                button type="submit" { "Save" }
            }
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
                @if d.password.is_some() {
                    span.badge.credential-set { "credential set" }
                }
            }
            form.settings-rename hx-post="/settings/device/rename" hx-target="#settings-page" hx-swap="outerHTML" {
                input type="hidden" name="host" value=(d.host);
                label for=(format!("name-{id}")) { "Name" }
                input type="text" id=(format!("name-{id}")) name="name" value=(d.name) required;
                button type="submit" { "Rename" }
            }
            form.settings-credentials hx-post="/settings/device/credentials" hx-target="#settings-page" hx-swap="outerHTML" {
                input type="hidden" name="host" value=(d.host);
                label for=(format!("password-{id}")) { "Password" }
                input type="password" id=(format!("password-{id}")) name="password"
                    placeholder="New password (blank clears it)" autocomplete="new-password";
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
            form.settings-remove hx-post="/settings/device/remove" hx-target="#settings-page" hx-swap="outerHTML" {
                input type="hidden" name="host" value=(d.host);
                button type="submit" class="btn-danger" { "Remove" }
            }
        }
    }
}

fn devices_section(devices: &[DeviceConfig]) -> Markup {
    html! {
        section.settings-devices {
            h2 { "Devices" }
            @if devices.is_empty() {
                p.empty {
                    "No devices configured. Add one from "
                    a href="/discover" { "Discover" }
                    "."
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
            h1 { "Settings" }
            (auth_section(&config.auth))
            (poll_interval_section(config.poll_interval_secs))
            (devices_section(&config.devices))
        }
    }
}
