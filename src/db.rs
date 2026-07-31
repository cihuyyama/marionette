use crate::error::{AppError, AppResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::path::Path;
use uuid::Uuid;

pub const GROK_TOKEN_QUOTA: i64 = 1_000_000;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Account {
    pub id: String,
    pub provider: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub is_active: i64,
    pub priority: i64,
    pub data: String,
    pub cooldown_until: Option<String>,
    pub last_error: Option<String>,
    pub last_used_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub quota_limit: i64,
    pub quota_remaining: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountPublic {
    pub id: String,
    pub provider: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub is_active: bool,
    pub priority: i64,
    pub data: Value,
    pub cooldown_until: Option<String>,
    pub last_error: Option<String>,
    pub last_used_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub status: String,
    pub quota_limit: i64,
    pub quota_remaining: i64,
    pub quota_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ApiKeyRow {
    pub id: String,
    pub key_hash: String,
    pub name: Option<String>,
    pub is_active: i64,
    pub created_at: String,
}

pub async fn connect(db_path: &Path) -> AppResult<SqlitePool> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let opts = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        // WAL + NORMAL is crash-safe yet skips FULL's fsync-per-commit.
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        .busy_timeout(std::time::Duration::from_secs(5))
        // Negative cache_size = KiB, so -65536 = 64 MiB page cache.
        .pragma("cache_size", "-65536")
        .pragma("temp_store", "MEMORY");
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;
    migrate(&pool).await?;
    Ok(pool)
}

const ACCOUNT_COLUMNS: &str = r#"
    id, provider, email, name, is_active, priority, data,
    cooldown_until, last_error, last_used_at, created_at, updated_at,
    quota_limit, quota_remaining
"#;

async fn migrate(pool: &SqlitePool) -> AppResult<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS accounts (
          id TEXT PRIMARY KEY,
          provider TEXT NOT NULL,
          email TEXT,
          name TEXT,
          is_active INTEGER NOT NULL DEFAULT 1,
          priority INTEGER NOT NULL DEFAULT 0,
          data TEXT NOT NULL,
          cooldown_until TEXT,
          last_error TEXT,
          last_used_at TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          quota_limit INTEGER NOT NULL DEFAULT 0,
          quota_remaining INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_accounts_provider_active
          ON accounts(provider, is_active);
        CREATE TABLE IF NOT EXISTS api_keys (
          id TEXT PRIMARY KEY,
          key_hash TEXT NOT NULL,
          name TEXT,
          is_active INTEGER NOT NULL DEFAULT 1,
          created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS request_logs (
          id TEXT PRIMARY KEY,
          created_at TEXT NOT NULL,
          provider TEXT NOT NULL,
          model TEXT,
          status TEXT NOT NULL,
          stream INTEGER NOT NULL DEFAULT 0,
          duration_ms INTEGER,
          prompt_tokens INTEGER,
          completion_tokens INTEGER,
          total_tokens INTEGER,
          credits_used INTEGER,
          account_quota_before INTEGER,
          account_quota_after INTEGER,
          account_id TEXT,
          account_email TEXT,
          error_message TEXT,
          request_body TEXT,
          response_body TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_request_logs_created
          ON request_logs(created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_request_logs_provider
          ON request_logs(provider, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_request_logs_account
          ON request_logs(account_id, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_api_keys_key_hash
          ON api_keys(key_hash);
        CREATE TABLE IF NOT EXISTS provider_settings (
          provider TEXT PRIMARY KEY,
          load_balance TEXT NOT NULL DEFAULT 'round_robin',
          sticky_account_id TEXT,
          rr_cursor TEXT,
          pick_mode TEXT NOT NULL DEFAULT 'normal',
          updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS proxies (
          id TEXT PRIMARY KEY,
          scheme TEXT NOT NULL DEFAULT 'http',
          host TEXT NOT NULL,
          port INTEGER NOT NULL,
          username TEXT,
          password TEXT,
          label TEXT,
          country TEXT,
          is_active INTEGER NOT NULL DEFAULT 1,
          health TEXT NOT NULL DEFAULT 'unknown',
          latency_ms INTEGER,
          last_check_at TEXT,
          last_error TEXT,
          source TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_proxies_hostport
          ON proxies(host, port, username);
        CREATE INDEX IF NOT EXISTS idx_proxies_active_health
          ON proxies(is_active, health);
        CREATE TABLE IF NOT EXISTS account_proxy (
          account_id TEXT PRIMARY KEY,
          proxy_id TEXT NOT NULL,
          assigned_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_account_proxy_proxy
          ON account_proxy(proxy_id);
        CREATE TABLE IF NOT EXISTS proxy_settings (
          id INTEGER PRIMARY KEY CHECK (id = 1),
          chat_mode TEXT NOT NULL DEFAULT 'off',
          automation_mode TEXT NOT NULL DEFAULT 'sticky',
          on_dead TEXT NOT NULL DEFAULT 'direct',
          updated_at TEXT NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;
    migrate_quota_columns(pool).await?;
    migrate_provider_settings_columns(pool).await?;
    migrate_request_log_body_columns(pool).await?;
    Ok(())
}

async fn migrate_request_log_body_columns(pool: &SqlitePool) -> AppResult<()> {
    let alters = [
        "ALTER TABLE request_logs ADD COLUMN request_body TEXT",
        "ALTER TABLE request_logs ADD COLUMN response_body TEXT",
    ];
    for sql in alters {
        if let Err(e) = sqlx::query(sql).execute(pool).await {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(e.into());
            }
        }
    }
    Ok(())
}

async fn migrate_quota_columns(pool: &SqlitePool) -> AppResult<()> {
    let alters = [
        "ALTER TABLE accounts ADD COLUMN quota_limit INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE accounts ADD COLUMN quota_remaining INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE request_logs ADD COLUMN credits_used INTEGER",
        "ALTER TABLE request_logs ADD COLUMN account_quota_before INTEGER",
        "ALTER TABLE request_logs ADD COLUMN account_quota_after INTEGER",
    ];
    for sql in alters {
        if let Err(e) = sqlx::query(sql).execute(pool).await {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(e.into());
            }
        }
    }
    sqlx::query(
        r#"
        UPDATE accounts
        SET quota_limit = ?, quota_remaining = ?
        WHERE provider = 'grok-cli' AND quota_limit = 0 AND quota_remaining = 0
        "#,
    )
    .bind(GROK_TOKEN_QUOTA)
    .bind(GROK_TOKEN_QUOTA)
    .execute(pool)
    .await?;
    Ok(())
}

async fn migrate_provider_settings_columns(pool: &SqlitePool) -> AppResult<()> {
    if let Err(e) = sqlx::query(
        "ALTER TABLE provider_settings ADD COLUMN pick_mode TEXT NOT NULL DEFAULT 'normal'",
    )
    .execute(pool)
    .await
    {
        let msg = e.to_string();
        if !msg.contains("duplicate column") {
            return Err(e.into());
        }
    }
    Ok(())
}

pub fn default_quota_for_provider(provider: &str) -> (i64, i64) {
    match provider {
        "grok-cli" => (GROK_TOKEN_QUOTA, GROK_TOKEN_QUOTA),
        "qoder" => (0, 0),
        _ => (0, 0),
    }
}

pub fn quota_kind_for_provider(provider: &str) -> &'static str {
    match provider {
        "grok-cli" => "tokens",
        "qoder" => "credits",
        _ => "none",
    }
}

pub fn is_qoder_free_tier_model(model: &str) -> bool {
    let m = model.rsplit('/').next().unwrap_or(model).trim();
    m.eq_ignore_ascii_case("lite")
}

pub fn quota_blocks_pick(account: &Account, model: Option<&str>) -> bool {
    if !account.has_quota_budget() {
        return false;
    }
    if account.quota_remaining > 0 {
        return false;
    }
    if account.provider == "qoder" {
        if let Some(m) = model {
            if is_qoder_free_tier_model(m) {
                return false;
            }
            if crate::providers::qoder::model_has_free_activity_path(account, m) {
                return false;
            }
        }
    }
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadBalance {
    RoundRobin,
    Sequential,
    LeastUsed,
    Priority,
    Random,
}

impl LoadBalance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RoundRobin => "round_robin",
            Self::Sequential => "sequential",
            Self::LeastUsed => "least_used",
            Self::Priority => "priority",
            Self::Random => "random",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "round_robin" | "round-robin" | "rr" => Some(Self::RoundRobin),
            "sequential" | "sticky" | "stick" => Some(Self::Sequential),
            "least_used" | "least-used" | "lru" => Some(Self::LeastUsed),
            "priority" => Some(Self::Priority),
            "random" => Some(Self::Random),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::RoundRobin => "Round robin",
            Self::Sequential => "Sequential",
            Self::LeastUsed => "Least used",
            Self::Priority => "Priority",
            Self::Random => "Random",
        }
    }
}

impl Default for LoadBalance {
    fn default() -> Self {
        Self::RoundRobin
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QoderPickMode {
    Normal,
    UltimateFree,
}

impl QoderPickMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::UltimateFree => "ultimate_free",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "normal" | "default" | "credits" | "" => Some(Self::Normal),
            "ultimate_free" | "ultimate-free" | "ultimate_free_only" | "free_ultimate" => {
                Some(Self::UltimateFree)
            }
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal (credits + free)",
            Self::UltimateFree => "Ultimate free only",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Normal => {
                "Use plan credits and Free Ultimate when available (default)"
            }
            Self::UltimateFree => {
                "Only rotate accounts with Free Ultimate left — never spend Pro Trial credits on Ultimate"
            }
        }
    }
}

impl Default for QoderPickMode {
    fn default() -> Self {
        Self::Normal
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProviderSettingsRow {
    pub provider: String,
    pub load_balance: String,
    pub sticky_account_id: Option<String>,
    pub rr_cursor: Option<String>,
    pub pick_mode: String,
    pub updated_at: String,
}

pub async fn get_provider_settings(
    pool: &SqlitePool,
    provider: &str,
) -> AppResult<ProviderSettingsRow> {
    if let Some(row) = sqlx::query_as::<_, ProviderSettingsRow>(
        "SELECT * FROM provider_settings WHERE provider = ?",
    )
    .bind(provider)
    .fetch_optional(pool)
    .await?
    {
        return Ok(row);
    }
    let now = now_rfc3339();
    let row = ProviderSettingsRow {
        provider: provider.into(),
        load_balance: LoadBalance::default().as_str().into(),
        sticky_account_id: None,
        rr_cursor: None,
        pick_mode: QoderPickMode::default().as_str().into(),
        updated_at: now.clone(),
    };
    sqlx::query(
        r#"
        INSERT INTO provider_settings (provider, load_balance, sticky_account_id, rr_cursor, pick_mode, updated_at)
        VALUES (?, ?, NULL, NULL, ?, ?)
        "#,
    )
    .bind(&row.provider)
    .bind(&row.load_balance)
    .bind(&row.pick_mode)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(row)
}

pub async fn list_provider_settings(pool: &SqlitePool) -> AppResult<Vec<ProviderSettingsRow>> {
    let providers = ["grok-cli", "qoder"];
    let mut out = Vec::with_capacity(providers.len());
    for p in providers {
        out.push(get_provider_settings(pool, p).await?);
    }
    Ok(out)
}

pub async fn set_provider_load_balance(
    pool: &SqlitePool,
    provider: &str,
    strategy: LoadBalance,
) -> AppResult<ProviderSettingsRow> {
    let _ = get_provider_settings(pool, provider).await?;
    let now = now_rfc3339();
    sqlx::query(
        r#"
        UPDATE provider_settings
        SET load_balance = ?, sticky_account_id = NULL, rr_cursor = NULL, updated_at = ?
        WHERE provider = ?
        "#,
    )
    .bind(strategy.as_str())
    .bind(&now)
    .bind(provider)
    .execute(pool)
    .await?;
    get_provider_settings(pool, provider).await
}

pub async fn set_provider_pick_mode(
    pool: &SqlitePool,
    provider: &str,
    mode: QoderPickMode,
) -> AppResult<ProviderSettingsRow> {
    if provider != "qoder" {
        return Err(AppError::BadRequest(
            "pick_mode is only supported for provider qoder".into(),
        ));
    }
    let _ = get_provider_settings(pool, provider).await?;
    let now = now_rfc3339();
    sqlx::query(
        r#"
        UPDATE provider_settings
        SET pick_mode = ?, updated_at = ?
        WHERE provider = ?
        "#,
    )
    .bind(mode.as_str())
    .bind(&now)
    .bind(provider)
    .execute(pool)
    .await?;
    get_provider_settings(pool, provider).await
}

async fn set_rr_cursor(pool: &SqlitePool, provider: &str, account_id: &str) -> AppResult<()> {
    let now = now_rfc3339();
    sqlx::query(
        "UPDATE provider_settings SET rr_cursor = ?, updated_at = ? WHERE provider = ?",
    )
    .bind(account_id)
    .bind(&now)
    .bind(provider)
    .execute(pool)
    .await?;
    Ok(())
}

async fn set_sticky_account(
    pool: &SqlitePool,
    provider: &str,
    account_id: Option<&str>,
) -> AppResult<()> {
    let now = now_rfc3339();
    sqlx::query(
        "UPDATE provider_settings SET sticky_account_id = ?, updated_at = ? WHERE provider = ?",
    )
    .bind(account_id)
    .bind(&now)
    .bind(provider)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn note_pick_success(
    pool: &SqlitePool,
    provider: &str,
    strategy: LoadBalance,
    account_id: &str,
) -> AppResult<()> {
    match strategy {
        LoadBalance::RoundRobin => set_rr_cursor(pool, provider, account_id).await,
        LoadBalance::Sequential => set_sticky_account(pool, provider, Some(account_id)).await,
        _ => Ok(()),
    }
}

pub async fn note_pick_failure(
    pool: &SqlitePool,
    provider: &str,
    strategy: LoadBalance,
    account_id: &str,
) -> AppResult<()> {
    if strategy == LoadBalance::Sequential {
        let settings = get_provider_settings(pool, provider).await?;
        if settings.sticky_account_id.as_deref() == Some(account_id) {
            set_sticky_account(pool, provider, None).await?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RequestLog {
    pub id: String,
    pub created_at: String,
    pub provider: String,
    pub model: Option<String>,
    pub status: String,
    pub stream: i64,
    pub duration_ms: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub credits_used: Option<i64>,
    pub account_quota_before: Option<i64>,
    pub account_quota_after: Option<i64>,
    pub account_id: Option<String>,
    pub account_email: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RequestLogDetail {
    pub id: String,
    pub created_at: String,
    pub provider: String,
    pub model: Option<String>,
    pub status: String,
    pub stream: i64,
    pub duration_ms: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub credits_used: Option<i64>,
    pub account_quota_before: Option<i64>,
    pub account_quota_after: Option<i64>,
    pub account_id: Option<String>,
    pub account_email: Option<String>,
    pub error_message: Option<String>,
    pub request_body: Option<String>,
    pub response_body: Option<String>,
}

const REQUEST_LOG_LIGHT_COLUMNS: &str = r#"
    id, created_at, provider, model, status, stream, duration_ms,
    prompt_tokens, completion_tokens, total_tokens,
    credits_used, account_quota_before, account_quota_after,
    account_id, account_email, error_message
"#;

#[derive(Debug, Clone)]
pub struct NewRequestLog {
    pub provider: String,
    pub model: Option<String>,
    pub status: String,
    pub stream: bool,
    pub duration_ms: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub credits_used: Option<i64>,
    pub account_quota_before: Option<i64>,
    pub account_quota_after: Option<i64>,
    pub account_id: Option<String>,
    pub account_email: Option<String>,
    pub error_message: Option<String>,
    pub request_body: Option<String>,
    pub response_body: Option<String>,
}

pub async fn insert_request_log(pool: &SqlitePool, log: NewRequestLog) -> AppResult<String> {
    let id = Uuid::new_v4().to_string();
    let created_at = now_rfc3339();
    sqlx::query(
        r#"
        INSERT INTO request_logs (
          id, created_at, provider, model, status, stream, duration_ms,
          prompt_tokens, completion_tokens, total_tokens,
          credits_used, account_quota_before, account_quota_after,
          account_id, account_email, error_message,
          request_body, response_body
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&id)
    .bind(&created_at)
    .bind(&log.provider)
    .bind(&log.model)
    .bind(&log.status)
    .bind(if log.stream { 1 } else { 0 })
    .bind(log.duration_ms)
    .bind(log.prompt_tokens)
    .bind(log.completion_tokens)
    .bind(log.total_tokens)
    .bind(log.credits_used)
    .bind(log.account_quota_before)
    .bind(log.account_quota_after)
    .bind(&log.account_id)
    .bind(&log.account_email)
    .bind(&log.error_message)
    .bind(&log.request_body)
    .bind(&log.response_body)
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn get_request_log(pool: &SqlitePool, id: &str) -> AppResult<RequestLogDetail> {
    sqlx::query_as::<_, RequestLogDetail>(
        r#"
        SELECT
          id, created_at, provider, model, status, stream, duration_ms,
          prompt_tokens, completion_tokens, total_tokens,
          credits_used, account_quota_before, account_quota_after,
          account_id, account_email, error_message,
          request_body, response_body
        FROM request_logs
        WHERE id = ?
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("request log {id}")))
}

pub async fn update_request_log_usage(
    pool: &SqlitePool,
    id: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    credits_used: Option<i64>,
    account_quota_before: Option<i64>,
    account_quota_after: Option<i64>,
    duration_ms: Option<i64>,
) -> AppResult<()> {
    sqlx::query(
        r#"
        UPDATE request_logs SET
          prompt_tokens = ?,
          completion_tokens = ?,
          total_tokens = ?,
          credits_used = COALESCE(?, credits_used),
          account_quota_before = COALESCE(?, account_quota_before),
          account_quota_after = COALESCE(?, account_quota_after),
          duration_ms = COALESCE(?, duration_ms)
        WHERE id = ?
        "#,
    )
    .bind(prompt_tokens)
    .bind(completion_tokens)
    .bind(total_tokens)
    .bind(credits_used)
    .bind(account_quota_before)
    .bind(account_quota_after)
    .bind(duration_ms)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_request_logs(
    pool: &SqlitePool,
    provider: Option<&str>,
    limit: i64,
    since: Option<&str>,
) -> AppResult<Vec<RequestLog>> {
    let limit = limit.clamp(1, 500);
    match (provider, since) {
        (Some(p), Some(s)) => {
            let sql = format!(
                r#"
                SELECT {REQUEST_LOG_LIGHT_COLUMNS} FROM request_logs
                WHERE provider = ? AND created_at >= ?
                ORDER BY created_at DESC
                LIMIT ?
                "#
            );
            let rows = sqlx::query_as::<_, RequestLog>(&sql)
                .bind(p)
                .bind(s)
                .bind(limit)
                .fetch_all(pool)
                .await?;
            Ok(rows)
        }
        (Some(p), None) => {
            let sql = format!(
                r#"
                SELECT {REQUEST_LOG_LIGHT_COLUMNS} FROM request_logs
                WHERE provider = ?
                ORDER BY created_at DESC
                LIMIT ?
                "#
            );
            let rows = sqlx::query_as::<_, RequestLog>(&sql)
                .bind(p)
                .bind(limit)
                .fetch_all(pool)
                .await?;
            Ok(rows)
        }
        (None, Some(s)) => {
            let sql = format!(
                r#"
                SELECT {REQUEST_LOG_LIGHT_COLUMNS} FROM request_logs
                WHERE created_at >= ?
                ORDER BY created_at DESC
                LIMIT ?
                "#
            );
            let rows = sqlx::query_as::<_, RequestLog>(&sql)
                .bind(s)
                .bind(limit)
                .fetch_all(pool)
                .await?;
            Ok(rows)
        }
        (None, None) => {
            let sql = format!(
                r#"
                SELECT {REQUEST_LOG_LIGHT_COLUMNS} FROM request_logs
                ORDER BY created_at DESC
                LIMIT ?
                "#
            );
            let rows = sqlx::query_as::<_, RequestLog>(&sql)
                .bind(limit)
                .fetch_all(pool)
                .await?;
            Ok(rows)
        }
    }
}

pub async fn usage_summary(
    pool: &SqlitePool,
    since: Option<&str>,
) -> AppResult<serde_json::Value> {
    let totals = if let Some(s) = since {
        sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64)>(
            r#"
            SELECT
              COUNT(*) as requests,
              COALESCE(SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END), 0) as success,
              COALESCE(SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END), 0) as errors,
              COALESCE(SUM(prompt_tokens), 0) as prompt_tokens,
              COALESCE(SUM(completion_tokens), 0) as completion_tokens,
              COALESCE(SUM(total_tokens), 0) as total_tokens
            FROM request_logs
            WHERE created_at >= ?
            "#,
        )
        .bind(s)
        .fetch_one(pool)
        .await?
    } else {
        sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64)>(
            r#"
            SELECT
              COUNT(*) as requests,
              COALESCE(SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END), 0) as success,
              COALESCE(SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END), 0) as errors,
              COALESCE(SUM(prompt_tokens), 0) as prompt_tokens,
              COALESCE(SUM(completion_tokens), 0) as completion_tokens,
              COALESCE(SUM(total_tokens), 0) as total_tokens
            FROM request_logs
            "#,
        )
        .fetch_one(pool)
        .await?
    };

    let by_model = if let Some(s) = since {
        sqlx::query_as::<_, (Option<String>, String, i64, i64, i64, i64)>(
            r#"
            SELECT
              model,
              provider,
              COUNT(*) as requests,
              COALESCE(SUM(prompt_tokens), 0) as prompt_tokens,
              COALESCE(SUM(completion_tokens), 0) as completion_tokens,
              COALESCE(SUM(total_tokens), 0) as total_tokens
            FROM request_logs
            WHERE created_at >= ?
            GROUP BY model, provider
            ORDER BY total_tokens DESC, requests DESC
            LIMIT 50
            "#,
        )
        .bind(s)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, (Option<String>, String, i64, i64, i64, i64)>(
            r#"
            SELECT
              model,
              provider,
              COUNT(*) as requests,
              COALESCE(SUM(prompt_tokens), 0) as prompt_tokens,
              COALESCE(SUM(completion_tokens), 0) as completion_tokens,
              COALESCE(SUM(total_tokens), 0) as total_tokens
            FROM request_logs
            GROUP BY model, provider
            ORDER BY total_tokens DESC, requests DESC
            LIMIT 50
            "#,
        )
        .fetch_all(pool)
        .await?
    };

    let models: Vec<serde_json::Value> = by_model
        .into_iter()
        .map(|(model, provider, requests, prompt, completion, total)| {
            serde_json::json!({
                "model": model.unwrap_or_else(|| "unknown".into()),
                "provider": provider,
                "requests": requests,
                "prompt_tokens": prompt,
                "completion_tokens": completion,
                "total_tokens": total,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "requests": totals.0,
        "success": totals.1,
        "errors": totals.2,
        "prompt_tokens": totals.3,
        "completion_tokens": totals.4,
        "total_tokens": totals.5,
        "by_model": models,
        "since": since,
    }))
}

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

impl Account {
    pub fn data_json(&self) -> Value {
        serde_json::from_str(&self.data).unwrap_or_else(|_| Value::Object(Default::default()))
    }

    pub fn set_data_json(&mut self, v: &Value) {
        self.data = v.to_string();
    }

    pub fn is_cooling(&self) -> bool {
        match &self.cooldown_until {
            Some(s) => parse_rfc3339(s).map(|t| t > Utc::now()).unwrap_or(false),
            None => false,
        }
    }

    pub fn status_label(&self) -> &'static str {
        if self.is_active == 0 {
            return "cut";
        }
        if self.is_cooling() {
            return "sealed";
        }
        if self.is_quota_exhausted() {
            return "sealed";
        }
        if self
            .last_error
            .as_deref()
            .map(|e| !e.trim().is_empty())
            .unwrap_or(false)
        {
            return "fallen";
        }
        "bound"
    }

    pub fn to_public(&self) -> AccountPublic {
        let mut data = self.data_json();
        mask_secrets(&mut data);
        AccountPublic {
            id: self.id.clone(),
            provider: self.provider.clone(),
            email: self.email.clone(),
            name: self.name.clone(),
            is_active: self.is_active != 0,
            priority: self.priority,
            data,
            cooldown_until: self.cooldown_until.clone(),
            last_error: self.last_error.clone(),
            last_used_at: self.last_used_at.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            status: self.status_label().into(),
            quota_limit: self.quota_limit,
            quota_remaining: self.quota_remaining,
            quota_kind: quota_kind_for_provider(&self.provider).into(),
        }
    }

    pub fn has_quota_budget(&self) -> bool {
        self.quota_limit > 0
    }

    pub fn is_quota_exhausted(&self) -> bool {
        self.has_quota_budget() && self.quota_remaining <= 0
    }
}

fn mask_secrets(v: &mut Value) {
    let keys = [
        "accessToken",
        "refreshToken",
        "idToken",
        "personalToken",
        "securityOauthToken",
        "machineToken",
        "access_token",
        "refresh_token",
        "id_token",
    ];
    if let Value::Object(map) = v {
        for k in keys {
            if let Some(Value::String(s)) = map.get(k) {
                map.insert(k.to_string(), Value::String(mask_token(s)));
            }
        }
    }
}

pub fn mask_token(s: &str) -> String {
    if s.len() <= 8 {
        return "****".into();
    }
    format!("{}…{}", &s[..4], &s[s.len() - 4..])
}

pub async fn list_accounts(
    pool: &SqlitePool,
    provider: Option<&str>,
    status: Option<&str>,
) -> AppResult<Vec<Account>> {
    let rows = if let Some(p) = provider {
        let sql = format!(
            "SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE provider = ? ORDER BY priority ASC, created_at ASC"
        );
        sqlx::query_as::<_, Account>(&sql)
            .bind(p)
            .fetch_all(pool)
            .await?
    } else {
        let sql = format!(
            "SELECT {ACCOUNT_COLUMNS} FROM accounts ORDER BY provider, priority ASC, created_at ASC"
        );
        sqlx::query_as::<_, Account>(&sql)
            .fetch_all(pool)
            .await?
    };

    let filtered = if let Some(st) = status {
        rows.into_iter()
            .filter(|a| a.status_label() == st)
            .collect()
    } else {
        rows
    };
    Ok(filtered)
}

pub async fn get_account(pool: &SqlitePool, id: &str) -> AppResult<Account> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE id = ?");
    sqlx::query_as::<_, Account>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("account {id}")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertKind {
    Inserted,
    Updated,
}

pub async fn find_account_by_provider_email(
    pool: &SqlitePool,
    provider: &str,
    email: &str,
) -> AppResult<Option<Account>> {
    let email = email.trim();
    if email.is_empty() || !email.contains('@') {
        return Ok(None);
    }
    let sql = format!(
        r#"
        SELECT {ACCOUNT_COLUMNS} FROM accounts
        WHERE provider = ?
          AND email IS NOT NULL
          AND lower(trim(email)) = lower(trim(?))
        ORDER BY created_at ASC, id ASC
        "#
    );
    let rows = sqlx::query_as::<_, Account>(&sql)
        .bind(provider)
        .bind(email)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().next())
}

fn merge_import_into_existing(existing: &Account, incoming: &Account) -> Account {
    let now = now_rfc3339();
    Account {
        id: existing.id.clone(),
        provider: incoming.provider.clone(),
        email: incoming
            .email
            .clone()
            .or_else(|| existing.email.clone()),
        name: incoming.name.clone().or_else(|| existing.name.clone()),
        is_active: if incoming.is_active != 0 { 1 } else { 0 },
        priority: existing.priority,
        data: incoming.data.clone(),
        cooldown_until: None,
        last_error: None,
        last_used_at: existing.last_used_at.clone(),
        created_at: existing.created_at.clone(),
        updated_at: now,
        quota_limit: existing.quota_limit,
        quota_remaining: existing.quota_remaining,
    }
}

async fn deactivate_email_duplicates(
    pool: &SqlitePool,
    provider: &str,
    email: &str,
    keep_id: &str,
) -> AppResult<u64> {
    let email = email.trim();
    if email.is_empty() || !email.contains('@') {
        return Ok(0);
    }
    let now = now_rfc3339();
    let res = sqlx::query(
        r#"
        UPDATE accounts
        SET is_active = 0,
            last_error = 'duplicate: superseded by same email import',
            updated_at = ?
        WHERE provider = ?
          AND id != ?
          AND email IS NOT NULL
          AND lower(trim(email)) = lower(trim(?))
          AND is_active != 0
        "#,
    )
    .bind(&now)
    .bind(provider)
    .bind(keep_id)
    .bind(email)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

async fn write_account_row(pool: &SqlitePool, acc: &Account) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO accounts (
          id, provider, email, name, is_active, priority, data,
          cooldown_until, last_error, last_used_at, created_at, updated_at,
          quota_limit, quota_remaining
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
          provider=excluded.provider,
          email=excluded.email,
          name=excluded.name,
          is_active=excluded.is_active,
          priority=excluded.priority,
          data=excluded.data,
          cooldown_until=excluded.cooldown_until,
          last_error=excluded.last_error,
          last_used_at=excluded.last_used_at,
          updated_at=excluded.updated_at,
          quota_limit=excluded.quota_limit,
          quota_remaining=excluded.quota_remaining
        "#,
    )
    .bind(&acc.id)
    .bind(&acc.provider)
    .bind(&acc.email)
    .bind(&acc.name)
    .bind(acc.is_active)
    .bind(acc.priority)
    .bind(&acc.data)
    .bind(&acc.cooldown_until)
    .bind(&acc.last_error)
    .bind(&acc.last_used_at)
    .bind(&acc.created_at)
    .bind(&acc.updated_at)
    .bind(acc.quota_limit)
    .bind(acc.quota_remaining)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn upsert_account(pool: &SqlitePool, acc: &Account) -> AppResult<UpsertKind> {
    let (kind, _) = upsert_account_returning(pool, acc).await?;
    Ok(kind)
}

pub async fn upsert_account_returning(
    pool: &SqlitePool,
    acc: &Account,
) -> AppResult<(UpsertKind, Account)> {
    if let Some(email) = acc
        .email
        .as_ref()
        .map(|e| e.trim())
        .filter(|e| !e.is_empty() && e.contains('@'))
    {
        if let Some(existing) = find_account_by_provider_email(pool, &acc.provider, email).await? {
            let merged = merge_import_into_existing(&existing, acc);
            write_account_row(pool, &merged).await?;
            let _ = deactivate_email_duplicates(pool, &merged.provider, email, &merged.id).await?;
            return Ok((UpsertKind::Updated, merged));
        }
    }

    if let Ok(by_id) = get_account(pool, &acc.id).await {
        let mut row = acc.clone();
        row.created_at = by_id.created_at;
        if row.email.as_ref().map(|e| e.trim().is_empty()).unwrap_or(true) {
            row.email = by_id.email;
        }
        write_account_row(pool, &row).await?;
        if let Some(email) = row
            .email
            .as_ref()
            .map(|e| e.trim())
            .filter(|e| !e.is_empty() && e.contains('@'))
        {
            let _ = deactivate_email_duplicates(pool, &row.provider, email, &row.id).await?;
        }
        return Ok((UpsertKind::Updated, row));
    }

    write_account_row(pool, acc).await?;
    if let Some(email) = acc
        .email
        .as_ref()
        .map(|e| e.trim())
        .filter(|e| !e.is_empty() && e.contains('@'))
    {
        let _ = deactivate_email_duplicates(pool, &acc.provider, email, &acc.id).await?;
    }
    Ok((UpsertKind::Inserted, acc.clone()))
}

pub async fn update_account(pool: &SqlitePool, acc: &Account) -> AppResult<()> {
    sqlx::query(
        r#"
        UPDATE accounts SET
          provider=?, email=?, name=?, is_active=?, priority=?, data=?,
          cooldown_until=?, last_error=?, last_used_at=?, updated_at=?,
          quota_limit=?, quota_remaining=?
        WHERE id=?
        "#,
    )
    .bind(&acc.provider)
    .bind(&acc.email)
    .bind(&acc.name)
    .bind(acc.is_active)
    .bind(acc.priority)
    .bind(&acc.data)
    .bind(&acc.cooldown_until)
    .bind(&acc.last_error)
    .bind(&acc.last_used_at)
    .bind(&acc.updated_at)
    .bind(acc.quota_limit)
    .bind(acc.quota_remaining)
    .bind(&acc.id)
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProxyRow {
    pub id: String,
    pub scheme: String,
    pub host: String,
    pub port: i64,
    pub username: Option<String>,
    pub password: Option<String>,
    pub label: Option<String>,
    pub country: Option<String>,
    pub is_active: i64,
    pub health: String,
    pub latency_ms: Option<i64>,
    pub last_check_at: Option<String>,
    pub last_error: Option<String>,
    pub source: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

fn encode_userinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl ProxyRow {
    pub fn url(&self) -> String {
        match (&self.username, &self.password) {
            (Some(u), Some(p)) if !u.is_empty() => format!(
                "{}://{}:{}@{}:{}",
                self.scheme,
                encode_userinfo(u),
                encode_userinfo(p),
                self.host,
                self.port
            ),
            _ => format!("{}://{}:{}", self.scheme, self.host, self.port),
        }
    }

    pub fn to_public(&self) -> Value {
        let has_auth = self
            .username
            .as_deref()
            .map(|u| !u.is_empty())
            .unwrap_or(false);
        serde_json::json!({
            "id": self.id,
            "scheme": self.scheme,
            "host": self.host,
            "port": self.port,
            "username": self.username,
            "has_auth": has_auth,
            "label": self.label,
            "country": self.country,
            "is_active": self.is_active != 0,
            "health": self.health,
            "latency_ms": self.latency_ms,
            "last_check_at": self.last_check_at,
            "last_error": self.last_error,
            "source": self.source,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }
}

pub async fn list_proxies(pool: &SqlitePool) -> AppResult<Vec<ProxyRow>> {
    Ok(
        sqlx::query_as::<_, ProxyRow>("SELECT * FROM proxies ORDER BY created_at ASC")
            .fetch_all(pool)
            .await?,
    )
}

pub async fn list_active_proxies(pool: &SqlitePool) -> AppResult<Vec<ProxyRow>> {
    Ok(sqlx::query_as::<_, ProxyRow>(
        "SELECT * FROM proxies WHERE is_active = 1 ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await?)
}

pub async fn get_proxy(pool: &SqlitePool, id: &str) -> AppResult<ProxyRow> {
    sqlx::query_as::<_, ProxyRow>("SELECT * FROM proxies WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("proxy {id}")))
}

/// Insert a proxy, or update the existing row when (host, port, username)
/// already exists. Idempotent so re-importing a file never duplicates.
pub async fn upsert_proxy(pool: &SqlitePool, p: &ProxyRow) -> AppResult<(String, bool)> {
    let now = now_rfc3339();
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM proxies WHERE host = ? AND port = ? AND IFNULL(username,'') = IFNULL(?,'')",
    )
    .bind(&p.host)
    .bind(p.port)
    .bind(&p.username)
    .fetch_optional(pool)
    .await?;
    if let Some(id) = existing {
        sqlx::query(
            "UPDATE proxies SET scheme=?, password=?, label=?, country=?, source=?, updated_at=? WHERE id=?",
        )
        .bind(&p.scheme)
        .bind(&p.password)
        .bind(&p.label)
        .bind(&p.country)
        .bind(&p.source)
        .bind(&now)
        .bind(&id)
        .execute(pool)
        .await?;
        return Ok((id, false));
    }
    let id = if p.id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        p.id.clone()
    };
    sqlx::query(
        r#"
        INSERT INTO proxies
          (id, scheme, host, port, username, password, label, country,
           is_active, health, latency_ms, last_check_at, last_error, source,
           created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, 'unknown', NULL, NULL, NULL, ?, ?, ?)
        "#,
    )
    .bind(&id)
    .bind(&p.scheme)
    .bind(&p.host)
    .bind(p.port)
    .bind(&p.username)
    .bind(&p.password)
    .bind(&p.label)
    .bind(&p.country)
    .bind(&p.source)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok((id, true))
}

pub async fn set_proxy_health(
    pool: &SqlitePool,
    id: &str,
    health: &str,
    latency_ms: Option<i64>,
    last_error: Option<&str>,
) -> AppResult<()> {
    let now = now_rfc3339();
    sqlx::query(
        "UPDATE proxies SET health=?, latency_ms=?, last_error=?, last_check_at=?, updated_at=? WHERE id=?",
    )
    .bind(health)
    .bind(latency_ms)
    .bind(last_error)
    .bind(&now)
    .bind(&now)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_proxy_active(pool: &SqlitePool, id: &str, active: bool) -> AppResult<()> {
    sqlx::query("UPDATE proxies SET is_active=?, updated_at=? WHERE id=?")
        .bind(if active { 1 } else { 0 })
        .bind(now_rfc3339())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_proxy(pool: &SqlitePool, id: &str) -> AppResult<()> {
    let r = sqlx::query("DELETE FROM proxies WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("proxy {id}")));
    }
    sqlx::query("DELETE FROM account_proxy WHERE proxy_id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_account_proxy_map(pool: &SqlitePool) -> AppResult<Vec<(String, String)>> {
    Ok(
        sqlx::query_as::<_, (String, String)>("SELECT account_id, proxy_id FROM account_proxy")
            .fetch_all(pool)
            .await?,
    )
}

pub async fn get_account_proxy(pool: &SqlitePool, account_id: &str) -> AppResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT proxy_id FROM account_proxy WHERE account_id = ?")
            .bind(account_id)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn set_account_proxy(
    pool: &SqlitePool,
    account_id: &str,
    proxy_id: &str,
) -> AppResult<()> {
    sqlx::query(
        r#"
        INSERT INTO account_proxy (account_id, proxy_id, assigned_at)
        VALUES (?, ?, ?)
        ON CONFLICT(account_id) DO UPDATE SET proxy_id = excluded.proxy_id, assigned_at = excluded.assigned_at
        "#,
    )
    .bind(account_id)
    .bind(proxy_id)
    .bind(now_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn clear_account_proxy(pool: &SqlitePool, account_id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM account_proxy WHERE account_id = ?")
        .bind(account_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn proxy_assignment_counts(pool: &SqlitePool) -> AppResult<Vec<(String, i64)>> {
    Ok(sqlx::query_as::<_, (String, i64)>(
        "SELECT proxy_id, COUNT(*) FROM account_proxy GROUP BY proxy_id",
    )
    .fetch_all(pool)
    .await?)
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProxySettingsRow {
    pub id: i64,
    pub chat_mode: String,
    pub automation_mode: String,
    pub on_dead: String,
    pub updated_at: String,
}

pub async fn get_proxy_settings(pool: &SqlitePool) -> AppResult<ProxySettingsRow> {
    if let Some(row) =
        sqlx::query_as::<_, ProxySettingsRow>("SELECT * FROM proxy_settings WHERE id = 1")
            .fetch_optional(pool)
            .await?
    {
        return Ok(row);
    }
    let now = now_rfc3339();
    sqlx::query(
        "INSERT INTO proxy_settings (id, chat_mode, automation_mode, on_dead, updated_at) VALUES (1, 'off', 'sticky', 'direct', ?)",
    )
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(ProxySettingsRow {
        id: 1,
        chat_mode: "off".into(),
        automation_mode: "sticky".into(),
        on_dead: "direct".into(),
        updated_at: now,
    })
}

pub async fn update_proxy_settings(
    pool: &SqlitePool,
    chat_mode: Option<&str>,
    automation_mode: Option<&str>,
    on_dead: Option<&str>,
) -> AppResult<ProxySettingsRow> {
    let cur = get_proxy_settings(pool).await?;
    let chat = chat_mode.unwrap_or(&cur.chat_mode);
    let auto = automation_mode.unwrap_or(&cur.automation_mode);
    let dead = on_dead.unwrap_or(&cur.on_dead);
    sqlx::query(
        "UPDATE proxy_settings SET chat_mode=?, automation_mode=?, on_dead=?, updated_at=? WHERE id=1",
    )
    .bind(chat)
    .bind(auto)
    .bind(dead)
    .bind(now_rfc3339())
    .execute(pool)
    .await?;
    get_proxy_settings(pool).await
}

pub async fn decrement_quota(
    pool: &SqlitePool,
    account_id: &str,
    credits_used: i64,
) -> AppResult<(i64, i64, i64)> {
    let used = credits_used.max(0);
    if used == 0 {
        let acc = get_account(pool, account_id).await?;
        return Ok((acc.quota_remaining, acc.quota_remaining, 0));
    }
    // The UPDATE below is the atomic guard against overspend: SQLite serializes
    // writers, so two concurrent requests can't both subtract from the same
    // baseline. The RETURNING gives the authoritative post-charge remaining.
    let after: Option<i64> = sqlx::query_scalar(
        r#"
        UPDATE accounts
        SET quota_remaining = MAX(0, quota_remaining - ?),
            updated_at = ?
        WHERE id = ? AND quota_limit > 0
        RETURNING quota_remaining
        "#,
    )
    .bind(used)
    .bind(now_rfc3339())
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    match after {
        Some(after) => {
            let before = after + used;
            Ok((before, after, used))
        }
        None => {
            let acc = get_account(pool, account_id).await?;
            Ok((acc.quota_remaining, acc.quota_remaining, 0))
        }
    }
}

pub async fn delete_account(pool: &SqlitePool, id: &str) -> AppResult<()> {
    let r = sqlx::query("DELETE FROM accounts WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("account {id}")));
    }
    Ok(())
}

pub async fn delete_accounts_by_providers(
    pool: &SqlitePool,
    providers: &[&str],
) -> AppResult<u64> {
    let mut total = 0u64;
    for p in providers {
        let r = sqlx::query("DELETE FROM accounts WHERE provider = ?")
            .bind(p)
            .execute(pool)
            .await?;
        total += r.rows_affected();
    }
    Ok(total)
}

pub async fn list_eligible_accounts(
    pool: &SqlitePool,
    provider: &str,
    model: Option<&str>,
) -> AppResult<Vec<Account>> {
    let now = now_rfc3339();
    let _ = sqlx::query(
        r#"
        UPDATE accounts
        SET quota_remaining = quota_limit,
            updated_at = ?
        WHERE provider = ?
          AND is_active = 1
          AND quota_limit > 0
          AND quota_remaining = 0
          AND cooldown_until IS NOT NULL
          AND cooldown_until < ?
          AND (
            last_error LIKE '%payment required%'
            OR last_error LIKE '%PaymentRequired%'
            OR last_error LIKE '%402%'
            OR last_error LIKE '%spending-limit%'
            OR last_error LIKE '%credit block%'
          )
        "#,
    )
    .bind(&now)
    .bind(provider)
    .bind(&now)
    .execute(pool)
    .await;

    let sql = format!(
        r#"
        SELECT {ACCOUNT_COLUMNS} FROM accounts
        WHERE provider = ?
          AND is_active = 1
          AND (cooldown_until IS NULL OR cooldown_until < ?)
        ORDER BY priority ASC, last_used_at IS NOT NULL, last_used_at ASC, created_at ASC
        "#
    );
    let rows = sqlx::query_as::<_, Account>(&sql)
        .bind(provider)
        .bind(&now)
        .fetch_all(pool)
    .await?;

    let mut eligible: Vec<Account> = rows
        .into_iter()
        .filter(|a| !quota_blocks_pick(a, model))
        .collect();

    if provider == "qoder" {
        let settings = get_provider_settings(pool, provider).await?;
        let mode = QoderPickMode::parse(&settings.pick_mode).unwrap_or_default();
        if mode == QoderPickMode::UltimateFree {
            if let Some(m) = model {
                if crate::providers::qoder::is_ultimate_free_activity_model(m) {
                    eligible.retain(|a| {
                        crate::providers::qoder::model_has_free_activity_path(a, m)
                    });
                }
            }
        }
    }

    Ok(eligible)
}

pub async fn pick_account(
    pool: &SqlitePool,
    provider: &str,
    exclude_ids: &[String],
    model: Option<&str>,
) -> AppResult<(Account, LoadBalance)> {
    let settings = get_provider_settings(pool, provider).await?;
    let strategy =
        LoadBalance::parse(&settings.load_balance).unwrap_or_default();
    let mut eligible = list_eligible_accounts(pool, provider, model).await?;
    if !exclude_ids.is_empty() {
        eligible.retain(|a| !exclude_ids.iter().any(|id| id == &a.id));
    }
    if eligible.is_empty() {
        return Err(AppError::NoAccounts(provider.into()));
    }

    let chosen = match strategy {
        LoadBalance::Sequential => {
            if let Some(sticky) = settings.sticky_account_id.as_ref() {
                if let Some(a) = eligible.iter().find(|a| &a.id == sticky) {
                    a.clone()
                } else {
                    eligible.into_iter().next().unwrap()
                }
            } else {
                eligible.into_iter().next().unwrap()
            }
        }
        LoadBalance::RoundRobin => {
            let cursor = settings.rr_cursor.as_deref();
            if let Some(cur) = cursor {
                if let Some(idx) = eligible.iter().position(|a| a.id == cur) {
                    let next = (idx + 1) % eligible.len();
                    eligible[next].clone()
                } else {
                    eligible[0].clone()
                }
            } else {
                eligible[0].clone()
            }
        }
        LoadBalance::LeastUsed => {
            eligible.sort_by(|a, b| {
                match (&a.last_used_at, &b.last_used_at) {
                    (None, Some(_)) => std::cmp::Ordering::Less,
                    (Some(_), None) => std::cmp::Ordering::Greater,
                    (None, None) => a.created_at.cmp(&b.created_at),
                    (Some(x), Some(y)) => x.cmp(y).then_with(|| a.created_at.cmp(&b.created_at)),
                }
            });
            eligible[0].clone()
        }
        LoadBalance::Priority => {
            eligible.sort_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then_with(|| match (&a.last_used_at, &b.last_used_at) {
                        (None, Some(_)) => std::cmp::Ordering::Less,
                        (Some(_), None) => std::cmp::Ordering::Greater,
                        (None, None) => a.created_at.cmp(&b.created_at),
                        (Some(x), Some(y)) => x.cmp(y),
                    })
            });
            eligible[0].clone()
        }
        LoadBalance::Random => {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            now_rfc3339().hash(&mut hasher);
            provider.hash(&mut hasher);
            eligible.len().hash(&mut hasher);
            if let Some(first) = eligible.first() {
                first.id.hash(&mut hasher);
            }
            let idx = (hasher.finish() as usize) % eligible.len();
            eligible[idx].clone()
        }
    };

    Ok((chosen, strategy))
}

pub async fn stats(pool: &SqlitePool) -> AppResult<serde_json::Value> {
    #[derive(sqlx::FromRow)]
    struct StatusRow {
        provider: String,
        is_active: i64,
        cooldown_until: Option<String>,
        last_error: Option<String>,
        quota_limit: i64,
        quota_remaining: i64,
    }
    // Status counting needs no token data; skip the heavy `data` blob column.
    let rows = sqlx::query_as::<_, StatusRow>(
        r#"
        SELECT provider, is_active, cooldown_until, last_error,
               quota_limit, quota_remaining
        FROM accounts
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut total = 0u64;
    let mut bound = 0u64;
    let mut sealed = 0u64;
    let mut cut = 0u64;
    let mut fallen = 0u64;
    let mut by_provider = serde_json::Map::new();

    for r in &rows {
        total += 1;
        let probe = Account {
            id: String::new(),
            provider: r.provider.clone(),
            email: None,
            name: None,
            is_active: r.is_active,
            priority: 0,
            data: String::new(),
            cooldown_until: r.cooldown_until.clone(),
            last_error: r.last_error.clone(),
            last_used_at: None,
            created_at: String::new(),
            updated_at: String::new(),
            quota_limit: r.quota_limit,
            quota_remaining: r.quota_remaining,
        };
        match probe.status_label() {
            "bound" => bound += 1,
            "sealed" => sealed += 1,
            "cut" => cut += 1,
            "fallen" => fallen += 1,
            _ => {}
        }
        let status_key = probe.status_label();
        let entry = by_provider
            .entry(r.provider.clone())
            .or_insert_with(|| {
                serde_json::json!({
                    "total": 0, "bound": 0, "sealed": 0, "cut": 0, "fallen": 0
                })
            });
        if let Some(obj) = entry.as_object_mut() {
            *obj.get_mut("total").unwrap() =
                serde_json::json!(obj["total"].as_u64().unwrap_or(0) + 1);
            let key = status_key;
            *obj.get_mut(key).unwrap() =
                serde_json::json!(obj[key].as_u64().unwrap_or(0) + 1);
        }
    }

    Ok(serde_json::json!({
        "total": total,
        "bound": bound,
        "sealed": sealed,
        "cut": cut,
        "fallen": fallen,
        "by_provider": by_provider
    }))
}

pub fn new_account_id() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_account() -> Account {
        Account {
            id: "a1".into(),
            provider: "grok-cli".into(),
            email: Some("x@y.z".into()),
            name: None,
            is_active: 1,
            priority: 0,
            data: "{}".into(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: "t".into(),
            updated_at: "t".into(),
            quota_limit: GROK_TOKEN_QUOTA,
            quota_remaining: GROK_TOKEN_QUOTA,
        }
    }

    #[test]
    fn status_bound_when_healthy() {
        assert_eq!(sample_account().status_label(), "bound");
    }

    #[test]
    fn status_cut_when_inactive() {
        let mut a = sample_account();
        a.is_active = 0;
        a.last_error = Some("anything".into());
        assert_eq!(a.status_label(), "cut");
    }

    #[test]
    fn status_sealed_cooldown_beats_error() {
        let mut a = sample_account();
        a.cooldown_until = Some("2099-01-01T00:00:00.000Z".into());
        a.last_error = Some("upstream 500: boom".into());
        assert_eq!(a.status_label(), "sealed");
    }

    #[test]
    fn status_fallen_on_any_residual_error() {
        let mut a = sample_account();
        a.last_error = Some("transport: connection reset".into());
        assert_eq!(a.status_label(), "fallen");
    }

    #[test]
    fn status_fallen_not_only_invalid_grant() {
        let mut a = sample_account();
        a.last_error = Some("upstream 500: internal".into());
        assert_eq!(a.status_label(), "fallen");
        a.last_error = Some("other: weird".into());
        assert_eq!(a.status_label(), "fallen");
    }

    #[test]
    fn quota_kind_qoder_is_credits() {
        assert_eq!(quota_kind_for_provider("qoder"), "credits");
        assert_eq!(quota_kind_for_provider("grok-cli"), "tokens");
        assert_eq!(quota_kind_for_provider("other"), "none");
    }

    #[test]
    fn default_quota_qoder_stays_zero() {
        assert_eq!(default_quota_for_provider("qoder"), (0, 0));
        assert_eq!(
            default_quota_for_provider("grok-cli"),
            (GROK_TOKEN_QUOTA, GROK_TOKEN_QUOTA)
        );
    }

    #[test]
    fn free_tier_model_only_lite() {
        assert!(is_qoder_free_tier_model("lite"));
        assert!(is_qoder_free_tier_model("qd/lite"));
        assert!(is_qoder_free_tier_model("QD/Lite"));
        assert!(!is_qoder_free_tier_model("auto"));
        assert!(!is_qoder_free_tier_model("qd/auto"));
        assert!(!is_qoder_free_tier_model("ultimate"));
        assert!(!is_qoder_free_tier_model(""));
        assert!(!is_qoder_free_tier_model("gcli/grok-4"));
    }

    fn qoder_exhausted() -> Account {
        Account {
            id: "q1".into(),
            provider: "qoder".into(),
            email: Some("q@x.y".into()),
            name: None,
            is_active: 1,
            priority: 0,
            data: "{}".into(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: "t".into(),
            updated_at: "t".into(),
            quota_limit: 300,
            quota_remaining: 0,
        }
    }

    #[test]
    fn quota_blocks_paid_when_exhausted() {
        let a = qoder_exhausted();
        assert!(quota_blocks_pick(&a, Some("qd/auto")));
        assert!(quota_blocks_pick(&a, Some("auto")));
        assert!(quota_blocks_pick(&a, None), "None is strict (paid)");
    }

    #[test]
    fn quota_allows_lite_when_exhausted() {
        let a = qoder_exhausted();
        assert!(!quota_blocks_pick(&a, Some("qd/lite")));
        assert!(!quota_blocks_pick(&a, Some("lite")));
    }

    #[test]
    fn quota_allows_ultimate_when_free_activity_remaining() {
        let mut a = qoder_exhausted();
        a.data = r#"{
          "personalToken":"pt",
          "free_limit":200,
          "free_remaining":50,
          "free_model_keys":["ultimate","quest-ultimate","experts-ultimate"]
        }"#
        .into();
        assert!(!quota_blocks_pick(&a, Some("qd/ultimate")));
        assert!(!quota_blocks_pick(&a, Some("ultimate")));
        assert!(quota_blocks_pick(&a, Some("qd/auto")));
    }

    #[test]
    fn quota_blocks_ultimate_when_free_activity_empty() {
        let mut a = qoder_exhausted();
        a.data = r#"{
          "personalToken":"pt",
          "free_limit":200,
          "free_remaining":0,
          "free_model_keys":["ultimate"]
        }"#
        .into();
        assert!(quota_blocks_pick(&a, Some("qd/ultimate")));
    }

    #[test]
    fn quota_allows_unsynced_qoder() {
        let mut a = qoder_exhausted();
        a.quota_limit = 0;
        a.quota_remaining = 0;
        assert!(!quota_blocks_pick(&a, Some("qd/auto")));
        assert!(!quota_blocks_pick(&a, Some("qd/lite")));
        assert!(!quota_blocks_pick(&a, None));
    }

    #[test]
    fn quota_blocks_grok_when_exhausted() {
        let mut a = sample_account();
        a.quota_remaining = 0;
        assert!(quota_blocks_pick(&a, Some("gcli/grok-4")));
        assert!(quota_blocks_pick(&a, None));
    }

    #[tokio::test]
    async fn list_eligible_qoder_free_vs_paid() {
        let dir = std::env::temp_dir().join(format!("marionette-db-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pool = connect(&dir.join("test.sqlite")).await.unwrap();
        let mut acc = qoder_exhausted();
        acc.created_at = now_rfc3339();
        acc.updated_at = now_rfc3339();
        upsert_account(&pool, &acc).await.unwrap();

        let paid = list_eligible_accounts(&pool, "qoder", Some("qd/auto"))
            .await
            .unwrap();
        assert!(
            paid.is_empty(),
            "exhausted qoder must not be eligible for paid models"
        );

        let free = list_eligible_accounts(&pool, "qoder", Some("qd/lite"))
            .await
            .unwrap();
        assert_eq!(free.len(), 1);
        assert_eq!(free[0].id, "q1");

        let strict = list_eligible_accounts(&pool, "qoder", None).await.unwrap();
        assert!(
            strict.is_empty(),
            "model=None must be strict and exclude exhausted"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ultimate_free_pick_skips_credit_only_accounts() {
        let dir = std::env::temp_dir().join(format!("marionette-uf-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pool = connect(&dir.join("test.sqlite")).await.unwrap();

        let now = now_rfc3339();
        let mut credit_only = Account {
            id: "q-credit".into(),
            provider: "qoder".into(),
            email: Some("c@x.y".into()),
            name: None,
            is_active: 1,
            priority: 0,
            data: r#"{"personalToken":"pt"}"#.into(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            quota_limit: 300,
            quota_remaining: 200,
        };
        let mut free_acc = Account {
            id: "q-free".into(),
            provider: "qoder".into(),
            email: Some("f@x.y".into()),
            name: None,
            is_active: 1,
            priority: 0,
            data: r#"{
              "personalToken":"pt",
              "free_limit":200,
              "free_remaining":50,
              "free_model_keys":["ultimate","quest-ultimate","experts-ultimate"]
            }"#
            .into(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            quota_limit: 300,
            quota_remaining: 0,
        };
        upsert_account(&pool, &credit_only).await.unwrap();
        upsert_account(&pool, &free_acc).await.unwrap();

        set_provider_pick_mode(&pool, "qoder", QoderPickMode::UltimateFree)
            .await
            .unwrap();

        let ultimate = list_eligible_accounts(&pool, "qoder", Some("qd/ultimate"))
            .await
            .unwrap();
        assert_eq!(ultimate.len(), 1, "only free Ultimate accounts");
        assert_eq!(ultimate[0].id, "q-free");

        let auto = list_eligible_accounts(&pool, "qoder", Some("qd/auto"))
            .await
            .unwrap();
        assert_eq!(
            auto.len(),
            1,
            "ultimate_free mode must not filter non-Ultimate models"
        );
        assert_eq!(auto[0].id, "q-credit");

        free_acc.data = r#"{
          "personalToken":"pt",
          "free_limit":200,
          "free_remaining":0,
          "free_model_keys":["ultimate"]
        }"#
        .into();
        free_acc.updated_at = now_rfc3339();
        upsert_account(&pool, &free_acc).await.unwrap();

        let empty = list_eligible_accounts(&pool, "qoder", Some("qd/ultimate"))
            .await
            .unwrap();
        assert!(
            empty.is_empty(),
            "no free remaining → skip (not cut); empty eligible set"
        );

        set_provider_pick_mode(&pool, "qoder", QoderPickMode::Normal)
            .await
            .unwrap();
        credit_only.quota_remaining = 100;
        credit_only.updated_at = now_rfc3339();
        upsert_account(&pool, &credit_only).await.unwrap();
        let normal = list_eligible_accounts(&pool, "qoder", Some("qd/ultimate"))
            .await
            .unwrap();
        assert!(
            normal.iter().any(|a| a.id == "q-credit"),
            "normal mode allows credit-bearing Ultimate"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn decrement_quota_is_atomic_and_never_overspends() {
        let dir = std::env::temp_dir().join(format!("marionette-dq-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pool = connect(&dir.join("test.sqlite")).await.unwrap();

        let now = now_rfc3339();
        let acc = Account {
            id: "q-dec".into(),
            provider: "qoder".into(),
            email: Some("d@x.y".into()),
            name: None,
            is_active: 1,
            priority: 0,
            data: r#"{"personalToken":"pt"}"#.into(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            quota_limit: 300,
            quota_remaining: 10,
        };
        upsert_account(&pool, &acc).await.unwrap();

        let (a, b, c) = tokio::join!(
            decrement_quota(&pool, "q-dec", 4),
            decrement_quota(&pool, "q-dec", 4),
            decrement_quota(&pool, "q-dec", 4),
        );
        a.unwrap();
        b.unwrap();
        c.unwrap();

        let final_acc = get_account(&pool, "q-dec").await.unwrap();
        assert_eq!(
            final_acc.quota_remaining, 0,
            "3x4=12 charged against 10 must clamp at 0, never negative"
        );

        let (before, after, used) = decrement_quota(&pool, "q-dec", 5).await.unwrap();
        assert_eq!(after, 0, "already exhausted stays at 0");
        assert_eq!(before, used, "before == after + used bookkeeping holds");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn qoder_pick_mode_parse_labels() {
        assert_eq!(
            QoderPickMode::parse("ultimate_free"),
            Some(QoderPickMode::UltimateFree)
        );
        assert_eq!(
            QoderPickMode::parse("free_ultimate"),
            Some(QoderPickMode::UltimateFree)
        );
        assert_eq!(QoderPickMode::parse("normal"), Some(QoderPickMode::Normal));
        assert!(QoderPickMode::parse("bogus").is_none());
        assert_eq!(QoderPickMode::UltimateFree.label(), "Ultimate free only");
    }

    #[tokio::test]
    async fn upsert_merges_same_provider_email_keeps_id() {
        let dir = std::env::temp_dir().join(format!("marionette-db-merge-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pool = connect(&dir.join("test.sqlite")).await.unwrap();

        let now = now_rfc3339();
        let first = Account {
            id: "keep-me".into(),
            provider: "grok-cli".into(),
            email: Some("a@x.ai".into()),
            name: Some("A".into()),
            is_active: 1,
            priority: 3,
            data: r#"{"accessToken":"old-access","refreshToken":"old-rt"}"#.into(),
            cooldown_until: Some("2099-01-01T00:00:00.000Z".into()),
            last_error: Some("rate limited".into()),
            last_used_at: Some("2026-01-01T00:00:00.000Z".into()),
            created_at: now.clone(),
            updated_at: now.clone(),
            quota_limit: GROK_TOKEN_QUOTA,
            quota_remaining: 42,
        };
        assert_eq!(
            upsert_account(&pool, &first).await.unwrap(),
            UpsertKind::Inserted
        );

        let second = Account {
            id: "new-uuid-from-farm".into(),
            provider: "grok-cli".into(),
            email: Some("A@x.ai".into()),
            name: Some("A relog".into()),
            is_active: 1,
            priority: 0,
            data: r#"{"accessToken":"new-access","refreshToken":"new-rt"}"#.into(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            quota_limit: GROK_TOKEN_QUOTA,
            quota_remaining: GROK_TOKEN_QUOTA,
        };
        let (kind, saved) = upsert_account_returning(&pool, &second).await.unwrap();
        assert_eq!(kind, UpsertKind::Updated);
        assert_eq!(saved.id, "keep-me");
        assert!(saved.data.contains("new-access"));
        assert!(saved.data.contains("new-rt"));
        assert_eq!(saved.priority, 3);
        assert_eq!(saved.quota_remaining, 42);
        assert!(saved.cooldown_until.is_none());
        assert!(saved.last_error.is_none());
        assert_eq!(
            saved.last_used_at.as_deref(),
            Some("2026-01-01T00:00:00.000Z")
        );

        let all = list_accounts(&pool, Some("grok-cli"), None)
            .await
            .unwrap();
        assert_eq!(all.len(), 1, "must not create duplicate row");
        assert_eq!(all[0].id, "keep-me");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn upsert_deactivates_extra_same_email_rows() {
        let dir = std::env::temp_dir().join(format!("marionette-db-dup-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pool = connect(&dir.join("test.sqlite")).await.unwrap();
        let now = now_rfc3339();

        let a1 = Account {
            id: "row-1".into(),
            provider: "grok-cli".into(),
            email: Some("dup@x.ai".into()),
            name: None,
            is_active: 1,
            priority: 0,
            data: r#"{"accessToken":"t1"}"#.into(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            quota_limit: GROK_TOKEN_QUOTA,
            quota_remaining: GROK_TOKEN_QUOTA,
        };
        write_account_row(&pool, &a1).await.unwrap();
        let a2 = Account {
            id: "row-2".into(),
            provider: "grok-cli".into(),
            email: Some("dup@x.ai".into()),
            name: None,
            is_active: 1,
            priority: 0,
            data: r#"{"accessToken":"t2"}"#.into(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: now.clone(),
            updated_at: now,
            quota_limit: GROK_TOKEN_QUOTA,
            quota_remaining: GROK_TOKEN_QUOTA,
        };
        write_account_row(&pool, &a2).await.unwrap();

        let fresh = Account {
            id: "farm-new".into(),
            provider: "grok-cli".into(),
            email: Some("dup@x.ai".into()),
            name: None,
            is_active: 1,
            priority: 0,
            data: r#"{"accessToken":"t3","refreshToken":"r3"}"#.into(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            quota_limit: GROK_TOKEN_QUOTA,
            quota_remaining: GROK_TOKEN_QUOTA,
        };
        let (kind, saved) = upsert_account_returning(&pool, &fresh).await.unwrap();
        assert_eq!(kind, UpsertKind::Updated);
        assert_eq!(saved.id, "row-1");
        assert!(saved.data.contains("t3"));

        let all = list_accounts(&pool, Some("grok-cli"), None)
            .await
            .unwrap();
        assert_eq!(all.len(), 2);
        let active: Vec<_> = all.iter().filter(|a| a.is_active != 0).collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "row-1");
        let cut = all.iter().find(|a| a.id == "row-2").unwrap();
        assert_eq!(cut.is_active, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
