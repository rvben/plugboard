use maud::{DOCTYPE, Markup, PreEscaped, html};

use crate::views::components::power_mark;

/// Inline SVG favicon (accent rounded square + the standby symbol), embedded
/// as a data URI so the single-binary app needs no extra asset route for it.
const FAVICON: &str = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'%3E%3Crect width='32' height='32' rx='7' fill='%233060db'/%3E%3Cpath d='M16 7.5v8.5' stroke='white' stroke-width='3' stroke-linecap='round'/%3E%3Cpath d='M10.4 10.6a8 8 0 1 0 11.2 0' stroke='white' stroke-width='3' stroke-linecap='round' fill='none'/%3E%3C/svg%3E";

/// Which top-level nav item the current page belongs to. The device detail
/// page counts as `Devices` (it is reached from the grid).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Devices,
    Discover,
    Settings,
    /// No nav item highlighted (the login page).
    None,
}

/// Per-page chrome: the active nav item, and whether to offer Sign out
/// (builtin auth only - in proxy mode the reverse proxy owns the session, so
/// a local sign-out would just be confusing).
#[derive(Clone, Copy)]
pub struct Chrome {
    pub active: Nav,
    pub show_logout: bool,
}

impl Chrome {
    /// The login page's chrome: nothing highlighted, no sign-out.
    pub fn login() -> Self {
        Chrome {
            active: Nav::None,
            show_logout: false,
        }
    }
}

fn nav_link(href: &str, label: &str, active: bool) -> Markup {
    html! {
        a href=(href) aria-current=[active.then_some("page")] { (label) }
    }
}

/// A full styled error page for non-htmx navigations (a mistyped URL, a
/// stale link to a removed device). htmx requests keep their plain-text
/// error bodies for the toast layer; this page is the browser-navigation
/// equivalent. `detail` is the (already scrubbed) reason, shown verbatim.
pub fn error_page(status: axum::http::StatusCode, detail: &str) -> Markup {
    let title = match status.as_u16() {
        404 => "Not found",
        403 => "Forbidden",
        400 => "Bad request",
        502 => "Device unreachable",
        _ => status.canonical_reason().unwrap_or("Error"),
    };
    let body = html! {
        div.error-page {
            (power_mark(30))
            p.error-code { (status.as_u16()) }
            h1 { (title) }
            @if !detail.is_empty() {
                p.hint { (detail) }
            }
            a.error-home href="/" { "Back to devices" }
        }
    };
    page(title, "", Chrome::login(), body)
}

/// The full HTML document shell: loads htmx + the SSE extension, and provides
/// OOB swap targets (`#modal`, `#toasts`) shared by every page. `csrf` is the
/// current session's CSRF token (see `crate::auth::Csrf`): it is embedded as
/// a meta tag, and `csrf.js` copies it into every htmx request header so
/// write routes can verify it.
pub fn page(title: &str, csrf: &str, chrome: Chrome, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="csrf-token" content=(csrf);
                title { (title) " - plugboard" }
                link rel="icon" href=(PreEscaped(FAVICON));
                link rel="stylesheet" href="/assets/app.css";
                script src="/assets/htmx.min.js" {}
                script src="/assets/sse.js" {}
                script src="/assets/csrf.js" {}
                script src="/assets/app.js" {}
            }
            body hx-ext="sse" {
                header.topbar {
                    a.brand href="/" { (power_mark(18)) "plugboard" }
                    nav {
                        (nav_link("/", "Devices", chrome.active == Nav::Devices))
                        (nav_link("/discover", "Discover", chrome.active == Nav::Discover))
                        (nav_link("/settings", "Settings", chrome.active == Nav::Settings))
                        @if chrome.show_logout {
                            form hx-post="/logout" {
                                button type="submit" { "Sign out" }
                            }
                        }
                    }
                }
                main { (body) }
                // OOB swap targets: modals open into #modal, toasts append into #toasts.
                div id="modal" {}
                div id="toasts" role="status" aria-live="polite" {}
            }
        }
    }
}
