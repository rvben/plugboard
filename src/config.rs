use std::net::SocketAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};

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
}

fn default_bind() -> SocketAddr {
    "127.0.0.1:8088".parse().unwrap()
}
fn default_interval() -> u64 {
    5
}

impl Default for Config {
    fn default() -> Self {
        Config {
            bind: default_bind(),
            poll_interval_secs: default_interval(),
            auth: AuthConfig::default(),
            devices: Vec::new(),
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
