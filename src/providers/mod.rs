pub mod grok_cli;
pub mod qoder;

use crate::db::Account;
use crate::error::ProviderError;
use crate::openai::ChatCompletionRequest;
use async_trait::async_trait;
use axum::response::Response;
use serde_json::Value;

pub enum ChatOutcome {
    Json(Value),
    Stream(Response),
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
