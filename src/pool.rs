use crate::config::Config;
use crate::db::{self, Account, NewRequestLog};
use crate::error::{AppError, AppResult, ProviderError};
use crate::images::{
    ImageResponseFormat, build_images_response, image_provider_id, imagine_to_responses_model,
};
use crate::openai::ChatCompletionRequest;
use crate::providers::{ChatOutcome, Provider};
use crate::state::AppState;
use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, warn};

/// Pool job for OpenAI Images generations/edits (grok-cli only).
pub struct ImageJob {
    pub model: String,
    pub prompt: String,
    pub refs: Vec<String>,
    pub format: ImageResponseFormat,
    pub n: u32,
    pub require_refs: bool,
}

/// Pick grok-cli account, ensure auth, call image generation, apply errors like chat.
pub async fn handle_image(state: &AppState, job: ImageJob) -> AppResult<Value> {
    let provider_id = image_provider_id(&job.model).ok_or_else(|| {
        AppError::BadRequest(format!("unknown or unsupported image model: {}", job.model))
    })?;
    if provider_id != "grok-cli" {
        return Err(AppError::NotImplemented(format!(
            "image generation for provider {provider_id}"
        )));
    }
    if job.require_refs && job.refs.is_empty() {
        return Err(AppError::BadRequest(
            "image edits require at least one image".into(),
        ));
    }

    let model = job.model.clone();
    let started = Instant::now();
    let mut last_err: Option<AppError> = None;
    let mut last_account: Option<Account> = None;
    let mut tried: Vec<String> = Vec::new();
    let responses_model = imagine_to_responses_model(&model);

    for attempt in 0..8 {
        let (mut account, strategy) =
            match db::pick_account(&state.pool, provider_id, &tried, Some(model.as_str())).await {
                Ok(v) => v,
                Err(e) => {
                    if attempt == 0 {
                        let _ = log_request(
                            &state.pool,
                            provider_id,
                            &model,
                            "error",
                            false,
                            started.elapsed().as_millis() as i64,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            Some(e.to_string()),
                        )
                        .await;
                        return Err(e);
                    }
                    break;
                }
            };

        tried.push(account.id.clone());
        info!(
            account = %account.id,
            provider = provider_id,
            strategy = strategy.as_str(),
            attempt,
            "picked account for image"
        );

        if let Err(e) = state.grok.ensure_fresh_auth(&mut account).await {
            apply_provider_error(&state.pool, &state.config, &mut account, &e).await?;
            let _ = db::note_pick_failure(&state.pool, provider_id, strategy, &account.id).await;
            last_account = Some(account);
            last_err = Some(e.into());
            continue;
        }
        account.updated_at = db::now_rfc3339();
        db::update_account(&state.pool, &account).await?;

        match state
            .grok
            .generate_image(
                &account,
                responses_model,
                &job.prompt,
                &job.refs,
                job.n,
            )
            .await
        {
            Ok(b64_list) => {
                if b64_list.is_empty() {
                    let e = ProviderError::Other("no image in upstream response".into());
                    warn!(account = %account.id, "image generation returned empty");
                    apply_provider_error(&state.pool, &state.config, &mut account, &e).await?;
                    let _ = db::note_pick_failure(&state.pool, provider_id, strategy, &account.id)
                        .await;
                    last_account = Some(account);
                    last_err = Some(e.into());
                    continue;
                }
                let duration_ms = started.elapsed().as_millis() as i64;
                let _ = log_request(
                    &state.pool,
                    provider_id,
                    &model,
                    "success",
                    false,
                    duration_ms,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(&account),
                    None,
                )
                .await;
                let _ = db::note_pick_success(&state.pool, provider_id, strategy, &account.id).await;
                account.last_used_at = Some(db::now_rfc3339());
                account.last_error = None;
                account.updated_at = db::now_rfc3339();
                db::update_account(&state.pool, &account).await?;
                let created = Utc::now().timestamp();
                return Ok(build_images_response(created, &b64_list, job.format));
            }
            Err(e) => {
                if should_retry_same_account(provider_id, &e, false) {
                    warn!(account = %account.id, error = %e, "image auth expired; force_refresh + retry");
                    match state.grok.force_refresh(&mut account).await {
                        Ok(()) => {
                            account.updated_at = db::now_rfc3339();
                            let _ = db::update_account(&state.pool, &account).await;
                            match state
                                .grok
                                .generate_image(
                                    &account,
                                    responses_model,
                                    &job.prompt,
                                    &job.refs,
                                    job.n,
                                )
                                .await
                            {
                                Ok(b64_list) if !b64_list.is_empty() => {
                                    let duration_ms = started.elapsed().as_millis() as i64;
                                    let _ = log_request(
                                        &state.pool,
                                        provider_id,
                                        &model,
                                        "success",
                                        false,
                                        duration_ms,
                                        None,
                                        None,
                                        None,
                                        None,
                                        None,
                                        None,
                                        Some(&account),
                                        None,
                                    )
                                    .await;
                                    let _ = db::note_pick_success(
                                        &state.pool,
                                        provider_id,
                                        strategy,
                                        &account.id,
                                    )
                                    .await;
                                    account.last_used_at = Some(db::now_rfc3339());
                                    account.last_error = None;
                                    account.updated_at = db::now_rfc3339();
                                    db::update_account(&state.pool, &account).await?;
                                    let created = Utc::now().timestamp();
                                    return Ok(build_images_response(
                                        created,
                                        &b64_list,
                                        job.format,
                                    ));
                                }
                                Ok(_) => {
                                    let e2 = ProviderError::Other(
                                        "no image in upstream response after refresh".into(),
                                    );
                                    apply_provider_error(
                                        &state.pool,
                                        &state.config,
                                        &mut account,
                                        &e2,
                                    )
                                    .await?;
                                    let _ = db::note_pick_failure(
                                        &state.pool,
                                        provider_id,
                                        strategy,
                                        &account.id,
                                    )
                                    .await;
                                    last_account = Some(account);
                                    last_err = Some(e2.into());
                                    continue;
                                }
                                Err(e2) => {
                                    apply_provider_error(
                                        &state.pool,
                                        &state.config,
                                        &mut account,
                                        &e2,
                                    )
                                    .await?;
                                    let _ = db::note_pick_failure(
                                        &state.pool,
                                        provider_id,
                                        strategy,
                                        &account.id,
                                    )
                                    .await;
                                    last_account = Some(account);
                                    last_err = Some(e2.into());
                                    continue;
                                }
                            }
                        }
                        Err(re) => {
                            apply_provider_error(&state.pool, &state.config, &mut account, &re)
                                .await?;
                            let _ = db::note_pick_failure(
                                &state.pool,
                                provider_id,
                                strategy,
                                &account.id,
                            )
                            .await;
                            last_account = Some(account);
                            last_err = Some(re.into());
                            continue;
                        }
                    }
                }
                warn!(account = %account.id, error = %e, "upstream image failed");
                apply_provider_error(&state.pool, &state.config, &mut account, &e).await?;
                let _ = db::note_pick_failure(&state.pool, provider_id, strategy, &account.id).await;
                last_account = Some(account);
                last_err = Some(e.into());
            }
        }
    }

    let err = last_err.unwrap_or_else(|| AppError::NoAccounts(provider_id.into()));
    let _ = log_request(
        &state.pool,
        provider_id,
        &model,
        "error",
        false,
        started.elapsed().as_millis() as i64,
        None,
        None,
        None,
        None,
        None,
        None,
        last_account.as_ref(),
        Some(err.to_string()),
    )
    .await;
    Err(err)
}

pub async fn handle_chat(
    state: &AppState,
    req: ChatCompletionRequest,
) -> AppResult<ChatOutcome> {
    let provider_id = req
        .provider_id()
        .ok_or_else(|| AppError::BadRequest(format!("unknown model: {}", req.model)))?;

    let provider: Arc<dyn Provider> = match provider_id {
        "grok-cli" => state.grok.clone() as Arc<dyn Provider>,
        "qoder" => state.qoder.clone() as Arc<dyn Provider>,
        other => return Err(AppError::NotImplemented(other.into())),
    };

    let model = req.model.clone();
    let started = Instant::now();
    let mut last_err: Option<AppError> = None;
    let mut last_account: Option<Account> = None;
    let mut tried: Vec<String> = Vec::new();

    for attempt in 0..8 {
        let (mut account, strategy) =
            match db::pick_account(&state.pool, provider_id, &tried, Some(model.as_str())).await {
                Ok(v) => v,
                Err(e) => {
                    if attempt == 0 {
                        let _ = log_request(
                            &state.pool,
                            provider_id,
                            &model,
                            "error",
                            false,
                            started.elapsed().as_millis() as i64,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            Some(e.to_string()),
                        )
                        .await;
                        return Err(e);
                    }
                    break;
                }
            };

        tried.push(account.id.clone());
        info!(
            account = %account.id,
            provider = provider_id,
            strategy = strategy.as_str(),
            attempt,
            "picked account"
        );

        if let Err(e) = provider.ensure_fresh_auth(&mut account).await {
            apply_provider_error(&state.pool, &state.config, &mut account, &e).await?;
            let _ = db::note_pick_failure(&state.pool, provider_id, strategy, &account.id).await;
            last_account = Some(account);
            last_err = Some(e.into());
            continue;
        }
        if should_server_resync_quota(provider_id) && account.quota_limit == 0 {
            if let Err(e) = provider.sync_quota(&mut account).await {
                warn!(account = %account.id, error = %e, "pre-chat quota sync failed");
            }
        }
        account.updated_at = db::now_rfc3339();
        db::update_account(&state.pool, &account).await?;

        match provider.chat(&account, &req).await {
            Ok(outcome) => {
                return handle_chat_success(
                    state,
                    provider.clone(),
                    provider_id,
                    &model,
                    strategy,
                    started,
                    account,
                    outcome,
                )
                .await;
            }
            Err(e) => {
                if should_retry_same_account(provider_id, &e, false) {
                    warn!(account = %account.id, provider = provider_id, error = %e, "auth expired; force_refresh + retry");
                    match provider.force_refresh(&mut account).await {
                        Ok(()) => {
                            account.updated_at = db::now_rfc3339();
                            if let Err(db_e) = db::update_account(&state.pool, &account).await {
                                warn!(account = %account.id, error = %db_e, "persist after force_refresh failed");
                            }
                            match provider.chat(&account, &req).await {
                                Ok(outcome) => {
                                    return handle_chat_success(
                                        state,
                                        provider.clone(),
                                        provider_id,
                                        &model,
                                        strategy,
                                        started,
                                        account,
                                        outcome,
                                    )
                                    .await;
                                }
                                Err(e2) => {
                                    warn!(account = %account.id, provider = provider_id, error = %e2, "retry after force_refresh still failed");
                                    apply_provider_error(&state.pool, &state.config, &mut account, &e2).await?;
                                    let _ = db::note_pick_failure(&state.pool, provider_id, strategy, &account.id).await;
                                    last_account = Some(account);
                                    last_err = Some(e2.into());
                                    continue;
                                }
                            }
                        }
                        Err(re) => {
                            warn!(account = %account.id, provider = provider_id, error = %re, "force_refresh failed");
                            apply_provider_error(&state.pool, &state.config, &mut account, &re).await?;
                            let _ = db::note_pick_failure(&state.pool, provider_id, strategy, &account.id).await;
                            last_account = Some(account);
                            last_err = Some(re.into());
                            continue;
                        }
                    }
                }
                warn!(account = %account.id, error = %e, "upstream chat failed");
                apply_provider_error(&state.pool, &state.config, &mut account, &e).await?;
                let _ = db::note_pick_failure(&state.pool, provider_id, strategy, &account.id).await;
                last_account = Some(account);
                last_err = Some(e.into());
            }
        }
    }

    let err = last_err.unwrap_or_else(|| AppError::NoAccounts(provider_id.into()));
    let _ = log_request(
        &state.pool,
        provider_id,
        &model,
        "error",
        false,
        started.elapsed().as_millis() as i64,
        None,
        None,
        None,
        None,
        None,
        None,
        last_account.as_ref(),
        Some(err.to_string()),
    )
    .await;
    Err(err)
}

fn should_retry_same_account(
    provider_id: &str,
    err: &ProviderError,
    already_retried: bool,
) -> bool {
    matches!(provider_id, "grok-cli" | "qoder")
        && matches!(err, ProviderError::AuthExpired)
        && !already_retried
}

fn should_local_token_decrement(provider_id: &str) -> bool {
    provider_id == "grok-cli"
}

fn should_server_resync_quota(provider_id: &str) -> bool {
    provider_id == "qoder"
}

async fn qoder_should_optimistic_free(
    state: &AppState,
    provider_id: &str,
    account: &Account,
    model: &str,
) -> bool {
    if provider_id != "qoder" {
        return false;
    }
    if !crate::providers::qoder::is_ultimate_free_activity_model(model) {
        return false;
    }
    if crate::providers::qoder::free_remaining_for_account_model(account, model) <= 0 {
        return false;
    }
    if !account.has_quota_budget() || account.quota_remaining <= 0 {
        return true;
    }
    match db::get_provider_settings(&state.pool, "qoder").await {
        Ok(s) => {
            db::QoderPickMode::parse(&s.pick_mode).unwrap_or_default()
                == db::QoderPickMode::UltimateFree
        }
        Err(_) => false,
    }
}

async fn qoder_free_path_cap(
    state: &AppState,
    provider_id: &str,
    account: &mut Account,
    model: &str,
) -> Option<i64> {
    if !qoder_should_optimistic_free(state, provider_id, account, model).await {
        return None;
    }
    crate::providers::qoder::optimistic_consume_free_call(account, model)
}

async fn handle_chat_success(
    state: &AppState,
    provider: Arc<dyn Provider>,
    provider_id: &str,
    model: &str,
    strategy: db::LoadBalance,
    started: Instant,
    mut account: Account,
    outcome: ChatOutcome,
) -> AppResult<ChatOutcome> {
    let duration_ms = started.elapsed().as_millis() as i64;
    match outcome {
        ChatOutcome::Json(v) => {
            let (prompt, completion, total) = extract_usage(&v);
            let token_spend = total.or_else(|| match (prompt, completion) {
                (Some(p), Some(c)) => Some(p + c),
                (Some(p), None) => Some(p),
                (None, Some(c)) => Some(c),
                _ => None,
            });
            let (q_before, q_after, used) = apply_success_quota(
                state,
                provider.as_ref(),
                provider_id,
                &mut account,
                token_spend,
                model,
            )
            .await;
            let _ = log_request(
                &state.pool,
                provider_id,
                model,
                "success",
                false,
                duration_ms,
                prompt,
                completion,
                total,
                if used > 0 { Some(used) } else { None },
                if account.has_quota_budget() || q_before != q_after {
                    Some(q_before)
                } else {
                    None
                },
                if account.has_quota_budget() || q_before != q_after {
                    Some(q_after)
                } else {
                    None
                },
                Some(&account),
                None,
            )
            .await;
            let _ = db::note_pick_success(&state.pool, provider_id, strategy, &account.id).await;
            account.last_used_at = Some(db::now_rfc3339());
            account.last_error = None;
            account.updated_at = db::now_rfc3339();
            db::update_account(&state.pool, &account).await?;
            Ok(ChatOutcome::Json(v))
        }
        ChatOutcome::Stream {
            response,
            usage_rx,
        } => {
            let log_id = match log_request_returning_id(
                &state.pool,
                provider_id,
                model,
                "success",
                true,
                duration_ms,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(&account),
                None,
            )
            .await
            {
                Ok(id) => id,
                Err(e) => {
                    warn!(error = %e, "stream request log insert failed");
                    String::new()
                }
            };
            let _ = db::note_pick_success(&state.pool, provider_id, strategy, &account.id).await;
            account.last_used_at = Some(db::now_rfc3339());
            account.last_error = None;
            account.updated_at = db::now_rfc3339();
            db::update_account(&state.pool, &account).await?;

            let free_cap_pre = qoder_free_path_cap(state, provider_id, &mut account, model).await;
            if free_cap_pre.is_some() {
                account.updated_at = db::now_rfc3339();
                let _ = db::update_account(&state.pool, &account).await;
            }
            let pool = state.pool.clone();
            let account_id = account.id.clone();
            let stream_model = model.to_string();
            let local_decr = should_local_token_decrement(provider_id);
            let server_sync = should_server_resync_quota(provider_id);
            let started_at = started;
            tokio::spawn(async move {
                let usage = match usage_rx.await {
                    Ok(Some(u)) => u.normalized(),
                    Ok(None) | Err(_) => {
                        if server_sync {
                            if let Ok(mut acc) = db::get_account(&pool, &account_id).await {
                                match provider.sync_quota(&mut acc).await {
                                    Ok(()) => {
                                        if let Some(cap) = free_cap_pre {
                                            crate::providers::qoder::cap_free_remaining_after_sync(
                                                &mut acc,
                                                &stream_model,
                                                cap,
                                            );
                                        }
                                        acc.updated_at = db::now_rfc3339();
                                        let _ = db::update_account(&pool, &acc).await;
                                    }
                                    Err(e) => {
                                        warn!(account = %account_id, error = %e, "stream end quota sync failed");
                                    }
                                }
                            }
                        }
                        return;
                    }
                };
                if log_id.is_empty() && !server_sync {
                    return;
                }
                let total = if usage.total_tokens > 0 {
                    usage.total_tokens
                } else {
                    usage.prompt_tokens + usage.completion_tokens
                };
                let mut credits_used = None;
                let mut q_before = None;
                let mut q_after = None;
                if local_decr && total > 0 {
                    if let Ok((before, after, used)) =
                        db::decrement_quota(&pool, &account_id, total).await
                    {
                        if used > 0 {
                            credits_used = Some(used);
                            q_before = Some(before);
                            q_after = Some(after);
                        }
                    }
                } else if server_sync {
                    if let Ok(mut acc) = db::get_account(&pool, &account_id).await {
                        let before = acc.quota_remaining;
                        match provider.sync_quota(&mut acc).await {
                            Ok(()) => {
                                if let Some(cap) = free_cap_pre {
                                    crate::providers::qoder::cap_free_remaining_after_sync(
                                        &mut acc,
                                        &stream_model,
                                        cap,
                                    );
                                }
                                let after = acc.quota_remaining;
                                let used = (before - after).max(0);
                                if used > 0 {
                                    credits_used = Some(used);
                                }
                                q_before = Some(before);
                                q_after = Some(after);
                                acc.updated_at = db::now_rfc3339();
                                let _ = db::update_account(&pool, &acc).await;
                            }
                            Err(e) => {
                                warn!(account = %account_id, error = %e, "stream end quota sync failed");
                            }
                        }
                    }
                }
                if !log_id.is_empty() && !usage.is_empty() {
                    let full_ms = started_at.elapsed().as_millis() as i64;
                    let _ = db::update_request_log_usage(
                        &pool,
                        &log_id,
                        usage.prompt_tokens,
                        usage.completion_tokens,
                        total,
                        credits_used,
                        q_before,
                        q_after,
                        Some(full_ms),
                    )
                    .await;
                }
            });

            let (_tx, dummy_rx) = tokio::sync::oneshot::channel();
            Ok(ChatOutcome::Stream {
                response,
                usage_rx: dummy_rx,
            })
        }
    }
}

async fn apply_success_quota(
    state: &AppState,
    provider: &dyn Provider,
    provider_id: &str,
    account: &mut Account,
    total_tokens: Option<i64>,
    model: &str,
) -> (i64, i64, i64) {
    if should_local_token_decrement(provider_id) {
        let credits = if account.has_quota_budget() {
            total_tokens
        } else {
            None
        };
        if let Some(c) = credits.filter(|c| *c > 0) {
            return db::decrement_quota(&state.pool, &account.id, c)
                .await
                .map(|(b, a, u)| {
                    if u > 0 {
                        account.quota_remaining = a;
                    }
                    (b, a, u)
                })
                .unwrap_or((account.quota_remaining, account.quota_remaining, 0));
        }
        return (account.quota_remaining, account.quota_remaining, 0);
    }

    if should_server_resync_quota(provider_id) {
        let free_cap = qoder_free_path_cap(state, provider_id, account, model).await;
        let before = account.quota_remaining;
        match provider.sync_quota(account).await {
            Ok(()) => {
                if let Some(cap) = free_cap {
                    crate::providers::qoder::cap_free_remaining_after_sync(account, model, cap);
                }
                let after = account.quota_remaining;
                let used = (before - after).max(0);
                return (before, after, used);
            }
            Err(e) => {
                warn!(account = %account.id, error = %e, "post-chat quota sync failed");
                return (before, before, 0);
            }
        }
    }

    (account.quota_remaining, account.quota_remaining, 0)
}

fn extract_usage(v: &Value) -> (Option<i64>, Option<i64>, Option<i64>) {
    let usage = match v.get("usage") {
        Some(u) => u,
        None => return (None, None, None),
    };
    let as_i64 = |key: &str| {
        usage
            .get(key)
            .and_then(|x| x.as_i64().or_else(|| x.as_u64().map(|n| n as i64)))
    };
    let prompt = as_i64("prompt_tokens").or_else(|| as_i64("input_tokens"));
    let completion = as_i64("completion_tokens").or_else(|| as_i64("output_tokens"));
    let total = as_i64("total_tokens").or_else(|| match (prompt, completion) {
        (Some(p), Some(c)) => Some(p + c),
        _ => None,
    });
    (prompt, completion, total)
}

async fn log_request(
    pool: &SqlitePool,
    provider: &str,
    model: &str,
    status: &str,
    stream: bool,
    duration_ms: i64,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    total_tokens: Option<i64>,
    credits_used: Option<i64>,
    account_quota_before: Option<i64>,
    account_quota_after: Option<i64>,
    account: Option<&Account>,
    error_message: Option<String>,
) -> AppResult<()> {
    let _ = log_request_returning_id(
        pool,
        provider,
        model,
        status,
        stream,
        duration_ms,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        credits_used,
        account_quota_before,
        account_quota_after,
        account,
        error_message,
    )
    .await?;
    Ok(())
}

async fn log_request_returning_id(
    pool: &SqlitePool,
    provider: &str,
    model: &str,
    status: &str,
    stream: bool,
    duration_ms: i64,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    total_tokens: Option<i64>,
    credits_used: Option<i64>,
    account_quota_before: Option<i64>,
    account_quota_after: Option<i64>,
    account: Option<&Account>,
    error_message: Option<String>,
) -> AppResult<String> {
    db::insert_request_log(
        pool,
        NewRequestLog {
            provider: provider.into(),
            model: Some(model.into()),
            status: status.into(),
            stream,
            duration_ms: Some(duration_ms),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            credits_used,
            account_quota_before,
            account_quota_after,
            account_id: account.map(|a| a.id.clone()),
            account_email: account.and_then(|a| a.email.clone()),
            error_message: error_message.map(|e| e.chars().take(500).collect()),
        },
    )
    .await
}

async fn apply_provider_error(
    pool: &SqlitePool,
    config: &Config,
    account: &mut Account,
    err: &ProviderError,
) -> AppResult<()> {
    account.updated_at = db::now_rfc3339();
    account.last_error = Some(err.to_string().chars().take(500).collect());

    match err {
        ProviderError::RateLimited { retry_after_secs } => {
            let hours = config.cooldown_hours as i64;
            let until = if let Some(s) = retry_after_secs {
                Utc::now() + Duration::seconds(*s as i64)
            } else {
                Utc::now() + Duration::hours(hours)
            };
            account.cooldown_until =
                Some(until.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
            info!(account = %account.id, until = ?account.cooldown_until, "sealed (429 cooldown)");
        }
        ProviderError::AuthExpired => {
            let hours = config.auth_cooldown_hours as i64;
            let until = Utc::now() + Duration::hours(hours);
            account.cooldown_until =
                Some(until.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
            info!(account = %account.id, until = ?account.cooldown_until, "sealed (auth expired)");
        }
        ProviderError::AuthInvalid(_) | ProviderError::AccessDenied => {
            account.is_active = 0;
            info!(account = %account.id, "cut (disabled)");
        }
        ProviderError::PaymentRequired
        | ProviderError::Upstream { status: 402, .. } => {
            let hours = config.cooldown_hours as i64;
            let until = Utc::now() + Duration::hours(hours);
            account.cooldown_until =
                Some(until.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
            if account.has_quota_budget() {
                account.quota_remaining = 0;
            }
            info!(
                account = %account.id,
                until = ?account.cooldown_until,
                "sealed (402/payment credit block; quota zeroed, not cut)"
            );
        }
        ProviderError::Upstream { status, .. } if *status == 403 => {
            account.is_active = 0;
            info!(account = %account.id, "cut (403 access denied)");
        }
        other => {
            info!(
                account = %account.id,
                error = %other,
                "fallen (unclassified provider error; last_error kept)"
            );
        }
    }

    db::update_account(pool, account).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_qoder_authexpired_first_time() {
        assert!(should_retry_same_account("qoder", &ProviderError::AuthExpired, false));
    }

    #[test]
    fn retry_grok_authexpired_first_time() {
        assert!(should_retry_same_account("grok-cli", &ProviderError::AuthExpired, false));
    }

    #[test]
    fn no_retry_grok_already_retried() {
        assert!(!should_retry_same_account("grok-cli", &ProviderError::AuthExpired, true));
    }

    #[test]
    fn no_retry_qoder_already_retried() {
        assert!(!should_retry_same_account("qoder", &ProviderError::AuthExpired, true));
    }

    #[test]
    fn no_retry_qoder_ratelimited() {
        assert!(!should_retry_same_account(
            "qoder",
            &ProviderError::RateLimited { retry_after_secs: None },
            false
        ));
    }

    #[test]
    fn no_retry_qoder_accessdenied() {
        assert!(!should_retry_same_account("qoder", &ProviderError::AccessDenied, false));
    }

    #[test]
    fn grok_local_token_decrement_qoder_server_resync() {
        assert!(should_local_token_decrement("grok-cli"));
        assert!(!should_local_token_decrement("qoder"));
        assert!(should_server_resync_quota("qoder"));
        assert!(!should_server_resync_quota("grok-cli"));
    }
}
