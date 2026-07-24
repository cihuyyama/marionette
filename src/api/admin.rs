use crate::auth::AdminAuth;
use crate::db::{self, Account};
use crate::error::{AppError, AppResult};
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
pub struct PatchBody {
    pub is_active: Option<bool>,
    pub priority: Option<i64>,
    pub clear_cooldown: Option<bool>,
    pub name: Option<String>,
    pub email: Option<String>,
}

pub async fn stats(State(state): State<AppState>, _auth: AdminAuth) -> AppResult<Json<Value>> {
    Ok(Json(db::stats(&state.pool).await?))
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
    Json(body): Json<Value>,
) -> AppResult<Json<Value>> {
    let items = normalize_import_items(&body)?;
    let mut inserted = 0u64;
    let mut updated = 0u64;
    let mut skipped = 0u64;

    for item in items {
        match upsert_import_item(&state, &item).await {
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
        "skipped": skipped
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
