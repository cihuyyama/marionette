//! Keeps provider client versions current.
//!
//! A stale client version is not a degradation for these providers, it is a
//! hard stop: grok-cli answered `426 Upgrade Required` to every request while
//! pinned at 1.0.5. The request path reads a cached value only, so something
//! has to go and fetch the real one — this worker is that something.
//!
//! The first refresh runs shortly after boot rather than waiting a full
//! interval, because until it completes the provider is running on whatever
//! version was compiled in.

use crate::providers::client_version::ClientVersion;
use crate::providers::{grok_cli, kiro};
use crate::state::AppState;
use std::time::Duration;
use tracing::{info, warn};

/// Providers whose dispatch is gated on a client version.
fn tracked() -> Vec<&'static ClientVersion> {
    vec![grok_cli::client_version(), kiro::client_version()]
}

/// Interval between refreshes. Versions move on the order of weeks, so this is
/// about bounding staleness, not about catching releases promptly.
const DEFAULT_INTERVAL_SECS: u64 = 6 * 60 * 60;

/// Delay before the first refresh, so boot is not competing with it.
const FIRST_REFRESH_DELAY_SECS: u64 = 5;

pub fn spawn(state: AppState) {
    let interval_secs = std::env::var("MARIONETTE_VERSION_REFRESH_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS);

    if interval_secs == 0 {
        info!("client-version worker disabled (MARIONETTE_VERSION_REFRESH_INTERVAL_SECS=0)");
        return;
    }

    let tracked_count = tracked().len();
    info!(
        providers = tracked_count,
        interval_secs, "starting client-version worker"
    );

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(FIRST_REFRESH_DELAY_SECS)).await;
        loop {
            refresh_all(&state).await;
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        }
    });
}

/// Resolve every tracked version, in parallel. Failures are logged and leave
/// the previous value (or the pinned fallback) in place.
pub async fn refresh_all(state: &AppState) {
    let versions = tracked();
    let results = futures::future::join_all(
        versions
            .iter()
            .map(|v| v.refresh(&state.http)),
    )
    .await;

    for (v, resolved) in versions.iter().zip(results) {
        let (effective, from_upstream) = v.snapshot();
        if from_upstream && effective == resolved {
            info!(
                provider = v.name(),
                version = %effective,
                "client version active"
            );
        } else if !from_upstream {
            warn!(
                provider = v.name(),
                pinned = v.pinned(),
                reason = v.last_error().unwrap_or_default(),
                "client version unresolved; using pinned fallback"
            );
        }
    }
}
