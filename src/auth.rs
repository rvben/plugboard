//! Sessions and CSRF protection (Task 6a). The built-in login handler is
//! added in Task 11; this module only provides the security foundation every
//! write route depends on: a per-session CSRF token and a same-origin check.

use axum::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use tower_sessions::{MemoryStore, Session, SessionManagerLayer};

const CSRF_KEY: &str = "csrf";

/// In-memory sessions (single-instance app). HttpOnly + SameSite=Lax always; the
/// Secure flag follows config (default true; see `AuthConfig::cookie_secure`).
pub fn session_layer(secure: bool) -> SessionManagerLayer<MemoryStore> {
    SessionManagerLayer::new(MemoryStore::default())
        .with_secure(secure)
        .with_same_site(tower_sessions::cookie::SameSite::Lax)
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
                let _ = session.insert(CSRF_KEY, &t).await;
                t
            }
        };
        Ok(Csrf(token))
    }
}

/// Constant-time equality on the token bytes: the CSRF token is a secret, so
/// comparing it with `==` would leak timing information about how many
/// leading bytes matched.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
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
            .rsplit("//")
            .next()
            .map(|o| o == host)
            .unwrap_or(false),
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
