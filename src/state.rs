use crate::config::Config;
use crate::providers::grok_cli::GrokCliProvider;
use crate::providers::qoder::QoderProvider;
use sqlx::SqlitePool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub config: Arc<Config>,
    pub http: reqwest::Client,
    pub grok: Arc<GrokCliProvider>,
    pub qoder: Arc<QoderProvider>,
}

impl AppState {
    pub fn new(pool: SqlitePool, config: Config) -> Self {
        let config = Arc::new(config);
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("http client");
        let grok = Arc::new(GrokCliProvider::new(config.clone()));
        let qoder = Arc::new(QoderProvider::new(http.clone()));
        Self {
            pool,
            config,
            http,
            grok,
            qoder,
        }
    }
}
