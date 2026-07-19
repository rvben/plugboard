use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{RwLock, broadcast};

use tasmota_core::{Credentials, DeviceAddr, HttpTransport};

use crate::config::Config;
use crate::fleet::Fleet;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Inner>,
}

pub struct Inner {
    pub config: RwLock<Config>,
    pub config_path: PathBuf,
    pub fleet: RwLock<Fleet>,
    pub tx: broadcast::Sender<()>,
    pub transport: HttpTransport,
}

impl AppState {
    pub fn new(config: Config, config_path: PathBuf) -> Self {
        let fleet = Fleet::from_config(&config.devices);
        let (tx, _) = broadcast::channel(16);
        AppState {
            inner: Arc::new(Inner {
                transport: HttpTransport::new(Duration::from_secs(5)),
                config: RwLock::new(config),
                config_path,
                fleet: RwLock::new(fleet),
                tx,
            }),
        }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.inner.tx.subscribe()
    }
    pub fn notify(&self) {
        let _ = self.inner.tx.send(());
    }
    /// Build a device address with credentials from the current config.
    pub async fn addr_for(&self, host: &str) -> DeviceAddr {
        let cfg = self.inner.config.read().await;
        let creds = cfg
            .devices
            .iter()
            .find(|d| d.host == host)
            .and_then(|d| d.password.clone())
            .map(|p| Credentials {
                user: "admin".into(),
                password: p,
            });
        DeviceAddr::new(host.to_string()).with_credentials(creds)
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
