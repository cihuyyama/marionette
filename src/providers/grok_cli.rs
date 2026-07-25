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
use uuid::Uuid;

const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const RESPONSES_URL: &str = "https://cli-chat-proxy.grok.com/v1/responses";
const USER_AGENT: &str = "grok-shell/0.2.99 (linux; x86_64)";

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

    fn resolve_model_and_effort(model_name: &str) -> (String, Option<String>) {
        let (model, effort) = if model_name.ends_with("-high") {
            (model_name.trim_end_matches("-high"), Some("high".to_string()))
        } else if model_name.ends_with("-medium") {
            (model_name.trim_end_matches("-medium"), Some("medium".to_string()))
        } else if model_name.ends_with("-low") {
            (model_name.trim_end_matches("-low"), Some("low".to_string()))
        } else if model_name.ends_with("-xhigh") {
            (model_name.trim_end_matches("-xhigh"), Some("xhigh".to_string()))
        } else {
            (model_name, None)
        };

        let resolved = match model {
            "gb" | "grok-build" => "grok-build",
            m => m,
        };

        (resolved.to_string(), effort)
    }

    fn build_responses_input(messages: &[crate::openai::ChatMessage]) -> Vec<Value> {
        let mut input = Vec::new();
        for msg in messages {
            let content_str = match &msg.content {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            input.push(json!({
                "type": "message",
                "role": msg.role,
                "content": content_str
            }));
        }
        if input.is_empty() {
            input.push(json!({
                "type": "message",
                "role": "user",
                "content": "..."
            }));
        }
        input
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

        let model_name = req.upstream_model();
        let (upstream_model_id, effort) = Self::resolve_model_and_effort(model_name);
        let input_items = Self::build_responses_input(&req.messages);

        let supports_effort = upstream_model_id.starts_with("grok-4.5");
        let mut reasoning = json!({
            "summary": "concise"
        });
        if supports_effort {
            let effort_val = effort.unwrap_or_else(|| "high".to_string());
            reasoning["effort"] = json!(effort_val);
        }

        // Always request stream upstream
        let body = json!({
            "model": upstream_model_id,
            "input": input_items,
            "stream": true,
            "store": false,
            "reasoning": reasoning,
            "include": ["reasoning.encrypted_content"]
        });

        let session_id = Uuid::new_v4().to_string();
        let req_id = Uuid::new_v4().to_string();

        let mut req_builder = self
            .client
            .post(RESPONSES_URL)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("User-Agent", USER_AGENT)
            .header("x-grok-client-identifier", "grok-shell")
            .header("x-grok-client-version", "0.2.99")
            .header("x-grok-session-id", &session_id)
            .header("x-grok-conv-id", &session_id)
            .header("x-grok-req-id", &req_id)
            .header("x-grok-turn-idx", "1")
            .header("x-grok-model-override", &upstream_model_id);

        if let Some(email) = data.get("email").and_then(|v| v.as_str()) {
            req_builder = req_builder.header("x-email", email);
        }
        if let Some(user_id) = data.get("userId").or_else(|| data.get("user_id")).and_then(|v| v.as_str()) {
            req_builder = req_builder.header("x-userid", user_id);
        }

        let resp = req_builder
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

        let req_model = req.model.clone();

        if req.stream_enabled() {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);
            let mut upstream_stream = resp.bytes_stream();

            tokio::spawn(async move {
                let mut buffer = String::new();
                let mut resp_id = format!("chatcmpl-{}", Uuid::new_v4().simple());
                let mut first_chunk_sent = false;

                while let Some(chunk_res) = upstream_stream.next().await {
                    let chunk = match chunk_res {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = tx.send(Err(std::io::Error::new(std::io::ErrorKind::Other, e))).await;
                            return;
                        }
                    };

                    buffer.push_str(&String::from_utf8_lossy(&chunk));

                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].trim_end_matches('\r').to_string();
                        buffer = buffer[pos + 1..].to_string();

                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }

                        if let Some(data_str) = trimmed.strip_prefix("data:") {
                            let data_str = data_str.trim();
                            if data_str == "[DONE]" {
                                let _ = tx.send(Ok(bytes::Bytes::from("data: [DONE]\n\n"))).await;
                                return;
                            }

                            if let Ok(v) = serde_json::from_str::<Value>(data_str) {
                                let event_type = v.get("type").and_then(|s| s.as_str()).unwrap_or("");

                                match event_type {
                                    "response.created" | "response.in_progress" => {
                                        if let Some(id) = v.pointer("/response/id").and_then(|s| s.as_str()) {
                                            resp_id = format!("chatcmpl-{id}");
                                        }
                                        if !first_chunk_sent {
                                            first_chunk_sent = true;
                                            let chunk_json = json!({
                                                "id": resp_id,
                                                "object": "chat.completion.chunk",
                                                "created": Utc::now().timestamp(),
                                                "model": req_model,
                                                "choices": [{
                                                    "index": 0,
                                                    "delta": { "role": "assistant", "content": "" },
                                                    "finish_reason": null
                                                }]
                                            });
                                            let msg = format!("data: {}\n\n", serde_json::to_string(&chunk_json).unwrap());
                                            if tx.send(Ok(bytes::Bytes::from(msg))).await.is_err() {
                                                return;
                                            }
                                        }
                                    }
                                    "response.output_text.delta" => {
                                        if let Some(delta) = v.get("delta").and_then(|s| s.as_str()) {
                                            if !delta.is_empty() {
                                                let chunk_json = json!({
                                                    "id": resp_id,
                                                    "object": "chat.completion.chunk",
                                                    "created": Utc::now().timestamp(),
                                                    "model": req_model,
                                                    "choices": [{
                                                        "index": 0,
                                                        "delta": { "content": delta },
                                                        "finish_reason": null
                                                    }]
                                                });
                                                let msg = format!("data: {}\n\n", serde_json::to_string(&chunk_json).unwrap());
                                                if tx.send(Ok(bytes::Bytes::from(msg))).await.is_err() {
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                    "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                                        if let Some(delta) = v.get("delta").and_then(|s| s.as_str()) {
                                            if !delta.is_empty() {
                                                let chunk_json = json!({
                                                    "id": resp_id,
                                                    "object": "chat.completion.chunk",
                                                    "created": Utc::now().timestamp(),
                                                    "model": req_model,
                                                    "choices": [{
                                                        "index": 0,
                                                        "delta": { "reasoning_content": delta },
                                                        "finish_reason": null
                                                    }]
                                                });
                                                let msg = format!("data: {}\n\n", serde_json::to_string(&chunk_json).unwrap());
                                                if tx.send(Ok(bytes::Bytes::from(msg))).await.is_err() {
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                    "response.completed" => {
                                        let chunk_json = json!({
                                            "id": resp_id,
                                            "object": "chat.completion.chunk",
                                            "created": Utc::now().timestamp(),
                                            "model": req_model,
                                            "choices": [{
                                                "index": 0,
                                                "delta": {},
                                                "finish_reason": "stop"
                                            }]
                                        });
                                        let msg = format!("data: {}\n\ndata: [DONE]\n\n", serde_json::to_string(&chunk_json).unwrap());
                                        let _ = tx.send(Ok(bytes::Bytes::from(msg))).await;
                                        return;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }

                // If stream ended without response.completed or [DONE]
                let chunk_json = json!({
                    "id": resp_id,
                    "object": "chat.completion.chunk",
                    "created": Utc::now().timestamp(),
                    "model": req_model,
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": "stop"
                    }]
                });
                let msg = format!("data: {}\n\ndata: [DONE]\n\n", serde_json::to_string(&chunk_json).unwrap());
                let _ = tx.send(Ok(bytes::Bytes::from(msg))).await;
            });

            let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
            let mut headers = HeaderMap::new();
            headers.insert("content-type", HeaderValue::from_static("text/event-stream"));
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
            let mut buffer = String::new();
            let mut resp_id = format!("chatcmpl-{}", Uuid::new_v4().simple());
            let mut accumulated_text = String::new();
            let mut prompt_tokens: i64 = 0;
            let mut completion_tokens: i64 = 0;
            let mut total_tokens: i64 = 0;
            let mut upstream_stream = resp.bytes_stream();

            while let Some(chunk_res) = upstream_stream.next().await {
                let chunk = chunk_res.map_err(|e| ProviderError::Transport(e.to_string()))?;
                buffer.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(pos) = buffer.find('\n') {
                    let line = buffer[..pos].trim_end_matches('\r').to_string();
                    buffer = buffer[pos + 1..].to_string();

                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    if let Some(data_str) = trimmed.strip_prefix("data:") {
                        let data_str = data_str.trim();
                        if data_str == "[DONE]" {
                            break;
                        }

                        if let Ok(v) = serde_json::from_str::<Value>(data_str) {
                            let event_type = v.get("type").and_then(|s| s.as_str()).unwrap_or("");
                            match event_type {
                                "response.created" | "response.in_progress" => {
                                    if let Some(id) = v.pointer("/response/id").and_then(|s| s.as_str()) {
                                        resp_id = format!("chatcmpl-{id}");
                                    }
                                }
                                "response.output_text.delta" => {
                                    if let Some(delta) = v.get("delta").and_then(|s| s.as_str()) {
                                        accumulated_text.push_str(delta);
                                    }
                                }
                                "response.completed" => {
                                    if let Some(u) = v.pointer("/response/usage").or_else(|| v.get("usage"))
                                    {
                                        prompt_tokens = usage_i64(u, "input_tokens")
                                            .or_else(|| usage_i64(u, "prompt_tokens"))
                                            .unwrap_or(prompt_tokens);
                                        completion_tokens = usage_i64(u, "output_tokens")
                                            .or_else(|| usage_i64(u, "completion_tokens"))
                                            .unwrap_or(completion_tokens);
                                        total_tokens = usage_i64(u, "total_tokens").unwrap_or(
                                            if prompt_tokens > 0 || completion_tokens > 0 {
                                                prompt_tokens + completion_tokens
                                            } else {
                                                total_tokens
                                            },
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }

            if total_tokens == 0 && completion_tokens == 0 && !accumulated_text.is_empty() {
                completion_tokens = estimate_tokens(&accumulated_text);
                total_tokens = prompt_tokens + completion_tokens;
            }

            let result_json = json!({
                "id": resp_id,
                "object": "chat.completion",
                "created": Utc::now().timestamp(),
                "model": req_model,
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": accumulated_text
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "total_tokens": total_tokens
                }
            });

            Ok(ChatOutcome::Json(result_json))
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

fn usage_i64(usage: &Value, key: &str) -> Option<i64> {
    usage
        .get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|n| n as i64)))
}

fn estimate_tokens(text: &str) -> i64 {
    let n = (text.chars().count() as f64 / 4.0).ceil() as i64;
    n.max(1)
}
