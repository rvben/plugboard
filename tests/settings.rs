//! Integration tests for the settings page (Task 10): rename/remove/
//! credentials/protected/poll-interval handlers, all through the REAL
//! router (session + CSRF/same-origin middleware included), plus the
//! non-vacuous proof that a stored device password and the auth
//! `password_hash` are never rendered into `GET /settings` HTML.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use plugboard::auth::hash_password;
use plugboard::config::{AuthConfig, AuthMode, Config, DeviceConfig};
use plugboard::fleet::device_id;
use plugboard::routes;
use plugboard::state::AppState;
use switchkit::Vendor;

/// RFC 5737 TEST-NET-3 address used as the fake client IP for tests that
/// exercise `POST /login` (its per-IP rate limiter needs a `ConnectInfo`,
/// supplied here via `MockConnectInfo` since `.oneshot()` bypasses the real
/// `into_make_service_with_connect_info` wiring `main` uses).
fn test_client_addr() -> SocketAddr {
    SocketAddr::from(([203, 0, 113, 42], 0))
}

fn config_with(devices: Vec<DeviceConfig>) -> Config {
    Config {
        devices,
        ..Config::default()
    }
}

fn device(host: &str, name: &str) -> DeviceConfig {
    DeviceConfig {
        name: name.into(),
        host: host.into(),
        password: None,
        protected: false,
        vendor: Vendor::Tasmota,
    }
}

async fn get_cookie_and_token(app: &Router) -> (String, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let cookie = response
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .expect("GET / should set a session cookie")
        .to_str()
        .unwrap()
        .to_string();
    let cookie = cookie.split(';').next().unwrap().to_string();

    let body = body_string(response).await;
    let marker = r#"name="csrf-token" content=""#;
    let start = body.find(marker).expect("csrf meta tag present") + marker.len();
    let end = start + body[start..].find('"').expect("closing quote");
    (cookie, body[start..end].to_string())
}

/// Like `get_cookie_and_token`, but against the public `GET /login` route
/// instead of `/` - needed once a config is in `AuthMode::Builtin`, where
/// `/` is gated behind `require_auth` and only `/login` is reachable
/// without a prior session.
async fn get_login_cookie_and_token(app: &Router) -> (String, String) {
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

    let cookie = response
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .expect("GET /login should set a session cookie")
        .to_str()
        .unwrap()
        .to_string();
    let cookie = cookie.split(';').next().unwrap().to_string();

    let body = body_string(response).await;
    let marker = r#"name="csrf-token" content=""#;
    let start = body.find(marker).expect("csrf meta tag present") + marker.len();
    let end = start + body[start..].find('"').expect("closing quote");
    (cookie, body[start..end].to_string())
}

async fn body_string(response: Response<Body>) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// POSTs a form-urlencoded body, same-origin and CSRF-authenticated (mirrors
/// `tests/discover.rs::post_form`).
async fn post_form(
    app: &Router,
    cookie: &str,
    token: &str,
    path: &str,
    body: &str,
) -> Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("cookie", cookie)
                .header("x-csrf-token", token)
                .header("sec-fetch-site", "same-origin")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn get_route(app: &Router, cookie: &str, path: &str) -> Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// A per-test scratch config path outside the repo: every handler here
/// actually calls `state.save_config()` (like `routes::discover::add`), so a
/// shared/relative path would either fail to write or leave a stray file in
/// the repo. `name` keeps concurrently-run tests from colliding.
fn temp_config_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "plugboard-test-settings-{name}-{}.toml",
        std::process::id()
    ))
}

// ---------------------------------------------------------------------------
// rename
// ---------------------------------------------------------------------------

/// POSTing a rename updates the config AND the fleet's `DeviceView` (same
/// host, so the same id) - proven from both sides, plus a disk reload.
#[tokio::test]
async fn rename_updates_config_and_fleet() {
    let path = temp_config_path("rename");
    let host = "192.0.2.5";
    let config = config_with(vec![device(host, "Old Name")]);
    let state = AppState::new(config, path.clone());
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(
        &app,
        &cookie,
        &token,
        "/settings/device/rename",
        &format!("host={host}&name=New+Name"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let cfg = state.inner.config.read().await;
    assert_eq!(cfg.devices[0].name, "New Name");
    drop(cfg);

    let fleet = state.inner.fleet.read().await;
    let id = device_id(host);
    assert_eq!(fleet.get(&id).expect("device present").name, "New Name");
    drop(fleet);

    let reloaded = Config::load(&path).expect("saved config should reload");
    assert_eq!(reloaded.devices[0].name, "New Name");
    let _ = std::fs::remove_file(&path);
}

/// Renaming an unknown host 404s rather than silently no-op-ing.
#[tokio::test]
async fn rename_unknown_host_is_not_found() {
    let path = temp_config_path("rename-unknown");
    let state = AppState::new(Config::default(), path.clone());
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(
        &app,
        &cookie,
        &token,
        "/settings/device/rename",
        "host=192.0.2.9&name=X",
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// credentials: write-only, non-vacuous proof it is never rendered
// ---------------------------------------------------------------------------

/// A posted credential is stored in config, but the value never appears in
/// the subsequent `GET /settings` HTML. Proven non-vacuous: the device row
/// itself (name/host) DOES appear in that same body, so the password's
/// absence is not just because nothing rendered.
#[tokio::test]
async fn credentials_are_stored_but_never_rendered_in_get() {
    let path = temp_config_path("credentials");
    let host = "192.0.2.6";
    let config = config_with(vec![device(host, "Plug")]);
    let state = AppState::new(config, path.clone());
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let secret = "s3cr3t-device-password-xyz";
    let response = post_form(
        &app,
        &cookie,
        &token,
        "/settings/device/credentials",
        &format!("host={host}&password={secret}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // Stored server-side.
    let cfg = state.inner.config.read().await;
    assert_eq!(cfg.devices[0].password.as_deref(), Some(secret));
    drop(cfg);

    // Never echoed back, not even in the POST's own response body.
    let post_body = body_string(response).await;
    assert!(
        !post_body.contains(secret),
        "the POST /settings/device/credentials response must not echo the password, got: {post_body}"
    );

    let get_response = get_route(&app, &cookie, "/settings").await;
    assert_eq!(get_response.status(), StatusCode::OK);
    let get_body = body_string(get_response).await;
    assert!(
        !get_body.contains(secret),
        "GET /settings must never render a stored device password, got: {get_body}"
    );
    // Non-vacuous: the device row itself is present, so the missing secret
    // isn't just an empty/broken page.
    assert!(get_body.contains("Plug"), "got: {get_body}");
    assert!(get_body.contains(host), "got: {get_body}");
    // The "credential set" badge proves the page KNOWS a password exists,
    // without revealing it.
    assert!(get_body.contains("credential set"), "got: {get_body}");

    let _ = std::fs::remove_file(&path);
}

/// Posting an empty credential clears a previously-set password.
#[tokio::test]
async fn credentials_empty_submission_clears_password() {
    let path = temp_config_path("credentials-clear");
    let host = "192.0.2.7";
    let mut d = device(host, "Plug");
    d.password = Some("old-password".into());
    let config = config_with(vec![d]);
    let state = AppState::new(config, path.clone());
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(
        &app,
        &cookie,
        &token,
        "/settings/device/credentials",
        &format!("host={host}&password="),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let cfg = state.inner.config.read().await;
    assert_eq!(cfg.devices[0].password, None);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// remove
// ---------------------------------------------------------------------------

/// POSTing a remove drops the device from both config and fleet.
#[tokio::test]
async fn remove_drops_device_from_config_and_fleet() {
    let path = temp_config_path("remove");
    let host = "192.0.2.8";
    let config = config_with(vec![device(host, "Plug")]);
    let state = AppState::new(config, path.clone());
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(
        &app,
        &cookie,
        &token,
        "/settings/device/remove",
        &format!("host={host}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let cfg = state.inner.config.read().await;
    assert!(cfg.devices.is_empty());
    drop(cfg);

    let fleet = state.inner.fleet.read().await;
    assert!(fleet.devices.is_empty());
    drop(fleet);

    let reloaded = Config::load(&path).expect("saved config should reload");
    assert!(reloaded.devices.is_empty());
    let _ = std::fs::remove_file(&path);
}

/// If `save_config` fails, the removed device must be rolled back into both
/// config and fleet - otherwise it silently vanishes from the dashboard
/// despite the disk write never having happened. Proven non-vacuous: the
/// device count stays 1 (not 0), and a second remove attempt for the same
/// host still finds it (still fails to save, but with the SAME error, not a
/// 404 - a lingering "already removed" state would 404 instead).
#[tokio::test]
async fn remove_rolls_back_on_save_failure() {
    let blocker = std::env::temp_dir().join(format!(
        "plugboard-test-settings-blocker-{}.tmp",
        std::process::id()
    ));
    std::fs::write(&blocker, b"not a directory").expect("write blocker file");
    let bad_path = blocker.join("config.toml");

    let host = "192.0.2.11";
    let config = config_with(vec![device(host, "Plug")]);
    let state = AppState::new(config, bad_path);
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let first = post_form(
        &app,
        &cookie,
        &token,
        "/settings/device/remove",
        &format!("host={host}"),
    )
    .await;
    assert_eq!(
        first.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "a save failure must surface as an error status, not 200"
    );

    let cfg = state.inner.config.read().await;
    assert_eq!(
        cfg.devices.len(),
        1,
        "a failed save must roll the removed device back into config"
    );
    drop(cfg);

    let fleet = state.inner.fleet.read().await;
    assert_eq!(
        fleet.devices.len(),
        1,
        "a failed save must never have touched the fleet"
    );
    drop(fleet);

    // Non-vacuous: retry the SAME host. If the rollback were missing, the
    // device would already be gone and this would 404 instead of failing
    // again with 500.
    let second = post_form(
        &app,
        &cookie,
        &token,
        "/settings/device/remove",
        &format!("host={host}"),
    )
    .await;
    assert_eq!(
        second.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "the device must still be present after a rolled-back remove, so a retry \
         fails on save again rather than 404ing on an already-gone device"
    );

    let _ = std::fs::remove_file(&blocker);
}

// ---------------------------------------------------------------------------
// protected
// ---------------------------------------------------------------------------

/// Checking the protected box sets it on config AND the fleet's `DeviceView`
/// (the field the toggle/admin routes actually gate on); unchecking (an
/// absent form field) clears it again - proving `Option<String>` correctly
/// distinguishes checked from unchecked, not just present-vs-absent-key.
#[tokio::test]
async fn protected_toggle_updates_config_and_fleet_both_ways() {
    let path = temp_config_path("protected");
    let host = "192.0.2.12";
    let config = config_with(vec![device(host, "Plug")]);
    let state = AppState::new(config, path.clone());
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let on = post_form(
        &app,
        &cookie,
        &token,
        "/settings/device/protected",
        &format!("host={host}&protected=true"),
    )
    .await;
    assert_eq!(on.status(), StatusCode::OK);
    {
        let cfg = state.inner.config.read().await;
        assert!(cfg.devices[0].protected);
        drop(cfg);
        let fleet = state.inner.fleet.read().await;
        assert!(fleet.get(&device_id(host)).expect("present").protected);
    }

    // An unchecked checkbox omits the field entirely.
    let off = post_form(
        &app,
        &cookie,
        &token,
        "/settings/device/protected",
        &format!("host={host}"),
    )
    .await;
    assert_eq!(off.status(), StatusCode::OK);
    let cfg = state.inner.config.read().await;
    assert!(!cfg.devices[0].protected);
    drop(cfg);
    let fleet = state.inner.fleet.read().await;
    assert!(!fleet.get(&device_id(host)).expect("present").protected);

    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// poll interval
// ---------------------------------------------------------------------------

#[tokio::test]
async fn poll_interval_updates_config() {
    let path = temp_config_path("poll-interval");
    let state = AppState::new(Config::default(), path.clone());
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(&app, &cookie, &token, "/settings/poll-interval", "secs=42").await;
    assert_eq!(response.status(), StatusCode::OK);

    let cfg = state.inner.config.read().await;
    assert_eq!(cfg.poll_interval_secs, 42);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// auth mode: read-only, password_hash never rendered
// ---------------------------------------------------------------------------

/// `GET /settings` shows the auth mode and whether a built-in credential is
/// configured, but the `password_hash` string itself never appears -
/// proven non-vacuous by asserting the mode label DOES render.
///
/// Builtin mode gates `/settings` behind `require_auth` (Task 11), so this
/// test must actually log in first (`GET /login` then `POST /login` with the
/// real credential the hash was generated from) before it can reach the
/// page at all - `GET /` is no longer a valid way to seed a session here.
#[tokio::test]
async fn auth_password_hash_is_never_rendered() {
    let path = temp_config_path("auth-hash");
    let password = "correct-horse-battery-staple";
    let hash = hash_password(password);
    let config = Config {
        auth: AuthConfig {
            mode: AuthMode::Builtin,
            username: Some("admin".into()),
            password_hash: Some(hash.clone()),
            cookie_secure: true,
        },
        ..Config::default()
    };
    let state = AppState::new(config, path.clone());
    let app = routes::router(state, false).layer(MockConnectInfo(test_client_addr()));

    let (login_cookie, login_token) = get_login_cookie_and_token(&app).await;
    let login_response = post_form(
        &app,
        &login_cookie,
        &login_token,
        "/login",
        &format!("username=admin&password={password}"),
    )
    .await;
    assert_eq!(login_response.status(), StatusCode::OK);
    let authed_cookie = login_response
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .expect("a successful login rotates the session cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let response = get_route(&app, &authed_cookie, "/settings").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;

    assert!(
        !body.contains(&hash),
        "the auth password_hash must never be rendered, got: {body}"
    );
    // Non-vacuous: the page does render auth-mode info, just not the hash.
    assert!(body.contains("builtin"), "got: {body}");
    assert!(body.contains("configured"), "got: {body}");
    let _ = std::fs::remove_file(&path);
}

/// Proxy mode (the default) renders without any "configured" credential
/// claim, and obviously without ever having a hash to leak.
#[tokio::test]
async fn auth_proxy_mode_renders_without_credential_claim() {
    let path = temp_config_path("auth-proxy");
    let state = AppState::new(Config::default(), path.clone());
    let app = routes::router(state, false);
    let (cookie, _token) = get_cookie_and_token(&app).await;

    let response = get_route(&app, &cookie, "/settings").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("proxy"), "got: {body}");
    let _ = std::fs::remove_file(&path);
}
