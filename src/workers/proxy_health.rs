use crate::db;
use crate::proxy;
use crate::state::AppState;
use futures::stream::{self, StreamExt};
use std::time::Duration;
use tracing::{info, warn};

const CHECK_CONCURRENCY: usize = 16;

pub fn spawn(state: AppState) {
    let interval_secs = state.config.proxy_health_interval_secs;
    if interval_secs == 0 {
        info!("proxy health worker disabled (MARIONETTE_PROXY_HEALTH_INTERVAL_SECS=0)");
        return;
    }
    info!(interval_secs, "starting proxy health worker");
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            if let Err(e) = run_cycle(&state).await {
                warn!(error = %e, "proxy health cycle failed");
            }
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        }
    });
}

async fn run_cycle(state: &AppState) -> Result<(), crate::error::AppError> {
    let proxies = db::list_active_proxies(&state.pool).await?;
    let total = proxies.len();
    if total == 0 {
        return Ok(());
    }
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
        .buffer_unordered(CHECK_CONCURRENCY)
        .collect()
        .await;
    let healthy = results.iter().filter(|&&ok| ok).count();
    info!(total, healthy, dead = total - healthy, "proxy health cycle done");
    Ok(())
}
