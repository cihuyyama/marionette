use crate::db;
use crate::error::{AppError, AppResult, ProviderError};
use crate::providers::Provider;
use crate::state::AppState;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

const MAX_HISTORY: usize = 20;
const DEFAULT_CONCURRENCY: u32 = 8;
const MAX_CONCURRENCY: u32 = 32;
const MAX_RETRIES: u32 = 2;
const RETRY_BASE_MS: u64 = 250;
const MAX_RETRY_AFTER_MS: u64 = 5_000;

/// Request body for `POST /admin/accounts/refresh-all`.
#[derive(Debug, Deserialize)]
pub struct RefreshAllRequest {
    /// Only `grok-cli` is supported (OAuth refresh flow).
    #[serde(default)]
    pub provider: Option<String>,
    /// Force refresh every account regardless of expiry. Defaults to `true`.
    #[serde(default)]
    pub force: Option<bool>,
    /// Sliding-window concurrency. Clamped to 1..=32. Defaults to 8.
    #[serde(default)]
    pub concurrency: Option<u32>,
}

/// Serializable progress snapshot for a refresh-all job.
#[derive(Debug, Clone, Serialize)]
pub struct RefreshJob {
    pub id: String,
    pub provider: String,
    /// running | succeeded | cancelled
    pub status: String,
    pub force: bool,
    pub concurrency: u32,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub total: u32,
    pub processed: u32,
    pub ok: u32,
    pub failed: u32,
    /// accounts deactivated because refresh token is invalid
    pub cut: u32,
    pub error: Option<String>,
    pub last_email: Option<String>,
}

struct RefreshState {
    current: Option<Arc<Mutex<RefreshJob>>>,
    cancel: Option<Arc<AtomicBool>>,
    history: VecDeque<RefreshJob>,
}

#[derive(Clone)]
pub struct RefreshManager {
    inner: Arc<Mutex<RefreshState>>,
}

impl Default for RefreshManager {
    fn default() -> Self {
        Self::new()
    }
}

enum Outcome {
    Ok,
    Cut,
    Failed,
}

impl RefreshManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RefreshState {
                current: None,
                cancel: None,
                history: VecDeque::new(),
            })),
        }
    }

    /// Start a background refresh-all job. Rejects if one is already running.
    pub async fn start(&self, state: AppState, req: RefreshAllRequest) -> AppResult<Value> {
        let provider = req
            .provider
            .unwrap_or_else(|| "grok-cli".to_string());
        if provider != "grok-cli" {
            return Err(AppError::BadRequest(
                "refresh-all currently supports provider grok-cli only".into(),
            ));
        }
        let force = req.force.unwrap_or(true);
        let concurrency = req
            .concurrency
            .unwrap_or(DEFAULT_CONCURRENCY)
            .clamp(1, MAX_CONCURRENCY);

        // Single in-flight job guard — avoid hammering auth.x.ai with parallel runs.
        {
            let st = self.inner.lock().await;
            if let Some(cur) = &st.current {
                let m = cur.lock().await;
                if m.status == "running" {
                    return Err(AppError::BadRequest(
                        "a refresh-all job is already running".into(),
                    ));
                }
            }
        }

        let accounts = db::list_accounts(&state.pool, Some(&provider), None).await?;
        let candidates: Vec<_> = accounts
            .into_iter()
            .filter(|a| a.is_active == 1)
            .collect();
        let total = candidates.len() as u32;

        let now = db::now_rfc3339();
        let meta = RefreshJob {
            id: Uuid::new_v4().to_string(),
            provider: provider.clone(),
            status: "running".to_string(),
            force,
            concurrency,
            created_at: now.clone(),
            started_at: Some(now),
            finished_at: None,
            total,
            processed: 0,
            ok: 0,
            failed: 0,
            cut: 0,
            error: None,
            last_email: None,
        };

        let shared = Arc::new(Mutex::new(meta.clone()));
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut st = self.inner.lock().await;
            st.current = Some(shared.clone());
            st.cancel = Some(cancel.clone());
        }

        info!(
            job = %meta.id,
            total,
            concurrency,
            force,
            "starting refresh-all job"
        );

        let mgr = self.clone();
        tokio::spawn(async move {
            run_refresh(
                state,
                candidates,
                concurrency,
                force,
                shared.clone(),
                cancel.clone(),
            )
            .await;

            let final_meta = {
                let mut m = shared.lock().await;
                if m.status == "running" {
                    m.status = if cancel.load(Ordering::Relaxed) {
                        "cancelled".to_string()
                    } else {
                        "succeeded".to_string()
                    };
                }
                m.finished_at = Some(db::now_rfc3339());
                m.clone()
            };

            info!(
                job = %final_meta.id,
                status = %final_meta.status,
                ok = final_meta.ok,
                failed = final_meta.failed,
                cut = final_meta.cut,
                total = final_meta.total,
                "refresh-all job done"
            );

            let mut st = mgr.inner.lock().await;
            st.history.push_front(final_meta);
            while st.history.len() > MAX_HISTORY {
                st.history.pop_back();
            }
            st.current = None;
            st.cancel = None;
        });

        Ok(json!(meta))
    }

    /// Snapshot of the current (or most recent) job plus recent history.
    pub async fn snapshot(&self) -> Value {
        let st = self.inner.lock().await;
        let current = match &st.current {
            Some(cur) => Some(cur.lock().await.clone()),
            None => st.history.front().cloned(),
        };
        let history: Vec<_> = st.history.iter().cloned().collect();
        json!({ "current": current, "history": history })
    }

    pub async fn get_job(&self, id: &str) -> AppResult<Value> {
        let st = self.inner.lock().await;
        if let Some(cur) = &st.current {
            let m = cur.lock().await;
            if m.id == id {
                return Ok(json!(m.clone()));
            }
        }
        if let Some(found) = st.history.iter().find(|j| j.id == id) {
            return Ok(json!(found.clone()));
        }
        Err(AppError::NotFound(format!("refresh job {id}")))
    }

    /// Signal the running job to stop launching new refreshes.
    pub async fn cancel(&self) -> AppResult<Value> {
        let st = self.inner.lock().await;
        match (&st.current, &st.cancel) {
            (Some(cur), Some(flag)) => {
                flag.store(true, Ordering::Relaxed);
                let m = cur.lock().await;
                Ok(json!({ "cancelling": true, "id": m.id }))
            }
            _ => Err(AppError::BadRequest("no refresh-all job running".into())),
        }
    }
}

async fn run_refresh(
    state: AppState,
    candidates: Vec<db::Account>,
    concurrency: u32,
    force: bool,
    shared: Arc<Mutex<RefreshJob>>,
    cancel: Arc<AtomicBool>,
) {
    stream::iter(candidates)
        .for_each_concurrent(concurrency as usize, |mut account| {
            let state = state.clone();
            let shared = shared.clone();
            let cancel = cancel.clone();
            async move {
                // Stop launching new work once cancelled; in-flight ones finish.
                if cancel.load(Ordering::Relaxed) {
                    let mut m = shared.lock().await;
                    m.processed += 1;
                    return;
                }
                let id = account.id.clone();
                let email = account.email.clone().unwrap_or_else(|| id.clone());
                let outcome = refresh_one(&state, &mut account, force).await;
                let mut m = shared.lock().await;
                m.processed += 1;
                m.last_email = Some(email);
                match outcome {
                    Outcome::Ok => m.ok += 1,
                    Outcome::Cut => m.cut += 1,
                    Outcome::Failed => m.failed += 1,
                }
            }
        })
        .await;
}

async fn refresh_one(state: &AppState, account: &mut db::Account, force: bool) -> Outcome {
    let id = account.id.clone();
    let email = account.email.clone().unwrap_or_else(|| id.clone());
    let mut attempt: u32 = 0;

    loop {
        let res = if force {
            state.grok.force_refresh(account).await
        } else {
            state.grok.ensure_fresh_auth(account).await
        };

        match res {
            Ok(()) => {
                account.last_error = None;
                account.updated_at = db::now_rfc3339();
                if let Err(e) = db::update_account(&state.pool, account).await {
                    warn!(account = %id, error = %e, "refresh-all persist failed");
                    return Outcome::Failed;
                }
                return Outcome::Ok;
            }
            Err(ProviderError::AuthInvalid(msg)) => {
                account.is_active = 0;
                account.last_error =
                    Some(format!("auth invalid: {msg}").chars().take(500).collect());
                account.updated_at = db::now_rfc3339();
                let _ = db::update_account(&state.pool, account).await;
                warn!(account = %id, email = %email, "refresh-all cut: auth invalid");
                return Outcome::Cut;
            }
            Err(ProviderError::AuthExpired) => {
                account.is_active = 0;
                account.last_error = Some("auth expired".to_string());
                account.updated_at = db::now_rfc3339();
                let _ = db::update_account(&state.pool, account).await;
                warn!(account = %id, email = %email, "refresh-all cut: auth expired");
                return Outcome::Cut;
            }
            Err(e) if attempt < MAX_RETRIES && transient_backoff_ms(&e, attempt).is_some() => {
                let backoff = transient_backoff_ms(&e, attempt).unwrap();
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(backoff)).await;
                continue;
            }
            Err(e) => {
                account.last_error = Some(e.to_string().chars().take(500).collect());
                account.updated_at = db::now_rfc3339();
                let _ = db::update_account(&state.pool, account).await;
                warn!(account = %id, email = %email, error = %e, "refresh-all failed");
                return Outcome::Failed;
            }
        }
    }
}

/// Returns the backoff (ms) to wait before retrying a *transient* error, or
/// `None` for permanent errors that must not be retried (e.g. invalid_grant).
fn transient_backoff_ms(e: &ProviderError, attempt: u32) -> Option<u64> {
    match e {
        ProviderError::RateLimited { retry_after_secs } => {
            let ra = retry_after_secs
                .map(|s| s.saturating_mul(1000))
                .unwrap_or_else(|| RETRY_BASE_MS << attempt);
            Some(ra.min(MAX_RETRY_AFTER_MS))
        }
        ProviderError::Transport(_) => Some((RETRY_BASE_MS << attempt).min(MAX_RETRY_AFTER_MS)),
        ProviderError::Upstream { status, .. } if *status >= 500 => {
            Some((RETRY_BASE_MS << attempt).min(MAX_RETRY_AFTER_MS))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_errors_backoff_permanent_do_not() {
        // permanent — must not retry
        assert!(transient_backoff_ms(&ProviderError::AuthInvalid("x".into()), 0).is_none());
        assert!(transient_backoff_ms(&ProviderError::AuthExpired, 0).is_none());
        assert!(transient_backoff_ms(&ProviderError::PaymentRequired, 0).is_none());
        assert!(
            transient_backoff_ms(&ProviderError::Upstream { status: 400, body: "b".into() }, 0)
                .is_none()
        );

        // transient — must retry with backoff
        assert!(transient_backoff_ms(&ProviderError::Transport("t".into()), 0).is_some());
        assert!(
            transient_backoff_ms(&ProviderError::Upstream { status: 503, body: "b".into() }, 0)
                .is_some()
        );
    }

    #[test]
    fn rate_limit_honors_retry_after_capped() {
        let ms = transient_backoff_ms(
            &ProviderError::RateLimited { retry_after_secs: Some(999) },
            0,
        )
        .unwrap();
        assert_eq!(ms, MAX_RETRY_AFTER_MS);

        let ms = transient_backoff_ms(
            &ProviderError::RateLimited { retry_after_secs: Some(1) },
            0,
        )
        .unwrap();
        assert_eq!(ms, 1000);
    }

    #[test]
    fn backoff_grows_with_attempt() {
        let a0 = transient_backoff_ms(&ProviderError::Transport("t".into()), 0).unwrap();
        let a1 = transient_backoff_ms(&ProviderError::Transport("t".into()), 1).unwrap();
        assert!(a1 > a0);
    }
}
