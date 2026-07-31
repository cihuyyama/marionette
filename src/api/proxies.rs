use crate::auth::AdminAuth;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::proxy::{self, parse_proxy_line};
use crate::state::AppState;
use axum::{
    Json,
    extract::{Path, State},
};
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};

const HEALTH_CONCURRENCY: usize = 16;

pub async fn list_proxies(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> AppResult<Json<Value>> {
    let proxies = db::list_proxies(&state.pool).await?;
    let counts: std::collections::HashMap<String, i64> =
        db::proxy_assignment_counts(&state.pool).await?.into_iter().collect();
    let settings = db::get_proxy_settings(&state.pool).await?;
    let items: Vec<Value> = proxies
        .iter()
        .map(|p| {
            let mut v = p.to_public();
            v["assigned_count"] = json!(counts.get(&p.id).copied().unwrap_or(0));
            v
        })
        .collect();
    let healthy = proxies
        .iter()
        .filter(|p| p.is_active != 0 && p.health == "ok")
        .count();
    Ok(Json(json!({
        "proxies": items,
        "total": proxies.len(),
        "healthy": healthy,
        "settings": {
            "chat_mode": settings.chat_mode,
            "automation_mode": settings.automation_mode,
            "on_dead": settings.on_dead,
        },
    })))
}

#[derive(Debug, Deserialize)]
pub struct ImportProxiesBody {
    pub text: Option<String>,
    #[serde(default)]
    pub proxies: Vec<String>,
    pub scheme: Option<String>,
    pub country: Option<String>,
    pub label: Option<String>,
    pub source: Option<String>,
}

pub async fn import_proxies(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(body): Json<ImportProxiesBody>,
) -> AppResult<Json<Value>> {
    let mut lines: Vec<String> = Vec::new();
    if let Some(t) = &body.text {
        lines.extend(t.lines().map(|s| s.to_string()));
    }
    lines.extend(body.proxies.iter().cloned());

    let source = body.source.as_deref().or(Some("import"));
    let mut inserted = 0u32;
    let mut updated = 0u32;
    let mut skipped = 0u32;
    for line in lines {
        match parse_proxy_line(&line, source) {
            Some(mut p) => {
                if let Some(sc) = &body.scheme {
                    p.scheme = sc.to_ascii_lowercase();
                }
                if body.country.is_some() {
                    p.country = body.country.clone();
                }
                if body.label.is_some() {
                    p.label = body.label.clone();
                }
                let (_, is_new) = db::upsert_proxy(&state.pool, &p).await?;
                if is_new {
                    inserted += 1;
                } else {
                    updated += 1;
                }
            }
            None => skipped += 1,
        }
    }
    state.proxies.invalidate_cache_blocking();
    Ok(Json(json!({
        "inserted": inserted,
        "updated": updated,
        "skipped": skipped,
    })))
}

pub async fn delete_proxy(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    db::delete_proxy(&state.pool, &id).await?;
    state.proxies.invalidate_cache_blocking();
    Ok(Json(json!({ "deleted": true, "id": id })))
}

#[derive(Debug, Deserialize)]
pub struct ToggleBody {
    pub is_active: bool,
}

pub async fn toggle_proxy(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
    Json(body): Json<ToggleBody>,
) -> AppResult<Json<Value>> {
    db::set_proxy_active(&state.pool, &id, body.is_active).await?;
    state.proxies.invalidate_cache_blocking();
    Ok(Json(db::get_proxy(&state.pool, &id).await?.to_public()))
}

pub async fn check_all(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> AppResult<Json<Value>> {
    let proxies = db::list_active_proxies(&state.pool).await?;
    let total = proxies.len();
    let pool = state.pool.clone();
    let results: Vec<bool> = stream::iter(proxies)
        .map(|p| {
            let pool = pool.clone();
            async move {
                let (ok, latency, err) = proxy::check_proxy(&p).await;
                let health = if ok { "ok" } else { "dead" };
                let _ = db::set_proxy_health(&pool, &p.id, health, latency, err.as_deref()).await;
                ok
            }
        })
        .buffer_unordered(HEALTH_CONCURRENCY)
        .collect()
        .await;
    let healthy = results.iter().filter(|&&ok| ok).count();
    Ok(Json(json!({
        "checked": total,
        "healthy": healthy,
        "dead": total - healthy,
    })))
}

pub async fn check_one(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let p = db::get_proxy(&state.pool, &id).await?;
    let (ok, latency, err) = proxy::check_proxy(&p).await;
    let health = if ok { "ok" } else { "dead" };
    db::set_proxy_health(&state.pool, &id, health, latency, err.as_deref()).await?;
    Ok(Json(db::get_proxy(&state.pool, &id).await?.to_public()))
}

#[derive(Debug, Deserialize)]
pub struct AssignBody {
    pub provider: Option<String>,
}

pub async fn assign(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(body): Json<AssignBody>,
) -> AppResult<Json<Value>> {
    let provider = body.provider.unwrap_or_else(|| "grok-cli".to_string());
    let assigned = state.proxies.assign_provider(&provider).await?;
    Ok(Json(json!({ "provider": provider, "assigned": assigned })))
}

pub async fn clear_assignments(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(account_id): Path<String>,
) -> AppResult<Json<Value>> {
    db::clear_account_proxy(&state.pool, &account_id).await?;
    state.proxies.invalidate_cache_blocking();
    Ok(Json(json!({ "cleared": true, "account_id": account_id })))
}

#[derive(Debug, Deserialize)]
pub struct SettingsBody {
    pub chat_mode: Option<String>,
    pub automation_mode: Option<String>,
    pub on_dead: Option<String>,
}

pub async fn get_settings(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> AppResult<Json<Value>> {
    let s = db::get_proxy_settings(&state.pool).await?;
    Ok(Json(json!({
        "chat_mode": s.chat_mode,
        "automation_mode": s.automation_mode,
        "on_dead": s.on_dead,
        "updated_at": s.updated_at,
    })))
}

pub async fn update_settings(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(body): Json<SettingsBody>,
) -> AppResult<Json<Value>> {
    if let Some(m) = &body.chat_mode {
        if !matches!(m.as_str(), "off" | "follow-account" | "rotating") {
            return Err(AppError::BadRequest(format!("invalid chat_mode {m}")));
        }
    }
    if let Some(m) = &body.automation_mode {
        if !matches!(m.as_str(), "off" | "sticky" | "rotating") {
            return Err(AppError::BadRequest(format!("invalid automation_mode {m}")));
        }
    }
    if let Some(m) = &body.on_dead {
        if !matches!(m.as_str(), "direct" | "reassign" | "fail") {
            return Err(AppError::BadRequest(format!("invalid on_dead {m}")));
        }
    }
    let s = db::update_proxy_settings(
        &state.pool,
        body.chat_mode.as_deref(),
        body.automation_mode.as_deref(),
        body.on_dead.as_deref(),
    )
    .await?;
    state.proxies.invalidate_cache_blocking();
    Ok(Json(json!({
        "chat_mode": s.chat_mode,
        "automation_mode": s.automation_mode,
        "on_dead": s.on_dead,
    })))
}
