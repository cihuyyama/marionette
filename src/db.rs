use crate::error::{AppError, AppResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
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
    let url = format!("sqlite:{}?mode=rwc", db_path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await?;
    migrate(&pool).await?;
    Ok(pool)
}

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
          updated_at TEXT NOT NULL
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
          error_message TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_request_logs_created
          ON request_logs(created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_request_logs_provider
          ON request_logs(provider, created_at DESC);
        CREATE TABLE IF NOT EXISTS provider_settings (
          provider TEXT PRIMARY KEY,
          load_balance TEXT NOT NULL DEFAULT 'round_robin',
          sticky_account_id TEXT,
          rr_cursor TEXT,
          updated_at TEXT NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;
    migrate_quota_columns(pool).await?;
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

pub fn default_quota_for_provider(provider: &str) -> (i64, i64) {
    match provider {
        "grok-cli" => (GROK_TOKEN_QUOTA, GROK_TOKEN_QUOTA),
        _ => (0, 0),
    }
}

pub fn quota_kind_for_provider(provider: &str) -> &'static str {
    match provider {
        "grok-cli" => "tokens",
        _ => "none",
    }
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

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProviderSettingsRow {
    pub provider: String,
    pub load_balance: String,
    pub sticky_account_id: Option<String>,
    pub rr_cursor: Option<String>,
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
        updated_at: now.clone(),
    };
    sqlx::query(
        r#"
        INSERT INTO provider_settings (provider, load_balance, sticky_account_id, rr_cursor, updated_at)
        VALUES (?, ?, NULL, NULL, ?)
        "#,
    )
    .bind(&row.provider)
    .bind(&row.load_balance)
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
          account_id, account_email, error_message
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn list_request_logs(
    pool: &SqlitePool,
    provider: Option<&str>,
    limit: i64,
) -> AppResult<Vec<RequestLog>> {
    let limit = limit.clamp(1, 500);
    if let Some(p) = provider {
        let rows = sqlx::query_as::<_, RequestLog>(
            r#"
            SELECT * FROM request_logs
            WHERE provider = ?
            ORDER BY created_at DESC
            LIMIT ?
            "#,
        )
        .bind(p)
        .bind(limit)
        .fetch_all(pool)
        .await?;
        Ok(rows)
    } else {
        let rows = sqlx::query_as::<_, RequestLog>(
            r#"
            SELECT * FROM request_logs
            ORDER BY created_at DESC
            LIMIT ?
            "#,
        )
        .bind(limit)
        .fetch_all(pool)
        .await?;
        Ok(rows)
    }
}

pub async fn usage_summary(pool: &SqlitePool) -> AppResult<serde_json::Value> {
    let totals = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64)>(
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
    .await?;

    let by_model = sqlx::query_as::<_, (Option<String>, String, i64, i64, i64, i64)>(
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
    .await?;

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
            .map(|e| {
                e.contains("invalid_grant")
                    || e.contains("AuthInvalid")
                    || e.contains("dead")
            })
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
        sqlx::query_as::<_, Account>(
            "SELECT * FROM accounts WHERE provider = ? ORDER BY priority ASC, created_at ASC",
        )
        .bind(p)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, Account>("SELECT * FROM accounts ORDER BY provider, priority ASC, created_at ASC")
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
    sqlx::query_as::<_, Account>("SELECT * FROM accounts WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("account {id}")))
}

pub async fn upsert_account(pool: &SqlitePool, acc: &Account) -> AppResult<()> {
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

pub async fn decrement_quota(
    pool: &SqlitePool,
    account_id: &str,
    credits_used: i64,
) -> AppResult<(i64, i64, i64)> {
    let mut acc = get_account(pool, account_id).await?;
    let before = acc.quota_remaining;
    if !acc.has_quota_budget() || credits_used <= 0 {
        return Ok((before, before, 0));
    }
    let used = credits_used.max(0);
    acc.quota_remaining = (acc.quota_remaining - used).max(0);
    acc.updated_at = now_rfc3339();
    update_account(pool, &acc).await?;
    Ok((before, acc.quota_remaining, used))
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

pub async fn list_eligible_accounts(
    pool: &SqlitePool,
    provider: &str,
) -> AppResult<Vec<Account>> {
    let now = now_rfc3339();
    let rows = sqlx::query_as::<_, Account>(
        r#"
        SELECT * FROM accounts
        WHERE provider = ?
          AND is_active = 1
          AND (cooldown_until IS NULL OR cooldown_until < ?)
          AND (quota_limit <= 0 OR quota_remaining > 0)
        ORDER BY priority ASC, last_used_at IS NOT NULL, last_used_at ASC, created_at ASC
        "#,
    )
    .bind(provider)
    .bind(&now)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn pick_account(
    pool: &SqlitePool,
    provider: &str,
    exclude_ids: &[String],
) -> AppResult<(Account, LoadBalance)> {
    let settings = get_provider_settings(pool, provider).await?;
    let strategy =
        LoadBalance::parse(&settings.load_balance).unwrap_or_default();
    let mut eligible = list_eligible_accounts(pool, provider).await?;
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
    let all = list_accounts(pool, None, None).await?;
    let mut total = 0u64;
    let mut bound = 0u64;
    let mut sealed = 0u64;
    let mut cut = 0u64;
    let mut fallen = 0u64;
    let mut by_provider = serde_json::Map::new();

    for a in &all {
        total += 1;
        match a.status_label() {
            "bound" => bound += 1,
            "sealed" => sealed += 1,
            "cut" => cut += 1,
            "fallen" => fallen += 1,
            _ => {}
        }
        let entry = by_provider
            .entry(a.provider.clone())
            .or_insert_with(|| {
                serde_json::json!({
                    "total": 0, "bound": 0, "sealed": 0, "cut": 0, "fallen": 0
                })
            });
        if let Some(obj) = entry.as_object_mut() {
            *obj.get_mut("total").unwrap() =
                serde_json::json!(obj["total"].as_u64().unwrap_or(0) + 1);
            let key = a.status_label();
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
