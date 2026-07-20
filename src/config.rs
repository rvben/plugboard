use std::net::SocketAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};
use switchkit::Vendor;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
    #[serde(default = "default_interval")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub devices: Vec<DeviceConfig>,
    /// Whether `GET /metrics` (the Prometheus exporter) is served at all.
    /// Defaults to on; the route itself is unauthenticated (see
    /// `routes::router`), so set this false to disable it entirely rather
    /// than relying on a reverse proxy to hide it.
    #[serde(default = "default_true")]
    pub metrics_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    #[serde(default)]
    pub mode: AuthMode,
    pub username: Option<String>,
    pub password_hash: Option<String>,
    /// Set the `Secure` flag on the session cookie. Default true: works behind a TLS
    /// proxy AND on `http://localhost` (browsers treat localhost as a secure context).
    /// Set false ONLY for a trusted plain-http LAN deployment (documented as insecure).
    #[serde(default = "default_true")]
    pub cookie_secure: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        AuthConfig {
            mode: AuthMode::default(),
            username: None,
            password_hash: None,
            cookie_secure: true,
        }
    }
}

impl AuthConfig {
    /// Both a username and a password hash are configured, and neither is an
    /// empty string (an empty string counts the same as unset). `None` means
    /// `Builtin` login must fail closed: reject every attempt regardless of
    /// what is submitted, since there is nothing valid to compare against.
    pub fn configured_credentials(&self) -> Option<(&str, &str)> {
        let username = self.username.as_deref()?;
        let hash = self.password_hash.as_deref()?;
        if username.is_empty() || hash.is_empty() {
            return None;
        }
        Some((username, hash))
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    #[default]
    Proxy,
    Builtin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceConfig {
    pub name: String,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default)]
    pub protected: bool,
    /// Which vendor's client serves this device. Defaults to `Tasmota` so
    /// every config written before this field existed loads unchanged and
    /// keeps behaving exactly as it did (the fleet was Tasmota-only then).
    /// `switchkit::Vendor`'s own `#[serde(rename_all = "lowercase")]` already
    /// serializes/deserializes as `"tasmota"`/`"shelly"`, so no local
    /// wrapper is needed here.
    #[serde(default = "default_vendor")]
    pub vendor: Vendor,
}

fn default_bind() -> SocketAddr {
    "127.0.0.1:8088".parse().unwrap()
}
fn default_interval() -> u64 {
    5
}
fn default_vendor() -> Vendor {
    Vendor::Tasmota
}

impl Default for Config {
    fn default() -> Self {
        Config {
            bind: default_bind(),
            poll_interval_secs: default_interval(),
            auth: AuthConfig::default(),
            devices: Vec::new(),
            metrics_enabled: true,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(toml::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config written before `vendor` existed (no `vendor` key at all) must
    /// load as `Tasmota` - the fleet was Tasmota-only before this field was
    /// added, so an old config must keep behaving exactly as it did - and
    /// re-save with the field made explicit, never silently vanish it.
    #[test]
    fn device_config_without_vendor_loads_as_tasmota_and_resaves_explicit() {
        let toml_str = r#"
name = "Plug"
host = "192.0.2.10"
"#;
        let cfg: DeviceConfig = toml::from_str(toml_str).expect("parses without a vendor key");
        assert_eq!(cfg.vendor, Vendor::Tasmota);

        let resaved = toml::to_string(&cfg).expect("re-serializes");
        assert!(
            resaved.contains(r#"vendor = "tasmota""#),
            "resaved config must make the vendor explicit, got:\n{resaved}"
        );
    }

    /// A config with an explicit `vendor = "shelly"` loads as `Shelly`, never
    /// silently coerced to the Tasmota default.
    #[test]
    fn device_config_with_shelly_vendor_loads_as_shelly() {
        let toml_str = r#"
name = "Plug"
host = "192.0.2.11"
vendor = "shelly"
"#;
        let cfg: DeviceConfig = toml::from_str(toml_str).expect("parses with an explicit vendor");
        assert_eq!(cfg.vendor, Vendor::Shelly);
    }
}
