//! Integration test for the dashboard route: proves absent-data-not-zero at
//! the view layer (a device with no energy sensor renders "n/a", never "0").

use std::path::PathBuf;

use http_body_util::BodyExt;
use tower::ServiceExt;

use tasmota_core::{DeviceStatus, NetInfo};
use tasmota_web::config::{Config, DeviceConfig};
use tasmota_web::fleet::device_id;
use tasmota_web::routes;
use tasmota_web::state::AppState;

/// A status with no `energy` block at all: the device has no energy sensor,
/// which must render as "n/a", never as "0 W".
fn status_without_energy(host: &str) -> DeviceStatus {
    DeviceStatus {
        host: host.into(),
        name: Some("Test Plug".into()),
        friendly_names: vec!["Test Plug".into()],
        module: Some(1),
        relays: Vec::new(),
        firmware: Some("14.2.0".into()),
        net: NetInfo::default(),
        uptime: Some("1T00:00:00".into()),
        wifi_rssi: None,
        energy: None,
        mqtt: None,
    }
}

#[tokio::test]
async fn dashboard_renders_card_with_na_for_missing_energy() {
    let host = "192.0.2.30".to_string();
    let config = Config {
        devices: vec![DeviceConfig {
            name: "Test Plug".into(),
            host: host.clone(),
            password: None,
            protected: false,
        }],
        ..Config::default()
    };
    let state = AppState::new(config, PathBuf::from("unused.toml"));

    // Mark the device reachable with a status that has no energy sensor, as
    // the poller would after a successful read from such a device.
    {
        let mut fleet = state.inner.fleet.write().await;
        let dev = fleet.devices.first_mut().expect("one device configured");
        dev.reachable = true;
        dev.status = Some(status_without_energy(&host));
    }

    let app = routes::router(state, false);
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body.to_vec()).unwrap();

    assert!(body.contains("Test Plug"), "device name should be shown");
    let expected_card_id = format!("card-{}", device_id(&host));
    assert!(
        body.contains(&expected_card_id),
        "card should carry a stable id for SSE swaps"
    );
    assert!(
        body.contains(">n/a<"),
        "missing energy must render the muted n/a marker, not a coerced value"
    );
    assert!(
        !body.contains(">0 W<"),
        "a device with no energy sensor must never render as 0 W"
    );
}
