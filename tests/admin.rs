//! Integration tests for the per-device admin panel (Task 8): console,
//! config get/set, firmware check/update, and the backup download. Every
//! request goes through the REAL router (`routes::router`), so it also
//! exercises the session + CSRF/same-origin middleware from Task 6a. Every
//! "device NOT hit" claim is proven non-vacuous with a paired positive
//! control on the SAME mock, mirroring `tests/control.rs`.

use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use httpmock::prelude::*;
use serde_json::json;
use tower::ServiceExt;

use switchkit::Vendor;
use tasmota_web::config::{Config, DeviceConfig};
use tasmota_web::fleet::device_id;
use tasmota_web::routes;
use tasmota_web::routes::admin::sanitize_filename;
use tasmota_web::state::AppState;

fn config_with(host: &str, name: &str) -> Config {
    Config {
        devices: vec![DeviceConfig {
            name: name.into(),
            host: host.into(),
            password: None,
            protected: false,
            vendor: Vendor::Tasmota,
        }],
        ..Config::default()
    }
}

fn mock_cmnd<'a>(
    server: &'a MockServer,
    cmnd: &str,
    body: serde_json::Value,
) -> httpmock::Mock<'a> {
    server.mock(|when, then| {
        when.method(GET).path("/cm").query_param("cmnd", cmnd);
        then.status(200).json_body(body);
    })
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

/// POSTs a form-urlencoded body to an admin route, same-origin and
/// CSRF-authenticated (mirrors `tests/control.rs::post_toggle`/`post_bulk`).
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

// ---------------------------------------------------------------------------
// console
// ---------------------------------------------------------------------------

/// A known-safe console command (`Status 8`, classified `Hazard::Safe`)
/// executes directly, with no confirmation, and its JSON response is
/// rendered into the admin panel's result area.
#[tokio::test]
async fn console_safe_command_executes_and_renders_json() {
    let server = MockServer::start();
    let status8 = mock_cmnd(&server, "Status 8", json!({"StatusSNS": {"Power": 12.3}}));
    let host = server.address().to_string();
    let id = device_id(&host);

    let state = AppState::new(
        config_with(&host, "Test Plug"),
        PathBuf::from("unused.toml"),
    );
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/console"),
        "command=Status+8",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        status8.hits(),
        1,
        "a safe console command must reach the device"
    );

    let body = body_string(response).await;
    assert!(
        body.contains(r#"id="admin-result""#),
        "response should carry the admin-result fragment, body was: {body}"
    );
    assert!(
        body.contains("StatusSNS") && body.contains("12.3"),
        "the device's JSON response should be rendered in the panel, body was: {body}"
    );
}

/// The guardrail gate, proven non-vacuous with a paired negative and positive
/// control on the SAME mock: `Reset 1` (destructive) without `confirmed` is
/// gated (modal, 0 hits); the identical command with `confirmed=true` DOES
/// reach the device.
#[tokio::test]
async fn console_destructive_command_requires_confirmation_before_hitting_device() {
    let server = MockServer::start();
    let reset = mock_cmnd(&server, "Reset 1", json!({"Reset": "Reset 1"}));
    let host = server.address().to_string();
    let id = device_id(&host);

    let state = AppState::new(
        config_with(&host, "Test Plug"),
        PathBuf::from("unused.toml"),
    );
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    // Negative control: no `confirmed` -> modal naming the hazard, device untouched.
    let gated = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/console"),
        "command=Reset+1",
    )
    .await;
    assert_eq!(gated.status(), StatusCode::OK);
    let gated_body = body_string(gated).await;
    assert!(
        gated_body.contains("destructive"),
        "the modal should name the command as destructive, body was: {gated_body}"
    );
    assert!(
        gated_body.contains(r#"id="modal" hx-swap-oob="true""#),
        "the modal must be an OOB swap into #modal"
    );
    assert_eq!(
        reset.hits(),
        0,
        "an unconfirmed destructive console command must not reach the device"
    );

    // Positive control: identical command plus confirmed=true -> the SAME mock
    // now receives a hit, proving the prior 0 was a real gate.
    let confirmed = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/console"),
        "command=Reset+1&confirmed=true",
    )
    .await;
    assert_eq!(confirmed.status(), StatusCode::OK);
    assert_eq!(reset.hits(), 1, "confirmed=true must send the command");
    let confirmed_body = body_string(confirmed).await;
    assert!(confirmed_body.contains(r#"id="admin-result""#));
    assert!(confirmed_body.contains("Reset"));
}

/// The guardrail gate for the `Hazard::RequiresConfirmation` arm (distinct
/// from the `Destructive` arm covered above), proven non-vacuous with a
/// paired negative and positive control on the SAME mock: `SetOption65 1`
/// (a config write, not on the known-safe list, but also not
/// `Hazard::Destructive`) without `confirmed` is gated (modal, 0 hits); the
/// identical command with `confirmed=true` DOES reach the device.
#[tokio::test]
async fn console_requires_confirmation_command_requires_confirmation_before_hitting_device() {
    // Self-documents the hazard class this test exercises: fails loudly if
    // upstream `tasmota_core::guardrail` ever reclassifies this command.
    assert!(
        matches!(
            tasmota_core::guardrail::classify("SetOption65 1"),
            tasmota_core::guardrail::Hazard::RequiresConfirmation
        ),
        "`SetOption65 1` must classify as Hazard::RequiresConfirmation for this test to guard \
         the `| Hazard::RequiresConfirmation` arm of the console gate"
    );

    let server = MockServer::start();
    let setoption = mock_cmnd(&server, "SetOption65 1", json!({"SetOption65": "1"}));
    let host = server.address().to_string();
    let id = device_id(&host);

    let state = AppState::new(
        config_with(&host, "Test Plug"),
        PathBuf::from("unused.toml"),
    );
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    // Negative control: no `confirmed` -> modal, device untouched.
    let gated = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/console"),
        "command=SetOption65+1",
    )
    .await;
    assert_eq!(gated.status(), StatusCode::OK);
    let gated_body = body_string(gated).await;
    assert!(
        gated_body.contains(r#"id="modal" hx-swap-oob="true""#),
        "an unconfirmed RequiresConfirmation console command should return a confirm modal, \
         body was: {gated_body}"
    );
    assert_eq!(
        setoption.hits(),
        0,
        "an unconfirmed RequiresConfirmation console command must not reach the device"
    );

    // Positive control: identical command plus confirmed=true -> the SAME
    // mock now receives a hit, proving the prior 0 was a real gate.
    let confirmed = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/console"),
        "command=SetOption65+1&confirmed=true",
    )
    .await;
    assert_eq!(confirmed.status(), StatusCode::OK);
    assert_eq!(setoption.hits(), 1, "confirmed=true must send the command");
    let confirmed_body = body_string(confirmed).await;
    assert!(confirmed_body.contains(r#"id="admin-result""#));
    assert!(confirmed_body.contains("SetOption65"));
}

// ---------------------------------------------------------------------------
// config get / set
// ---------------------------------------------------------------------------

/// `config set` with a setting that smuggles a Backlog/argument (contains a
/// space and `;`) is rejected with 400 before any network I/O, regardless of
/// `confirmed`. A paired positive control (a legitimate single-word setting,
/// confirmed) proves the mock/route wiring itself is sound.
#[tokio::test]
async fn config_set_rejects_smuggled_backlog_setting() {
    let server = MockServer::start();
    let poweronstate = mock_cmnd(&server, "PowerOnState 0", json!({"PowerOnState": "0"}));
    // No mock for a `Backlog`/`Reset` command: if the smuggled setting ever
    // reached the device, the request would 502 (Core error) rather than 400.
    let host = server.address().to_string();
    let id = device_id(&host);

    let state = AppState::new(
        config_with(&host, "Test Plug"),
        PathBuf::from("unused.toml"),
    );
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let rejected = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/config/set"),
        "setting=Backlog+x%3B+Reset+1&value=1&confirmed=true",
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        poweronstate.hits(),
        0,
        "a smuggled Backlog setting must never reach the device"
    );

    // Positive control: a legitimate single-word setting, confirmed, DOES hit
    // the device via the SAME router/state, proving the 400 above was a real
    // rejection rather than a broken mock or route.
    let ok = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/config/set"),
        "setting=PowerOnState&value=0&confirmed=true",
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(
        poweronstate.hits(),
        1,
        "a valid confirmed config set must hit the device"
    );
}

/// `config set` is destructive by nature (it writes device config), so even
/// a perfectly valid setting is gated behind confirmation: proven with a
/// paired negative (0 hits) and positive (1 hit) control on the SAME mock.
#[tokio::test]
async fn config_set_requires_confirmation_for_a_valid_setting() {
    let server = MockServer::start();
    let poweronstate = mock_cmnd(&server, "PowerOnState 1", json!({"PowerOnState": "1"}));
    let host = server.address().to_string();
    let id = device_id(&host);

    let state = AppState::new(
        config_with(&host, "Test Plug"),
        PathBuf::from("unused.toml"),
    );
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let gated = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/config/set"),
        "setting=PowerOnState&value=1",
    )
    .await;
    assert_eq!(gated.status(), StatusCode::OK);
    let gated_body = body_string(gated).await;
    assert!(
        gated_body.contains(r#"id="modal" hx-swap-oob="true""#),
        "an unconfirmed config set should return a confirm modal, body was: {gated_body}"
    );
    assert_eq!(
        poweronstate.hits(),
        0,
        "an unconfirmed config set must not reach the device"
    );

    let confirmed = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/config/set"),
        "setting=PowerOnState&value=1&confirmed=true",
    )
    .await;
    assert_eq!(confirmed.status(), StatusCode::OK);
    assert_eq!(
        poweronstate.hits(),
        1,
        "confirmed=true must write the setting to the device"
    );
}

/// `config get` on a bare destructive command word (no whitespace/`;`, so it
/// passes the smuggling check, but IS `Hazard::Destructive`) is rejected with
/// 400, never sent. A paired positive control (a non-destructive setting)
/// proves the route/mock wiring is sound.
#[tokio::test]
async fn config_get_rejects_bare_destructive_setting() {
    let server = MockServer::start();
    let reset = mock_cmnd(&server, "Reset", json!({"Reset": "0"}));
    let poweronstate = mock_cmnd(&server, "PowerOnState", json!({"PowerOnState": 0}));
    let host = server.address().to_string();
    let id = device_id(&host);

    let state = AppState::new(
        config_with(&host, "Test Plug"),
        PathBuf::from("unused.toml"),
    );
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let rejected = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/config/get"),
        "setting=Reset",
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        reset.hits(),
        0,
        "a bare destructive setting must never reach the device"
    );

    // Positive control: a non-destructive setting IS read from the SAME device.
    let ok = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/config/get"),
        "setting=PowerOnState",
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(
        poweronstate.hits(),
        1,
        "a valid setting must be read from the device"
    );
}

// ---------------------------------------------------------------------------
// firmware
// ---------------------------------------------------------------------------

/// Firmware update is always destructive: unconfirmed it returns a modal and
/// never touches the device; confirmed it sends `Upgrade 1`.
#[tokio::test]
async fn firmware_update_requires_confirmation_then_sends_upgrade() {
    let server = MockServer::start();
    let upgrade = mock_cmnd(&server, "Upgrade 1", json!({"Upgrade": "1"}));
    let host = server.address().to_string();
    let id = device_id(&host);

    let state = AppState::new(
        config_with(&host, "Test Plug"),
        PathBuf::from("unused.toml"),
    );
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let gated = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/firmware/update"),
        "",
    )
    .await;
    assert_eq!(gated.status(), StatusCode::OK);
    let gated_body = body_string(gated).await;
    assert!(
        gated_body.contains(r#"id="modal" hx-swap-oob="true""#),
        "an unconfirmed firmware update should return a confirm modal, body was: {gated_body}"
    );
    assert_eq!(
        upgrade.hits(),
        0,
        "an unconfirmed firmware update must not reach the device"
    );

    let confirmed = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/firmware/update"),
        "confirmed=true",
    )
    .await;
    assert_eq!(confirmed.status(), StatusCode::OK);
    assert_eq!(upgrade.hits(), 1, "confirmed=true must send Upgrade 1");
}

/// `firmware_check` is read-only: it queries `Status 2` and renders the
/// returned `StatusFWR.Version` into the admin panel, with no confirm modal.
#[tokio::test]
async fn firmware_check_hits_device_and_renders_version() {
    let server = MockServer::start();
    let status2 = mock_cmnd(
        &server,
        "Status 2",
        json!({"StatusFWR": {"Version": "14.2.0"}}),
    );
    let host = server.address().to_string();
    let id = device_id(&host);

    let state = AppState::new(
        config_with(&host, "Test Plug"),
        PathBuf::from("unused.toml"),
    );
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/firmware/check"),
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        status2.hits(),
        1,
        "firmware_check must query the device's firmware version"
    );

    let body = body_string(response).await;
    assert!(
        body.contains(r#"id="admin-result""#),
        "response should carry the admin-result fragment, body was: {body}"
    );
    assert!(
        body.contains("14.2.0"),
        "the device's reported firmware version should be rendered, body was: {body}"
    );
}

// ---------------------------------------------------------------------------
// backup
// ---------------------------------------------------------------------------

/// A device whose name carries header-hostile bytes (a quote, then CR/LF)
/// still yields a 200 backup download with a `Content-Disposition` header
/// that parses as a valid `HeaderValue` and a filename matching
/// `[A-Za-z0-9._-]+\.dmp`.
#[tokio::test]
async fn backup_sanitizes_header_hostile_device_name() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/dl");
        then.status(200).body(b"dmp-bytes");
    });
    let host = server.address().to_string();
    let hostile_name = "Kit\"chen\r\nX-Injected: evil";
    let id = device_id(&host);

    let state = AppState::new(
        config_with(&host, hostile_name),
        PathBuf::from("unused.toml"),
    );
    let app = routes::router(state, false);
    let (cookie, _token) = get_cookie_and_token(&app).await;

    let response = get_route(&app, &cookie, &format!("/device/{id}/backup")).await;
    assert_eq!(response.status(), StatusCode::OK);

    let disposition = response
        .headers()
        .get(axum::http::header::CONTENT_DISPOSITION)
        .expect("Content-Disposition header must be present")
        .to_str()
        .expect("Content-Disposition must be a valid, parseable header value");

    let marker = "filename=\"";
    let start = disposition.find(marker).expect("filename present") + marker.len();
    let end = start + disposition[start..].find('"').expect("closing quote");
    let filename = &disposition[start..end];

    assert!(
        filename.ends_with(".dmp"),
        "filename must end in .dmp, got {filename:?}"
    );
    let stem = &filename[..filename.len() - ".dmp".len()];
    assert!(!stem.is_empty(), "filename stem must not be empty");
    assert!(
        stem.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'),
        "filename stem must only contain [A-Za-z0-9._-], got {stem:?}"
    );
}

#[test]
fn sanitize_filename_keeps_only_the_safe_charset_and_falls_back_when_empty() {
    let safe = sanitize_filename("Kit\"chen\r\nX-Injected: evil");
    assert!(
        safe.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'),
        "got {safe:?}"
    );
    assert!(!safe.is_empty());
    assert_eq!(sanitize_filename(""), "tasmota-backup");
}
