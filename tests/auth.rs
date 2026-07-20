//! Integration tests for Task 11 (proxy-trust default + optional built-in
//! login), through the REAL router (`routes::router`): the `require_auth`
//! gate in both modes, `POST /login`'s username+password check and fail-
//! closed behavior, session fixation defense (id rotation on login), logout
//! (session flush), and the per-IP rate limiter. Every request goes through
//! the same session + CSRF/same-origin middleware exercised by
//! `tests/csrf.rs`, `tests/control.rs`, and `tests/settings.rs`.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use plugboard::auth::{MAX_LOGIN_ATTEMPTS, hash_password};
use plugboard::config::{AuthConfig, AuthMode, Config};
use plugboard::routes;
use plugboard::state::AppState;

const USERNAME: &str = "admin";
const PASSWORD: &str = "S3cure-Pass-2026";

/// RFC 5737 TEST-NET-3 address used as the fake client IP throughout (the
/// per-IP rate limiter needs a `ConnectInfo`, supplied via `MockConnectInfo`
/// since `.oneshot()` bypasses the real `into_make_service_with_connect_info`
/// wiring `main` uses in production).
fn test_client_addr() -> SocketAddr {
    SocketAddr::from(([203, 0, 113, 77], 0))
}

fn builtin_config() -> Config {
    Config {
        auth: AuthConfig {
            mode: AuthMode::Builtin,
            username: Some(USERNAME.into()),
            password_hash: Some(hash_password(PASSWORD)),
            cookie_secure: false,
        },
        ..Config::default()
    }
}

fn app(config: Config) -> Router {
    let state = AppState::new(config, PathBuf::from("unused.toml"));
    routes::router(state, false).layer(MockConnectInfo(test_client_addr()))
}

async fn body_string(response: Response<Body>) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn cookie_from(response: &Response<Body>) -> String {
    response
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .expect("response should set a session cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

/// GETs `/login`, returning the session cookie and the CSRF token scraped
/// from the `<meta name="csrf-token">` tag `views::layout::page` embeds
/// (mirrors `tests/control.rs::get_cookie_and_token`, pointed at `/login`
/// instead of `/` since `/` is gated behind `require_auth` in Builtin mode).
async fn get_login(app: &Router) -> (String, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = cookie_from(&response);

    let body = body_string(response).await;
    let marker = r#"name="csrf-token" content=""#;
    let start = body.find(marker).expect("csrf meta tag present") + marker.len();
    let end = start + body[start..].find('"').expect("closing quote");
    (cookie, body[start..end].to_string())
}

/// POSTs `/login`, same-origin and CSRF-authenticated, with `username` and
/// `password` as a form-urlencoded body.
async fn post_login(
    app: &Router,
    cookie: &str,
    token: &str,
    username: &str,
    password: &str,
) -> Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/login")
                .header("cookie", cookie)
                .header("x-csrf-token", token)
                .header("sec-fetch-site", "same-origin")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "username={username}&password={password}"
                )))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn get_root(app: &Router, cookie: Option<&str>) -> Response<Body> {
    let mut builder = Request::builder().method("GET").uri("/");
    if let Some(c) = cookie {
        builder = builder.header("cookie", c);
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// Logs in with the correct credentials and returns the rotated,
/// authenticated session cookie plus the (unchanged - `cycle_id` retains
/// session data) CSRF token, ready for further authenticated requests.
async fn login(app: &Router) -> (String, String) {
    let (cookie, token) = get_login(app).await;
    let response = post_login(app, &cookie, &token, USERNAME, PASSWORD).await;
    assert_eq!(response.status(), StatusCode::OK);
    let authed_cookie = cookie_from(&response);
    (authed_cookie, token)
}

// ---------------------------------------------------------------------------
// require_auth gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn builtin_get_root_without_session_redirects_to_login() {
    let app = app(builtin_config());
    let response = get_root(&app, None).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::LOCATION)
            .unwrap(),
        "/login"
    );
}

#[tokio::test]
async fn proxy_mode_get_root_returns_200_without_login() {
    let app = app(Config::default());
    let response = get_root(&app, None).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn get_login_renders_csrf_token_without_prior_session() {
    let app = app(builtin_config());
    let (_cookie, token) = get_login(&app).await;
    assert!(!token.is_empty(), "login page must carry a CSRF token");
}

// ---------------------------------------------------------------------------
// POST /login: username + password, both required
// ---------------------------------------------------------------------------

/// Wrong password with the correct username is rejected, paired here with
/// the correct-credentials case below (same username, same session) so the
/// rejection is proven to come from the password check, not a coincidental
/// setup error.
#[tokio::test]
async fn wrong_password_is_rejected() {
    let app = app(builtin_config());
    let (cookie, token) = get_login(&app).await;

    let response = post_login(&app, &cookie, &token, USERNAME, "totally-wrong-password").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = body_string(response).await;
    assert!(body.contains("invalid credentials"), "body was: {body}");

    // Non-vacuous: the SAME session, given the correct password, does authenticate.
    let ok = post_login(&app, &cookie, &token, USERNAME, PASSWORD).await;
    assert_eq!(ok.status(), StatusCode::OK);
}

/// A correct password with the WRONG username must be rejected - the
/// username gate is not bypassable by a correct password alone. Paired with
/// a same-session correct-credentials attempt to prove the setup itself
/// (session, CSRF, hash) is valid and would succeed given the right username.
#[tokio::test]
async fn wrong_username_with_correct_password_is_rejected() {
    let app = app(builtin_config());
    let (cookie, token) = get_login(&app).await;

    let response = post_login(&app, &cookie, &token, "not-admin", PASSWORD).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = body_string(response).await;
    assert!(body.contains("invalid credentials"), "body was: {body}");

    // Non-vacuous: the SAME password, with the correct username, does authenticate.
    let ok = post_login(&app, &cookie, &token, USERNAME, PASSWORD).await;
    assert_eq!(ok.status(), StatusCode::OK);
}

/// Both the username (constant-time compare) and the argon2-verified
/// password matching authenticates the session, which then grants access to
/// a gated route.
#[tokio::test]
async fn correct_credentials_authenticate_and_grant_access() {
    let app = app(builtin_config());
    let (authed_cookie, _token) = login(&app).await;

    let response = get_root(&app, Some(&authed_cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Fail closed: Builtin mode with a missing username or password_hash
// ---------------------------------------------------------------------------

/// An empty `password_hash` must reject every login attempt, even one using
/// the correct username with a plausible password - there is nothing valid
/// to compare against, so this must fail closed rather than coincidentally
/// matching an empty/unparseable hash.
#[tokio::test]
async fn fails_closed_when_password_hash_is_empty_string() {
    let config = Config {
        auth: AuthConfig {
            mode: AuthMode::Builtin,
            username: Some(USERNAME.into()),
            password_hash: Some(String::new()),
            cookie_secure: false,
        },
        ..Config::default()
    };
    let app = app(config);
    let (cookie, token) = get_login(&app).await;

    let response = post_login(&app, &cookie, &token, USERNAME, PASSWORD).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Non-vacuous: the rejected session never became authenticated - GET / still
    // redirects to /login rather than granting access some other way.
    let root = get_root(&app, Some(&cookie)).await;
    assert_eq!(root.status(), StatusCode::SEE_OTHER);
}

/// A missing `username` must reject every login attempt, even one carrying
/// the password that DOES match the configured hash.
#[tokio::test]
async fn fails_closed_when_username_missing() {
    let config = Config {
        auth: AuthConfig {
            mode: AuthMode::Builtin,
            username: None,
            password_hash: Some(hash_password(PASSWORD)),
            cookie_secure: false,
        },
        ..Config::default()
    };
    let app = app(config);
    let (cookie, token) = get_login(&app).await;

    let response = post_login(&app, &cookie, &token, USERNAME, PASSWORD).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let root = get_root(&app, Some(&cookie)).await;
    assert_eq!(root.status(), StatusCode::SEE_OTHER);
}

// ---------------------------------------------------------------------------
// Session fixation + logout
// ---------------------------------------------------------------------------

/// `POST /login` must rotate the session id on success (`session.cycle_id()`
/// before the authenticated marker is set): the pre-login and post-login
/// `Set-Cookie` values must differ, or an attacker who fixed the pre-auth
/// session id could ride the victim's authenticated session.
#[tokio::test]
async fn login_rotates_session_id_session_fixation_defense() {
    let app = app(builtin_config());
    let (pre_login_cookie, token) = get_login(&app).await;

    let response = post_login(&app, &pre_login_cookie, &token, USERNAME, PASSWORD).await;
    assert_eq!(response.status(), StatusCode::OK);
    let post_login_cookie = cookie_from(&response);

    assert_ne!(
        pre_login_cookie, post_login_cookie,
        "the session id must be rotated on successful login"
    );
}

/// `POST /logout` flushes the session entirely: the old (authenticated)
/// cookie must no longer authenticate afterward.
#[tokio::test]
async fn logout_flushes_session_old_cookie_no_longer_authenticates() {
    let app = app(builtin_config());
    let (authed_cookie, token) = login(&app).await;

    // Confirm the cookie really is authenticated before logging out.
    let before = get_root(&app, Some(&authed_cookie)).await;
    assert_eq!(before.status(), StatusCode::OK);

    let logout_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/logout")
                .header("cookie", &authed_cookie)
                .header("x-csrf-token", &token)
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Logout answers the htmx Sign out POST with the same full-page
    // `hx-redirect` navigation contract as login (an XHR-followed 303 would
    // hand htmx the login page's HTML to swap inline instead of navigating).
    assert_eq!(logout_response.status(), StatusCode::OK);
    assert_eq!(
        logout_response
            .headers()
            .get("hx-redirect")
            .expect("logout must carry an hx-redirect header"),
        "/login"
    );

    let after = get_root(&app, Some(&authed_cookie)).await;
    assert_eq!(
        after.status(),
        StatusCode::SEE_OTHER,
        "the old session cookie must no longer authenticate after logout"
    );
}

// ---------------------------------------------------------------------------
// Rate limiting
// ---------------------------------------------------------------------------

/// After `MAX_LOGIN_ATTEMPTS` attempts from the same IP, the NEXT attempt is
/// rejected with 429 - even one carrying the correct credentials - proving
/// the limiter is enforced before the credential check runs, not just
/// coincidentally exhausted by wrong attempts.
#[tokio::test]
async fn rate_limit_blocks_after_max_attempts() {
    let app = app(builtin_config());
    let (cookie, token) = get_login(&app).await;

    for _ in 0..MAX_LOGIN_ATTEMPTS {
        let response = post_login(&app, &cookie, &token, USERNAME, "wrong-password").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    let blocked = post_login(&app, &cookie, &token, USERNAME, PASSWORD).await;
    assert_eq!(
        blocked.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the (N+1)th attempt must be rate-limited even with correct credentials"
    );
}

// ---------------------------------------------------------------------------
// Assets: no session/CSRF/auth gate
// ---------------------------------------------------------------------------

/// Static assets stay reachable with no cookie at all, even in Builtin mode
/// with no session - `/assets/:file` sits outside every layer `router()`
/// applies.
#[tokio::test]
async fn assets_route_has_no_session_or_auth_gate() {
    let app = app(builtin_config());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/assets/app.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
