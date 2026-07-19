//! Integration tests for the relay toggle write route: instant feedback (the
//! returned card reflects the post-toggle refresh, exactly like the poller),
//! the undo toast, and the protected-device confirm gate. Every request goes
//! through the REAL router (`routes::router`), so it also exercises the
//! session + CSRF/same-origin middleware from Task 6a.

use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use http_body_util::BodyExt;
use httpmock::prelude::*;
use serde_json::json;
use tower::ServiceExt;

use tasmota_web::config::{Config, DeviceConfig};
use tasmota_web::fleet::device_id;
use tasmota_web::routes;
use tasmota_web::state::AppState;

fn mock_power_toggle<'a>(server: &'a MockServer, new_state: &str) -> httpmock::Mock<'a> {
    server.mock(|when, then| {
        when.method(GET)
            .path("/cm")
            .query_param("cmnd", "Power TOGGLE");
        then.status(200).json_body(json!({"POWER": new_state}));
    })
}

fn mock_statetext(server: &MockServer) {
    server.mock(|when, then| {
        when.method(GET)
            .path("/cm")
            .query_param("cmnd", "StateText");
        then.status(200)
            .json_body(json!({"StateText1": "OFF", "StateText2": "ON"}));
    });
}

fn mock_status(server: &MockServer, power: &str) {
    server.mock(|when, then| {
        when.method(GET).path("/cm").query_param("cmnd", "Status 0");
        then.status(200).json_body(json!({
            "Status": {"DeviceName": "TestPlug", "Module": 1, "FriendlyName": ["TestPlug"], "Power": 1},
            "StatusFWR": {"Version": "14.2.0"},
            "StatusNET": {"IPAddress": "192.0.2.50", "Mac": "AA:BB:CC:00:11:22", "Hostname": "testplug"},
            "StatusSTS": {"POWER": power, "Uptime": "1T02:03:04", "Wifi": {"RSSI": 76}}
        }));
    });
}

fn config_with(host: &str, protected: bool) -> Config {
    Config {
        devices: vec![DeviceConfig {
            name: "Test Plug".into(),
            host: host.into(),
            password: None,
            protected,
        }],
        ..Config::default()
    }
}

/// GETs `/` to establish a session, returning the `Cookie` header value
/// (name=value only, so it round-trips as a request `Cookie` header) and the
/// session's CSRF token, scraped from the `<meta name="csrf-token">` tag that
/// `views::layout::page` embeds (mirrors `tests/csrf.rs`, which reads the
/// token from a bare probe body instead of a real page).
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

/// POSTs `/device/{id}/toggle`, same-origin and CSRF-authenticated, with
/// `confirmed` set (or omitted) as form-urlencoded body.
async fn post_toggle(
    app: &Router,
    cookie: &str,
    token: &str,
    id: &str,
    confirmed: Option<&str>,
) -> Response<Body> {
    let body = match confirmed {
        Some(v) => format!("confirmed={v}"),
        None => String::new(),
    };
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/device/{id}/toggle"))
                .header("cookie", cookie)
                .header("x-csrf-token", token)
                .header("sec-fetch-site", "same-origin")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// The primary happy path: an unprotected device's toggle hits `Power TOGGLE`
/// on the real device, then refreshes via `Status 0` exactly like the poller,
/// and the returned card shows the new "on" state plus an undo toast.
#[tokio::test]
async fn unprotected_toggle_hits_device_and_updates_card() {
    let server = MockServer::start();
    let power_mock = mock_power_toggle(&server, "ON");
    mock_statetext(&server);
    mock_status(&server, "ON");
    let host = server.address().to_string();
    let id = device_id(&host);

    let state = AppState::new(config_with(&host, false), PathBuf::from("unused.toml"));
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_toggle(&app, &cookie, &token, &id, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        power_mock.hits(),
        1,
        "toggle must send exactly one Power TOGGLE command"
    );

    let body = body_string(response).await;
    assert!(
        body.contains(&format!(r#"id="card-{id}""#)),
        "response should carry the card fragment"
    );
    assert!(
        body.contains(r#"class="badge on""#),
        "card should reflect the refreshed ON state, body was: {body}"
    );
    assert!(
        body.contains("Switched to on"),
        "an undo toast should report the confirmed relay state"
    );
    assert!(body.contains("Undo"), "the toast should offer an undo");
}

/// The protected-device gate, proven non-vacuous with a paired negative and
/// positive control on the SAME mock: without `confirmed`, the modal is
/// returned and the device is never touched (0 hits); with `confirmed=true`
/// on the identical request, the device IS toggled (hits increments to 1).
/// If the gate were broken (e.g. always skipped, or never actually enforced),
/// the first assertion would still coincidentally pass only if the mock
/// itself were miswired - which the second assertion rules out by proving the
/// same mock DOES receive a hit once confirmed.
#[tokio::test]
async fn protected_device_requires_confirmation_before_toggling() {
    let server = MockServer::start();
    let power_mock = mock_power_toggle(&server, "ON");
    mock_statetext(&server);
    mock_status(&server, "ON");
    let host = server.address().to_string();
    let id = device_id(&host);

    let state = AppState::new(config_with(&host, true), PathBuf::from("unused.toml"));
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    // Negative control: no `confirmed` field -> modal, device untouched.
    let gated = post_toggle(&app, &cookie, &token, &id, None).await;
    assert_eq!(gated.status(), StatusCode::OK);
    let gated_body = body_string(gated).await;
    assert!(
        gated_body.contains("Confirm"),
        "an unconfirmed protected toggle should return a confirm modal; body was: {gated_body}"
    );
    assert!(
        gated_body.contains(r#"id="modal" hx-swap-oob="true""#),
        "the modal must be an OOB swap into #modal"
    );
    assert_eq!(
        power_mock.hits(),
        0,
        "a protected device must not be toggled without confirmation"
    );

    // Positive control: identical request plus confirmed=true -> the SAME mock
    // now receives a hit, proving the prior 0 was a real gate, not a broken mock.
    let confirmed = post_toggle(&app, &cookie, &token, &id, Some("true")).await;
    assert_eq!(confirmed.status(), StatusCode::OK);
    assert_eq!(
        power_mock.hits(),
        1,
        "confirmed=true must execute the toggle against the device"
    );
    let confirmed_body = body_string(confirmed).await;
    assert!(confirmed_body.contains(&format!(r#"id="card-{id}""#)));
    assert!(confirmed_body.contains(r#"class="badge on""#));

    // There is NO separate confirm-bypass route: only /device/:id/toggle ever
    // executes a power command.
    let bypass = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/device/{id}/confirm"))
                .header("cookie", cookie)
                .header("x-csrf-token", token)
                .header("sec-fetch-site", "same-origin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bypass.status(), StatusCode::NOT_FOUND);
}

/// Global invariant: a control action never fabricates reachability. When the
/// toggle command itself succeeds but the follow-up refresh fails, the card
/// must render OFFLINE (n/a), never a half-confirmed "on" reading carried
/// over from the command response.
#[tokio::test]
async fn toggle_renders_offline_when_post_toggle_refresh_fails() {
    let server = MockServer::start();
    let power_mock = mock_power_toggle(&server, "ON");
    mock_statetext(&server);
    // Deliberately no `Status 0` mock: the refresh after the toggle fails.
    let host = server.address().to_string();
    let id = device_id(&host);

    let state = AppState::new(config_with(&host, false), PathBuf::from("unused.toml"));
    let app = routes::router(state.clone(), false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_toggle(&app, &cookie, &token, &id, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(power_mock.hits(), 1, "the toggle command itself succeeded");

    let body = body_string(response).await;
    assert!(
        body.contains("offline"),
        "a failed post-toggle refresh must render the card offline, body was: {body}"
    );
    assert!(
        !body.contains(r#"class="badge on""#),
        "the confirmed ON must never leak onto the card as a live reading"
    );
    assert!(
        body.contains(">n/a<"),
        "telemetry must render n/a, not a stale/coerced value"
    );
    assert!(
        body.contains("Switched to on"),
        "the command's confirmed relay is still reported in the undo toast"
    );

    let fleet = state.inner.fleet.read().await;
    let dev = fleet.get(&id).expect("device present");
    assert!(
        !dev.reachable,
        "reachable must be false after a failed refresh"
    );
    assert!(dev.status.is_none(), "status must be cleared, not stale");
    assert!(dev.error.is_some(), "an error should be recorded");
}

/// A device that has never been polled (`status: None`, `reachable: false`
/// at startup) must still toggle successfully and come back ONLINE with the
/// new relay state and an enabled toggle button - the card must not invent a
/// "last known" state, but a live post-toggle refresh IS live data.
#[tokio::test]
async fn toggle_on_never_polled_device_renders_online() {
    let server = MockServer::start();
    let power_mock = mock_power_toggle(&server, "ON");
    mock_statetext(&server);
    mock_status(&server, "ON");
    let host = server.address().to_string();
    let id = device_id(&host);

    let state = AppState::new(config_with(&host, false), PathBuf::from("unused.toml"));
    {
        let fleet = state.inner.fleet.read().await;
        let dev = fleet.get(&id).expect("device present");
        assert!(
            !dev.reachable,
            "device must start unreachable (never polled)"
        );
        assert!(dev.status.is_none(), "device must start with no status");
    }
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_toggle(&app, &cookie, &token, &id, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(power_mock.hits(), 1);

    let body = body_string(response).await;
    assert!(
        body.contains(r#"class="badge on""#),
        "a successful post-toggle refresh must render the new ON state, body was: {body}"
    );
    assert!(
        !body.contains(r#">offline<"#),
        "the device must be online after a successful toggle + refresh"
    );
    assert!(
        !body.contains(r#"type="submit" disabled"#),
        "the toggle button must be enabled once the device is online, body was: {body}"
    );
}
