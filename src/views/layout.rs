use maud::{DOCTYPE, Markup, html};

/// The full HTML document shell: loads htmx + the SSE extension, and provides
/// OOB swap targets (`#modal`, `#toasts`) shared by every page. `csrf` is the
/// current session's CSRF token (see `crate::auth::Csrf`): it is embedded as
/// a meta tag, and `csrf.js` copies it into every htmx request header so
/// write routes can verify it.
pub fn page(title: &str, csrf: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="csrf-token" content=(csrf);
                title { (title) " - plugboard" }
                link rel="stylesheet" href="/assets/app.css";
                script src="/assets/htmx.min.js" {}
                script src="/assets/sse.js" {}
                script src="/assets/csrf.js" {}
            }
            body hx-ext="sse" {
                header.topbar { a.brand href="/" { "plugboard" } nav { a href="/discover" { "Discover" } a href="/settings" { "Settings" } } }
                main { (body) }
                // OOB swap targets: modals open into #modal, toasts append into #toasts.
                div id="modal" {}
                div id="toasts" {}
            }
        }
    }
}
