use crate::db;
use crate::error::ProviderError;
use crate::providers::Provider;
use crate::state::AppState;
use futures::stream::{self, StreamExt};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tracing::{info, warn};

pub fn spawn(state: AppState) {
    let interval_secs = state.config.refresh_interval_secs;
    if interval_secs == 0 {
        info!("background grok refresh worker disabled (MARIONETTE_REFRESH_INTERVAL_SECS=0)");
        return;
    }

    info!(
        interval_secs,
        lead_secs = state.config.refresh_lead_secs,
        workers = state.config.refresh_workers,
        "starting background grok refresh worker"
    );

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(15)).await;
        loop {
            if let Err(e) = run_cycle(&state).await {
                warn!(error = %e, "refresh cycle failed");
            }
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        }
    });
}

async fn run_cycle(state: &AppState) -> Result<(), crate::error::AppError> {
    let accounts = db::list_accounts(&state.pool, Some("grok-cli"), None).await?;
    let candidates: Vec<_> = accounts
        .into_iter()
        .filter(|a| a.is_active == 1)
        .collect();

    let total = candidates.len();
    if total == 0 {
        info!("refresh cycle: no active grok-cli accounts");
        return Ok(());
    }

    let ok = AtomicU64::new(0);
    let failed = AtomicU64::new(0);
    let disabled = AtomicU64::new(0);
    let workers = state.config.refresh_workers;

    stream::iter(candidates)
        .for_each_concurrent(workers, |mut account| {
            let state = state.clone();
            let ok = &ok;
            let failed = &failed;
            let disabled = &disabled;
            async move {
                let id = account.id.clone();
                let email = account.email.clone().unwrap_or_else(|| id.clone());
                match state.grok.ensure_fresh_auth(&mut account).await {
                    Ok(()) => {
                        account.last_error = None;
                        account.updated_at = db::now_rfc3339();
                        if let Err(e) = db::update_account(&state.pool, &account).await {
                            warn!(account = %id, error = %e, "refresh persist failed");
                            failed.fetch_add(1, Ordering::Relaxed);
                            return;
                        }
                        ok.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(ProviderError::AuthInvalid(msg)) => {
                        account.is_active = 0;
                        account.last_error = Some(
                            format!("auth invalid: {msg}")
                                .chars()
                                .take(500)
                                .collect(),
                        );
                        account.updated_at = db::now_rfc3339();
                        let _ = db::update_account(&state.pool, &account).await;
                        warn!(account = %id, email = %email, "refresh cut: auth invalid");
                        disabled.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        account.last_error =
                            Some(e.to_string().chars().take(500).collect());
                        account.updated_at = db::now_rfc3339();
                        let _ = db::update_account(&state.pool, &account).await;
                        warn!(account = %id, email = %email, error = %e, "refresh failed");
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        })
        .await;

    info!(
        total,
        ok = ok.load(Ordering::Relaxed),
        failed = failed.load(Ordering::Relaxed),
        disabled = disabled.load(Ordering::Relaxed),
        "refresh cycle done"
    );
    Ok(())
}
