//! Regression test for the update checker rebuilding its HTTP client every
//! cycle (`updates::check_fleet` used to call `reqwest::Client::builder()...
//! .build()` on every call instead of reusing `AppState`'s
//! `updates_http` client). A rebuilt client starts with an empty connection
//! pool, so every cycle pays for a fresh TCP (and, against the real HTTPS
//! release feed, TLS) handshake instead of reusing a warm one; a reused
//! client keeps the same pooled connection alive across cycles. A
//! hand-rolled HTTP/1.1 keep-alive server (not `httpmock`, which does not
//! expose a connection count) counts how many distinct TCP connections it
//! accepts while `check_fleet` runs several cycles back to back: one
//! connection for a reused client, one per cycle for a rebuilt one.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use httpmock::prelude::*;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use plugboard::config::{Config, DeviceConfig, UpdatesConfig};
use plugboard::poller::refresh_once;
use plugboard::state::AppState;
use plugboard::updates::check_fleet;
use switchkit::Vendor;

const RELEASE_BODY: &str = r#"{"tag_name":"v99.99.99"}"#;

/// Accepts connections and counts each one, then serves as many keep-alive
/// HTTP/1.1 requests as the client sends on it before closing. A reused
/// client should open this exactly once and send every `check_fleet` call's
/// request down that same connection; a client rebuilt every call gets a
/// fresh, empty connection pool each time and must open a new one per call.
async fn spawn_counting_keepalive_server() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind counting server");
    let addr = listener.local_addr().expect("local addr");
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = connections.clone();
    tokio::spawn(async move {
        loop {
            let (socket, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => break,
            };
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(handle_keepalive_connection(socket));
        }
    });
    (addr, connections)
}

async fn handle_keepalive_connection(mut socket: tokio::net::TcpStream) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
        RELEASE_BODY.len(),
        RELEASE_BODY
    );
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        // Read one request off this connection (up to the blank line ending
        // its headers; a GET has no body), then answer and loop for the next
        // keep-alive request on the SAME socket.
        buf.clear();
        loop {
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
            match socket.read(&mut chunk).await {
                Ok(0) => return, // client closed the connection
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => return,
            }
        }
        if socket.write_all(response.as_bytes()).await.is_err() {
            return;
        }
    }
}

#[tokio::test]
async fn check_fleet_reuses_one_connection_across_many_cycles() {
    let tasmota = MockServer::start_async().await;
    tasmota.mock(|when, then| {
        when.method(GET).path("/cm").query_param("cmnd", "Status 0");
        then.status(200).json_body(json!({
            "Status": {"DeviceName": "Plug", "Module": 1, "FriendlyName": ["Plug"], "Power": 1},
            "StatusFWR": {"Version": "14.2.0"},
            "StatusNET": {"IPAddress": "192.0.2.50", "Mac": "AA:BB:CC:00:11:22", "Hostname": "plug"},
            "StatusSTS": {"POWER": "ON", "Uptime": "1T00:00:00", "Wifi": {"RSSI": 70}}
        }));
    });

    let (release_addr, connections) = spawn_counting_keepalive_server().await;

    let config = Config {
        devices: vec![DeviceConfig {
            name: "Plug".into(),
            host: tasmota.address().to_string(),
            password: None,
            protected: false,
            group: None,
            vendor: Vendor::Tasmota,
        }],
        updates: UpdatesConfig {
            enabled: true,
            interval_secs: 21_600,
            auto_apply: false,
            tasmota_release_url: format!("http://{release_addr}/repos/x/releases/latest"),
        },
        ..Config::default()
    };
    let state = AppState::new(config, PathBuf::from("unused.toml"));

    // One real poll so the device has a confirmed current version; check_fleet
    // only calls the release feed at all when a Tasmota device is confirmed.
    refresh_once(&state).await;

    const CYCLES: usize = 10;
    for _ in 0..CYCLES {
        check_fleet(&state).await;
    }

    let opened = connections.load(Ordering::SeqCst);
    assert_eq!(
        opened, 1,
        "expected check_fleet to reuse one pooled connection across {CYCLES} calls, but the \
         release feed saw {opened} distinct TCP connections, meaning a fresh client (and a fresh \
         connection) was created on every call instead of reusing AppState's HTTP client"
    );
}
