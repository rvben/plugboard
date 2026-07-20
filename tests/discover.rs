//! Integration tests for device discovery (Task 9, mixed-vendor per Plan C
//! Task 3): route validation + the real scan wiring (empty path), results
//! rendering (pure view), and add (including non-vacuous duplicate rejection
//! and the server-side vendor-confirmation security gate).
//!
//! `hosts_in_cidr` yields bare IPs probed on port 80, so a route-level SCAN
//! cannot reach an `httpmock` on a random loopback port, and putting a
//! loopback host where only RFC 5737 device addresses are allowed is
//! disallowed - `scan_unreachable_range_returns_empty_hint` below stays on a
//! documentation range. `POST /discover/add`, though, probes exactly ONE
//! caller-supplied host (not a CIDR range), so it legitimately CAN target an
//! `httpmock` address (`server.address()`) - that is the path the
//! mixed-vendor / forged-vendor security tests below use to exercise the
//! real `switchkit::discover` probe against both a mocked Tasmota and a
//! mocked Shelly device.

use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use httpmock::prelude::*;
use serde_json::json;
use tower::ServiceExt;

use switchkit::Vendor;
use tasmota_web::config::Config;
use tasmota_web::routes;
use tasmota_web::state::AppState;
use tasmota_web::views::discover;

/// Mocks a Tasmota `Status 0` probe response (same shape as
/// `tests/control.rs::mock_status`), enough for `tasmota_core::HttpTransport`'s
/// `probe()` (via `parse::looks_like_tasmota`, which requires
/// `StatusFWR.Version`) to confirm the host as Tasmota.
fn mock_tasmota_status0(server: &MockServer) {
    server.mock(|when, then| {
        when.method(GET).path("/cm").query_param("cmnd", "Status 0");
        then.status(200).json_body(json!({
            "Status": {"DeviceName": "TestPlug", "Module": 1, "FriendlyName": ["TestPlug"], "Power": 1},
            "StatusFWR": {"Version": "14.2.0"},
            "StatusNET": {"IPAddress": "192.0.2.50", "Mac": "AA:BB:CC:00:11:22", "Hostname": "testplug"},
            "StatusSTS": {"POWER": "ON", "Uptime": "1T02:03:04", "Wifi": {"RSSI": 76}}
        }));
    });
}

/// Mocks a Shelly Gen2 `/shelly` info response (same shape as
/// `tests/admin.rs::mock_shelly_gen2_info`) plus the `/rpc/Shelly.GetStatus`
/// call the Shelly `probe()` path also issues, enough for `shelly_core`'s
/// `ShellyClient::probe()` to confirm the host as Shelly.
fn mock_shelly_probe(server: &MockServer) {
    server.mock(|when, then| {
        when.method(GET).path("/shelly");
        then.status(200).json_body(json!({
            "id": "shellyplus1pm-aabbccddeeff",
            "mac": "AABBCCDDEEFF",
            "model": "SNSW-001P16EU",
            "gen": 2,
            "ver": "1.2.3",
            "app": "Plus1PM",
            "auth_en": false
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/rpc/Shelly.GetStatus");
        then.status(200).json_body(json!({
            "switch:0": {"output": true, "apower": 12.3}
        }));
    });
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
/// `switchkit::discover` directly on the async runtime (proving the wiring
/// compiles and executes end to end, no `spawn_blocking`) and returns 200
/// with the empty-results hint, never an error and never a fabricated row.
///
/// Proves the real scan wiring returns 200 + the hint end to end against an
/// unreachable range. The Tasmota client's `HttpTransport` bounds its connect
/// timeout, so an unreachable host fails fast rather than hanging the test.
/// The empty-hint RENDERING itself also stays covered by the fast,
/// network-free `results_renders_hint_when_empty` below.
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

/// `views::discover::results` lists the host, its discovered vendor, and
/// renders an Add form carrying `name`+`host` (deliberately no `vendor`
/// field - see the module doc comment and `routes::discover::add`) as hidden
/// fields - the exact contract `routes::discover::scan` depends on when it
/// maps `Discovered` to `(display_name, host, vendor)` triples.
#[test]
fn results_renders_host_and_add_form_hidden_fields() {
    let found = vec![("Lab".to_string(), "192.0.2.5".to_string(), Vendor::Tasmota)];
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
        markup.contains("Tasmota"),
        "body should show the discovered vendor, got: {markup}"
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
    assert!(
        !markup.contains(r#"name="vendor""#),
        "the Add form must never carry a client-supplied vendor field - add() \
         always re-confirms the vendor server-side, got: {markup}"
    );
}

/// A Shelly discovery result renders the Shelly vendor tag, not Tasmota's -
/// proves `results` actually threads the per-row vendor through rather than
/// defaulting every row to the same label.
#[test]
fn results_renders_shelly_vendor_distinctly_from_tasmota() {
    let found = vec![
        ("Lab".to_string(), "192.0.2.5".to_string(), Vendor::Tasmota),
        (
            "Plug".to_string(),
            "198.51.100.5".to_string(),
            Vendor::Shelly,
        ),
    ];
    let markup = discover::results(&found).into_string();
    assert!(markup.contains("Tasmota"), "got: {markup}");
    assert!(markup.contains("Shelly"), "got: {markup}");
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

/// `POST /discover/add` re-probes the host (see `probe_host`), and on a
/// confirmed vendor appends a new device to both the config and the fleet.
#[tokio::test]
async fn add_appends_device_to_config_and_fleet() {
    let server = MockServer::start();
    mock_tasmota_status0(&server);
    let host = server.address().to_string();

    let path = temp_config_path("appends");
    let state = AppState::new(Config::default(), path.clone());
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(
        &app,
        &cookie,
        &token,
        "/discover/add",
        &format!("name=Lab&host={host}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let cfg = state.inner.config.read().await;
    assert_eq!(cfg.devices.len(), 1);
    assert_eq!(cfg.devices[0].host, host);
    assert_eq!(cfg.devices[0].name, "Lab");
    assert_eq!(
        cfg.devices[0].vendor,
        Vendor::Tasmota,
        "the persisted vendor must be the one the server-side probe confirmed"
    );
    drop(cfg);

    let fleet = state.inner.fleet.read().await;
    assert_eq!(fleet.devices.len(), 1);
    assert_eq!(fleet.devices[0].host, host);

    // The add also persisted to disk (state.save_config(), off the async
    // runtime): reload from the path and confirm the device survived.
    let reloaded = Config::load(&path).expect("saved config should reload");
    assert_eq!(reloaded.devices.len(), 1);
    assert_eq!(reloaded.devices[0].host, host);
    let _ = std::fs::remove_file(&path);
}

/// A duplicate add is rejected, proven non-vacuous: the first add of a host
/// succeeds (200), the second identical add is rejected (400/409) AND the
/// fleet/config still contain exactly one entry for that host, never two.
#[tokio::test]
async fn add_rejects_duplicate_host_without_duplicating() {
    let server = MockServer::start();
    mock_tasmota_status0(&server);
    let host = server.address().to_string();

    let path = temp_config_path("duplicate");
    let state = AppState::new(Config::default(), path.clone());
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let first = post_form(
        &app,
        &cookie,
        &token,
        "/discover/add",
        &format!("name=Lab&host={host}"),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK, "the first add must succeed");

    let second = post_form(
        &app,
        &cookie,
        &token,
        "/discover/add",
        &format!("name=Lab+Duplicate&host={host}"),
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
    let server = MockServer::start();
    mock_tasmota_status0(&server);
    let host = server.address().to_string();

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
        &format!("name=Lab&host={host}"),
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
        &format!("name=Lab&host={host}"),
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

// ---------------------------------------------------------------------------
// mixed-vendor add + the SECURITY gate (server-side re-probe, never a
// caller-supplied vendor)
// ---------------------------------------------------------------------------

/// `add` persists the vendor `switchkit::discover` confirms for a mocked
/// Shelly host - proves the mixed-vendor probe actually reaches and
/// recognizes Shelly, not just Tasmota (every other `add_*` test above uses a
/// Tasmota fixture).
#[tokio::test]
async fn add_persists_probe_confirmed_vendor_for_a_mocked_shelly_host() {
    let server = MockServer::start();
    mock_shelly_probe(&server);
    let host = server.address().to_string();

    let path = temp_config_path("shelly-confirmed");
    let state = AppState::new(Config::default(), path.clone());
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(
        &app,
        &cookie,
        &token,
        "/discover/add",
        &format!("name=Plug&host={host}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let cfg = state.inner.config.read().await;
    assert_eq!(cfg.devices.len(), 1);
    assert_eq!(
        cfg.devices[0].vendor,
        Vendor::Shelly,
        "the persisted vendor must be the one the server-side probe confirmed"
    );
    drop(cfg);
    let _ = std::fs::remove_file(&path);
}

/// A host that no wired client confirms as any vendor is REJECTED and never
/// added - never guessed, never defaulted to Tasmota. Proven non-vacuous:
/// the SAME state, after the rejection, still has zero devices in both config
/// and fleet.
#[tokio::test]
async fn add_rejects_a_host_no_vendor_confirms_without_adding() {
    // A mock server that answers HTTP but confirms neither vendor's probe:
    // no `/cm` (Tasmota) or `/shelly` (Shelly) mock is registered, so every
    // vendor probe gets a 404/connection-refused-shaped mismatch, not a hang.
    let server = MockServer::start();
    let host = server.address().to_string();

    let path = temp_config_path("no-vendor-confirms");
    let state = AppState::new(Config::default(), path.clone());
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(
        &app,
        &cookie,
        &token,
        "/discover/add",
        &format!("name=Mystery&host={host}"),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "a host no vendor confirms must be rejected, never added"
    );

    let cfg = state.inner.config.read().await;
    assert!(
        cfg.devices.is_empty(),
        "a rejected host must never be persisted to config"
    );
    drop(cfg);

    let fleet = state.inner.fleet.read().await;
    assert!(
        fleet.devices.is_empty(),
        "a rejected host must never be added to the fleet"
    );
    drop(fleet);
    let _ = std::fs::remove_file(&path);
}

/// SECURITY: a forged `vendor` field in the POST body is ignored - `AddForm`
/// has no `vendor` field to deserialize it into, so the persisted vendor
/// comes ONLY from the server-side probe. Proven non-vacuous: the host is
/// mocked as SHELLY, the form claims `vendor=tasmota`, and the persisted
/// device is Shelly, not Tasmota - if the handler ever read a submitted
/// vendor field, this would silently persist Tasmota instead and the
/// assertion below would fail.
#[tokio::test]
async fn add_ignores_forged_vendor_form_field_and_persists_the_probed_vendor() {
    let server = MockServer::start();
    mock_shelly_probe(&server);
    let host = server.address().to_string();

    let path = temp_config_path("forged-vendor");
    let state = AppState::new(Config::default(), path.clone());
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(
        &app,
        &cookie,
        &token,
        "/discover/add",
        &format!("name=Plug&host={host}&vendor=tasmota"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let cfg = state.inner.config.read().await;
    assert_eq!(cfg.devices.len(), 1);
    assert_eq!(
        cfg.devices[0].vendor,
        Vendor::Shelly,
        "a forged vendor=tasmota form field must be ignored; the persisted \
         vendor must be the one the server-side probe against the mocked \
         Shelly host actually confirmed"
    );
    drop(cfg);
    let _ = std::fs::remove_file(&path);
}
