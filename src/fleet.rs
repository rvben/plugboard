use tasmota_core::DeviceStatus;

use crate::config::DeviceConfig;

/// A unique, CSS/selector-safe id for a device, stable across restarts for a host.
///
/// Injective: a full lowercase-hex encoding of the host bytes, so DISTINCT hosts ALWAYS
/// get distinct ids - there is no hash, hence no possible collision. The `d-` prefix
/// keeps the id a valid CSS/HTML id even when the host starts with a digit (an IP like
/// `192.0.2.5`), and the output is `[a-z0-9-]` only (safe in a `#selector` and a route).
pub fn device_id(host: &str) -> String {
    let hex: String = host.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    format!("d-{hex}")
}

#[derive(Debug, Clone)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub host: String,
    pub protected: bool,
    /// Whether the last STATUS read (a poll, or a control action's follow-up refresh)
    /// succeeded. This is the single source of truth for "online" AND for whether any
    /// live value may be rendered. A failed read makes the device offline; nothing
    /// stale - relay OR telemetry - is ever shown as live.
    pub reachable: bool,
    /// Telemetry AND relay from the last SUCCESSFUL `get_status` ONLY. Cleared (`None`)
    /// whenever a read fails, so neither energy/RSSI nor the relay badge are ever
    /// rendered stale-as-live. The relay is read from `status.relays`; there is
    /// deliberately no separate carried-over relay field.
    pub status: Option<DeviceStatus>,
    pub error: Option<String>,
}

impl DeviceView {
    pub fn from_config(c: &DeviceConfig) -> Self {
        DeviceView {
            id: device_id(&c.host),
            name: c.name.clone(),
            host: c.host.clone(),
            protected: c.protected,
            reachable: false, // unknown until first poll/command
            status: None,
            error: None,
        }
    }
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.host
        } else {
            &self.name
        }
    }
    /// Live power, or None when offline or not yet known (never stale-as-live).
    pub fn power_w(&self) -> Option<f64> {
        if !self.reachable {
            return None;
        }
        self.status
            .as_ref()
            .and_then(|s| s.energy.as_ref())
            .and_then(|e| e.power_w)
    }
    pub fn today_kwh(&self) -> Option<f64> {
        if !self.reachable {
            return None;
        }
        self.status
            .as_ref()
            .and_then(|s| s.energy.as_ref())
            .and_then(|e| e.today_kwh)
    }
    pub fn rssi(&self) -> Option<i64> {
        if !self.reachable {
            return None;
        }
        self.status.as_ref().and_then(|s| s.wifi_rssi)
    }
    pub fn is_online(&self) -> bool {
        self.reachable
    }
}

#[derive(Debug, Clone, Default)]
pub struct Fleet {
    pub devices: Vec<DeviceView>,
}

impl Fleet {
    pub fn from_config(devices: &[DeviceConfig]) -> Self {
        Fleet {
            devices: devices.iter().map(DeviceView::from_config).collect(),
        }
    }
    pub fn get(&self, id: &str) -> Option<&DeviceView> {
        self.devices.iter().find(|d| d.id == id)
    }
    pub fn get_mut(&mut self, id: &str) -> Option<&mut DeviceView> {
        self.devices.iter_mut().find(|d| d.id == id)
    }
}

#[cfg(test)]
mod tests {
    use tasmota_core::{DeviceStatus, Energy, NetInfo};

    use super::DeviceView;

    /// A fully-populated status: energy + RSSI present, as a real device would
    /// report right before going offline.
    fn sample_status() -> DeviceStatus {
        DeviceStatus {
            host: "192.0.2.20".into(),
            name: Some("Plug".into()),
            friendly_names: vec!["Plug".into()],
            module: Some(1),
            relays: Vec::new(),
            firmware: Some("14.2.0".into()),
            net: NetInfo::default(),
            uptime: Some("1T00:00:00".into()),
            wifi_rssi: Some(-50),
            energy: Some(Energy {
                power_w: Some(42.0),
                voltage_v: None,
                current_a: None,
                today_kwh: Some(1.5),
                yesterday_kwh: None,
                total_kwh: None,
            }),
            mqtt: None,
        }
    }

    fn view_with(reachable: bool) -> DeviceView {
        DeviceView {
            id: "d-test".into(),
            name: "Plug".into(),
            host: "192.0.2.20".into(),
            protected: false,
            reachable,
            status: Some(sample_status()),
            error: None,
        }
    }

    /// A stale status must never leak through as live telemetry while the device
    /// is marked offline: this is the last line of defense against "absent data
    /// rendered as a plausible value".
    #[test]
    fn offline_device_never_leaks_stale_status() {
        let dev = view_with(false);
        assert_eq!(dev.power_w(), None);
        assert_eq!(dev.today_kwh(), None);
        assert_eq!(dev.rssi(), None);
    }

    #[test]
    fn online_device_reports_populated_status() {
        let dev = view_with(true);
        assert_eq!(dev.power_w(), Some(42.0));
        assert_eq!(dev.today_kwh(), Some(1.5));
        assert_eq!(dev.rssi(), Some(-50));
    }

    #[test]
    fn device_id_is_injective_and_selector_safe() {
        // Distinct hosts that a naive slug would collapse together get distinct ids.
        assert_ne!(
            super::device_id("plug.local"),
            super::device_id("plug-local")
        );
        assert_ne!(
            super::device_id("192.0.2.10"),
            super::device_id("192-0-2-10")
        );
        // Stable for the same host.
        assert_eq!(
            super::device_id("192.0.2.10"),
            super::device_id("192.0.2.10")
        );
        // Selector-safe: `d-` prefix (never starts with a digit) and `[a-z0-9-]` only.
        let id = super::device_id("192.0.2.10");
        assert!(id.starts_with("d-"));
        assert!(
            id.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        );
    }
}
