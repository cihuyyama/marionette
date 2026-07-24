use crate::error::{AppError, AppResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::path::Path;
use uuid::Uuid;

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
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
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
        }
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
          cooldown_until, last_error, last_used_at, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
          updated_at=excluded.updated_at
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
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_account(pool: &SqlitePool, acc: &Account) -> AppResult<()> {
    sqlx::query(
        r#"
        UPDATE accounts SET
          provider=?, email=?, name=?, is_active=?, priority=?, data=?,
          cooldown_until=?, last_error=?, last_used_at=?, updated_at=?
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
    .bind(&acc.id)
    .execute(pool)
    .await?;
    Ok(())
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

pub async fn pick_account(pool: &SqlitePool, provider: &str) -> AppResult<Account> {
    let now = now_rfc3339();
    let rows = sqlx::query_as::<_, Account>(
        r#"
        SELECT * FROM accounts
        WHERE provider = ?
          AND is_active = 1
          AND (cooldown_until IS NULL OR cooldown_until < ?)
        ORDER BY priority ASC, last_used_at IS NOT NULL, last_used_at ASC, created_at ASC
        LIMIT 8
        "#,
    )
    .bind(provider)
    .bind(&now)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .next()
        .ok_or_else(|| AppError::NoAccounts(provider.into()))
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
