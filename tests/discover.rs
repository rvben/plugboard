//! Integration tests for device discovery (Task 9): route validation + the
//! real scan wiring (empty path), results rendering (pure view), and add
//! (including non-vacuous duplicate rejection).
//!
//! `hosts_in_cidr` yields bare IPs probed on port 80, so a route-level scan
//! cannot reach an `httpmock` on a random loopback port, and putting a
//! loopback host where only RFC 5737 device addresses are allowed is
//! disallowed. Scan-REACHABILITY (a live device answering) is already
//! `tasmota-core`'s tested contract; these tests only exercise what
//! `tasmota-web` owns: the scan WIRING (the real `discovery::scan` runs
//! inside `spawn_blocking` and returns empty for an unreachable doc range),
//! the `(name, host)` rendering, and add - all with RFC 5737 addresses and no
//! loopback host.

use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use tasmota_web::config::Config;
use tasmota_web::routes;
use tasmota_web::state::AppState;
use tasmota_web::views::discover;

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

async fn body_string(response: Response<Body>) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// POSTs a form-urlencoded body, same-origin and CSRF-authenticated (mirrors
/// `tests/admin.rs::post_form`).
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

fn test_app() -> Router {
    let state = AppState::new(Config::default(), PathBuf::from("unused.toml"));
    routes::router(state, false)
}

/// A per-test scratch config path outside the repo: `add` actually calls
/// `state.save_config()` (unlike every other route exercised in this crate's
/// test suite so far), so a shared/relative path would either fail to write
/// or leave a stray file in the repo. `name` keeps concurrently-run tests
/// from colliding on the same file.
fn temp_config_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "tasmota-web-test-discover-{name}-{}.toml",
        std::process::id()
    ))
}

/// `GET /discover` renders the scan form (posting to `/discover/scan`) inside
/// the normal page shell. Does not assert a specific default range value:
/// `discovery::detect_local_cidr()` reflects whatever network the test runs
/// on, so pinning its output would coincidentally bake in a real local
/// subnet.
#[tokio::test]
async fn discover_index_renders_scan_form() {
    let app = test_app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/discover")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains(r#"hx-post="/discover/scan""#), "got: {body}");
    assert!(body.contains(r#"name="range""#), "got: {body}");
    assert!(
        body.contains(r#"id="discover-results""#),
        "the results swap target must be present, got: {body}"
    );
}

// ---------------------------------------------------------------------------
// route validation + real-scan wiring (empty path)
// ---------------------------------------------------------------------------

/// A malformed range is rejected with 400 before any scan runs - `hosts_in_cidr`'s
/// own guard, surfaced as `AppError::BadRequest`.
#[tokio::test]
async fn scan_rejects_invalid_range_with_bad_request() {
    let app = test_app();
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(&app, &cookie, &token, "/discover/scan", "range=not-a-cidr").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// A syntactically valid but unreachable documentation range runs the REAL
/// `discovery::scan` inside `spawn_blocking` (proving the wiring compiles and
/// executes on a real worker) and returns 200 with the empty-results hint,
/// never an error and never a fabricated row.
///
/// Proves the real scan wiring returns 200 + the hint end to end against an
/// unreachable range. `tasmota-core` 0.1.2+ bounds the TCP connect (its
/// `HttpTransport` sets ureq's `.timeout_connect()` to 2s), so an unreachable
/// host fails fast instead of paying ureq's 30s connect default. The empty-hint
/// RENDERING itself also stays covered by the fast, network-free
/// `results_renders_hint_when_empty` below.
#[tokio::test]
async fn scan_unreachable_range_returns_empty_hint() {
    let app = test_app();
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(
        &app,
        &cookie,
        &token,
        "/discover/scan",
        "range=192.0.2.0/30",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(
        body.contains("Check the range"),
        "an empty scan should render the check-the-range hint, body was: {body}"
    );
    assert!(
        !body.contains("discover-results-list"),
        "an empty scan must not render a results list, body was: {body}"
    );
}

// ---------------------------------------------------------------------------
// results rendering (pure view, no network)
// ---------------------------------------------------------------------------

/// `views::discover::results` lists the host and renders an Add form carrying
/// `name`+`host` as hidden fields - the exact contract `routes::discover::scan`
/// depends on when it maps `Discovered` to `(display_name, host)` pairs.
#[test]
fn results_renders_host_and_add_form_hidden_fields() {
    let found = vec![("Lab".to_string(), "192.0.2.5".to_string())];
    let markup = discover::results(&found).into_string();

    assert!(
        markup.contains("192.0.2.5"),
        "body should list the host, got: {markup}"
    );
    assert!(
        markup.contains("Lab"),
        "body should list the display name, got: {markup}"
    );
    assert!(
        markup.contains(r#"hx-post="/discover/add""#),
        "the Add button must post to /discover/add, got: {markup}"
    );
    assert!(
        markup.contains(r#"name="name" value="Lab""#),
        "the Add form must carry the name as a hidden field, got: {markup}"
    );
    assert!(
        markup.contains(r#"name="host" value="192.0.2.5""#),
        "the Add form must carry the host as a hidden field, got: {markup}"
    );
}

/// An empty result renders the hint, not an empty (or missing) list element.
#[test]
fn results_renders_hint_when_empty() {
    let markup = discover::results(&[]).into_string();
    assert!(markup.contains("No devices found"), "got: {markup}");
    assert!(!markup.contains("discover-results-list"), "got: {markup}");
}

// ---------------------------------------------------------------------------
// add
// ---------------------------------------------------------------------------

/// `POST /discover/add` appends a new device to both the config and the
/// fleet.
#[tokio::test]
async fn add_appends_device_to_config_and_fleet() {
    let path = temp_config_path("appends");
    let state = AppState::new(Config::default(), path.clone());
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(
        &app,
        &cookie,
        &token,
        "/discover/add",
        "name=Lab&host=192.0.2.5",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let cfg = state.inner.config.read().await;
    assert_eq!(cfg.devices.len(), 1);
    assert_eq!(cfg.devices[0].host, "192.0.2.5");
    assert_eq!(cfg.devices[0].name, "Lab");
    drop(cfg);

    let fleet = state.inner.fleet.read().await;
    assert_eq!(fleet.devices.len(), 1);
    assert_eq!(fleet.devices[0].host, "192.0.2.5");

    // The add also persisted to disk (state.save_config(), off the async
    // runtime): reload from the path and confirm the device survived.
    let reloaded = Config::load(&path).expect("saved config should reload");
    assert_eq!(reloaded.devices.len(), 1);
    assert_eq!(reloaded.devices[0].host, "192.0.2.5");
    let _ = std::fs::remove_file(&path);
}

/// A duplicate add is rejected, proven non-vacuous: the first add of a host
/// succeeds (200), the second identical add is rejected (400/409) AND the
/// fleet/config still contain exactly one entry for that host, never two.
#[tokio::test]
async fn add_rejects_duplicate_host_without_duplicating() {
    let path = temp_config_path("duplicate");
    let state = AppState::new(Config::default(), path.clone());
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let first = post_form(
        &app,
        &cookie,
        &token,
        "/discover/add",
        "name=Lab&host=192.0.2.5",
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK, "the first add must succeed");

    let second = post_form(
        &app,
        &cookie,
        &token,
        "/discover/add",
        "name=Lab+Duplicate&host=192.0.2.5",
    )
    .await;
    assert!(
        second.status() == StatusCode::BAD_REQUEST || second.status() == StatusCode::CONFLICT,
        "a duplicate host must be rejected with 400 or 409, got: {}",
        second.status()
    );

    let cfg = state.inner.config.read().await;
    assert_eq!(
        cfg.devices.len(),
        1,
        "a duplicate add must not append a second config entry"
    );
    drop(cfg);

    let fleet = state.inner.fleet.read().await;
    assert_eq!(
        fleet.devices.len(),
        1,
        "a duplicate add must not append a second fleet entry"
    );
    drop(fleet);
    let _ = std::fs::remove_file(&path);
}

/// If `state.save_config()` fails, the just-pushed device must be rolled back
/// out of the in-memory config - otherwise it lingers as a "ghost" entry that
/// was never persisted or added to the fleet, yet still fails every future
/// duplicate check for that host.
///
/// `Config::save` fails deterministically when `path.parent()` exists but is
/// NOT a directory: `std::fs::create_dir_all` on a path whose parent is a
/// regular file returns `Err(AlreadyExists)`. Using a plain file as the
/// config path's directory forces `save_config().await` to fail on every
/// call, with no mocking required.
///
/// Proven non-vacuous: the first add must fail with a server error (not
/// 200), AND a second add for the SAME host must NOT be rejected as a
/// duplicate (400) - if the rollback were missing, the ghost entry from the
/// first attempt would trip the duplicate check and the second attempt would
/// come back 400 instead of failing again with a save error.
#[tokio::test]
async fn add_rolls_back_on_save_failure_without_ghosting() {
    let blocker = std::env::temp_dir().join(format!(
        "tasmota-web-test-discover-blocker-{}.tmp",
        std::process::id()
    ));
    std::fs::write(&blocker, b"not a directory").expect("write blocker file");
    let bad_path = blocker.join("config.toml");

    let state = AppState::new(Config::default(), bad_path);
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let first = post_form(
        &app,
        &cookie,
        &token,
        "/discover/add",
        "name=Lab&host=192.0.2.9",
    )
    .await;
    assert_eq!(
        first.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "a save failure must surface as an error status, not 200"
    );

    let cfg = state.inner.config.read().await;
    assert!(
        cfg.devices.is_empty(),
        "a failed save must roll back the in-memory push, got: {:?}",
        cfg.devices.iter().map(|d| &d.host).collect::<Vec<_>>()
    );
    drop(cfg);

    let fleet = state.inner.fleet.read().await;
    assert!(
        fleet.devices.is_empty(),
        "a failed save must never have updated the fleet"
    );
    drop(fleet);

    // Non-vacuous proof: retry the SAME host. A lingering ghost would make
    // this come back 400 (duplicate) instead of failing again with 500.
    let second = post_form(
        &app,
        &cookie,
        &token,
        "/discover/add",
        "name=Lab&host=192.0.2.9",
    )
    .await;
    assert_ne!(
        second.status(),
        StatusCode::BAD_REQUEST,
        "a retry for the same host must not be rejected as a duplicate - that \
         would mean a ghost entry survived the failed save"
    );

    let _ = std::fs::remove_file(&blocker);
}
