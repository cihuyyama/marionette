use crate::db;
use crate::state::AppState;
use chrono::Utc;
use std::time::Duration;
use tracing::{info, warn};

/// Rows per batch. Small enough that one statement never holds the write lock
/// long enough to stall request logging; large enough that a 100k-row backlog
/// clears in a handful of passes.
const BATCH: i64 = 500;


/// large slice of the file. 4096 pages @ 4 KiB = 16 MiB per pass.


pub fn spawn(state: AppState) {
    let interval_secs = state.config.retention_interval_secs;
    if interval_secs == 0 {
        info!("log retention worker disabled (MARIONETTE_RETENTION_INTERVAL_SECS=0)");
        return;
    }
    info!(
        interval_secs,
        log_retention_days = state.config.log_retention_days,
        body_retention_days = state.config.log_body_retention_days,
        "starting log retention worker"
    );
    tokio::spawn(async move {
        // Let startup finish (migrate + first traffic) before the first sweep.
        tokio::time::sleep(Duration::from_secs(60)).await;
        loop {
            if let Err(e) = run_cycle(&state).await {
                warn!(error = %e, "log retention cycle failed");
            }
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        }
    });
}

async fn run_cycle(state: &AppState) -> Result<(), crate::error::AppError> {
    let now = Utc::now();

    // Stage 1 — drop bodies. They are ~99% of the bytes but the metadata row
    // (tokens, credits, status, timing) stays useful long after, so the two
    // have separate windows instead of one shared expiry.
    let body_cutoff = (now - chrono::Duration::days(state.config.log_body_retention_days as i64))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let nulled = db::null_old_log_bodies(&state.pool, &body_cutoff, BATCH).await?;

    // Stage 2 — delete the whole row once it passes the longer window.
    let row_cutoff = (now - chrono::Duration::days(state.config.log_retention_days as i64))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let deleted = db::delete_old_request_logs(&state.pool, &row_cutoff, BATCH).await?;

    let (pages, freelist) = db::db_page_stats(&state.pool).await?;
    info!(
        bodies_cleared = nulled,
        rows_deleted = deleted,
        pages,
        free_pages = freelist,
        "log retention cycle done"
    );
    Ok(())
}
