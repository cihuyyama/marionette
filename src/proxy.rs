use crate::db::{self, ProxyRow};
use crate::error::{AppError, AppResult};
use reqwest::Client;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::warn;

const CLIENT_TIMEOUT_SECS: u64 = 300;
const CLIENT_CONNECT_SECS: u64 = 30;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChatMode {
    Off,
    FollowAccount,
    Rotating,
}

impl ChatMode {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "follow-account" | "follow_account" | "sticky" => Self::FollowAccount,
            "rotating" | "rotate" => Self::Rotating,
            _ => Self::Off,
        }
    }
}

/// Parse one `host:port:user:pass` (or `host:port`, or `scheme://…`) line into
/// a ProxyRow. Returns None for blank/comment/malformed lines.
pub fn parse_proxy_line(raw: &str, source: Option<&str>) -> Option<ProxyRow> {
    let mut s = raw.trim();
    if s.is_empty() || s.starts_with('#') {
        return None;
    }
    if let Some((head, _)) = s.split_once(" #") {
        s = head.trim();
    }

    let (scheme, rest) = match s.split_once("://") {
        Some((sc, r)) => (sc.to_ascii_lowercase(), r),
        None => ("http".to_string(), s),
    };

    let (host, port, username, password) = if let Some((creds, hostport)) = rest.split_once('@') {
        let (host, port) = split_host_port(hostport)?;
        let (u, p) = match creds.split_once(':') {
            Some((u, p)) => (Some(u.to_string()), Some(p.to_string())),
            None => (Some(creds.to_string()), None),
        };
        (host, port, u, p)
    } else {
        let parts: Vec<&str> = rest.split(':').collect();
        match parts.as_slice() {
            [h, p] => (h.to_string(), p.parse().ok()?, None, None),
            [h, p, u, rest @ ..] => (
                h.to_string(),
                p.parse().ok()?,
                Some((*u).to_string()),
                Some(rest.join(":")),
            ),
            _ => return None,
        }
    };

    if host.is_empty() || port == 0 {
        return None;
    }

    let now = db::now_rfc3339();
    Some(ProxyRow {
        id: String::new(),
        scheme,
        host,
        port,
        username,
        password: password.filter(|p| !p.is_empty()),
        label: None,
        country: None,
        is_active: 1,
        health: "unknown".to_string(),
        latency_ms: None,
        last_check_at: None,
        last_error: None,
        source: source.map(|s| s.to_string()),
        created_at: now.clone(),
        updated_at: now,
    })
}

fn split_host_port(s: &str) -> Option<(String, i64)> {
    let (h, p) = s.rsplit_once(':')?;
    Some((h.to_string(), p.parse().ok()?))
}

fn build_client(proxy: Option<&ProxyRow>) -> AppResult<Client> {
    let mut builder = Client::builder()
        .timeout(Duration::from_secs(CLIENT_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(CLIENT_CONNECT_SECS))
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(60))
        .tcp_nodelay(true);
    if let Some(p) = proxy {
        let rp = reqwest::Proxy::all(p.url())
            .map_err(|e| AppError::Internal(format!("bad proxy {}: {e}", p.host)))?;
        builder = builder.proxy(rp);
    }
    builder
        .build()
        .map_err(|e| AppError::Internal(format!("proxy client build: {e}")))
}

/// Owns one reqwest client per proxy id (plus a direct client), so provider
/// chat calls can route by the account's bound proxy without rebuilding a
/// client per request.
#[derive(Clone)]
pub struct ProxyManager {
    pool: SqlitePool,
    direct: Client,
    clients: Arc<RwLock<HashMap<String, Client>>>,
}

impl ProxyManager {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            direct: build_client(None).expect("direct client"),
            clients: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn direct(&self) -> Client {
        self.direct.clone()
    }

    async fn client_for_proxy(&self, proxy: &ProxyRow) -> Client {
        if let Some(c) = self.clients.read().await.get(&proxy.id) {
            return c.clone();
        }
        match build_client(Some(proxy)) {
            Ok(c) => {
                self.clients
                    .write()
                    .await
                    .insert(proxy.id.clone(), c.clone());
                c
            }
            Err(e) => {
                warn!(proxy = %proxy.host, error = %e, "proxy client build failed; using direct");
                self.direct.clone()
            }
        }
    }

    /// Resolve the HTTP client for an account's chat call, honoring chat mode,
    /// account binding, proxy health, and the on-dead fallback policy.
    pub async fn client_for_account(&self, account_id: &str) -> Client {
        let settings = match db::get_proxy_settings(&self.pool).await {
            Ok(s) => s,
            Err(_) => return self.direct.clone(),
        };
        let mode = ChatMode::parse(&settings.chat_mode);
        if mode == ChatMode::Off {
            return self.direct.clone();
        }

        let proxy = match mode {
            ChatMode::FollowAccount => self.bound_proxy(account_id).await,
            ChatMode::Rotating => self.least_loaded_healthy().await,
            ChatMode::Off => None,
        };

        match proxy {
            Some(p) if p.is_active != 0 && p.health != "dead" => self.client_for_proxy(&p).await,
            Some(_) | None => self.on_dead_client(&settings.on_dead, account_id).await,
        }
    }

    async fn bound_proxy(&self, account_id: &str) -> Option<ProxyRow> {
        let id = db::get_account_proxy(&self.pool, account_id).await.ok()??;
        db::get_proxy(&self.pool, &id).await.ok()
    }

    async fn on_dead_client(&self, on_dead: &str, account_id: &str) -> Client {
        match on_dead {
            "reassign" => match self.least_loaded_healthy().await {
                Some(p) => {
                    let _ = db::set_account_proxy(&self.pool, account_id, &p.id).await;
                    self.client_for_proxy(&p).await
                }
                None => self.direct.clone(),
            },
            "fail" | "direct" | _ => self.direct.clone(),
        }
    }

    pub async fn least_loaded_healthy(&self) -> Option<ProxyRow> {
        let proxies = db::list_active_proxies(&self.pool).await.ok()?;
        let healthy: Vec<ProxyRow> = proxies
            .into_iter()
            .filter(|p| p.health != "dead")
            .collect();
        if healthy.is_empty() {
            return None;
        }
        let counts: HashMap<String, i64> = db::proxy_assignment_counts(&self.pool)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
        healthy
            .into_iter()
            .min_by_key(|p| counts.get(&p.id).copied().unwrap_or(0))
    }

    /// Assign every account of `provider` to a proxy using least-loaded
    /// balancing (sticky: existing bindings are kept). Returns count assigned.
    pub async fn assign_provider(&self, provider: &str) -> AppResult<u32> {
        let proxies = db::list_active_proxies(&self.pool).await?;
        if proxies.is_empty() {
            return Err(AppError::BadRequest("no active proxies to assign".into()));
        }
        let accounts = db::list_accounts(&self.pool, Some(provider), None).await?;
        let mut counts: HashMap<String, i64> = db::proxy_assignment_counts(&self.pool)
            .await?
            .into_iter()
            .collect();
        let existing: std::collections::HashSet<String> = db::list_account_proxy_map(&self.pool)
            .await?
            .into_iter()
            .map(|(a, _)| a)
            .collect();

        let mut assigned = 0u32;
        for acc in accounts {
            if existing.contains(&acc.id) {
                continue;
            }
            let pick = proxies
                .iter()
                .min_by_key(|p| counts.get(&p.id).copied().unwrap_or(0))
                .cloned();
            if let Some(p) = pick {
                db::set_account_proxy(&self.pool, &acc.id, &p.id).await?;
                *counts.entry(p.id).or_insert(0) += 1;
                assigned += 1;
            }
        }
        Ok(assigned)
    }

    pub fn invalidate_cache_blocking(&self) {
        if let Ok(mut w) = self.clients.try_write() {
            w.clear();
        }
    }
}

const HEALTH_CHECK_URL: &str = "https://api.ipify.org?format=json";
const HEALTH_TIMEOUT_SECS: u64 = 15;

/// Probe a single proxy by making a real request through it. Returns
/// (healthy, latency_ms, error). Used by the manual and background checks.
pub async fn check_proxy(proxy: &ProxyRow) -> (bool, Option<i64>, Option<String>) {
    let client = match Client::builder()
        .timeout(Duration::from_secs(HEALTH_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(HEALTH_TIMEOUT_SECS))
        .proxy(match reqwest::Proxy::all(proxy.url()) {
            Ok(p) => p,
            Err(e) => return (false, None, Some(format!("bad url: {e}"))),
        })
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, None, Some(e.to_string())),
    };
    let start = std::time::Instant::now();
    match client.get(HEALTH_CHECK_URL).send().await {
        Ok(resp) if resp.status().is_success() => {
            (true, Some(start.elapsed().as_millis() as i64), None)
        }
        Ok(resp) => (false, None, Some(format!("http {}", resp.status().as_u16()))),
        Err(e) => (false, None, Some(e.to_string().chars().take(200).collect())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_port_user_pass() {
        let p = parse_proxy_line("82.21.231.243:7557:enowtampan:enowhtampan", Some("f")).unwrap();
        assert_eq!(p.scheme, "http");
        assert_eq!(p.host, "82.21.231.243");
        assert_eq!(p.port, 7557);
        assert_eq!(p.username.as_deref(), Some("enowtampan"));
        assert_eq!(p.password.as_deref(), Some("enowhtampan"));
        assert_eq!(p.source.as_deref(), Some("f"));
    }

    #[test]
    fn parses_host_port_only() {
        let p = parse_proxy_line("1.2.3.4:8080", None).unwrap();
        assert_eq!(p.host, "1.2.3.4");
        assert_eq!(p.port, 8080);
        assert!(p.username.is_none());
    }

    #[test]
    fn parses_scheme_url_with_auth() {
        let p = parse_proxy_line("socks5://user:pw@9.9.9.9:1080", None).unwrap();
        assert_eq!(p.scheme, "socks5");
        assert_eq!(p.host, "9.9.9.9");
        assert_eq!(p.port, 1080);
        assert_eq!(p.username.as_deref(), Some("user"));
        assert_eq!(p.password.as_deref(), Some("pw"));
    }

    #[test]
    fn password_with_colon_is_preserved() {
        let p = parse_proxy_line("h:1:u:pa:ss:word", None).unwrap();
        assert_eq!(p.password.as_deref(), Some("pa:ss:word"));
    }

    #[test]
    fn skips_blank_and_comments() {
        assert!(parse_proxy_line("", None).is_none());
        assert!(parse_proxy_line("   ", None).is_none());
        assert!(parse_proxy_line("# comment", None).is_none());
    }

    #[test]
    fn url_encodes_userinfo() {
        let p = parse_proxy_line("h:1:us er:p ss", None).unwrap();
        let url = p.url();
        assert!(url.contains("us%20er"), "url={url}");
        assert!(url.contains("p%20ss"), "url={url}");
        assert!(url.starts_with("http://"));
    }

    #[test]
    fn chat_mode_parse() {
        assert!(matches!(ChatMode::parse("follow-account"), ChatMode::FollowAccount));
        assert!(matches!(ChatMode::parse("sticky"), ChatMode::FollowAccount));
        assert!(matches!(ChatMode::parse("rotating"), ChatMode::Rotating));
        assert!(matches!(ChatMode::parse("off"), ChatMode::Off));
        assert!(matches!(ChatMode::parse("nonsense"), ChatMode::Off));
    }
}
