//! Integration test for the dashboard route: proves absent-data-not-zero at
//! the view layer (a device with no energy sensor renders "n/a", never "0").

use std::path::PathBuf;

use http_body_util::BodyExt;
use tower::ServiceExt;

use plugboard::config::{Config, DeviceConfig};
use plugboard::fleet::device_id;
use plugboard::routes;
use plugboard::state::AppState;
use switchkit::{
    Capabilities, DeviceSnapshot, Energy, Firmware, NetInfo, Relay, RelayState, Signal, Vendor,
};

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
            vendor: Vendor::Tasmota,
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

/// A status with a PARTIAL energy block (some fields present, some absent),
/// a real Wi-Fi signal reading, and the `metering`/`firmware_ota`
/// capabilities confirmed (so the capability-gated energy and firmware
/// sections actually render) - the exact shape needed to prove present
/// fields render their real values while absent ones render n/a, never 0.
fn status_with_partial_energy(host: &str) -> DeviceSnapshot {
    DeviceSnapshot {
        host: host.into(),
        name: Some("Test Plug".into()),
        capabilities: Capabilities {
            metering: true,
            firmware_ota: true,
            ..Default::default()
        },
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

/// There is no MQTT section anymore: `switchkit`'s vendor-neutral
/// `DeviceSnapshot` has no MQTT data model at all, so a permanent-n/a MQTT
/// section would only ever imply a capability that doesn't exist - it was
/// removed rather than kept hard-coded n/a. This test proves both halves of
/// that: present fields render their real values, absent ones render n/a
/// (never 0), and the page contains no MQTT section at all.
#[tokio::test]
async fn device_detail_renders_partial_status_with_na_and_no_mqtt_section() {
    let host = "192.0.2.31".to_string();
    let config = Config {
        devices: vec![DeviceConfig {
            name: "Test Plug".into(),
            host: host.clone(),
            password: None,
            protected: false,
            vendor: Vendor::Tasmota,
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

    // Absent fields (voltage_v, today_kwh, total_kwh) and the permanently-n/a
    // "Yesterday" row render the muted n/a marker inside the energy section,
    // plus the absent net.hostname in the network section: 5 markers total.
    let na_count = body.matches(">n/a<").count();
    assert_eq!(
        na_count, 5,
        "expected exactly 5 n/a markers (voltage, today, yesterday, total, hostname), got {na_count}"
    );
    assert!(
        !body.contains(">0<"),
        "absent numeric fields must never render as a bare 0"
    );

    // No MQTT section at all: switchkit's vendor-neutral DeviceSnapshot has
    // no MQTT data model, so there is nothing honest to render there.
    assert!(!body.contains("MQTT"), "MQTT section must not be rendered");
}

/// A Shelly-vendor device's Wi-Fi signal renders its real dBm reading, never
/// a fabricated percentage, and the page carries a "Shelly" vendor tag. The
/// device name deliberately avoids the word "Shelly" so the assertion below
/// can only match the vendor tag, not the page title or device name.
#[tokio::test]
async fn device_detail_shelly_vendor_renders_dbm_never_percent() {
    let host = "192.0.2.32".to_string();
    let config = Config {
        devices: vec![DeviceConfig {
            name: "Test Plug".into(),
            host: host.clone(),
            password: None,
            protected: false,
            vendor: Vendor::Shelly,
        }],
        ..Config::default()
    };
    let state = AppState::new(config, PathBuf::from("unused.toml"));
    let id = device_id(&host);

    {
        let mut fleet = state.inner.fleet.write().await;
        let dev = fleet.devices.first_mut().expect("one device configured");
        dev.reachable = true;
        dev.status = Some(DeviceSnapshot {
            host: host.clone(),
            name: Some("Test Plug".into()),
            capabilities: Capabilities {
                metering: true,
                console: true,
                ..Default::default()
            },
            signal: Some(Signal::from_dbm(-60)),
            energy: Some(Energy {
                power_w: Some(5.0),
                ..Default::default()
            }),
            ..Default::default()
        });
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

    assert!(body.contains(">Shelly<"), "vendor tag should show Shelly");
    assert!(
        body.contains("-60 dBm"),
        "signal should render the real dBm value"
    );
    assert!(
        !body.contains('%'),
        "a dBm-only signal must never render a fabricated percentage"
    );
    assert!(
        body.contains("admin-console"),
        "the confirmed console capability should render the console admin subsection"
    );
}

/// The Tasmota-vendor equivalent: still renders its Wi-Fi signal as a
/// percentage, and carries a "Tasmota" vendor tag - proving the rewrite
/// didn't regress the pre-existing (and still correct) percent path.
#[tokio::test]
async fn device_detail_tasmota_vendor_still_renders_percent() {
    let host = "192.0.2.33".to_string();
    let config = Config {
        devices: vec![DeviceConfig {
            name: "Test Plug".into(),
            host: host.clone(),
            password: None,
            protected: false,
            vendor: Vendor::Tasmota,
        }],
        ..Config::default()
    };
    let state = AppState::new(config, PathBuf::from("unused.toml"));
    let id = device_id(&host);

    {
        let mut fleet = state.inner.fleet.write().await;
        let dev = fleet.devices.first_mut().expect("one device configured");
        dev.reachable = true;
        dev.status = Some(DeviceSnapshot {
            host: host.clone(),
            name: Some("Test Plug".into()),
            signal: Some(Signal::from_quality_percent(80)),
            ..Default::default()
        });
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

    assert!(body.contains(">Tasmota<"), "vendor tag should show Tasmota");
    assert!(
        body.contains("80%"),
        "signal should render the real percentage"
    );
    assert!(
        !body.contains("dBm"),
        "a percent-only signal must never render a fabricated dBm value"
    );
}

/// A device with NO confirmed capabilities (offline, unpolled, or a bare
/// device with no admin/metering surface) renders none of the
/// capability-gated sections - they are absent, never shown as broken or
/// disabled controls.
#[tokio::test]
async fn device_detail_without_capabilities_hides_gated_sections() {
    let host = "192.0.2.34".to_string();
    let config = Config {
        devices: vec![DeviceConfig {
            name: "Test Plug".into(),
            host: host.clone(),
            password: None,
            protected: false,
            vendor: Vendor::Shelly,
        }],
        ..Config::default()
    };
    let state = AppState::new(config, PathBuf::from("unused.toml"));
    let id = device_id(&host);

    {
        let mut fleet = state.inner.fleet.write().await;
        let dev = fleet.devices.first_mut().expect("one device configured");
        dev.reachable = true;
        dev.status = Some(DeviceSnapshot {
            host: host.clone(),
            name: Some("Test Plug".into()),
            // Capabilities default to all-false: nothing confirmed.
            ..Default::default()
        });
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

    assert!(
        !body.contains("Energy"),
        "energy section must be absent without metering"
    );
    assert!(
        !body.contains("Firmware"),
        "firmware section must be absent without firmware_ota"
    );
    assert!(
        !body.contains("Admin"),
        "admin panel must be absent with no confirmed admin capability"
    );
    // Non-gated sections still render.
    assert!(
        body.contains("Relays"),
        "relays section is not capability-gated"
    );
    assert!(
        body.contains("Network"),
        "network section is not capability-gated"
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
