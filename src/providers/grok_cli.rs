use super::{ChatOutcome, Provider, classify_http_status};
use crate::config::Config;
use crate::db::{Account, parse_rfc3339};
use crate::error::ProviderError;
use crate::openai::ChatCompletionRequest;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use chrono::{Duration, Utc};
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};
use std::sync::Arc;

const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const CHAT_URL: &str = "https://api.x.ai/v1/chat/completions";
const USER_AGENT: &str = "grok-cli/marionette";

pub struct GrokCliProvider {
    client: Client,
    config: Arc<Config>,
}

impl GrokCliProvider {
    pub fn new(config: Arc<Config>) -> Self {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("reqwest client");
        Self { client, config }
    }

    fn client_id(&self, data: &Value) -> String {
        data.get("clientId")
            .or_else(|| data.get("client_id"))
            .and_then(|v| v.as_str())
            .unwrap_or(&self.config.grok_client_id)
            .to_string()
    }

    fn access_token(data: &Value) -> Option<String> {
        data.get("accessToken")
            .or_else(|| data.get("access_token"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    fn refresh_token(data: &Value) -> Option<String> {
        data.get("refreshToken")
            .or_else(|| data.get("refresh_token"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    fn needs_refresh(&self, data: &Value) -> bool {
        let exp = data
            .get("expiresAt")
            .or_else(|| data.get("expires_at"))
            .and_then(|v| v.as_str())
            .and_then(parse_rfc3339);
        match exp {
            Some(t) => {
                let lead = Duration::seconds(self.config.refresh_lead_secs);
                t <= Utc::now() + lead
            }
            None => true,
        }
    }

    async fn refresh(&self, account: &mut Account) -> Result<(), ProviderError> {
        let mut data = account.data_json();
        let refresh = Self::refresh_token(&data).ok_or_else(|| {
            ProviderError::AuthInvalid("missing refreshToken".into())
        })?;
        let client_id = self.client_id(&data);

        let resp = self
            .client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!(
                "grant_type=refresh_token&refresh_token={}&client_id={}",
                urlencoding_basic(&refresh),
                urlencoding_basic(&client_id)
            ))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        if status != 200 {
            let lower = text.to_lowercase();
            if lower.contains("invalid_grant") || lower.contains("invalid_request") {
                return Err(ProviderError::AuthInvalid(text.chars().take(300).collect()));
            }
            return Err(classify_http_status(status, &text));
        }

        let tok: Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Other(format!("refresh json: {e}")))?;

        if let Some(at) = tok.get("access_token").and_then(|v| v.as_str()) {
            data["accessToken"] = json!(at);
        }
        if let Some(rt) = tok.get("refresh_token").and_then(|v| v.as_str()) {
            data["refreshToken"] = json!(rt);
        }
        if let Some(id) = tok.get("id_token").and_then(|v| v.as_str()) {
            data["idToken"] = json!(id);
        }
        let expires_in = tok
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(21600) as i64;
        data["expiresIn"] = json!(expires_in);
        data["expiresAt"] = json!(
            (Utc::now() + Duration::seconds(expires_in))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );
        data["lastRefreshAt"] = json!(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));

        account.set_data_json(&data);
        account.updated_at = crate::db::now_rfc3339();
        Ok(())
    }

    fn build_upstream_body(req: &ChatCompletionRequest) -> Value {
        let mut body = json!({
            "model": req.upstream_model(),
            "messages": req.messages,
            "stream": req.stream_enabled(),
        });
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(m) = req.max_tokens {
            body["max_tokens"] = json!(m);
        }
        if let Some(p) = req.top_p {
            body["top_p"] = json!(p);
        }
        // merge safe extras
        if let Value::Object(extra) = &req.extra {
            if let Some(obj) = body.as_object_mut() {
                for (k, v) in extra {
                    if k == "model" || k == "messages" || k == "stream" {
                        continue;
                    }
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
        body
    }
}

#[async_trait]
impl Provider for GrokCliProvider {
    fn id(&self) -> &'static str {
        "grok-cli"
    }

    async fn ensure_fresh_auth(&self, account: &mut Account) -> Result<(), ProviderError> {
        let data = account.data_json();
        if Self::access_token(&data).is_none() && Self::refresh_token(&data).is_none() {
            return Err(ProviderError::AuthInvalid("no tokens".into()));
        }
        if self.needs_refresh(&data) || Self::access_token(&data).is_none() {
            self.refresh(account).await?;
        }
        Ok(())
    }

    async fn chat(
        &self,
        account: &Account,
        req: &ChatCompletionRequest,
    ) -> Result<ChatOutcome, ProviderError> {
        let data = account.data_json();
        let token = Self::access_token(&data)
            .ok_or_else(|| ProviderError::AuthInvalid("missing accessToken".into()))?;
        let body = Self::build_upstream_body(req);

        let resp = self
            .client
            .post(CHAT_URL)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("User-Agent", USER_AGENT)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let text = resp
                .text()
                .await
                .unwrap_or_default();
            return Err(classify_http_status(status, &text));
        }

        if req.stream_enabled() {
            let stream = resp.bytes_stream().map(|chunk| {
                chunk
                    .map(|b| b.to_vec())
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
            });
            let body = Body::from_stream(stream);
            let mut headers = HeaderMap::new();
            headers.insert(
                "content-type",
                HeaderValue::from_static("text/event-stream"),
            );
            headers.insert("cache-control", HeaderValue::from_static("no-cache"));
            headers.insert("connection", HeaderValue::from_static("keep-alive"));
            let response = Response::builder()
                .status(StatusCode::OK)
                .body(body)
                .map_err(|e| ProviderError::Other(e.to_string()))?;
            let (mut parts, body) = response.into_parts();
            parts.headers = headers;
            Ok(ChatOutcome::Stream(Response::from_parts(parts, body)))
        } else {
            let json: Value = resp
                .json()
                .await
                .map_err(|e| ProviderError::Transport(e.to_string()))?;
            Ok(ChatOutcome::Json(json))
        }
    }
}

fn urlencoding_basic(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
