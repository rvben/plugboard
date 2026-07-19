//! `GET`/`POST /login` and `POST /logout` (Task 11). Login checks BOTH the
//! username (constant-time) and the password (argon2, off-runtime) and fails
//! closed when `AuthMode::Builtin` has no configured credential; logout
//! flushes the session entirely. See `crate::auth` for the security
//! primitives these handlers are built on (`RateLimiter`, `verify_password`,
//! session rotation/flush).

use std::net::SocketAddr;

use axum::Form;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use maud::Markup;
use serde::Deserialize;
use tower_sessions::Session;

use crate::auth::{AUTHENTICATED_KEY, Csrf, ct_eq, verify_password};
use crate::state::AppState;
use crate::views::layout;
use crate::views::login::login_page;

const INVALID_CREDENTIALS: &str = "invalid credentials";
const TOO_MANY_ATTEMPTS: &str = "too many attempts, try again later";

/// `GET /login`. Public (session + CSRF, no `require_auth`): renders the
/// login form and, via the `Csrf` extractor, creates the anonymous
/// pre-auth session that holds the CSRF token `POST /login` will check.
pub async fn login_get(csrf: Csrf) -> Markup {
    layout::page("Login", &csrf.0, login_page(None))
}

#[derive(Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
}

/// `POST /login`. Order of operations matters for both security properties
/// the brief calls out:
/// 1. The per-IP rate limiter is checked/updated synchronously, before any
///    `.await`, so its lock is never held across one.
/// 2. When credentials ARE configured, the argon2 verify always runs (even
///    on a username mismatch) so response latency cannot act as a
///    username-enumeration oracle; only `username_matches && password_matches`
///    authenticates.
/// 3. On success, `session.cycle_id()` rotates the session ID BEFORE the
///    `AUTHENTICATED_KEY` marker is inserted (session-fixation defense).
pub async fn login_post(
    State(state): State<AppState>,
    session: Session,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    csrf: Csrf,
    Form(form): Form<LoginForm>,
) -> Response {
    if !state.inner.rate_limiter.attempt(addr.ip()) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            layout::page("Login", &csrf.0, login_page(Some(TOO_MANY_ATTEMPTS))),
        )
            .into_response();
    }

    let credentials = state
        .inner
        .config
        .read()
        .await
        .auth
        .configured_credentials()
        .map(|(username, hash)| (username.to_string(), hash.to_string()));

    let authenticated = match credentials {
        Some((configured_username, configured_hash)) => {
            let username_matches = ct_eq(form.username.as_bytes(), configured_username.as_bytes());
            let password = form.password.clone();
            let password_matches =
                tokio::task::spawn_blocking(move || verify_password(&configured_hash, &password))
                    .await
                    .unwrap_or(false);
            username_matches && password_matches
        }
        // Fail closed: Builtin mode with no configured username/password_hash rejects
        // every attempt (logged once at startup - see `main`'s misconfiguration warning).
        None => false,
    };

    if !authenticated {
        return (
            StatusCode::UNAUTHORIZED,
            layout::page("Login", &csrf.0, login_page(Some(INVALID_CREDENTIALS))),
        )
            .into_response();
    }

    if let Err(err) = session.cycle_id().await {
        tracing::error!(%err, "failed to cycle session id on login");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if let Err(err) = session.insert(AUTHENTICATED_KEY, true).await {
        tracing::error!(%err, "failed to mark session authenticated");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // htmx-recommended full-page navigation after an AJAX POST: forces the browser to
    // GET / rather than letting XHR auto-follow a redirect and swap unintended content.
    let mut response = StatusCode::OK.into_response();
    response
        .headers_mut()
        .insert("hx-redirect", HeaderValue::from_static("/"));
    response
}

/// `POST /logout` (app tier: session + CSRF + `require_auth`). Flushes the
/// session entirely (clears data, deletes it from the store, nulls the
/// session id) rather than just removing the `AUTHENTICATED_KEY` marker, so
/// the old cookie cannot authenticate again even if replayed.
pub async fn logout(session: Session) -> Redirect {
    if let Err(err) = session.flush().await {
        tracing::warn!(%err, "failed to flush session on logout");
    }
    Redirect::to("/login")
}
