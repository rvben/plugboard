use maud::{DOCTYPE, Markup, html};

/// The full HTML document shell: loads htmx + the SSE extension, and provides
/// OOB swap targets (`#modal`, `#toasts`) shared by every page.
///
/// Task 6a refactors this to take a `csrf: &str` (adds the `csrf-token` meta +
/// `csrf.js`) once `src/auth.rs` and `assets/csrf.js` exist.
pub fn page(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " - tasmota-web" }
                link rel="stylesheet" href="/assets/app.css";
                script src="/assets/htmx.min.js" {}
                script src="/assets/sse.js" {}
            }
            body hx-ext="sse" {
                header.topbar { a.brand href="/" { "tasmota" } nav { a href="/discover" { "Discover" } a href="/settings" { "Settings" } } }
                main { (body) }
                // OOB swap targets: modals open into #modal, toasts append into #toasts.
                div id="modal" {}
                div id="toasts" {}
            }
        }
    }
}
