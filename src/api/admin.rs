use crate::auth::AdminAuth;
use crate::db::{self, Account};
use crate::error::{AppError, AppResult, ProviderError};
use crate::farm::{RetryFarmRequest, StartFarmRequest};
use crate::import_util;
use crate::providers::Provider;
use crate::state::AppState;
use axum::{
    Json,
    extract::{Path, Query, State},
};
use futures::stream::{self, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{info, warn};
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
    pub load_balance: Option<String>,
    pub pick_mode: Option<String>,
}

fn provider_settings_json(row: &db::ProviderSettingsRow) -> Value {
    let lb = db::LoadBalance::parse(&row.load_balance).unwrap_or_default();
    let pick = db::QoderPickMode::parse(&row.pick_mode).unwrap_or_default();
    json!({
        "provider": row.provider,
        "load_balance": lb.as_str(),
        "load_balance_label": lb.label(),
        "pick_mode": pick.as_str(),
        "pick_mode_label": pick.label(),
        "sticky_account_id": row.sticky_account_id,
        "rr_cursor": row.rr_cursor,
        "updated_at": row.updated_at,
    })
}

pub async fn list_provider_settings(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> AppResult<Json<Value>> {
    let rows = db::list_provider_settings(&state.pool).await?;
    let providers: Vec<Value> = rows.iter().map(provider_settings_json).collect();
    Ok(Json(json!({
        "providers": providers,
        "strategies": [
            {"id": "round_robin", "label": "Round robin", "hint": "Rotate evenly across healthy accounts"},
            {"id": "sequential", "label": "Sequential", "hint": "Stick to one account until it fails or is sealed"},
            {"id": "least_used", "label": "Least used", "hint": "Prefer the account idle the longest"},
            {"id": "priority", "label": "Priority", "hint": "Lowest priority number first, then least used"},
            {"id": "random", "label": "Random", "hint": "Pick a random healthy account"},
        ],
        "pick_modes": [
            {
                "id": db::QoderPickMode::Normal.as_str(),
                "label": db::QoderPickMode::Normal.label(),
                "hint": db::QoderPickMode::Normal.hint(),
            },
            {
                "id": db::QoderPickMode::UltimateFree.as_str(),
                "label": db::QoderPickMode::UltimateFree.label(),
                "hint": db::QoderPickMode::UltimateFree.hint(),
            },
        ],
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
    if body.load_balance.is_none() && body.pick_mode.is_none() {
        return Err(AppError::BadRequest(
            "provide load_balance and/or pick_mode".into(),
        ));
    }
    if let Some(ref lb) = body.load_balance {
        let strategy = db::LoadBalance::parse(lb).ok_or_else(|| {
            AppError::BadRequest(format!(
                "invalid load_balance: {lb} (use round_robin|sequential|least_used|priority|random)"
            ))
        })?;
        db::set_provider_load_balance(&state.pool, &provider, strategy).await?;
    }
    if let Some(ref mode_s) = body.pick_mode {
        if provider != "qoder" {
            return Err(AppError::BadRequest(
                "pick_mode is only supported for qoder".into(),
            ));
        }
        let mode = db::QoderPickMode::parse(mode_s).ok_or_else(|| {
            AppError::BadRequest(format!(
                "invalid pick_mode: {mode_s} (use normal|ultimate_free)"
            ))
        })?;
        db::set_provider_pick_mode(&state.pool, &provider, mode).await?;
    }
    let row = db::get_provider_settings(&state.pool, &provider).await?;
    Ok(Json(provider_settings_json(&row)))
}

#[derive(Debug, Deserialize)]
pub struct RequestsQuery {
    pub provider: Option<String>,
    pub limit: Option<i64>,
    pub range: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    pub range: Option<String>,
}

fn usage_range_since(range: Option<&str>) -> Option<String> {
    let key = range.unwrap_or("all").trim().to_ascii_lowercase();
    let now = chrono::Utc::now();
    let since = match key.as_str() {
        "day" | "today" | "1d" => Some(now - chrono::Duration::days(1)),
        "week" | "7d" => Some(now - chrono::Duration::days(7)),
        "month" | "30d" => Some(now - chrono::Duration::days(30)),
        "all" | "" => None,
        _ => None,
    };
    since.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn usage_range_label(range: Option<&str>) -> &'static str {
    match range
        .unwrap_or("all")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "day" | "today" | "1d" => "day",
        "week" | "7d" => "week",
        "month" | "30d" => "month",
        _ => "all",
    }
}

pub async fn list_requests(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(q): Query<RequestsQuery>,
) -> AppResult<Json<Value>> {
    let limit = q.limit.unwrap_or(100);
    let range = usage_range_label(q.range.as_deref());
    let since = usage_range_since(Some(range));
    let rows = db::list_request_logs(
        &state.pool,
        q.provider.as_deref(),
        limit,
        since.as_deref(),
    )
    .await?;
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
    Ok(Json(json!({
        "requests": logs,
        "range": range,
        "since": since,
    })))
}

fn parse_stored_body(raw: Option<&str>) -> Value {
    match raw {
        None => Value::Null,
        Some(s) if s.trim().is_empty() => Value::Null,
        Some(s) => serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string())),
    }
}

pub async fn get_request(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let r = db::get_request_log(&state.pool, &id).await?;
    Ok(Json(json!({
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
        "request_body": parse_stored_body(r.request_body.as_deref()),
        "response_body": parse_stored_body(r.response_body.as_deref()),
    })))
}

pub async fn usage(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(q): Query<UsageQuery>,
) -> AppResult<Json<Value>> {
    let range = usage_range_label(q.range.as_deref());
    let since = usage_range_since(Some(range));
    let mut summary = db::usage_summary(&state.pool, since.as_deref()).await?;
    if let Some(obj) = summary.as_object_mut() {
        obj.insert("range".into(), json!(range));
        if let Some(s) = since {
            obj.insert("since".into(), json!(s));
        } else {
            obj.insert("since".into(), Value::Null);
        }
    }
    Ok(Json(summary))
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
            state
                .qoder
                .sync_quota(&mut acc)
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

#[derive(Debug, Deserialize)]
pub struct InjectQuery {
    pub headless: Option<bool>,
    pub refresh: Option<bool>,
}

fn qoder_personal_token(acc: &Account) -> Option<String> {
    let data = acc.data_json();
    for key in ["personalToken", "personal_token", "pat"] {
        if let Some(s) = data.get(key).and_then(|v| v.as_str()) {
            let t = s.trim();
            if !t.is_empty() && !t.contains('…') && !t.contains("...") {
                return Some(t.to_string());
            }
        }
    }
    None
}

pub async fn inject_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
    Query(q): Query<InjectQuery>,
) -> AppResult<Json<Value>> {
    let acc = db::get_account(&state.pool, &id).await?;
    if acc.provider != "qoder" {
        return Err(AppError::BadRequest(
            "dudul inject is only for provider qoder".into(),
        ));
    }
    let pat = qoder_personal_token(&acc).ok_or_else(|| {
        AppError::BadRequest(
            "account data has no personalToken (import a full PAT, not a masked export)"
                .into(),
        )
    })?;

    let headless = q.headless.unwrap_or(true);
    let do_refresh = q.refresh.unwrap_or(true);
    let email = acc.email.clone();

    info!(
        id = %id,
        email = email.as_deref().unwrap_or(""),
        headless,
        "admin dudul inject job start"
    );

    Ok(Json(
        state
            .farm
            .start_inject(&id, &pat, email.as_deref(), headless, do_refresh)
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct BulkInjectBody {
    pub account_ids: Option<Vec<String>>,
    pub include_inactive: Option<bool>,
    pub headless: Option<bool>,
    pub refresh: Option<bool>,
    pub only_no_credit: Option<bool>,
}

fn qoder_needs_inject_credit(acc: &Account) -> bool {
    acc.quota_limit <= 0 || acc.quota_remaining <= 0
}

pub async fn inject_bulk(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(q): Query<InjectQuery>,
    body: Option<Json<BulkInjectBody>>,
) -> AppResult<Json<Value>> {
    let body = body.map(|j| j.0).unwrap_or(BulkInjectBody {
        account_ids: None,
        include_inactive: None,
        headless: None,
        refresh: None,
        only_no_credit: None,
    });
    let include_inactive = body.include_inactive.unwrap_or(false);
    let only_no_credit = body.only_no_credit.unwrap_or(true);
    let headless = body.headless.or(q.headless).unwrap_or(true);
    let do_refresh = body.refresh.or(q.refresh).unwrap_or(true);

    let rows = db::list_accounts(&state.pool, Some("qoder"), None).await?;
    let want: Option<std::collections::HashSet<String>> = body
        .account_ids
        .as_ref()
        .map(|ids| ids.iter().cloned().collect());

    let mut items = Vec::new();
    let mut skipped_no_pat = 0u32;
    let mut skipped_inactive = 0u32;
    let mut skipped_has_credit = 0u32;
    for acc in rows {
        if let Some(ref set) = want {
            if !set.contains(&acc.id) {
                continue;
            }
        } else if !include_inactive && acc.is_active == 0 {
            skipped_inactive += 1;
            continue;
        }
        if only_no_credit && !qoder_needs_inject_credit(&acc) {
            skipped_has_credit += 1;
            continue;
        }
        let Some(pat) = qoder_personal_token(&acc) else {
            skipped_no_pat += 1;
            continue;
        };
        items.push(crate::farm::InjectPatItem {
            account_id: acc.id.clone(),
            pat,
            email: acc.email.clone(),
        });
    }

    if items.is_empty() {
        return Err(AppError::BadRequest(
            "no qoder accounts need inject (not-synced / zero credit + unmasked PAT; active by default)"
                .into(),
        ));
    }

    let total = items.len();
    info!(
        total,
        headless,
        refresh = do_refresh,
        only_no_credit,
        skipped_no_pat,
        skipped_inactive,
        skipped_has_credit,
        "admin dudul inject bulk start"
    );

    let mut out = state
        .farm
        .start_inject_bulk(items, headless, do_refresh)
        .await?;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("total".into(), json!(total));
        obj.insert("only_no_credit".into(), json!(only_no_credit));
        obj.insert("skipped_no_pat".into(), json!(skipped_no_pat));
        obj.insert("skipped_inactive".into(), json!(skipped_inactive));
        obj.insert("skipped_has_credit".into(), json!(skipped_has_credit));
    }
    Ok(Json(out))
}

pub async fn claim_trial_account(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let mut acc = db::get_account(&state.pool, &id).await?;
    if acc.provider != "qoder" {
        return Err(AppError::BadRequest(
            "claim trial is only for provider qoder".into(),
        ));
    }
    let pat = qoder_personal_token(&acc).ok_or_else(|| {
        AppError::BadRequest(
            "account data has no personalToken (import a full PAT, not a masked export)"
                .into(),
        )
    })?;

    info!(
        id = %id,
        email = acc.email.as_deref().unwrap_or(""),
        "admin claim pro trial (openapi user/trial)"
    );

    let (http_status, upstream) = state
        .qoder
        .claim_pro_trial(&pat)
        .await
        .map_err(AppError::from)?;
    let ok = (200..300).contains(&http_status);

    let mut quota_synced = false;
    let mut sync_error: Option<String> = None;
    if ok {
        match state.qoder.ensure_fresh_auth(&mut acc).await {
            Ok(()) => {
                if let Err(e) = state.qoder.sync_quota(&mut acc).await {
                    sync_error = Some(format!("quota sync: {e}"));
                    warn!(id = %id, error = %e, "claim trial ok but quota sync failed");
                } else {
                    quota_synced = true;
                }
                acc.updated_at = db::now_rfc3339();
                if let Err(e) = db::update_account(&state.pool, &acc).await {
                    sync_error = Some(format!("persist: {e}"));
                }
            }
            Err(e) => {
                sync_error = Some(format!("auth refresh after claim: {e}"));
                warn!(id = %id, error = %e, "claim trial ok but auth refresh failed");
            }
        }
    }

    let message = upstream
        .get("message")
        .and_then(|v| v.as_str())
        .or_else(|| upstream.get("msg").and_then(|v| v.as_str()))
        .or_else(|| upstream.get("error").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if ok {
                "trial claim accepted".into()
            } else {
                format!("upstream HTTP {http_status}")
            }
        });

    Ok(Json(json!({
        "ok": ok,
        "http_status": http_status,
        "message": message,
        "upstream": upstream,
        "quota_synced": quota_synced,
        "sync_error": sync_error,
        "account": acc.to_public(),
        "account_id": id,
        "path": "self_claim",
    })))
}

pub async fn inject_get_job(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(state.farm.get_inject_job(&id).await?))
}

pub async fn inject_events(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
    Query(q): Query<FarmEventsQuery>,
) -> AppResult<Json<Value>> {
    Ok(Json(
        state
            .farm
            .inject_events_after(&id, q.after.unwrap_or(0))
            .await?,
    ))
}

pub async fn inject_cancel(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(state.farm.cancel_inject(&id).await?))
}

pub async fn inject_finish_refresh(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let job_wrap = state.farm.get_inject_job(&id).await?;
    let job = job_wrap
        .get("job")
        .cloned()
        .ok_or_else(|| AppError::NotFound(format!("inject job {id}")))?;
    let status = job
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    if status != "succeeded" {
        return Err(AppError::BadRequest(format!(
            "inject job not succeeded (status={status})"
        )));
    }
    let do_refresh = job
        .get("refresh")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let mut account_ids: Vec<String> = job
        .get("account_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if account_ids.is_empty() {
        if let Some(aid) = job.get("account_id").and_then(|v| v.as_str()) {
            if !aid.is_empty() {
                account_ids.push(aid.to_string());
            }
        }
    }
    if account_ids.is_empty() {
        return Err(AppError::BadRequest(
            "inject job missing account_id(s)".into(),
        ));
    }

    if !do_refresh {
        let acc = db::get_account(&state.pool, &account_ids[0]).await?;
        return Ok(Json(json!({
            "ok": true,
            "refreshed": false,
            "refresh_error": null,
            "account": acc.to_public(),
            "accounts_refreshed": 0,
            "accounts_failed": 0,
            "skipped": true,
        })));
    }

    let ok_ids: Option<std::collections::HashSet<String>> = job
        .get("inject_result")
        .and_then(|r| r.get("results"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|row| row.get("ok").and_then(|x| x.as_bool()) == Some(true))
                .filter_map(|row| {
                    row.get("account_id")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string())
                })
                .filter(|s| !s.is_empty())
                .collect()
        });

    let targets: Vec<String> = if let Some(ref set) = ok_ids {
        if set.is_empty() {
            account_ids.clone()
        } else {
            account_ids
                .iter()
                .filter(|id| set.contains(id.as_str()))
                .cloned()
                .collect()
        }
    } else {
        account_ids.clone()
    };

    let mut refreshed_n = 0u32;
    let mut failed_n = 0u32;
    let mut last_error: Option<String> = None;
    let mut last_acc: Option<db::Account> = None;

    for account_id in &targets {
        let mut acc = match db::get_account(&state.pool, account_id).await {
            Ok(a) => a,
            Err(e) => {
                failed_n += 1;
                last_error = Some(e.to_string());
                continue;
            }
        };
        match state.qoder.force_refresh(&mut acc).await {
            Ok(()) => {
                if let Err(e) = state.qoder.sync_quota(&mut acc).await {
                    last_error = Some(format!("quota sync: {e}"));
                    warn!(id = %account_id, error = %e, "inject ok but quota sync failed");
                    failed_n += 1;
                } else {
                    acc.last_error = None;
                    refreshed_n += 1;
                }
            }
            Err(e) => {
                last_error = Some(e.to_string());
                warn!(id = %account_id, error = %e, "inject ok but auth refresh failed");
                failed_n += 1;
            }
        }
        acc.updated_at = db::now_rfc3339();
        if let Err(e) = db::update_account(&state.pool, &acc).await {
            last_error = Some(e.to_string());
            failed_n += 1;
        }
        last_acc = Some(acc);
    }

    let account_public = if let Some(acc) = last_acc {
        acc.to_public()
    } else {
        db::get_account(&state.pool, &account_ids[0])
            .await?
            .to_public()
    };

    Ok(Json(json!({
        "ok": true,
        "refreshed": refreshed_n > 0,
        "refresh_error": last_error,
        "account": account_public,
        "accounts_refreshed": refreshed_n,
        "accounts_failed": failed_n,
        "accounts_targeted": targets.len(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct WarmupQuery {
    pub include_inactive: Option<bool>,
    pub concurrency: Option<usize>,
}

pub async fn warmup_qoder_accounts(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(q): Query<WarmupQuery>,
) -> AppResult<Json<Value>> {
    let include_inactive = q.include_inactive.unwrap_or(false);
    let workers = q
        .concurrency
        .unwrap_or(state.config.refresh_workers)
        .clamp(1, 32);

    let all = db::list_accounts(&state.pool, Some("qoder"), None).await?;
    let candidates: Vec<Account> = all
        .into_iter()
        .filter(|a| include_inactive || a.is_active != 0)
        .collect();
    let total = candidates.len() as u64;

    if total == 0 {
        return Ok(Json(json!({
            "provider": "qoder",
            "total": 0,
            "ok": 0,
            "failed": 0,
            "cut": 0,
            "results": [],
        })));
    }

    info!(total, workers, include_inactive, "qoder warmup start");

    let ok = AtomicU64::new(0);
    let failed = AtomicU64::new(0);
    let cut = AtomicU64::new(0);
    let results = tokio::sync::Mutex::new(Vec::<Value>::with_capacity(total as usize));

    stream::iter(candidates)
        .for_each_concurrent(workers, |mut acc| {
            let state = state.clone();
            let ok = &ok;
            let failed = &failed;
            let cut = &cut;
            let results = &results;
            async move {
                let id = acc.id.clone();
                let email = acc.email.clone();
                let before_limit = acc.quota_limit;
                let before_remaining = acc.quota_remaining;

                let outcome = async {
                    state.qoder.ensure_fresh_auth(&mut acc).await?;
                    state.qoder.sync_quota(&mut acc).await?;
                    Ok::<(), ProviderError>(())
                }
                .await;

                match outcome {
                    Ok(()) => {
                        acc.last_error = None;
                        acc.updated_at = db::now_rfc3339();
                        if let Err(e) = db::update_account(&state.pool, &acc).await {
                            warn!(account = %id, error = %e, "warmup persist failed");
                            failed.fetch_add(1, Ordering::Relaxed);
                            results.lock().await.push(json!({
                                "id": id,
                                "email": email,
                                "ok": false,
                                "error": format!("persist: {e}"),
                            }));
                            return;
                        }
                        ok.fetch_add(1, Ordering::Relaxed);
                        results.lock().await.push(json!({
                            "id": id,
                            "email": email,
                            "ok": true,
                            "quota_limit": acc.quota_limit,
                            "quota_remaining": acc.quota_remaining,
                            "quota_before": { "limit": before_limit, "remaining": before_remaining },
                        }));
                    }
                    Err(ProviderError::AuthInvalid(msg)) => {
                        acc.is_active = 0;
                        acc.last_error = Some(
                            format!("auth invalid: {msg}")
                                .chars()
                                .take(500)
                                .collect(),
                        );
                        acc.updated_at = db::now_rfc3339();
                        let _ = db::update_account(&state.pool, &acc).await;
                        cut.fetch_add(1, Ordering::Relaxed);
                        warn!(account = %id, "warmup cut: auth invalid");
                        results.lock().await.push(json!({
                            "id": id,
                            "email": email,
                            "ok": false,
                            "cut": true,
                            "error": format!("auth invalid: {msg}"),
                        }));
                    }
                    Err(e) => {
                        acc.last_error = Some(e.to_string().chars().take(500).collect());
                        acc.updated_at = db::now_rfc3339();
                        let _ = db::update_account(&state.pool, &acc).await;
                        failed.fetch_add(1, Ordering::Relaxed);
                        warn!(account = %id, error = %e, "warmup failed");
                        results.lock().await.push(json!({
                            "id": id,
                            "email": email,
                            "ok": false,
                            "error": e.to_string(),
                        }));
                    }
                }
            }
        })
        .await;

    let ok_n = ok.load(Ordering::Relaxed);
    let failed_n = failed.load(Ordering::Relaxed);
    let cut_n = cut.load(Ordering::Relaxed);
    let results = results.into_inner();

    info!(total, ok = ok_n, failed = failed_n, cut = cut_n, "qoder warmup done");

    Ok(Json(json!({
        "provider": "qoder",
        "total": total,
        "ok": ok_n,
        "failed": failed_n,
        "cut": cut_n,
        "results": results,
    })))
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
        let mut updated = 0u64;
        let mut skipped = 0u64;
        for acc in accounts {
            match db::upsert_account(&state.pool, &acc).await {
                Ok(db::UpsertKind::Inserted) => inserted += 1,
                Ok(db::UpsertKind::Updated) => updated += 1,
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
            "updated": updated,
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

    let now = db::now_rfc3339();
    let (q_lim, q_rem) = db::default_quota_for_provider(&provider);
    let acc = Account {
        id: item
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
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
    match db::upsert_account(&state.pool, &acc).await? {
        db::UpsertKind::Inserted => Ok(true),
        db::UpsertKind::Updated => Ok(false),
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

#[derive(Debug, Deserialize)]
pub struct FarmEventsQuery {
    pub after: Option<u64>,
}

pub async fn farm_status(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> AppResult<Json<Value>> {
    Ok(Json(state.farm.snapshot().await))
}

pub async fn farm_start(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Json(body): Json<StartFarmRequest>,
) -> AppResult<Json<Value>> {
    Ok(Json(
        state.farm.start(body, state.pool.clone()).await?,
    ))
}

pub async fn farm_get_job(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(state.farm.get_job(&id).await?))
}

pub async fn farm_events(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
    Query(q): Query<FarmEventsQuery>,
) -> AppResult<Json<Value>> {
    Ok(Json(
        state.farm.events_after(&id, q.after.unwrap_or(0)).await?,
    ))
}

pub async fn farm_cancel(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(state.farm.cancel(&id).await?))
}

pub async fn farm_import(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(state.farm.import_job(&id, &state.pool).await?))
}

pub async fn farm_retry_failed(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Path(id): Path<String>,
    body: Result<Json<RetryFarmRequest>, axum::extract::rejection::JsonRejection>,
) -> AppResult<Json<Value>> {
    let opts = body.map(|j| j.0).unwrap_or_default();
    Ok(Json(
        state
            .farm
            .retry_failed(&id, opts, state.pool.clone())
            .await?,
    ))
}
