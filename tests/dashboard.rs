//! Integration test for the dashboard route: proves absent-data-not-zero at
//! the view layer (a device with no energy sensor renders "n/a", never "0").

use std::path::PathBuf;

use http_body_util::BodyExt;
use tower::ServiceExt;

use switchkit::{DeviceSnapshot, Energy, Firmware, NetInfo, Relay, RelayState, Signal};
use tasmota_web::config::{Config, DeviceConfig};
use tasmota_web::fleet::device_id;
use tasmota_web::routes;
use tasmota_web::state::AppState;

/// A status with no `energy` block at all: the device has no energy sensor,
/// which must render as "n/a", never as "0 W".
fn status_without_energy(host: &str) -> DeviceSnapshot {
    DeviceSnapshot {
        host: host.into(),
        name: Some("Test Plug".into()),
        relays: Vec::new(),
        firmware: Some(Firmware {
            version: Some("14.2.0".into()),
            update_available: None,
        }),
        net: NetInfo::default(),
        uptime: Some("1T00:00:00".into()),
        energy: None,
        ..Default::default()
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
/// and a real Wi-Fi signal reading - the exact shape needed to prove present
/// fields render their real values while absent ones render n/a, never 0.
fn status_with_partial_energy(host: &str) -> DeviceSnapshot {
    DeviceSnapshot {
        host: host.into(),
        name: Some("Test Plug".into()),
        relays: vec![Relay {
            index: 0,
            state: RelayState::On,
            raw: "ON".into(),
        }],
        firmware: Some(Firmware {
            version: Some("14.2.0".into()),
            update_available: None,
        }),
        net: NetInfo {
            ip: Some("192.0.2.31".into()),
            mac: Some("AA:BB:CC:DD:EE:FF".into()),
            hostname: None,
        },
        uptime: Some("2T01:02:03".into()),
        signal: Some(Signal::from_quality_percent(63)),
        energy: Some(Energy {
            power_w: Some(12.5),
            voltage_v: None,
            current_a: Some(0.5),
            today_kwh: None,
            total_kwh: None,
        }),
        ..Default::default()
    }
}

/// `switchkit`'s vendor-neutral `DeviceSnapshot` carries no MQTT data at all
/// (a genuine, unavoidable capability gap vs. the old Tasmota-specific model -
/// see `views::device::mqtt_section`), so the whole MQTT section is now
/// hard-coded n/a unconditionally: there is no longer any status field to
/// feed a live MQTT value through, so this can no longer be proven
/// non-vacuously against a "device reports MQTT connected" fixture the way
/// the pre-migration test did. This test instead asserts the section's
/// unconditional n/a rendering directly.
#[tokio::test]
async fn device_detail_renders_partial_status_with_na_and_mqtt_always_na() {
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
        dev.status = Some(status_with_partial_energy(&host));
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
    assert!(body.contains("14.2.0"), "firmware should render");
    assert!(body.contains("192.0.2.31"), "ip should render");
    assert!(body.contains("63%"), "present Wi-Fi signal should render");

    // Absent fields (voltage_v, today_kwh, total_kwh, net.hostname), the
    // permanently-n/a "Yesterday" row, and all five permanently-n/a MQTT
    // fields each render the muted n/a marker, never 0 or blank.
    let na_count = body.matches(">n/a<").count();
    assert!(
        na_count >= 10,
        "expected at least 10 n/a markers (4 absent fields + yesterday + 5 mqtt fields), got {na_count}"
    );
    assert!(
        !body.contains(">0<"),
        "absent numeric fields must never render as a bare 0"
    );

    // The MQTT section is unconditionally n/a under switchkit's vendor-neutral
    // model: no live MQTT value can leak, since there is no status field left
    // to source one from.
    assert!(
        !body.contains(">true<"),
        "mqtt connected must never leak a bool as text"
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
