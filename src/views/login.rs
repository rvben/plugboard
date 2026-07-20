//! The built-in login form. Rendered by `GET /login` (fresh) and by
//! `POST /login` on a failed attempt (with an error banner), so a rejected
//! submission can retry without a page reload losing the session's CSRF
//! token.

use maud::{Markup, html};

use crate::views::components::power_mark;

/// `error` is the generic "invalid credentials" message on a failed attempt,
/// or a rate-limit notice; `None` on the first `GET`. The message is
/// deliberately identical for a wrong username and a wrong password (no
/// username-enumeration oracle) - see `routes::auth::login_post`.
pub fn login_page(error: Option<&str>) -> Markup {
    html! {
        div.login-page id="login-page" {
            div.login-brand {
                (power_mark(28))
                h1 { "Sign in" }
            }
            @if let Some(msg) = error {
                p.login-error role="alert" { (msg) }
            }
            form.login-form hx-post="/login" hx-target="#login-page" hx-swap="outerHTML" {
                div.field {
                    label for="username" { "Username" }
                    input type="text" id="username" name="username" autocomplete="username" required;
                }
                div.field {
                    label for="password" { "Password" }
                    input type="password" id="password" name="password" autocomplete="current-password" required;
                }
                button type="submit" { "Sign in" }
            }
        }
    }
}
