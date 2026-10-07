pub mod byok;
pub mod antigravity;
pub mod client_version;
pub mod cline;
pub mod commandcode;
pub mod grok_cli;
pub mod kiro;
pub mod kiro_event_stream;
pub mod qoder;

use crate::db::Account;
use crate::error::ProviderError;
use crate::openai::ChatCompletionRequest;
use async_trait::async_trait;
use axum::response::Response;
use serde_json::Value;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, Default)]
pub struct StreamUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

impl StreamUsage {
    pub fn is_empty(self) -> bool {
        self.prompt_tokens == 0 && self.completion_tokens == 0 && self.total_tokens == 0
    }

    pub fn normalized(self) -> Self {
        let total = if self.total_tokens > 0 {
            self.total_tokens
        } else {
            self.prompt_tokens + self.completion_tokens
        };
        Self {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            total_tokens: total,
        }
    }
}

pub enum ChatOutcome {
    Json(Value),
    Stream {
        response: Response,
        usage_rx: oneshot::Receiver<Option<StreamUsage>>,
    },
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    async fn ensure_fresh_auth(&self, account: &mut Account) -> Result<(), ProviderError>;
    /// Send a chat completion. `client` is the (possibly proxied) HTTP client
    /// the pool resolved for this account; providers must send the upstream
    /// request through it rather than a private client so proxy routing works.
    async fn chat(
        &self,
        client: &reqwest::Client,
        account: &Account,
        req: &ChatCompletionRequest,
    ) -> Result<ChatOutcome, ProviderError>;

    /// Force a token refresh even if not obviously expired (e.g. after a mid-request
    /// AuthExpired where the cached SOT/userId is silently stale). Default = ensure_fresh_auth.
    async fn force_refresh(&self, account: &mut Account) -> Result<(), ProviderError> {
        self.ensure_fresh_auth(account).await
    }

    async fn sync_quota(&self, _account: &mut Account) -> Result<(), ProviderError> {
        Ok(())
    }
}

pub fn classify_http_status(status: u16, body: &str) -> ProviderError {
    match status {
        401 => ProviderError::AuthExpired,
        402 => ProviderError::PaymentRequired,
        403 => ProviderError::AccessDenied,
        429 => ProviderError::RateLimited {
            retry_after_secs: None,
        },
        // Body-text heuristics apply to client errors only.
        //
        // They used to run for every other status, which meant a 5xx was
        // classified from a substring match on its body: "Failed to generate
        // response" contains "rate", so a transient upstream failure became
        // RateLimited and sealed the account for cooldown_hours. A 5xx is
        // never the account's fault, so it must stay `Upstream` and fall
        // rather than seal.
        400 | 404 => {
            let lower = body.to_lowercase();
            if lower.contains("invalid_grant") || lower.contains("invalid_request") {
                ProviderError::AuthInvalid(body.chars().take(200).collect())
            } else if lower.contains("rate")
                || lower.contains("quota")
                || lower.contains("limit")
            {
                ProviderError::RateLimited {
                    retry_after_secs: None,
                }
            } else {
                ProviderError::Upstream {
                    status,
                    body: body.chars().take(2000).collect(),
                }
            }
        }
        _ => ProviderError::Upstream {
            status,
            body: body.chars().take(2000).collect(),
        },
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn status_of(e: &ProviderError) -> Option<u16> {
        match e {
            ProviderError::Upstream { status, .. } => Some(*status),
            _ => None,
        }
    }

    #[test]
    fn server_errors_are_never_classified_from_the_body() {
        // A 5xx is not the account's fault. The body heuristic used to run for
        // every status, so "Failed to generate response" matched "rate" and a
        // transient upstream failure sealed the account for cooldown_hours.
        for body in [
            "Failed to generate response",
            "moderate load, retry later",
            "requested tokens exceed unlimited quota",
            "upstream rate limit exceeded",
        ] {
            let e = classify_http_status(500, body);
            assert_eq!(
                status_of(&e),
                Some(500),
                "{body:?} on a 500 must stay Upstream, got {e:?}"
            );
        }
        assert_eq!(status_of(&classify_http_status(502, "rate limited")), Some(502));
        assert_eq!(status_of(&classify_http_status(503, "quota exceeded")), Some(503));
    }

    #[test]
    fn client_errors_still_use_the_body() {
        // The heuristic is still useful where the status alone is ambiguous.
        assert!(matches!(
            classify_http_status(400, "invalid_grant"),
            ProviderError::AuthInvalid(_)
        ));
        assert!(matches!(
            classify_http_status(400, "quota exceeded"),
            ProviderError::RateLimited { .. }
        ));
        assert!(matches!(
            classify_http_status(400, "bad request"),
            ProviderError::Upstream { status: 400, .. }
        ));
        assert!(matches!(
            classify_http_status(404, "model not found"),
            ProviderError::Upstream { status: 404, .. }
        ));
    }

    #[test]
    fn status_codes_win_over_the_body() {
        // These must keep matching on status even when the body says otherwise.
        assert!(matches!(classify_http_status(401, "rate limit"), ProviderError::AuthExpired));
        assert!(matches!(
            classify_http_status(402, "limit"),
            ProviderError::PaymentRequired
        ));
        assert!(matches!(
            classify_http_status(403, "quota"),
            ProviderError::AccessDenied
        ));
        assert!(matches!(
            classify_http_status(429, "whatever"),
            ProviderError::RateLimited { .. }
        ));
    }
}
