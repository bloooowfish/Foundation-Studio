pub mod proxy;
pub mod sse;

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use reqwest::Client;
use rmp_bridge_relay::client::HousingSseEventMsg;
use tokio::sync::watch;

use crate::kawari::uri::{normalize_server_url, resolve_simulator_fallback};

pub struct HousingClientState {
    pub http: Client,
    pub sse_http: Client,
    sim_base_url: Arc<RwLock<String>>,
    sim_url_tx: watch::Sender<String>,
    rmp_sse_tx: Arc<Mutex<Option<std::sync::mpsc::Sender<HousingSseEventMsg>>>>,
}

impl HousingClientState {
    pub fn new(initial_server_url: &str) -> Self {
        let initial_sim = derive_simulator_base_from_server(initial_server_url);
        let (sim_url_tx, _rx) = watch::channel(initial_sim.clone());

        let http = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| Client::new());

        let sse_http = Client::builder()
            .connect_timeout(Duration::from_secs(4))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            http,
            sse_http,
            sim_base_url: Arc::new(RwLock::new(initial_sim)),
            sim_url_tx,
            rmp_sse_tx: Arc::new(Mutex::new(None)),
        }
    }

    pub fn simulator_base_url(&self) -> String {
        self.sim_base_url
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|_| "http://127.0.0.1:21064".to_string())
    }

    pub fn set_simulator_url(&self, raw_url: &str) -> String {
        let normalized = normalize_simulator_base(raw_url);
        let mut changed = false;
        if let Ok(mut guard) = self.sim_base_url.write()
            && *guard != normalized
        {
            *guard = normalized.clone();
            changed = true;
        }
        if changed {
            let _ = self.sim_url_tx.send(normalized.clone());
        }
        normalized
    }

    pub fn subscribe_url_watch(&self) -> watch::Receiver<String> {
        self.sim_url_tx.subscribe()
    }

    pub fn force_reconnect_sse(&self) {
        self.sim_url_tx.send_modify(|_| {});
    }

    pub fn set_rmp_sse_sender(&self, tx: Option<std::sync::mpsc::Sender<HousingSseEventMsg>>) {
        if let Ok(mut guard) = self.rmp_sse_tx.lock() {
            *guard = tx;
        }
    }

    pub fn forward_sse_to_rmp(&self, payload: &str) {
        let Ok(evt) = serde_json::from_str::<HousingSseEventMsg>(payload) else {
            return;
        };
        if let Ok(mut guard) = self.rmp_sse_tx.lock()
            && let Some(tx) = guard.as_ref()
            && tx.send(evt).is_err()
        {
            *guard = None;
        }
    }
}

pub fn normalize_simulator_base(raw_url: &str) -> String {
    let trimmed = raw_url.trim().trim_end_matches('/');
    let without_api = trimmed.strip_suffix("/api/v1/housing").unwrap_or(trimmed);
    if let Ok(url) = normalize_server_url(without_api) {
        url.as_str().trim_end_matches('/').to_string()
    } else if without_api.is_empty() {
        "http://127.0.0.1:21064".to_string()
    } else {
        without_api.to_string()
    }
}

pub fn derive_simulator_base_from_server(server_url: &str) -> String {
    normalize_server_url(server_url)
        .map(|u| {
            resolve_simulator_fallback(&u)
                .as_str()
                .trim_end_matches('/')
                .to_string()
        })
        .unwrap_or_else(|_| "http://127.0.0.1:21064".to_string())
}
