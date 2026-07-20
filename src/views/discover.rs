//! Device discovery page (mixed-vendor): a CIDR scan form, its results, and
//! an "Add" action per found device. `results` deliberately takes
//! `(display_name, host, vendor)` triples rather than
//! `switchkit::Discovered`, so this view carries no device-status coupling
//! and is trivially testable with documentation IPs (see `tests/discover.rs`).
//!
//! The "Add" form intentionally carries no `vendor` field: the discovered
//! vendor is shown here for the human to read, but `POST /discover/add`
//! always re-confirms it server-side (see `routes::discover::add`) rather
//! than trusting anything this page submits.

use maud::{Markup, html};
use switchkit::Vendor;

use crate::fleet::device_id;
use crate::views::components::vendor_tag;

/// The CIDR scan form: posts to `/discover/scan` and swaps the response into
/// `#discover-results`. `default_range` pre-fills the input (from
/// `discovery::detect_local_cidr()`, or a documentation-range placeholder
/// when detection fails, e.g. in a sandboxed/offline environment).
fn scan_form(default_range: &str) -> Markup {
    html! {
        form.discover-form hx-post="/discover/scan" hx-target="#discover-results" hx-swap="innerHTML" {
            div.field {
                label for="range" { "CIDR range" }
                input.mono type="text" id="range" name="range" value=(default_range) required;
            }
            button type="submit" class="btn-primary" { "Scan" }
        }
    }
}

/// Renders the found devices, one row each showing its discovered vendor and
/// an "Add" button whose form carries `name`+`host` as hidden fields to
/// `POST /discover/add`. An empty scan renders a hint rather than an empty
/// list, mirroring the CLI's own "no devices found" message.
pub fn results(found: &[(String, String, Vendor)]) -> Markup {
    html! {
        @if found.is_empty() {
            p.empty {
                strong { "No devices found" }
                span { "Check the range and try again." }
            }
        } @else {
            ul.discover-results-list {
                @for (name, host, vendor) in found {
                    li id=(format!("discover-row-{}", device_id(host))) {
                        span.discover-name { (name) }
                        span.discover-host { (host) }
                        (vendor_tag(*vendor))
                        form hx-post="/discover/add" hx-target=(format!("#discover-row-{}", device_id(host))) hx-swap="outerHTML" {
                            input type="hidden" name="name" value=(name);
                            input type="hidden" name="host" value=(host);
                            button type="submit" class="btn-primary" { "Add" }
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
            header.page-header {
                div {
                    h1 { "Discover devices" }
                    p.subtitle {
                        "Scan a network range for Tasmota and Shelly devices, then add them to the fleet. Scanning a /24 takes a few seconds."
                    }
                }
            }
            (scan_form(default_range))
            div id="discover-results" {}
        }
    }
}
