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

/// `attempts` plus the time of its last sweep, held behind one lock so a
/// sweep and the map it prunes are always updated together.
struct RateLimiterState {
    attempts: HashMap<IpAddr, (u32, Instant)>,
    last_sweep: Instant,
}

/// In-memory per-IP login attempt counter. `attempt` is a plain synchronous
/// call (the mutex is held only for the duration of the increment, never
/// across an `.await`), so callers check/update it BEFORE any async work
/// (notably the `spawn_blocking` argon2 verify).
pub struct RateLimiter {
    max_attempts: u32,
    window: Duration,
    state: Mutex<RateLimiterState>,
}

impl RateLimiter {
    pub fn new(max_attempts: u32, window: Duration) -> Self {
        RateLimiter {
            max_attempts,
            window,
            state: Mutex::new(RateLimiterState {
                attempts: HashMap::new(),
                last_sweep: Instant::now(),
            }),
        }
    }

    /// Records one attempt from `ip` and returns whether it is allowed to
    /// proceed. The per-IP counter resets once `window` has elapsed since
    /// the first attempt in the current window.
    ///
    /// A thin wrapper around `attempt_at` supplying the real clock; tests use
    /// `attempt_at` directly with explicit `Instant`s so the timing they
    /// exercise never depends on an actual sleep.
    pub fn attempt(&self, ip: IpAddr) -> bool {
        self.attempt_at(ip, Instant::now())
    }

    /// Nothing ever removes a key from `attempts` except the sweep below: an
    /// IP that attempts once and never returns would otherwise sit in the
    /// map for the life of the process, and since `ip` is attacker-controlled
    /// (anyone who can reach `/login`), that is an unbounded, attacker-driven
    /// memory leak. The sweep runs at most once per `window`, gated on the
    /// time since it last ran rather than on every call, so it stays bounded
    /// to the IPs seen within roughly the last two windows without scanning
    /// the whole map on each attempt.
    fn attempt_at(&self, ip: IpAddr, now: Instant) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let window = self.window;
        if now.duration_since(state.last_sweep) >= window {
            state
                .attempts
                .retain(|_, (_, first_seen)| now.duration_since(*first_seen) < window);
            state.last_sweep = now;
        }
        let entry = state.attempts.entry(ip).or_insert((0, now));
        // The sweep only bounds memory; it can keep an entry up to a window
        // past its expiry, so each attempt checks its own window as well.
        if now.duration_since(entry.1) >= window {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Within one window, distinct IPs accumulate as expected and the cap is
    /// enforced per IP, independent of other IPs' counts.
    #[test]
    fn attempt_allows_up_to_max_then_blocks_within_the_window() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        let ip: IpAddr = "203.0.113.10".parse().unwrap();
        assert!(limiter.attempt(ip));
        assert!(limiter.attempt(ip));
        assert!(limiter.attempt(ip));
        assert!(
            !limiter.attempt(ip),
            "a 4th attempt within the window must be blocked"
        );

        let other: IpAddr = "203.0.113.11".parse().unwrap();
        assert!(
            limiter.attempt(other),
            "a different IP must not be blocked by another IP's count"
        );
    }

    /// Once `window` elapses, the same IP's counter resets rather than
    /// staying blocked forever. Uses `attempt_at` with explicit `Instant`s
    /// instead of a real sleep, so this never flakes under load.
    #[test]
    fn attempt_resets_after_the_window_elapses() {
        let limiter = RateLimiter::new(1, Duration::from_millis(20));
        let ip: IpAddr = "203.0.113.12".parse().unwrap();
        let t0 = Instant::now();
        assert!(limiter.attempt_at(ip, t0));
        assert!(
            !limiter.attempt_at(ip, t0),
            "2nd attempt within the window is blocked"
        );
        assert!(
            limiter.attempt_at(ip, t0 + Duration::from_millis(30)),
            "attempt after the window elapsed must be allowed again"
        );
    }

    /// An IP's own window must end on time even when the map-wide sweep
    /// does not line up with it. Here `blocked` reaches its limit one second
    /// after a sweep, so the next sweep, a full window later, runs one second
    /// before `blocked`'s window expires and keeps its entry. One second after
    /// that its window has elapsed while the following sweep is still almost
    /// a window away; the attempt must be allowed then, not up to a window
    /// later. Fails on a version that only resets counters through the sweep.
    #[test]
    fn attempt_resets_on_its_own_window_between_sweeps() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let blocked: IpAddr = "203.0.113.13".parse().unwrap();
        let other: IpAddr = "203.0.113.14".parse().unwrap();
        let t0 = Instant::now();
        let at = |secs| t0 + Duration::from_secs(secs);
        assert!(limiter.attempt_at(other, at(61)), "runs a sweep at 61s");
        assert!(limiter.attempt_at(blocked, at(62)));
        assert!(!limiter.attempt_at(blocked, at(63)), "over the limit");
        assert!(
            limiter.attempt_at(other, at(121)),
            "runs the next sweep at 121s, while blocked's window is still open"
        );
        assert!(
            limiter.attempt_at(blocked, at(122)),
            "blocked's own window elapsed at 122s and must no longer block it"
        );
    }

    /// Without eviction, `attempts` gains one entry per distinct IP forever,
    /// since nothing else ever removes a key (every `ip` is attacker
    /// controlled: any client that can reach `/login`). This asserts the map
    /// stays bounded to roughly one window's worth of IPs across many
    /// windows, rather than growing with the total distinct IPs ever seen.
    /// Fails on the pre-fix code, where the map holds all 300 entries (3
    /// waves of 100 distinct IPs each) instead of at most 100. Uses
    /// `attempt_at` with explicit `Instant`s instead of a real sleep between
    /// waves, so this never flakes under load.
    #[test]
    fn attempts_map_stays_bounded_across_many_expired_windows() {
        let limiter = RateLimiter::new(5, Duration::from_millis(20));
        let mut now = Instant::now();
        for wave in 0..3u32 {
            for i in 0..100u32 {
                let ip: IpAddr = format!("2001:db8::{:x}", wave * 100 + i).parse().unwrap();
                limiter.attempt_at(ip, now);
            }
            now += Duration::from_millis(30);
        }
        let size = limiter.state.lock().unwrap().attempts.len();
        assert!(
            size <= 100,
            "attempts map should stay bounded to roughly one window's worth of IPs \
             (100), not the 300 distinct IPs seen across all windows; got {size}"
        );
    }

    /// The sweep runs at most once per `window`, not on every call: an IP
    /// can individually outlive its own `window` by up to about one more
    /// window before the next sweep actually catches it. This drives the
    /// state through exactly that gap. A first call forces a sweep (moving
    /// `last_sweep` forward on its own, decoupled from `target`'s insertion
    /// below). `target` is then inserted, and a later call lands just as a
    /// full `window` has passed since that sweep: this second sweep runs but
    /// does not evict `target` yet, since `target` is still younger than
    /// `window` at that moment, and it becomes the new reference point. A
    /// further call, well past `target`'s own `window` but well within a
    /// `window` of that second sweep, must find `target` still present.
    /// Only once a full `window` has passed since that second sweep does the
    /// next call evict it. Fails against a version that runs the sweep on
    /// every call, since that version evicts `target` as soon as it
    /// individually ages past `window`, before the "still present" check
    /// below runs. Uses `attempt_at` with explicit `Instant`s derived from a
    /// single captured `t0` instead of real sleeps, so this never flakes
    /// under load.
    #[test]
    fn sweep_runs_at_most_once_per_window_not_on_every_call() {
        let window = Duration::from_millis(200);
        let limiter = RateLimiter::new(5, window);
        let target: IpAddr = "203.0.113.20".parse().unwrap();
        let other: IpAddr = "203.0.113.21".parse().unwrap();
        let t0 = Instant::now();

        // The map is empty, so this sweep has nothing to evict; it only
        // moves `last_sweep` forward, ahead of `target`'s insertion below.
        let t1 = t0 + window + Duration::from_millis(50);
        limiter.attempt_at(other, t1);

        // `target` is inserted well after that sweep, so its own age and the
        // time since the last sweep diverge from here on.
        let t2 = t1 + Duration::from_millis(60);
        limiter.attempt_at(target, t2);

        // A full `window` has now passed since the first sweep, so this call
        // sweeps again. `target` is only about `window` minus 60ms old here,
        // so it survives, and this sweep becomes the new reference point.
        let t3 = t1 + window + Duration::from_millis(50);
        limiter.attempt_at(other, t3);

        // `target` has individually exceeded `window` by now, but only a
        // short time has passed since the sweep above, so this call must not
        // sweep again.
        let t4 = t3 + Duration::from_millis(70);
        limiter.attempt_at(other, t4);
        assert!(
            limiter.state.lock().unwrap().attempts.contains_key(&target),
            "target individually exceeded window, but the sweep only runs once \
             per window and the last one is still recent, so target must still \
             be present"
        );

        // A full `window` has now passed since the second sweep, so this
        // call finally evicts target.
        let t5 = t3 + window + Duration::from_millis(50);
        limiter.attempt_at(other, t5);
        assert!(
            !limiter.state.lock().unwrap().attempts.contains_key(&target),
            "a full window has now passed since the last sweep, so target must \
             have been evicted"
        );
    }
}
