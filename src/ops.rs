//! Async wrappers around the blocking `tasmota-core` device operations.
//!
//! `HttpTransport` is a synchronous (`ureq`-backed) client, so every call here
//! runs the blocking `tasmota_core::ops` function inside `spawn_blocking` to keep
//! device I/O off the async runtime's worker threads.

use serde_json::Value;
use tasmota_core::ops::PowerAction;
use tasmota_core::{DeviceAddr, DeviceStatus, HttpTransport, Relay, Result};

pub async fn get_status(t: &HttpTransport, addr: DeviceAddr) -> Result<DeviceStatus> {
    let t = t.clone();
    tokio::task::spawn_blocking(move || tasmota_core::ops::get_status(&t, &addr))
        .await
        .expect("blocking task")
}

pub async fn set_power(
    t: &HttpTransport,
    addr: DeviceAddr,
    relay: Option<u8>,
    action: PowerAction,
) -> Result<Relay> {
    let t = t.clone();
    tokio::task::spawn_blocking(move || tasmota_core::ops::set_power(&t, &addr, relay, action))
        .await
        .expect("blocking task")
}

pub async fn firmware_version(t: &HttpTransport, addr: DeviceAddr) -> Result<String> {
    let t = t.clone();
    tokio::task::spawn_blocking(move || tasmota_core::ops::firmware_version(&t, &addr))
        .await
        .expect("blocking task")
}

pub async fn firmware_update(
    t: &HttpTransport,
    addr: DeviceAddr,
    ota_url: Option<String>,
) -> Result<Value> {
    let t = t.clone();
    tokio::task::spawn_blocking(move || {
        tasmota_core::ops::firmware_update(&t, &addr, ota_url.as_deref())
    })
    .await
    .expect("blocking task")
}

pub async fn config_get(t: &HttpTransport, addr: DeviceAddr, setting: String) -> Result<Value> {
    let t = t.clone();
    tokio::task::spawn_blocking(move || tasmota_core::ops::config_get(&t, &addr, &setting))
        .await
        .expect("blocking task")
}

pub async fn config_set(
    t: &HttpTransport,
    addr: DeviceAddr,
    setting: String,
    value: String,
) -> Result<Value> {
    let t = t.clone();
    tokio::task::spawn_blocking(move || tasmota_core::ops::config_set(&t, &addr, &setting, &value))
        .await
        .expect("blocking task")
}

pub async fn console(t: &HttpTransport, addr: DeviceAddr, command: String) -> Result<Value> {
    let t = t.clone();
    tokio::task::spawn_blocking(move || tasmota_core::ops::console(&t, &addr, &command))
        .await
        .expect("blocking task")
}

pub async fn template_get(t: &HttpTransport, addr: DeviceAddr) -> Result<Value> {
    let t = t.clone();
    tokio::task::spawn_blocking(move || tasmota_core::ops::template_get(&t, &addr))
        .await
        .expect("blocking task")
}

pub async fn backup_config(t: &HttpTransport, addr: DeviceAddr) -> Result<Vec<u8>> {
    let t = t.clone();
    tokio::task::spawn_blocking(move || tasmota_core::ops::backup_config(&t, &addr))
        .await
        .expect("blocking task")
}
