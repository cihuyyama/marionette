use crate::auth::AdminAuth;
use crate::db::{self, Account};
use crate::error::{AppError, AppResult};
use crate::import_util;
use crate::providers::Provider;
use crate::state::AppState;
use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub provider: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ImportQuery {
    pub replace: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PatchBody {
    pub is_active: Option<bool>,
    pub priority: Option<i64>,
    pub clear_cooldown: Option<bool>,
    pub name: Option<String>,
    pub email: Option<String>,
    pub quota_limit: Option<i64>,
    pub quota_remaining: Option<i64>,
    pub reset_quota: Option<bool>,
}

pub async fn stats(State(state): State<AppState>, _auth: AdminAuth) -> AppResult<Json<Value>> {
    Ok(Json(db::stats(&state.pool).await?))
}

pub async fn connection(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> AppResult<Json<Value>> {
    let host = &state.config.host;
    let port = state.config.port;
    let public_host = if host == "0.0.0.0" || host == "::" {
        "127.0.0.1"
    } else {
        host.as_str()
    };
    Ok(Json(json!({
        "host": host,
        "port": port,
        "base_url": format!("http://{}:{}", public_host, port),
        "pool_key": state.config.api_key,
        "cors_origin": state.config.cors_origin,
    })))
}

#[derive(Debug, Deserialize)]
pub struct ProviderLbBody {
    pub load_balance: String,
}

pub async fn list_provider_settings(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> AppResult<Json<Value>> {
    let rows = db::list_provider_settings(&state.pool).await?;
    let providers: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let lb = db::LoadBalance::parse(&r.load_balance).unwrap_or_default();
            json!({
                "provider": r.provider,
                "load_balance": lb.as_str(),
                "load_balance_label": lb.label(),
                "sticky_account_id": r.sticky_account_id,
                "rr_cursor": r.rr_cursor,
                "updated_at": r.updated_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "providers": providers,
        "strategies": [
            {"id": "round_robin", "label": "Round robin", "hint": "Rotate evenly across healthy accounts"},
            {"id": "sequential", "label": "Sequential", "hint": "Stick to one account until it fails or is sealed"},
            {"id": "least_used", "label": "Least used", "hint": "Prefer the account idle the longest"},
            {"id": "priority", "label": "Priority", "hint": "Lowest priority number first, then least used"},
            {"id": "random", "label": "Random", "hint": "Pick a random healthy account"},
        ]
    })))
}

pub async fn patch_provider_settings(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(provider): Path<String>,
    Json(body): Json<ProviderLbBody>,
) -> AppResult<Json<Value>> {
    if provider != "grok-cli" && provider != "qoder" {
        return Err(AppError::BadRequest(format!("unknown provider: {provider}")));
    }
    let strategy = db::LoadBalance::parse(&body.load_balance).ok_or_else(|| {
        AppError::BadRequest(format!(
            "invalid load_balance: {} (use round_robin|sequential|least_used|priority|random)",
            body.load_balance
        ))
    })?;
    let row = db::set_provider_load_balance(&state.pool, &provider, strategy).await?;
    Ok(Json(json!({
        "provider": row.provider,
        "load_balance": strategy.as_str(),
        "load_balance_label": strategy.label(),
        "sticky_account_id": row.sticky_account_id,
        "rr_cursor": row.rr_cursor,
        "updated_at": row.updated_at,
    })))
}

#[derive(Debug, Deserialize)]
pub struct RequestsQuery {
    pub provider: Option<String>,
    pub limit: Option<i64>,
}

pub async fn list_requests(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(q): Query<RequestsQuery>,
) -> AppResult<Json<Value>> {
    let limit = q.limit.unwrap_or(100);
    let rows = db::list_request_logs(&state.pool, q.provider.as_deref(), limit).await?;
    let logs: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "created_at": r.created_at,
                "provider": r.provider,
                "model": r.model,
                "status": r.status,
                "stream": r.stream != 0,
                "duration_ms": r.duration_ms,
                "prompt_tokens": r.prompt_tokens,
                "completion_tokens": r.completion_tokens,
                "total_tokens": r.total_tokens,
                "credits_used": r.credits_used,
                "account_quota_before": r.account_quota_before,
                "account_quota_after": r.account_quota_after,
                "account_id": r.account_id,
                "account_email": r.account_email,
                "error_message": r.error_message,
            })
        })
        .collect();
    Ok(Json(json!({ "requests": logs })))
}

pub async fn usage(State(state): State<AppState>, _auth: AdminAuth) -> AppResult<Json<Value>> {
    Ok(Json(db::usage_summary(&state.pool).await?))
}

pub async fn list_accounts(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Value>> {
    let rows = db::list_accounts(
        &state.pool,
        q.provider.as_deref(),
        q.status.as_deref(),
    )
    .await?;
    let public: Vec<_> = rows.iter().map(|a| a.to_public()).collect();
    Ok(Json(json!({ "accounts": public })))
}

pub async fn get_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let acc = db::get_account(&state.pool, &id).await?;
    Ok(Json(json!(acc.to_public())))
}

pub async fn patch_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
    Json(body): Json<PatchBody>,
) -> AppResult<Json<Value>> {
    let mut acc = db::get_account(&state.pool, &id).await?;
    if let Some(v) = body.is_active {
        acc.is_active = if v { 1 } else { 0 };
    }
    if let Some(p) = body.priority {
        acc.priority = p;
    }
    if body.clear_cooldown.unwrap_or(false) {
        acc.cooldown_until = None;
    }
    if let Some(n) = body.name {
        acc.name = Some(n);
    }
    if let Some(e) = body.email {
        acc.email = Some(e);
    }
    if body.reset_quota.unwrap_or(false) {
        let (lim, rem) = db::default_quota_for_provider(&acc.provider);
        acc.quota_limit = lim;
        acc.quota_remaining = rem;
    } else {
        if let Some(lim) = body.quota_limit {
            acc.quota_limit = lim.max(0);
        }
        if let Some(rem) = body.quota_remaining {
            acc.quota_remaining = rem.max(0);
            if acc.quota_limit > 0 {
                acc.quota_remaining = acc.quota_remaining.min(acc.quota_limit);
            }
        }
    }
    acc.updated_at = db::now_rfc3339();
    db::update_account(&state.pool, &acc).await?;
    Ok(Json(json!(acc.to_public())))
}

pub async fn delete_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    db::delete_account(&state.pool, &id).await?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

pub async fn refresh_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut acc = db::get_account(&state.pool, &id).await?;
    match acc.provider.as_str() {
        "grok-cli" => {
            state
                .grok
                .ensure_fresh_auth(&mut acc)
                .await
                .map_err(AppError::from)?;
        }
        "qoder" => {
            state
                .qoder
                .ensure_fresh_auth(&mut acc)
                .await
                .map_err(AppError::from)?;
        }
        other => {
            return Err(AppError::BadRequest(format!("unknown provider {other}")));
        }
    }
    acc.updated_at = db::now_rfc3339();
    db::update_account(&state.pool, &acc).await?;
    Ok(Json(json!(acc.to_public())))
}

pub async fn import_accounts(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(q): Query<ImportQuery>,
    body: axum::body::Bytes,
) -> AppResult<Json<Value>> {
    let replace = q.replace.unwrap_or(false);
    if body.is_empty() {
        return Err(AppError::BadRequest("empty import body".into()));
    }
    let value: Value = serde_json::from_slice(&body).map_err(|e| {
        AppError::BadRequest(format!("invalid JSON: {e}"))
    })?;
    run_import(&state, value, replace).await
}

async fn run_import(state: &AppState, body: Value, replace: bool) -> AppResult<Json<Value>> {
    if import_util::is_9router_backup(&body) {
        let accounts = import_util::parse_9router_backup(&body);
        let total_parsed = accounts.len();

        let mut deleted = 0u64;
        if replace {
            deleted = db::delete_accounts_by_providers(
                &state.pool,
                import_util::SUPPORTED_PROVIDERS,
            )
            .await?;
            tracing::info!(deleted, "replace-all: wiped existing accounts");
        }

        let mut inserted = 0u64;
        let mut skipped = 0u64;
        for acc in accounts {
            match db::upsert_account(&state.pool, &acc).await {
                Ok(()) => inserted += 1,
                Err(e) => {
                    tracing::warn!(error = %e, id = %acc.id, "backup import skip");
                    skipped += 1;
                }
            }
        }

        return Ok(Json(json!({
            "source": "9router-backup",
            "parsed": total_parsed,
            "inserted": inserted,
            "updated": 0,
            "skipped": skipped,
            "deleted": deleted,
        })));
    }

    let mut deleted = 0u64;
    if replace {
        deleted = db::delete_accounts_by_providers(
            &state.pool,
            import_util::SUPPORTED_PROVIDERS,
        )
        .await?;
        tracing::info!(deleted, "replace-all: wiped existing accounts");
    }

    let items = normalize_import_items(&body)?;
    let mut inserted = 0u64;
    let mut updated = 0u64;
    let mut skipped = 0u64;

    for item in items {
        match upsert_import_item(state, &item).await {
            Ok(true) => inserted += 1,
            Ok(false) => updated += 1,
            Err(e) => {
                tracing::warn!(error = %e, "import skip");
                skipped += 1;
            }
        }
    }

    Ok(Json(json!({
        "inserted": inserted,
        "updated": updated,
        "skipped": skipped,
        "deleted": deleted,
    })))
}

fn normalize_import_items(body: &Value) -> AppResult<Vec<Value>> {
    if let Some(arr) = body.as_array() {
        return Ok(arr.clone());
    }
    if let Some(arr) = body.get("accounts").and_then(|v| v.as_array()) {
        return Ok(arr.clone());
    }
    if body.get("accessToken").is_some()
        || body.get("access_token").is_some()
        || body.get("personalToken").is_some()
        || body.get("data").is_some()
    {
        return Ok(vec![body.clone()]);
    }
    Err(AppError::BadRequest(
        "expected array, {accounts:[]}, or single account object".into(),
    ))
}

async fn upsert_import_item(state: &AppState, item: &Value) -> AppResult<bool> {
    let provider = item
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            if item.get("personalToken").is_some() || item.get("personal_token").is_some() {
                "qoder"
            } else {
                "grok-cli"
            }
        })
        .to_string();

    let email = item
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let name = item
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let data = if let Some(d) = item.get("data") {
        normalize_token_data(d)
    } else {
        normalize_token_data(item)
    };

    // find existing by email+provider
    let existing = if let Some(ref e) = email {
        let rows = db::list_accounts(&state.pool, Some(&provider), None).await?;
        rows.into_iter().find(|a| a.email.as_deref() == Some(e.as_str()))
    } else {
        None
    };

    let now = db::now_rfc3339();
    if let Some(mut acc) = existing {
        acc.data = data.to_string();
        if name.is_some() {
            acc.name = name;
        }
        acc.updated_at = now;
        db::update_account(&state.pool, &acc).await?;
        Ok(false)
    } else {
        let (q_lim, q_rem) = db::default_quota_for_provider(&provider);
        let acc = Account {
            id: Uuid::new_v4().to_string(),
            provider,
            email,
            name,
            is_active: 1,
            priority: 0,
            data: data.to_string(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: now.clone(),
            updated_at: now,
            quota_limit: q_lim,
            quota_remaining: q_rem,
        };
        db::upsert_account(&state.pool, &acc).await?;
        Ok(true)
    }
}

fn normalize_token_data(v: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Value::Object(m) = v {
        for (k, val) in m {
            // skip meta keys
            if matches!(
                k.as_str(),
                "provider" | "email" | "name" | "id" | "is_active" | "priority"
            ) {
                continue;
            }
            let nk = match k.as_str() {
                "access_token" => "accessToken",
                "refresh_token" => "refreshToken",
                "expires_at" => "expiresAt",
                "expires_in" => "expiresIn",
                "client_id" => "clientId",
                "id_token" => "idToken",
                "personal_token" => "personalToken",
                other => other,
            };
            out.insert(nk.to_string(), val.clone());
        }
    }
    Value::Object(out)
}
