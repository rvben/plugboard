//! Integration test for the dashboard route: proves absent-data-not-zero at
//! the view layer (a device with no energy sensor renders "n/a", never "0").

use std::path::PathBuf;

use http_body_util::BodyExt;
use tower::ServiceExt;

use tasmota_core::{DeviceStatus, Energy, MqttInfo, NetInfo, Relay, RelayState};
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

/// A status with a PARTIAL energy block (some fields present, some absent)
/// and an MQTT block whose `connected` is `Some(true)` - the exact shape
/// needed to prove the detail page's "connected is always n/a" rule is
/// non-vacuous (it must hide a `true`, not just an absent value).
fn status_with_partial_energy_and_mqtt(host: &str) -> DeviceStatus {
    DeviceStatus {
        host: host.into(),
        name: Some("Test Plug".into()),
        friendly_names: vec!["Test Plug".into()],
        module: Some(1),
        relays: vec![Relay {
            index: 0,
            state: RelayState::On,
            raw: "ON".into(),
        }],
        firmware: Some("14.2.0".into()),
        net: NetInfo {
            ip: Some("192.0.2.31".into()),
            mac: Some("AA:BB:CC:DD:EE:FF".into()),
            hostname: None,
        },
        uptime: Some("2T01:02:03".into()),
        wifi_rssi: Some(-55),
        energy: Some(Energy {
            power_w: Some(12.5),
            voltage_v: None,
            current_a: Some(0.5),
            today_kwh: None,
            yesterday_kwh: Some(0.8),
            total_kwh: None,
        }),
        mqtt: Some(MqttInfo {
            host: Some("mqtt.example.test".into()),
            port: Some(1883),
            client: Some("tasmota_ABC123".into()),
            reconnect_count: Some(3),
            connected: Some(true),
        }),
    }
}

#[tokio::test]
async fn device_detail_renders_partial_status_with_na_and_hides_mqtt_bool() {
    let host = "192.0.2.31".to_string();
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
    let id = device_id(&host);

    {
        let mut fleet = state.inner.fleet.write().await;
        let dev = fleet.devices.first_mut().expect("one device configured");
        dev.reachable = true;
        dev.status = Some(status_with_partial_energy_and_mqtt(&host));
    }

    let app = routes::router(state, false);
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/device/{id}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8(body.to_vec()).unwrap();

    // Present fields render their real values.
    assert!(body.contains("12.5"), "present power_w should render");
    assert!(body.contains("0.5"), "present current_a should render");
    assert!(body.contains("0.8"), "present yesterday_kwh should render");
    assert!(body.contains("14.2.0"), "firmware should render");
    assert!(body.contains("192.0.2.31"), "ip should render");
    assert!(
        body.contains("mqtt.example.test"),
        "mqtt host should render"
    );
    assert!(body.contains("1883"), "mqtt port should render");

    // Absent fields (voltage_v, today_kwh, total_kwh, net.hostname) each render
    // the muted n/a marker, never 0 or blank - plus the always-n/a mqtt connected line.
    let na_count = body.matches(">n/a<").count();
    assert!(
        na_count >= 5,
        "expected at least 5 n/a markers for the absent fields, got {na_count}"
    );
    assert!(
        !body.contains(">0<"),
        "absent numeric fields must never render as a bare 0"
    );

    // MQTT `connected` is HARD-CODED to n/a, even though this device's status
    // has `mqtt.connected = Some(true)`: proves the hardcoding is non-vacuous
    // (a naive `na(mqtt.connected)` would render the bool as the text `>true<`
    // here, not `n/a`). Scoped to a bool rendered as element text, so a legitimate
    // attribute value like `aria-hidden="true"` does not trip this check.
    assert!(
        !body.contains(">true<"),
        "mqtt connected must never leak the underlying bool as text"
    );
}

#[tokio::test]
async fn device_detail_unknown_id_is_404() {
    let state = AppState::new(Config::default(), PathBuf::from("unused.toml"));
    let app = routes::router(state, false);
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/device/does-not-exist")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
}
