use crate::config::Config;
use crate::db::{self, Account};
use crate::error::{AppError, AppResult, ProviderError};
use crate::openai::ChatCompletionRequest;
use crate::providers::{ChatOutcome, Provider};
use crate::state::AppState;
use chrono::{Duration, Utc};
use sqlx::SqlitePool;
use std::sync::Arc;
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

    // try up to 3 accounts
    let mut last_err: Option<AppError> = None;
    for attempt in 0..3 {
        let mut account = match db::pick_account(&state.pool, provider_id).await {
            Ok(a) => a,
            Err(e) => {
                if attempt == 0 {
                    return Err(e);
                }
                break;
            }
        };

        info!(
            account = %account.id,
            provider = provider_id,
            attempt,
            "picked account"
        );

        if let Err(e) = provider.ensure_fresh_auth(&mut account).await {
            apply_provider_error(&state.pool, &state.config, &mut account, &e).await?;
            last_err = Some(e.into());
            continue;
        }
        // persist refreshed tokens
        account.updated_at = db::now_rfc3339();
        db::update_account(&state.pool, &account).await?;

        match provider.chat(&account, &req).await {
            Ok(outcome) => {
                account.last_used_at = Some(db::now_rfc3339());
                account.last_error = None;
                account.updated_at = db::now_rfc3339();
                db::update_account(&state.pool, &account).await?;
                return Ok(outcome);
            }
            Err(e) => {
                warn!(account = %account.id, error = %e, "upstream chat failed");
                apply_provider_error(&state.pool, &state.config, &mut account, &e).await?;
                last_err = Some(e.into());
            }
        }
    }

    Err(last_err.unwrap_or_else(|| AppError::NoAccounts(provider_id.into())))
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
            // short cooldown; next pick may refresh
            let until = Utc::now() + Duration::minutes(5);
            account.cooldown_until =
                Some(until.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
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
