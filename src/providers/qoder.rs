//! Qoder provider — stub until Phase 5 full port from etteeum qoder.ts.
use super::{ChatOutcome, Provider};
use crate::db::Account;
use crate::error::ProviderError;
use crate::openai::ChatCompletionRequest;
use async_trait::async_trait;
use reqwest::Client;

pub struct QoderProvider {
    #[allow(dead_code)]
    client: Client,
}

impl QoderProvider {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Provider for QoderProvider {
    fn id(&self) -> &'static str {
        "qoder"
    }

    async fn ensure_fresh_auth(&self, _account: &mut Account) -> Result<(), ProviderError> {
        Err(ProviderError::Other(
            "qoder not implemented yet (Phase 5)".into(),
        ))
    }

    async fn chat(
        &self,
        _account: &Account,
        _req: &ChatCompletionRequest,
    ) -> Result<ChatOutcome, ProviderError> {
        Err(ProviderError::Other(
            "qoder not implemented yet (Phase 5)".into(),
        ))
    }
}
