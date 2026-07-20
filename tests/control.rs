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

use plugboard::config::{Config, DeviceConfig};
use plugboard::fleet::device_id;
use plugboard::routes;
use plugboard::state::AppState;
use switchkit::Vendor;

fn mock_power_toggle<'a>(server: &'a MockServer, new_state: &str) -> httpmock::Mock<'a> {
    server.mock(|when, then| {
        when.method(GET)
            .path("/cm")
            .query_param("cmnd", "Power TOGGLE");
        then.status(200).json_body(json!({"POWER": new_state}));
    })
}

/// Mocks a bulk `Power ON`/`Power OFF` command (`cmnd` is e.g. `"Power OFF"`).
fn mock_power<'a>(server: &'a MockServer, cmnd: &str, new_state: &str) -> httpmock::Mock<'a> {
    server.mock(|when, then| {
        when.method(GET).path("/cm").query_param("cmnd", cmnd);
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
            vendor: Vendor::Tasmota,
        }],
        ..Config::default()
    }
}

/// A fleet of multiple unprotected devices, one per host, for the bulk
/// all-on/off tests.
fn config_with_many(hosts: &[String]) -> Config {
    Config {
        devices: hosts
            .iter()
            .enumerate()
            .map(|(i, host)| DeviceConfig {
                name: format!("Plug {i}"),
                host: host.clone(),
                password: None,
                protected: false,
                vendor: Vendor::Tasmota,
            })
            .collect(),
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

/// POSTs `/devices/power`, same-origin and CSRF-authenticated, with `action`
/// and `confirmed` (set or omitted) as form-urlencoded body.
async fn post_bulk(
    app: &Router,
    cookie: &str,
    token: &str,
    action: &str,
    confirmed: Option<&str>,
) -> Response<Body> {
    let mut body = format!("action={action}");
    if let Some(v) = confirmed {
        body.push_str(&format!("&confirmed={v}"));
    }
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/devices/power")
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

/// The bulk confirm gate, proven non-vacuous with a paired negative and
/// positive control on the SAME mocks, across TWO devices: without
/// `confirmed`, a confirm modal is returned and NEITHER device is touched (0
/// hits each); with `confirmed=true` on the identical action, the SAME mocks
/// now receive a hit, proving the prior 0 was a real gate, not a broken mock
/// or an empty fleet.
#[tokio::test]
async fn bulk_power_requires_confirmation_before_switching_any_device() {
    let server_a = MockServer::start();
    let server_b = MockServer::start();
    let power_a = mock_power(&server_a, "Power OFF", "OFF");
    let power_b = mock_power(&server_b, "Power OFF", "OFF");
    mock_statetext(&server_a);
    mock_statetext(&server_b);
    mock_status(&server_a, "OFF");
    mock_status(&server_b, "OFF");
    let host_a = server_a.address().to_string();
    let host_b = server_b.address().to_string();

    let state = AppState::new(
        config_with_many(&[host_a, host_b]),
        PathBuf::from("unused.toml"),
    );
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    // Negative control: no `confirmed` field -> confirm modal, no device touched.
    let gated = post_bulk(&app, &cookie, &token, "off", None).await;
    assert_eq!(gated.status(), StatusCode::OK);
    let gated_body = body_string(gated).await;
    assert!(
        gated_body.contains("Confirm"),
        "an unconfirmed bulk request should return a confirm modal; body was: {gated_body}"
    );
    assert!(
        gated_body.contains(r#"id="modal" hx-swap-oob="true""#),
        "the modal must be an OOB swap into #modal"
    );
    assert!(
        gated_body.contains(r#"id="grid""#),
        "the primary response must still be the (unchanged) grid, body was: {gated_body}"
    );
    assert_eq!(
        power_a.hits(),
        0,
        "a bulk write must not touch device A without confirmation"
    );
    assert_eq!(
        power_b.hits(),
        0,
        "a bulk write must not touch device B without confirmation"
    );

    // Positive control: identical action plus confirmed=true -> the SAME mocks
    // now receive a hit, proving the prior 0 was a real gate.
    let confirmed = post_bulk(&app, &cookie, &token, "off", Some("true")).await;
    assert_eq!(confirmed.status(), StatusCode::OK);
    assert_eq!(power_a.hits(), 1, "confirmed=true must switch device A");
    assert_eq!(power_b.hits(), 1, "confirmed=true must switch device B");
    let confirmed_body = body_string(confirmed).await;
    assert!(
        confirmed_body.contains("2 switched"),
        "the summary toast should report both devices switched, body was: {confirmed_body}"
    );
    assert!(
        !confirmed_body.contains("failed"),
        "no device failed, so the toast must not mention failure, body was: {confirmed_body}"
    );
    assert!(
        confirmed_body.contains(r#"class="badge off""#),
        "the confirmed response should reflect the refreshed OFF state, body was: {confirmed_body}"
    );
}

/// Partial failure: one device is reachable and switches, the other has no
/// mock configured (so its command fails). The reachable device must still
/// switch, the overall response is still 200, and the summary toast reports
/// both the success and the failure count.
#[tokio::test]
async fn bulk_power_partial_failure_still_switches_others_and_reports_summary() {
    let reachable = MockServer::start();
    let power_ok = mock_power(&reachable, "Power OFF", "OFF");
    mock_statetext(&reachable);
    mock_status(&reachable, "OFF");
    let host_ok = reachable.address().to_string();

    // No mocks at all on this server: every command against it fails.
    let unreachable = MockServer::start();
    let host_bad = unreachable.address().to_string();

    let state = AppState::new(
        config_with_many(&[host_ok, host_bad]),
        PathBuf::from("unused.toml"),
    );
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_bulk(&app, &cookie, &token, "off", Some("true")).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "one unreachable device must not turn the bulk response into an error"
    );
    assert_eq!(
        power_ok.hits(),
        1,
        "the reachable device must still be switched"
    );

    let body = body_string(response).await;
    assert!(
        body.contains("1 switched, 1 failed"),
        "the toast must report both the success and the failure, body was: {body}"
    );
    assert!(
        body.contains(r#"class="badge off""#),
        "the switched device's card should reflect the new OFF state, body was: {body}"
    );
    assert!(
        body.contains("offline"),
        "the unreachable device should render offline after the post-bulk refresh, body was: {body}"
    );
}

/// An unrecognized `action` value is a 400, never silently treated as a no-op
/// or defaulted to on/off.
#[tokio::test]
async fn bulk_power_invalid_action_returns_bad_request() {
    let server = MockServer::start();
    let host = server.address().to_string();
    let state = AppState::new(config_with(&host, false), PathBuf::from("unused.toml"));
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_bulk(&app, &cookie, &token, "sideways", Some("true")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// CRITICAL regression test: `tasmota-core` builds device request URLs with
/// credentials in the query string (`.../cm?cmnd=...&user=admin&password=...`),
/// and `ureq` attaches the full request URL to a transport-level error (e.g.
/// connection refused). A raw `switchkit::Error` rendered into the toggle
/// route's response body would leak the device's plaintext password. This
/// drives a REAL connection-refused failure (a bound-then-dropped TCP
/// listener, so the port is guaranteed closed) against a device configured
/// with a password, through the real router, and proves the response body
/// never contains it.
#[tokio::test]
async fn toggle_error_response_never_leaks_device_password() {
    // Bind an ephemeral port then immediately drop the listener: connecting to
    // it now fails fast with a genuine OS-level "connection refused", the same
    // `ureq::Error::Transport` failure mode that leaked a real password (see
    // the fix this test guards).
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let host = listener.local_addr().unwrap().to_string();
    drop(listener);

    const SECRET: &str = "SUPER_SECRET_PW";
    let config = Config {
        devices: vec![DeviceConfig {
            name: "Test Plug".into(),
            host: host.clone(),
            password: Some(SECRET.into()),
            protected: false,
            vendor: Vendor::Tasmota,
        }],
        ..Config::default()
    };
    let id = device_id(&host);
    let state = AppState::new(config, PathBuf::from("unused.toml"));
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_toggle(&app, &cookie, &token, &id, None).await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_GATEWAY,
        "the initial (unrefreshed) toggle command itself must fail against an unreachable device"
    );
    let body = body_string(response).await;
    assert!(
        !body.contains(SECRET),
        "response body must never leak the device password, body was: {body}"
    );
    assert!(
        !body.contains("user=admin"),
        "response body must never leak the device username value, body was: {body}"
    );
    assert!(
        body.contains(&host),
        "the response should still be useful for debugging (contains the host), body was: {body}"
    );
}

/// `action=on` maps to `Power ON`, not just `action=off` to `Power OFF` - both
/// match arms are exercised, not only the one the other tests happen to use.
#[tokio::test]
async fn bulk_power_on_action_sends_power_on() {
    let server = MockServer::start();
    let power_on = mock_power(&server, "Power ON", "ON");
    mock_statetext(&server);
    mock_status(&server, "ON");
    let host = server.address().to_string();

    let state = AppState::new(config_with(&host, false), PathBuf::from("unused.toml"));
    let app = routes::router(state, false);
    let (cookie, token) = get_cookie_and_token(&app).await;

    let response = post_bulk(&app, &cookie, &token, "on", Some("true")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(power_on.hits(), 1, "action=on must send Power ON");

    let body = body_string(response).await;
    assert!(body.contains("1 switched"), "body was: {body}");
    assert!(body.contains(r#"class="badge on""#), "body was: {body}");
}
