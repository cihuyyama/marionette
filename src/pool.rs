use crate::config::Config;
use crate::db::{self, Account, NewRequestLog};
use crate::error::{AppError, AppResult, ProviderError};
use crate::openai::ChatCompletionRequest;
use crate::providers::{ChatOutcome, Provider};
use crate::state::AppState;
use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, warn};

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

    for attempt in 0..3 {
        let (mut account, strategy) =
            match db::pick_account(&state.pool, provider_id, &tried).await {
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
        account.updated_at = db::now_rfc3339();
        db::update_account(&state.pool, &account).await?;

        match provider.chat(&account, &req).await {
            Ok(outcome) => {
                let duration_ms = started.elapsed().as_millis() as i64;
                match outcome {
                    ChatOutcome::Json(v) => {
                        let (prompt, completion, total) = extract_usage(&v);
                        let credits = if account.has_quota_budget() {
                            total.or_else(|| match (prompt, completion) {
                                (Some(p), Some(c)) => Some(p + c),
                                (Some(p), None) => Some(p),
                                (None, Some(c)) => Some(c),
                                _ => None,
                            })
                        } else {
                            None
                        };
                        let (q_before, q_after, used) = if let Some(c) = credits.filter(|c| *c > 0)
                        {
                            db::decrement_quota(&state.pool, &account.id, c)
                                .await
                                .unwrap_or((account.quota_remaining, account.quota_remaining, 0))
                        } else {
                            (account.quota_remaining, account.quota_remaining, 0)
                        };
                        if used > 0 {
                            account.quota_remaining = q_after;
                        }
                        let _ = log_request(
                            &state.pool,
                            provider_id,
                            &model,
                            "success",
                            false,
                            duration_ms,
                            prompt,
                            completion,
                            total,
                            if used > 0 { Some(used) } else { None },
                            if account.has_quota_budget() {
                                Some(q_before)
                            } else {
                                None
                            },
                            if account.has_quota_budget() {
                                Some(q_after)
                            } else {
                                None
                            },
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
                        return Ok(ChatOutcome::Json(v));
                    }
                    ChatOutcome::Stream {
                        response,
                        usage_rx,
                    } => {
                        let log_id = match log_request_returning_id(
                            &state.pool,
                            provider_id,
                            &model,
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

                        let pool = state.pool.clone();
                        let account_id = account.id.clone();
                        let has_quota = account.has_quota_budget();
                        let started_at = started;
                        tokio::spawn(async move {
                            let usage = match usage_rx.await {
                                Ok(Some(u)) => u.normalized(),
                                Ok(None) | Err(_) => return,
                            };
                            if usage.is_empty() || log_id.is_empty() {
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
                            if has_quota && total > 0 {
                                if let Ok((before, after, used)) =
                                    db::decrement_quota(&pool, &account_id, total).await
                                {
                                    if used > 0 {
                                        credits_used = Some(used);
                                        q_before = Some(before);
                                        q_after = Some(after);
                                    }
                                }
                            }
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
                        });

                        let (_tx, dummy_rx) = tokio::sync::oneshot::channel();
                        return Ok(ChatOutcome::Stream {
                            response,
                            usage_rx: dummy_rx,
                        });
                    }
                }
            }
            Err(e) => {
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
    let prev_error = account.last_error.clone();
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
            let prev = prev_error.as_deref().unwrap_or("").to_lowercase();
            let repeated = prev.contains("auth expired") || prev.contains("auth invalid");
            if repeated {
                account.is_active = 0;
                info!(account = %account.id, "cut (repeated auth expired)");
            } else {
                let until = Utc::now() + Duration::minutes(5);
                account.cooldown_until =
                    Some(until.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
            }
        }
        ProviderError::AuthInvalid(_) | ProviderError::PaymentRequired | ProviderError::AccessDenied => {
            account.is_active = 0;
            info!(account = %account.id, "cut (disabled)");
        }
        ProviderError::Upstream { status, .. } if *status == 402 || *status == 403 => {
            account.is_active = 0;
        }
        _ => {}
    }

    db::update_account(pool, account).await?;
    Ok(())
}
