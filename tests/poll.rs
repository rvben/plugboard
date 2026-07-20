//! Integration test for the background poller against a mocked Tasmota device.

use std::path::PathBuf;

use httpmock::prelude::*;
use serde_json::json;

use tasmota_web::config::{Config, DeviceConfig};
use tasmota_web::poller::refresh_once;
use tasmota_web::state::AppState;

fn mock_status(server: &MockServer) {
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

fn mock_statetext(server: &MockServer) {
    server.mock(|when, then| {
        when.method(GET)
            .path("/cm")
            .query_param("cmnd", "StateText");
        then.status(200)
            .json_body(json!({"StateText1": "OFF", "StateText2": "ON"}));
    });
}

#[tokio::test]
async fn refresh_once_marks_device_reachable_with_status() {
    let server = MockServer::start();
    mock_status(&server);
    mock_statetext(&server);
    let host = server.address().to_string();

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

    refresh_once(&state).await;

    let fleet = state.inner.fleet.read().await;
    let dev = fleet.devices.first().expect("one device in fleet");
    assert!(dev.reachable, "device should be marked reachable");
    assert!(dev.error.is_none(), "no error expected on success");
    let status = dev.status.as_ref().expect("status should be populated");
    assert_eq!(
        status.firmware.as_ref().and_then(|f| f.version.as_deref()),
        Some("14.2.0")
    );
}

#[tokio::test]
async fn refresh_once_clears_stale_status_when_device_goes_offline() {
    // Reset (drop + delete) removes the mock so subsequent requests 404, simulating
    // a device that answered once and then went offline.
    let server = MockServer::start();
    let mut status_mock = server.mock(|when, then| {
        when.method(GET).path("/cm").query_param("cmnd", "Status 0");
        then.status(200).json_body(json!({
            "Status": {"DeviceName": "TestPlug", "Module": 1, "FriendlyName": ["TestPlug"], "Power": 1},
            "StatusFWR": {"Version": "14.2.0"},
            "StatusNET": {"IPAddress": "192.0.2.50", "Mac": "AA:BB:CC:00:11:22", "Hostname": "testplug"},
            "StatusSTS": {"POWER": "ON", "Uptime": "1T02:03:04", "Wifi": {"RSSI": 76}}
        }));
    });
    mock_statetext(&server);
    let host = server.address().to_string();

    let config = Config {
        devices: vec![DeviceConfig {
            name: "Test Plug".into(),
            host,
            password: None,
            protected: false,
        }],
        ..Config::default()
    };
    let state = AppState::new(config, PathBuf::from("unused.toml"));

    // First refresh succeeds: status gets populated.
    refresh_once(&state).await;
    {
        let fleet = state.inner.fleet.read().await;
        let dev = fleet.devices.first().expect("one device in fleet");
        assert!(dev.reachable);
        assert!(
            dev.status.is_some(),
            "status should be populated after a success"
        );
    }

    // The device goes offline: delete the mock so every request now fails.
    status_mock.delete();

    // Second refresh fails: the previously-populated status must be cleared, never
    // left stale-as-live.
    refresh_once(&state).await;

    let fleet = state.inner.fleet.read().await;
    let dev = fleet.devices.first().expect("one device in fleet");
    assert!(!dev.reachable, "device should be marked unreachable");
    assert!(
        dev.status.is_none(),
        "a stale status must be cleared when the device goes offline, not left as last-seen data"
    );
    assert!(dev.error.is_some(), "an error message should be recorded");
}
