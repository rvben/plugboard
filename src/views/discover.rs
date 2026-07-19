//! Device discovery page (Task 9): a CIDR scan form, its results, and an
//! "Add" action per found device. `results` deliberately takes `(display_name,
//! host)` pairs rather than `tasmota_core::discovery::Discovered`, so this
//! view carries no device-status coupling and is trivially testable with
//! documentation IPs (see `tests/discover.rs`).

use maud::{Markup, html};

use crate::fleet::device_id;

/// The CIDR scan form: posts to `/discover/scan` and swaps the response into
/// `#discover-results`. `default_range` pre-fills the input (from
/// `discovery::detect_local_cidr()`, or a documentation-range placeholder
/// when detection fails, e.g. in a sandboxed/offline environment).
fn scan_form(default_range: &str) -> Markup {
    html! {
        form.discover-form hx-post="/discover/scan" hx-target="#discover-results" hx-swap="innerHTML" {
            label for="range" { "CIDR range" }
            input type="text" id="range" name="range" value=(default_range) required;
            button type="submit" { "Scan" }
        }
    }
}

/// Renders the found devices, one row each with an "Add" button whose form
/// carries `name`+`host` as hidden fields to `POST /discover/add`. An empty
/// scan renders a hint rather than an empty list, mirroring the CLI's own
/// "no Tasmota devices found" message.
pub fn results(found: &[(String, String)]) -> Markup {
    html! {
        @if found.is_empty() {
            p.empty { "No devices found. Check the range and try again." }
        } @else {
            ul.discover-results-list {
                @for (name, host) in found {
                    li id=(format!("discover-row-{}", device_id(host))) {
                        span.discover-name { (name) }
                        span.discover-host { (host) }
                        form hx-post="/discover/add" hx-target=(format!("#discover-row-{}", device_id(host))) hx-swap="outerHTML" {
                            input type="hidden" name="name" value=(name);
                            input type="hidden" name="host" value=(host);
                            button type="submit" { "Add" }
                        }
                    }
                }
            }
        }
    }
}

/// The full `/discover` page body: the scan form above an initially-empty
/// results region that `POST /discover/scan` swaps `results()` into.
pub fn page(default_range: &str) -> Markup {
    html! {
        div.discover-page {
            h1 { "Discover devices" }
            (scan_form(default_range))
            div id="discover-results" {}
        }
    }
}
