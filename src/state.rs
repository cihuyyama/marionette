use crate::config::Config;
use crate::farm::FarmManager;
use crate::providers::grok_cli::GrokCliProvider;
use crate::providers::qoder::QoderProvider;
use crate::proxy::ProxyManager;
use crate::refresh_job::RefreshManager;
use crate::usage::UsageHandle;
use sqlx::SqlitePool;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub config: Arc<Config>,
    pub http: reqwest::Client,
    pub grok: Arc<GrokCliProvider>,
    pub qoder: Arc<QoderProvider>,
    pub farm: FarmManager,
    pub refresh: RefreshManager,
    pub proxies: ProxyManager,
    /// Sliding-window RPM limiter keyed by api_keys id (env master key never enters).
    pub rate_windows: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
    /// Latest resource-usage snapshot (marionette + automation process tree).
    pub usage: UsageHandle,
}

impl AppState {
    pub fn new(pool: SqlitePool, config: Config) -> Self {
        let farm = FarmManager::from_env(&config.db_path);
        let config = Arc::new(config);
        // Short idle timeout: releases idle TLS connection memory sooner.
        // The pool reopens connections on demand at negligible cost here.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(15))
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .tcp_nodelay(true)
            .build()
            .expect("http client");
        let grok = Arc::new(GrokCliProvider::new(config.clone()));
        let qoder = Arc::new(QoderProvider::new());
        let proxies = ProxyManager::new(pool.clone());
        Self {
            pool,
            config,
            http,
            grok,
            qoder,
            farm,
            refresh: RefreshManager::new(),
            proxies,
            rate_windows: Arc::new(Mutex::new(HashMap::new())),
            usage: crate::usage::new_handle(),
        }
    }
}
