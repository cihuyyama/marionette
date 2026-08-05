use crate::auth::AdminAuth;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::{
    Json,
    extract::{Path, State},
};
use rand::Rng;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::info;

#[derive(Debug, serde::Serialize)]
pub struct ApiKeyView {
    pub id: String,
    pub name: Option<String>,
    pub key_prefix: Option<String>,
    pub is_active: bool,
    pub rate_limit_rpm: Option<i64>,
    pub request_limit: Option<i64>,
    pub requests_used: i64,
    pub token_limit: Option<i64>,
    pub tokens_used: i64,
    pub model_allowlist: Option<Vec<String>>,
    pub last_used_at: Option<String>,
    pub created_at: String,
}

fn to_view(row: db::ApiKeyRow) -> ApiKeyView {
    let allowlist = row.allowlist_entries();
    let is_active = row.is_enabled();
    ApiKeyView {
        id: row.id,
        name: row.name,
        key_prefix: row.key_prefix,
        is_active,
        rate_limit_rpm: row.rate_limit_rpm,
        request_limit: row.request_limit,
        requests_used: row.requests_used,
        token_limit: row.token_limit,
        tokens_used: row.tokens_used,
        model_allowlist: if allowlist.is_empty() {
            None
        } else {
            Some(allowlist)
        },
        last_used_at: row.last_used_at,
        created_at: row.created_at,
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateApiKeyBody {
    pub name: Option<String>,
    pub rate_limit_rpm: Option<i64>,
    pub request_limit: Option<i64>,
    pub token_limit: Option<i64>,
    pub model_allowlist: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct PatchApiKeyBody {
    pub name: Option<String>,
    pub is_active: Option<bool>,
    pub rate_limit_rpm: Option<Option<i64>>,
    pub request_limit: Option<Option<i64>>,
    pub token_limit: Option<Option<i64>>,
    pub model_allowlist: Option<Option<Vec<String>>>,
}

fn validate_limits(
    rate_limit_rpm: Option<i64>,
    request_limit: Option<i64>,
    token_limit: Option<i64>,
) -> AppResult<()> {
    for (label, v) in [
        ("rate_limit_rpm", rate_limit_rpm),
        ("request_limit", request_limit),
        ("token_limit", token_limit),
    ] {
        if let Some(n) = v {
            if n < 0 {
                return Err(AppError::BadRequest(format!("{label} must be >= 0")));
            }
        }
    }
    Ok(())
}

fn validate_allowlist(list: Option<&[String]>) -> AppResult<()> {
    if let Some(entries) = list {
        for e in entries {
            if e.trim().is_empty() {
                return Err(AppError::BadRequest(
                    "model_allowlist entries must be non-empty".into(),
                ));
            }
        }
    }
    Ok(())
}

fn generate_key() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes);
    format!("mk-{}", hex::encode(bytes))
}

pub async fn list_keys(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> AppResult<Json<Value>> {
    let keys = db::list_api_keys(&state.pool)
        .await?
        .into_iter()
        .map(to_view)
        .collect::<Vec<_>>();
    Ok(Json(json!({ "keys": keys })))
}

pub async fn create_key(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(body): Json<CreateApiKeyBody>,
) -> AppResult<Json<Value>> {
    validate_limits(body.rate_limit_rpm, body.request_limit, body.token_limit)?;
    validate_allowlist(body.model_allowlist.as_deref())?;
    let plaintext = generate_key();
    let prefix: String = plaintext.chars().take(8).collect();
    let row = db::create_api_key(
        &state.pool,
        db::NewApiKey {
            key_hash: crate::auth::hash_key(&plaintext),
            key_prefix: prefix,
            name: body.name.clone(),
            rate_limit_rpm: body.rate_limit_rpm,
            request_limit: body.request_limit,
            token_limit: body.token_limit,
            model_allowlist: body.model_allowlist,
        },
    )
    .await?;
    info!(key_id = %row.id, name = ?body.name, "api key created");
    Ok(Json(json!({ "key": plaintext, "key_view": to_view(row) })))
}

pub async fn patch_key(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
    Json(body): Json<PatchApiKeyBody>,
) -> AppResult<Json<Value>> {
    validate_limits(
        body.rate_limit_rpm.flatten(),
        body.request_limit.flatten(),
        body.token_limit.flatten(),
    )?;
    if let Some(Some(list)) = body.model_allowlist.as_ref() {
        validate_allowlist(Some(list))?;
    }
    if let Some(name) = body.name.as_ref() {
        if name.trim().is_empty() {
            return Err(AppError::BadRequest("name must be non-empty".into()));
        }
    }
    let Some(row) = db::update_api_key(
        &state.pool,
        &id,
        db::UpdateApiKey {
            name: body.name.map(Some),
            is_active: body.is_active,
            rate_limit_rpm: body.rate_limit_rpm,
            request_limit: body.request_limit,
            token_limit: body.token_limit,
            model_allowlist: body.model_allowlist,
        },
    )
    .await?
    else {
        return Err(AppError::NotFound(format!("api key {id}")));
    };
    if let Some(active) = body.is_active {
        info!(key_id = %id, active, "api key active state changed");
    }
    Ok(Json(json!({ "key": to_view(row) })))
}

pub async fn delete_key(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    if !db::delete_api_key(&state.pool, &id).await? {
        return Err(AppError::NotFound(format!("api key {id}")));
    }
    info!(key_id = %id, "api key deleted");
    Ok(Json(json!({ "deleted": id })))
}

pub async fn key_usage(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    db::get_api_key(&state.pool, &id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("api key {id}")))?;
    let mut usage = db::api_key_usage(&state.pool, &id).await?;
    if let Some(obj) = usage.as_object_mut() {
        obj.insert("key_id".into(), json!(id));
    }
    Ok(Json(usage))
}
