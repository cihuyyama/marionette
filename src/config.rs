use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub db_path: PathBuf,
    pub api_key: String,
    pub admin_key: String,
    pub cors_origin: String,
    pub cooldown_hours: u64,
    pub grok_client_id: String,
    pub refresh_lead_secs: i64,
    pub static_dir: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Self {
        let _ = dotenvy::dotenv();
        let static_dir = env::var("MARIONETTE_STATIC_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                let p = PathBuf::from("web/dist");
                if p.exists() { Some(p) } else { None }
            });

        Self {
            host: env::var("MARIONETTE_HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: env::var("MARIONETTE_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1940),
            db_path: PathBuf::from(
                env::var("MARIONETTE_DB").unwrap_or_else(|_| "./data/marionette.sqlite".into()),
            ),
            api_key: env::var("MARIONETTE_API_KEY").unwrap_or_else(|_| "change-me".into()),
            admin_key: env::var("MARIONETTE_ADMIN_KEY")
                .unwrap_or_else(|_| "change-me-admin".into()),
            cors_origin: env::var("MARIONETTE_CORS_ORIGIN")
                .unwrap_or_else(|_| "http://localhost:1941".into()),
            cooldown_hours: env::var("MARIONETTE_COOLDOWN_HOURS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(25),
            grok_client_id: env::var("MARIONETTE_GROK_CLIENT_ID").unwrap_or_else(|_| {
                "b1a00492-073a-47ea-816f-4c329264a828".into()
            }),
            refresh_lead_secs: env::var("MARIONETTE_REFRESH_LEAD_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(300),
            static_dir,
        }
    }

    pub fn listen_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}
