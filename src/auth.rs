//! Sessions, CSRF protection (Task 6a), and the built-in login (Task 11):
//! `require_auth` (proxy-trust vs. builtin-session gate), the per-IP login
//! rate limiter, and the argon2 hash/verify helpers `POST /login` and the
//! `hash-password` subcommand build on.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::async_trait;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use tower_sessions::{MemoryStore, Session, SessionManagerLayer};

use crate::config::AuthMode;
use crate::state::AppState;

const CSRF_KEY: &str = "csrf";

/// Session key marking a `Builtin`-mode session as authenticated (set on a
/// successful `POST /login`, wiped by `POST /logout`'s `session.flush()`).
pub(crate) const AUTHENTICATED_KEY: &str = "authenticated";

/// How long a session may sit idle before it expires. Bounds the lifetime of
/// every session in `MemoryStore`, which otherwise defaults to a two-week
/// (`tower-sessions`' default `Expiry::OnSessionEnd` fallback) unbounded
/// lifetime with nothing to ever evict it - an unbounded number of
/// never-expiring sessions is an unbounded memory leak for a long-running
/// process.
const SESSION_INACTIVITY_TIMEOUT: tower_sessions::cookie::time::Duration =
    tower_sessions::cookie::time::Duration::minutes(30);

/// In-memory sessions (single-instance app). HttpOnly + SameSite=Lax always; the
/// Secure flag follows config (default true; see `AuthConfig::cookie_secure`).
///
/// `MemoryStore` (tower-sessions 0.13) does not implement `ExpiredDeletion`,
/// so there is no background sweep to actively evict expired records; a
/// session is instead dropped on its next `load()` once past expiry (`load`
/// filters out `!is_active(expiry_date)` records). `with_expiry` bounds the
/// lifetime every session is retained for, which is the part under this
/// crate's control.
pub fn session_layer(secure: bool) -> SessionManagerLayer<MemoryStore> {
    SessionManagerLayer::new(MemoryStore::default())
        .with_secure(secure)
        .with_same_site(tower_sessions::cookie::SameSite::Lax)
        .with_expiry(tower_sessions::Expiry::OnInactivity(
            SESSION_INACTIVITY_TIMEOUT,
        ))
}

fn gen_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The current session's CSRF token, read from the session or created and
/// stored if this is the session's first request. Every page handler takes
/// this extractor and threads `&csrf.0` into `views::layout::page` so the
/// rendered page can carry the token for htmx writes.
pub struct Csrf(pub String);

#[async_trait]
impl<S: Send + Sync> FromRequestParts<S> for Csrf {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, StatusCode> {
        let session = Session::from_request_parts(parts, state)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let token = match session.get::<String>(CSRF_KEY).await {
            Ok(Some(t)) => t,
            _ => {
                let t = gen_token();
                if let Err(err) = session.insert(CSRF_KEY, &t).await {
                    tracing::warn!(%err, "failed to store CSRF token in session");
                }
                t
            }
        };
        Ok(Csrf(token))
    }
}

/// Constant-time equality on the token bytes: the CSRF token (and, in Task
/// 11, the submitted login username) is compared against a secret, so
/// comparing it with `==` would leak timing information about how many
/// leading bytes matched.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn same_origin(headers: &HeaderMap) -> bool {
    if let Some(sfs) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        return sfs == "same-origin" || sfs == "none";
    }
    // Fallback for clients without Sec-Fetch-Site: Origin host must match Host.
    match (
        headers.get("origin").and_then(|v| v.to_str().ok()),
        headers.get("host").and_then(|v| v.to_str().ok()),
    ) {
        (Some(origin), Some(host)) => origin
            .split_once("://")
            .map(|(_, authority)| authority == host)
            .unwrap_or(false), // no "://" in Origin -> fail closed (does not match)
        (None, _) => true, // no Origin (e.g. same-origin non-CORS form) -> rely on CSRF token
        _ => false,
    }
}

/// Applied to every non-asset route. Safe methods (GET/HEAD/OPTIONS) pass
/// through unchecked. Every other method (the write routes added from Task 6
/// onward) must be same-origin AND carry an `X-CSRF-Token` header matching
/// the session's token, or the request is rejected with 403. Both checks
/// apply regardless of auth mode (proxy or builtin): a same-origin cookie is
/// ambient authority in either mode, so CSRF protection cannot be skipped for
/// either.
pub async fn csrf_and_origin(
    session: Session,
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(next.run(req).await);
    }
    if !same_origin(req.headers()) {
        return Err(StatusCode::FORBIDDEN);
    }
    let token = session.get::<String>(CSRF_KEY).await.ok().flatten();
    let header = req
        .headers()
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    match (token, header) {
        (Some(t), Some(h)) if ct_eq(t.as_bytes(), h.as_bytes()) => Ok(next.run(req).await),
        _ => Err(StatusCode::FORBIDDEN),
    }
}

/// Applied to every app route (everything except `/assets/:file` and the
/// public `GET`/`POST /login`). `AuthMode::Proxy` trusts the reverse proxy in
/// front of this app to have already authenticated the request and always
/// allows it through. `AuthMode::Builtin` requires the session to carry the
/// `AUTHENTICATED_KEY` marker set by a successful `POST /login`; otherwise
/// the request is redirected (303) to `/login`.
pub async fn require_auth(
    State(state): State<AppState>,
    session: Session,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let mode = state.inner.config.read().await.auth.mode;
    if mode == AuthMode::Proxy {
        return next.run(req).await;
    }
    let authenticated = session
        .get::<bool>(AUTHENTICATED_KEY)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    if authenticated {
        next.run(req).await
    } else {
        Redirect::to("/login").into_response()
    }
}

/// Default per-IP login attempt cap and the window it resets after. Chosen to
/// be generous enough for a human retrying a typo but tight enough to make
/// online password guessing impractical.
pub const MAX_LOGIN_ATTEMPTS: u32 = 5;
pub const LOGIN_RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);

/// In-memory per-IP login attempt counter. `attempt` is a plain synchronous
/// call (the mutex is held only for the duration of the increment, never
/// across an `.await`), so callers check/update it BEFORE any async work
/// (notably the `spawn_blocking` argon2 verify).
pub struct RateLimiter {
    max_attempts: u32,
    window: Duration,
    attempts: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

impl RateLimiter {
    pub fn new(max_attempts: u32, window: Duration) -> Self {
        RateLimiter {
            max_attempts,
            window,
            attempts: Mutex::new(HashMap::new()),
        }
    }

    /// Records one attempt from `ip` and returns whether it is allowed to
    /// proceed. The per-IP counter resets once `window` has elapsed since
    /// the first attempt in the current window.
    pub fn attempt(&self, ip: IpAddr) -> bool {
        let mut attempts = self
            .attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let entry = attempts.entry(ip).or_insert((0, now));
        if now.duration_since(entry.1) >= self.window {
            *entry = (0, now);
        }
        entry.0 += 1;
        entry.0 <= self.max_attempts
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        RateLimiter::new(MAX_LOGIN_ATTEMPTS, LOGIN_RATE_LIMIT_WINDOW)
    }
}

/// Hashes `password` with argon2 using a fresh random salt, returning a PHC
/// string suitable for `AuthConfig::password_hash`. Used by both the
/// `plugboard hash-password` subcommand and (indirectly, via that
/// subcommand's output) the config file an operator hand-writes.
pub fn hash_password(password: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::rand_core::OsRng;
    use argon2::password_hash::{PasswordHasher, SaltString};

    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hashing with a freshly generated salt does not fail")
        .to_string()
}

/// Verifies `password` against a stored argon2 PHC `hash`. Returns `false`
/// (never panics or errors out to the caller) both when the password does
/// not match AND when `hash` fails to parse as a PHC string, so a corrupt or
/// non-argon2 config value fails closed rather than panicking the request.
/// MUST be called from `tokio::task::spawn_blocking`: argon2 verification is
/// deliberately CPU-expensive and must never run on a Tokio worker thread.
pub(crate) fn verify_password(hash: &str, password: &str) -> bool {
    use argon2::Argon2;
    use argon2::password_hash::{PasswordHash, PasswordVerifier};

    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}
