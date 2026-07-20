use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{RwLock, broadcast};

use switchkit::{DeviceCredentials, DeviceTarget, SmartDevice, Vendor};

use crate::auth::RateLimiter;
use crate::config::Config;
use crate::fleet::Fleet;
use crate::history::HistoryState;
use crate::metrics::MetricsState;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Inner>,
}

pub struct Inner {
    pub config: RwLock<Config>,
    pub config_path: PathBuf,
    pub fleet: RwLock<Fleet>,
    pub tx: broadcast::Sender<()>,
    /// Per-vendor async `switchkit::SmartDevice` client. One instance per
    /// vendor, shared (cloned `Arc`) across every request/poll task rather
    /// than opened per call. Held behind `dyn SmartDevice + Send + Sync` so
    /// `AppState::client` can hand a task-movable handle to a `JoinSet`
    /// without naming the concrete vendor type at every call site.
    pub tasmota: Arc<dyn SmartDevice + Send + Sync>,
    pub shelly: Arc<dyn SmartDevice + Send + Sync>,
    /// Per-IP login attempt counter for `POST /login` (Task 11). One instance
    /// for the process lifetime, shared across every request.
    pub rate_limiter: RateLimiter,
    /// Accumulating per-device poll-outcome counters for `/metrics`, keyed by
    /// device id so a fleet rebuild (settings change) never resets them; see
    /// `crate::metrics`.
    pub metrics: MetricsState,
    /// Recent measured-power samples per device + fleet total, one sample per
    /// poll tick; offline ticks are gaps, never zeros. See `crate::history`.
    pub history: HistoryState,
}

impl AppState {
    pub fn new(config: Config, config_path: PathBuf) -> Self {
        let fleet = Fleet::from_config(&config.devices);
        let (tx, _) = broadcast::channel(16);
        AppState {
            inner: Arc::new(Inner {
                tasmota: Arc::new(tasmota_core::HttpTransport::new(Duration::from_secs(5))),
                shelly: Arc::new(shelly_core::ShellyClient::default()),
                config: RwLock::new(config),
                config_path,
                fleet: RwLock::new(fleet),
                tx,
                rate_limiter: RateLimiter::default(),
                metrics: Mutex::new(HashMap::new()),
                history: HistoryState::default(),
            }),
        }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.inner.tx.subscribe()
    }
    pub fn notify(&self) {
        let _ = self.inner.tx.send(());
    }

    /// The async client for `vendor`, cloned (a cheap `Arc` bump) so it can
    /// move into a spawned task. `Vendor` is `#[non_exhaustive]`, so this
    /// matches known vendors explicitly and falls back to `None` for any
    /// future variant this app does not yet wire a client for - never a
    /// guessed/default client for an unrecognized vendor.
    pub fn client(&self, vendor: Vendor) -> Option<Arc<dyn SmartDevice + Send + Sync>> {
        match vendor {
            Vendor::Tasmota => Some(self.inner.tasmota.clone()),
            Vendor::Shelly => Some(self.inner.shelly.clone()),
            _ => None,
        }
    }

    /// Whether the app runs builtin (username/password) auth. The layout
    /// offers Sign out only then: in proxy mode the reverse proxy owns the
    /// session, so a local sign-out would do nothing but confuse.
    pub async fn builtin_auth(&self) -> bool {
        self.inner.config.read().await.auth.mode == crate::config::AuthMode::Builtin
    }

    /// Build a device target with credentials from the current config.
    pub async fn target_for(&self, host: &str) -> DeviceTarget {
        let cfg = self.inner.config.read().await;
        let creds = cfg
            .devices
            .iter()
            .find(|d| d.host == host)
            .and_then(|d| d.password.clone())
            .map(|p| DeviceCredentials {
                user: "admin".into(),
                password: p,
            });
        DeviceTarget::new(host.to_string()).with_credentials(creds)
    }

    /// Persist the current config off the async runtime (no blocking fs on a worker,
    /// no lock held across the write).
    pub async fn save_config(&self) -> anyhow::Result<()> {
        let cfg = self.inner.config.read().await.clone();
        let path = self.inner.config_path.clone();
        tokio::task::spawn_blocking(move || cfg.save(&path))
            .await
            .expect("blocking task")
    }
}
