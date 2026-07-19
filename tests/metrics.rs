//! Integration tests for `GET /metrics`, exercised through the REAL router
//! (`routes::router`), proving it is reachable with no session/CSRF/auth at
//! all (a Prometheus scraper never logs in), in both auth modes, and that
//! `metrics_enabled = false` turns it into a plain 404.

use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use tasmota_web::config::{AuthMode, Config, DeviceConfig};
use tasmota_web::routes;
use tasmota_web::state::AppState;

fn config_with_device(host: &str) -> Config {
    Config {
        devices: vec![DeviceConfig {
            name: "Test Plug".into(),
            host: host.into(),
            password: None,
            protected: false,
        }],
        ..Config::default()
    }
}

async fn get_metrics(app: Router) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri("/metrics")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

/// Baseline: `/metrics` returns 200, a Prometheus-compatible content type, and
/// carries the always-present build-info series, with NO cookie, NO
/// `X-CSRF-Token`, and NO `Sec-Fetch-Site`/`Origin` header at all - exactly
/// what a Prometheus scraper sends.
#[tokio::test]
async fn metrics_route_returns_ok_with_plain_text_content_type() {
    let state = AppState::new(
        config_with_device("192.0.2.30"),
        PathBuf::from("unused.toml"),
    );
    let app = routes::router(state, false);

    let response = get_metrics(app).await;
    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .expect("content-type header present")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        content_type.starts_with("text/plain"),
        "content-type was: {content_type}"
    );

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(body.contains("tasmota_web_build_info"), "body was:\n{body}");
    assert!(
        body.contains("tasmota_web_device_reachable"),
        "body was:\n{body}"
    );
}

/// The unauthenticated guarantee, proven specifically under `AuthMode::Builtin`
/// (the mode that gates every OTHER app route behind a login): `/metrics`
/// must be reachable anyway, since it sits outside `require_auth` entirely.
#[tokio::test]
async fn metrics_route_bypasses_require_auth_even_in_builtin_mode() {
    let mut config = config_with_device("192.0.2.31");
    config.auth.mode = AuthMode::Builtin;
    let state = AppState::new(config, PathBuf::from("unused.toml"));
    let app = routes::router(state, false);

    let response = get_metrics(app).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a builtin-mode deployment must still allow an unauthenticated scrape"
    );
}

/// `metrics_enabled = false` turns the route into a plain 404, not a
/// redirect or an empty 200 body.
#[tokio::test]
async fn metrics_disabled_returns_404() {
    let mut config = config_with_device("192.0.2.32");
    config.metrics_enabled = false;
    let state = AppState::new(config, PathBuf::from("unused.toml"));
    let app = routes::router(state, false);

    let response = get_metrics(app).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Non-vacuous pairing for the toggle above: the SAME config with
/// `metrics_enabled` left at its default (true) serves 200, proving the 404
/// above is really the toggle and not, say, the route missing entirely.
#[tokio::test]
async fn metrics_enabled_by_default_returns_200() {
    let config = config_with_device("192.0.2.33");
    assert!(
        config.metrics_enabled,
        "metrics_enabled must default to true"
    );
    let state = AppState::new(config, PathBuf::from("unused.toml"));
    let app = routes::router(state, false);

    let response = get_metrics(app).await;
    assert_eq!(response.status(), StatusCode::OK);
}
