pub mod grok_cli;
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
    async fn chat(
        &self,
        account: &Account,
        req: &ChatCompletionRequest,
    ) -> Result<ChatOutcome, ProviderError>;

    /// Force a token refresh even if not obviously expired (e.g. after a mid-request
    /// AuthExpired where the cached SOT/userId is silently stale). Default = ensure_fresh_auth.
    async fn force_refresh(&self, account: &mut Account) -> Result<(), ProviderError> {
        self.ensure_fresh_auth(account).await
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
        _ => {
            let lower = body.to_lowercase();
            if lower.contains("invalid_grant") || lower.contains("invalid_request") {
                ProviderError::AuthInvalid(body.chars().take(200).collect())
            } else if lower.contains("rate") || lower.contains("quota") || lower.contains("limit") {
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
    }
}
