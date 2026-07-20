//! Thin async passthroughs to a vendor's `switchkit::SmartDevice` client.
//!
//! Every `switchkit` client is already async (no blocking transport, no
//! `spawn_blocking` needed here), so each wrapper below is just a named,
//! app-level call site for the corresponding trait method - kept as free
//! functions (rather than calling the trait directly from routes) so a
//! future cross-cutting concern (logging, metrics, retries) has one place to
//! land per operation.

use serde_json::Value;
use switchkit::{DeviceSnapshot, DeviceTarget, PowerAction, Relay, Result, SmartDevice};

pub async fn get_status(client: &dyn SmartDevice, target: &DeviceTarget) -> Result<DeviceSnapshot> {
    client.status(target).await
}

pub async fn set_power(
    client: &dyn SmartDevice,
    target: &DeviceTarget,
    channel: Option<u8>,
    action: PowerAction,
) -> Result<Relay> {
    client.set_power(target, channel, action).await
}

pub async fn firmware_version(
    client: &dyn SmartDevice,
    target: &DeviceTarget,
) -> Result<Option<String>> {
    client.firmware_version(target).await
}

pub async fn firmware_update(
    client: &dyn SmartDevice,
    target: &DeviceTarget,
    ota_url: Option<&str>,
) -> Result<()> {
    client.firmware_update(target, ota_url).await
}

pub async fn config_get(
    client: &dyn SmartDevice,
    target: &DeviceTarget,
    setting: &str,
) -> Result<Value> {
    client.config_get(target, setting).await
}

pub async fn config_set(
    client: &dyn SmartDevice,
    target: &DeviceTarget,
    setting: &str,
    value: &str,
) -> Result<Value> {
    client.config_set(target, setting, value).await
}

pub async fn console(
    client: &dyn SmartDevice,
    target: &DeviceTarget,
    command: &str,
) -> Result<Value> {
    client.console(target, command).await
}

pub async fn backup(client: &dyn SmartDevice, target: &DeviceTarget) -> Result<Vec<u8>> {
    client.backup(target).await
}
