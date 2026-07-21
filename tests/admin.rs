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

use plugboard::config::{Config, DeviceConfig};
use plugboard::fleet::device_id;
use plugboard::routes;
use plugboard::routes::admin::sanitize_filename;
use plugboard::state::AppState;
use switchkit::{Capabilities, DeviceSnapshot, Vendor};

fn config_with(host: &str, name: &str) -> Config {
    config_with_vendor(host, name, Vendor::Tasmota)
}

fn config_with_vendor(host: &str, name: &str, vendor: Vendor) -> Config {
    Config {
        devices: vec![DeviceConfig {
            name: name.into(),
            host: host.into(),
            password: None,
            protected: false,
            group: None,
            vendor,
        }],
        ..Config::default()
    }
}

/// A minimal Gen2 Shelly device info body: `probe_target`/`open` only need
/// enough to recognize a Gen2 device and route console calls to `rpc_raw`.
fn mock_shelly_gen2_info(server: &MockServer) {
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
}

/// A capabilities-populated `DeviceSnapshot`, standing in for whatever a real
/// poll/toggle would have last written into `DeviceView.status`, so the
/// admin-panel rendering tests below can assert on `admin_panel`'s output
/// without needing a live device round-trip for a pure-rendering check.
fn snapshot_with_console(host: &str) -> DeviceSnapshot {
    DeviceSnapshot {
        host: host.into(),
        capabilities: Capabilities {
            console: true,
            ..Default::default()
        },
        ..Default::default()
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
        body.contains("console-entry"),
        "response should carry a console-log entry, body was: {body}"
    );
    assert!(
        body.contains("Status 8"),
        "the entry should echo the command terminal-style, body was: {body}"
    );
    assert!(
        body.contains("StatusSNS") && body.contains("12.3"),
        "the device's JSON response should be rendered in the entry, body was: {body}"
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
    assert!(confirmed_body.contains("console-entry"));
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
    // the shared `switchkit::guardrail` (the ACTUAL table `routes::admin`
    // classifies through) ever reclassifies this command.
    assert!(
        matches!(
            switchkit::guardrail::classify(Vendor::Tasmota, "SetOption65 1"),
            switchkit::guardrail::Hazard::RequiresConfirmation
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
    assert!(confirmed_body.contains("console-entry"));
    assert!(confirmed_body.contains("SetOption65"));
}

/// A destructive Shelly RPC method (`Shelly.FactoryReset`, classified
/// `Hazard::Destructive` by the SAME shared `switchkit::guardrail` a Tasmota
/// command goes through) is gated exactly like the Tasmota case above: proven
/// non-vacuous with a paired negative control (unconfirmed - modal, the
/// device's `/shelly` info endpoint AND its `/rpc/Shelly.FactoryReset`
/// endpoint both see 0 hits) and positive control (confirmed - both are hit).
/// This is the proof that the vendor-aware guardrail actually covers Shelly,
/// not just Tasmota.
#[tokio::test]
async fn shelly_console_destructive_rpc_requires_confirmation_before_hitting_device() {
    let server = MockServer::start();
    mock_shelly_gen2_info(&server);
    let factory_reset = server.mock(|when, then| {
        when.method(GET).path("/rpc/Shelly.FactoryReset");
        then.status(200).json_body(json!({}));
    });
    let host = server.address().to_string();
    let id = device_id(&host);

    let state = AppState::new(
        config_with_vendor(&host, "Shelly Plug", Vendor::Shelly),
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
        "command=Shelly.FactoryReset",
    )
    .await;
    assert_eq!(gated.status(), StatusCode::OK);
    let gated_body = body_string(gated).await;
    assert!(
        gated_body.contains("performs a factory reset"),
        "the modal should name the RPC's hazard reason, body was: {gated_body}"
    );
    assert!(
        gated_body.contains(r#"id="modal" hx-swap-oob="true""#),
        "the modal must be an OOB swap into #modal"
    );
    assert_eq!(
        factory_reset.hits(),
        0,
        "an unconfirmed destructive Shelly RPC must not reach the device"
    );

    // Positive control: identical command plus confirmed=true -> the SAME
    // mock now receives a hit, proving the prior 0 was a real gate.
    let confirmed = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/console"),
        "command=Shelly.FactoryReset&confirmed=true",
    )
    .await;
    assert_eq!(confirmed.status(), StatusCode::OK);
    assert_eq!(
        factory_reset.hits(),
        1,
        "confirmed=true must send the RPC to the device"
    );
}

// ---------------------------------------------------------------------------
// per-vendor admin panel rendering
// ---------------------------------------------------------------------------

/// A Tasmota device's admin panel offers a console with Tasmota bare-command
/// placeholder copy, and never the Shelly-specific "RPC console" heading.
#[tokio::test]
async fn tasmota_admin_panel_offers_console_with_tasmota_placeholder() {
    let host = "192.0.2.40".to_string();
    let state = AppState::new(
        config_with(&host, "Test Plug"),
        PathBuf::from("unused.toml"),
    );
    {
        let mut fleet = state.inner.fleet.write().await;
        let dev = fleet.devices.first_mut().expect("one device configured");
        dev.reachable = true;
        dev.status = Some(snapshot_with_console(&host));
    }
    let app = routes::router(state, false);
    let (cookie, _token) = get_cookie_and_token(&app).await;
    let id = device_id(&host);

    let response = get_route(&app, &cookie, &format!("/device/{id}")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("Console"), "got: {body}");
    assert!(
        body.contains("e.g. Status 8"),
        "Tasmota placeholder should show a bare command example, got: {body}"
    );
    assert!(
        !body.contains("RPC console"),
        "a Tasmota panel must never show the Shelly-specific heading, got: {body}"
    );
}

/// A Shelly device's admin panel offers an RPC console (same
/// `/device/:id/console` route, same `SmartDevice::console` dispatch, just
/// different copy) with RPC-method placeholder text, and never the
/// Tasmota-specific bare-command placeholder.
#[tokio::test]
async fn shelly_admin_panel_offers_rpc_console_not_tasmota_surface() {
    let host = "198.51.100.40".to_string();
    let state = AppState::new(
        config_with_vendor(&host, "Shelly Plug", Vendor::Shelly),
        PathBuf::from("unused.toml"),
    );
    {
        let mut fleet = state.inner.fleet.write().await;
        let dev = fleet.devices.first_mut().expect("one device configured");
        dev.reachable = true;
        dev.status = Some(snapshot_with_console(&host));
    }
    let app = routes::router(state, false);
    let (cookie, _token) = get_cookie_and_token(&app).await;
    let id = device_id(&host);

    let response = get_route(&app, &cookie, &format!("/device/{id}")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(body.contains("RPC console"), "got: {body}");
    assert!(
        body.contains("Shelly.GetStatus"),
        "Shelly placeholder should show an RPC method example, got: {body}"
    );
    assert!(
        !body.contains("e.g. Status 8"),
        "a Shelly panel must never show the Tasmota-specific placeholder, got: {body}"
    );
    // Both vendors post to the SAME route.
    assert!(body.contains(&format!(r#"hx-post="/device/{id}/console""#)));
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

/// `POST /device/:id/updates/check` runs a real discovery pass against the
/// configured release feed and answers with the device's re-rendered
/// firmware callout (the fragment the Check now button swaps in); an
/// unknown device 404s before any work.
#[tokio::test]
async fn updates_check_returns_fresh_callout_and_404s_unknown_devices() {
    let server = MockServer::start();
    // The poll that establishes the device's current version.
    server.mock(|when, then| {
        when.method(httpmock::Method::GET).path("/cm");
        then.status(200).json_body(json!({
            "Status": {"DeviceName": "Test Plug", "FriendlyName": ["Test Plug"], "Power": 1},
            "StatusFWR": {"Version": "14.2.0"},
            "StatusNET": {"IPAddress": "192.0.2.50", "Mac": "AA:BB:CC:00:11:22", "Hostname": "plug"},
            "StatusSTS": {"POWER": "ON", "Uptime": "1T00:00:00", "Wifi": {"RSSI": 70}}
        }));
    });
    let feed = MockServer::start();
    feed.mock(|when, then| {
        when.method(httpmock::Method::GET).path("/latest");
        then.status(200).json_body(json!({"tag_name": "v15.5.0"}));
    });
    let host = server.address().to_string();
    let id = device_id(&host);

    let mut config = config_with(&host, "Test Plug");
    config.updates.tasmota_release_url = format!("{}/latest", feed.base_url());
    let state = AppState::new(config, PathBuf::from("unused.toml"));
    plugboard::poller::refresh_once(&state).await;
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_form(
        &app,
        &cookie,
        &token,
        &format!("/device/{id}/updates/check"),
        "",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    assert!(
        body.contains(r#"id="update-callout""#),
        "response must be the callout fragment, body was: {body}"
    );
    assert!(
        body.contains("15.5.0") && body.contains("Update to 15.5.0"),
        "a confirmed-newer version must surface with its action, body was: {body}"
    );

    let missing = post_form(&app, &cookie, &token, "/device/d-00/updates/check", "").await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
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
    assert_eq!(sanitize_filename(""), "plugboard-backup");
}
