//! End-to-end coverage of the firmware update checker (`plugboard::updates`):
//! a mocked Shelly device (its own `Shelly.CheckForUpdate`) and a mocked
//! Tasmota device against a mocked release feed, checked through the REAL
//! `check_fleet` entry point after a REAL poll - the same path the
//! background task runs.

use std::path::PathBuf;

use httpmock::prelude::*;
use plugboard::config::{Config, DeviceConfig};
use plugboard::fleet::device_id;
use plugboard::state::AppState;
use plugboard::{poller, updates};
use serde_json::json;
use switchkit::Vendor;

fn mock_tasmota(server: &MockServer, version: &str) {
    let version = version.to_string();
    server.mock(|when, then| {
        when.method(GET).path("/cm");
        then.status(200).json_body(json!({
            "Status": {"DeviceName": "Plug", "FriendlyName": ["Plug"], "Power": 1},
            "StatusFWR": {"Version": version},
            "StatusNET": {"IPAddress": "192.0.2.50", "Mac": "AA:BB:CC:00:11:22", "Hostname": "plug"},
            "StatusSTS": {"POWER": "ON", "Uptime": "1T00:00:00", "Wifi": {"RSSI": 70}}
        }));
    });
}

fn mock_shelly(server: &MockServer, version: &str, stable: Option<&str>) {
    let version = version.to_string();
    server.mock(|when, then| {
        when.method(GET).path("/shelly");
        then.status(200).json_body(json!({
            "id": "shellyplus1pm-aabbccddeeff",
            "mac": "AABBCCDDEEFF",
            "model": "SNSW-001P16EU",
            "gen": 2,
            "ver": version,
            "app": "Plus1PM"
        }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/rpc/Shelly.GetStatus");
        then.status(200).json_body(json!({
            "switch:0": {"id": 0, "output": true},
            "sys": {"uptime": 100}
        }));
    });
    let check_body = match stable {
        Some(v) => json!({"stable": {"version": v}}),
        None => json!({}),
    };
    server.mock(|when, then| {
        when.method(GET).path("/rpc/Shelly.CheckForUpdate");
        then.status(200).json_body(check_body.clone());
    });
    server.mock(|when, then| {
        when.method(POST).path("/rpc");
        then.status(200)
            .json_body(json!({"id": 1, "src": "mock", "result": check_body}));
    });
}

fn mock_release_feed(server: &MockServer, tag: &str) {
    let tag = tag.to_string();
    server.mock(|when, then| {
        when.method(GET).path("/latest");
        then.status(200).json_body(json!({"tag_name": tag}));
    });
}

fn config(devices: Vec<DeviceConfig>, release_url: String) -> Config {
    let mut cfg = Config {
        devices,
        ..Config::default()
    };
    cfg.updates.tasmota_release_url = release_url;
    cfg
}

fn device(name: &str, host: &str, vendor: Vendor) -> DeviceConfig {
    DeviceConfig {
        name: name.into(),
        host: host.into(),
        password: None,
        protected: false,
        vendor,
    }
}

/// A Tasmota device behind the latest release and a Shelly device whose own
/// check reports a newer stable both get a CONFIRMED available version; the
/// Shelly whose check returns nothing gets none.
#[tokio::test]
async fn check_fleet_confirms_updates_per_vendor() {
    let tasmota = MockServer::start_async().await;
    mock_tasmota(&tasmota, "14.2.0");
    let shelly_new = MockServer::start_async().await;
    mock_shelly(&shelly_new, "1.4.4", Some("1.5.1"));
    let shelly_current = MockServer::start_async().await;
    mock_shelly(&shelly_current, "1.5.1", None);
    let feed = MockServer::start_async().await;
    mock_release_feed(&feed, "v15.5.0");

    let t_host = tasmota.address().to_string();
    let s_new_host = shelly_new.address().to_string();
    let s_cur_host = shelly_current.address().to_string();
    let state = AppState::new(
        config(
            vec![
                device("T", &t_host, Vendor::Tasmota),
                device("S1", &s_new_host, Vendor::Shelly),
                device("S2", &s_cur_host, Vendor::Shelly),
            ],
            format!("{}/latest", feed.base_url()),
        ),
        PathBuf::from("unused.toml"),
    );

    // A real poll first: the checker only considers devices whose current
    // version a live snapshot confirmed.
    poller::refresh_once(&state).await;
    updates::check_fleet(&state).await;

    let map = updates::snapshot(&state.inner.updates);

    let t = map.get(&device_id(&t_host)).expect("tasmota entry");
    assert_eq!(t.current.as_deref(), Some("14.2.0"));
    assert_eq!(
        t.available(),
        Some("15.5.0"),
        "the release feed's newer tag must be offered (v-prefix stripped)"
    );

    let s_new = map.get(&device_id(&s_new_host)).expect("shelly entry");
    assert_eq!(s_new.current.as_deref(), Some("1.4.4"));
    assert_eq!(s_new.available(), Some("1.5.1"));

    let s_cur = map.get(&device_id(&s_cur_host)).expect("shelly entry");
    assert_eq!(s_cur.current.as_deref(), Some("1.5.1"));
    assert_eq!(
        s_cur.phase,
        updates::Phase::UpToDate,
        "an up-to-date device must claim nothing"
    );
}

/// The full applying lifecycle, driven by REAL observations: a commanded
/// update enters `Applying` (which a concurrent check must NOT clobber),
/// a poll observing the target version confirms it `Applied`, and a
/// timed-out window ends `Unconfirmed` - never a guessed success.
#[tokio::test]
async fn applying_lifecycle_confirms_by_observation_or_admits_the_unknown() {
    let tasmota = MockServer::start_async().await;
    mock_tasmota(&tasmota, "14.2.0");
    let feed = MockServer::start_async().await;
    mock_release_feed(&feed, "v15.5.0");
    let t_host = tasmota.address().to_string();
    let id = device_id(&t_host);
    let state = AppState::new(
        config(
            vec![device("T", &t_host, Vendor::Tasmota)],
            format!("{}/latest", feed.base_url()),
        ),
        PathBuf::from("unused.toml"),
    );
    poller::refresh_once(&state).await;

    updates::mark_applying(
        &state.inner.updates,
        &id,
        Some("15.5.0".into()),
        Some("14.2.0".into()),
        1_000,
    );

    // A check while applying preserves the in-flight entry verbatim.
    updates::check_fleet(&state).await;
    let map = updates::snapshot(&state.inner.updates);
    assert!(
        matches!(map.get(&id).unwrap().phase, updates::Phase::Applying { .. }),
        "a periodic check must never clobber an in-flight update"
    );

    // The device still reports the OLD version within the window: still
    // applying (an OTA downloads before it reboots), never concluded early.
    updates::observe_poll(
        &state.inner.updates,
        &[(id.clone(), true, Some("14.2.0".into()))],
        1_030,
    );
    let map = updates::snapshot(&state.inner.updates);
    assert!(matches!(
        map.get(&id).unwrap().phase,
        updates::Phase::Applying { .. }
    ));

    // The device comes back running the target: CONFIRMED applied.
    updates::observe_poll(
        &state.inner.updates,
        &[(id.clone(), true, Some("15.5.0".into()))],
        1_060,
    );
    let map = updates::snapshot(&state.inner.updates);
    assert_eq!(
        map.get(&id).unwrap().phase,
        updates::Phase::Applied {
            version: "15.5.0".into()
        }
    );
    assert_eq!(map.get(&id).unwrap().current.as_deref(), Some("15.5.0"));

    // Separately: a window that elapses with no confirmation ends honest.
    updates::mark_applying(
        &state.inner.updates,
        &id,
        Some("16.0.0".into()),
        None,
        2_000,
    );
    updates::observe_poll(
        &state.inner.updates,
        &[(id.clone(), false, None)],
        2_000 + updates::APPLY_TIMEOUT_SECS + 1,
    );
    let map = updates::snapshot(&state.inner.updates);
    assert_eq!(
        map.get(&id).unwrap().phase,
        updates::Phase::Unconfirmed,
        "an unconfirmable outcome must be reported as unknown, never success"
    );
}

/// An unreachable release feed claims NO Tasmota update (never a guess), and
/// an offline device gets no entry at all: there is no confirmed current
/// version to compare against, so any claim would be baseless.
#[tokio::test]
async fn check_fleet_claims_nothing_without_confirmation() {
    let tasmota = MockServer::start_async().await;
    mock_tasmota(&tasmota, "14.2.0");
    let t_host = tasmota.address().to_string();
    // A bound-then-dropped loopback port: connecting to it is a REAL, fast
    // connection-refused for both the offline device and the dead feed.
    let dead_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let dead_host = format!("127.0.0.1:{dead_port}");
    let state = AppState::new(
        config(
            vec![
                device("T", &t_host, Vendor::Tasmota),
                device("Off", &dead_host, Vendor::Tasmota),
            ],
            format!("http://{dead_host}/latest"),
        ),
        PathBuf::from("unused.toml"),
    );

    poller::refresh_once(&state).await;
    updates::check_fleet(&state).await;

    let map = updates::snapshot(&state.inner.updates);
    let t = map.get(&device_id(&t_host)).expect("tasmota entry");
    assert_eq!(
        t.available(),
        None,
        "a dead release feed must never produce an update claim"
    );
    assert!(
        !map.contains_key(&device_id(&dead_host)),
        "an offline device has no confirmed current version, so no entry"
    );
}
