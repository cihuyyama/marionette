use super::{ChatOutcome, Provider, StreamUsage, classify_http_status};
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
use tokio::sync::oneshot;
use uuid::Uuid;

const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const RESPONSES_URL: &str = "https://cli-chat-proxy.grok.com/v1/responses";
const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing";
const USER_AGENT: &str = "grok-shell/0.2.114 (linux; x86_64)";
const CLIENT_IDENTIFIER: &str = "grok-shell";
const CLIENT_VERSION: &str = "0.2.114";
const TOKEN_AUTH: &str = "xai-grok-cli";
const COMPACTION_AT: &str = "400000";

pub struct GrokCliProvider {
    client: Client,
    config: Arc<Config>,
}

impl GrokCliProvider {
    pub fn new(config: Arc<Config>) -> Self {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(15))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(60))
            .tcp_nodelay(true)
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

    pub async fn generate_image(
        &self,
        account: &Account,
        responses_model: &str,
        prompt: &str,
        refs: &[String],
        n: u32,
    ) -> Result<Vec<String>, ProviderError> {
        let data = account.data_json();
        let token = Self::access_token(&data)
            .ok_or_else(|| ProviderError::AuthInvalid("missing accessToken".into()))?;

        let (upstream_model_id, effort) = resolve_model_and_effort(responses_model);
        let content = crate::images::build_imagine_input_content(prompt, refs);
        let input_items = vec![json!({
            "role": "user",
            "content": content
        })];

        let mut reasoning = json!({ "summary": "concise" });
        if supports_reasoning_effort(&upstream_model_id) {
            let e = if effort.is_some() {
                normalize_effort(effort.as_deref())
            } else {
                "low".into()
            };
            reasoning["effort"] = json!(e);
        }

        let body = json!({
            "model": upstream_model_id,
            "input": input_items,
            "stream": true,
            "store": false,
            "tools": [{ "type": "image_generation" }],
            "tool_choice": { "type": "image_generation" },
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
            .header("x-xai-token-auth", TOKEN_AUTH)
            .header("x-grok-client-identifier", CLIENT_IDENTIFIER)
            .header("x-grok-client-version", CLIENT_VERSION)
            .header("x-authenticateresponse", "authenticate-response")
            .header("x-compaction-at", COMPACTION_AT)
            .header("x-grok-session-id", &session_id)
            .header("x-grok-conv-id", &session_id)
            .header("x-grok-req-id", &req_id)
            .header("x-grok-turn-idx", "1")
            .header("x-grok-model-override", &upstream_model_id);

        if let Some(email) = data.get("email").and_then(|v| v.as_str()) {
            req_builder = req_builder.header("x-email", email);
        }
        if let Some(user_id) = data
            .get("userId")
            .or_else(|| data.get("user_id"))
            .and_then(|v| v.as_str())
        {
            req_builder = req_builder.header("x-userid", user_id);
        }

        let resp = req_builder
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_http_status(status, &text));
        }

        let mut buffer = String::new();
        let mut upstream_stream = resp.bytes_stream();
        while let Some(chunk_res) = upstream_stream.next().await {
            let chunk = chunk_res.map_err(|e| ProviderError::Transport(e.to_string()))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
        }

        let mut images = crate::images::parse_image_b64_from_sse(&buffer);
        if images.is_empty() {
            let snippet: String = buffer.chars().take(240).collect();
            return Err(ProviderError::Other(format!(
                "no image_generation_call result in SSE (len={}, head={snippet:?})",
                buffer.len()
            )));
        }

        let cap = n.max(1) as usize;
        if images.len() > cap {
            images.truncate(cap);
        }
        Ok(images)
    }

    /// Fetch grok-cli account credit config from `GET /v1/billing`.
    /// Fails closed: non-200 (e.g. expired token -> 401) is an error,
    /// never a fabricated balance. Caller must `ensure_fresh_auth` first.
    pub async fn fetch_billing(&self, account: &Account) -> Result<Value, ProviderError> {
        let data = account.data_json();
        let token = Self::access_token(&data)
            .ok_or_else(|| ProviderError::AuthInvalid("missing accessToken".into()))?;

        let resp = self
            .client
            .get(BILLING_URL)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json")
            .header("x-xai-token-auth", TOKEN_AUTH)
            .header("x-grok-client-identifier", CLIENT_IDENTIFIER)
            .header("x-grok-client-version", CLIENT_VERSION)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        if status != 200 {
            return Err(classify_http_status(status, &text));
        }

        let parsed: Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Other(format!("billing json: {e}")))?;
        let config = parsed.get("config").cloned().unwrap_or(parsed);
        Ok(config)
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

    async fn force_refresh(&self, account: &mut Account) -> Result<(), ProviderError> {
        self.refresh(account).await
    }

    async fn chat(
        &self,
        client: &Client,
        account: &Account,
        req: &ChatCompletionRequest,
    ) -> Result<ChatOutcome, ProviderError> {
        let data = account.data_json();
        let token = Self::access_token(&data)
            .ok_or_else(|| ProviderError::AuthInvalid("missing accessToken".into()))?;

        let model_name = req.upstream_model();
        let (upstream_model_id, effort) = resolve_model_and_effort(model_name);
        let input_items = build_responses_input(&req.messages);

        let mut reasoning = json!({ "summary": "concise" });
        if supports_reasoning_effort(&upstream_model_id) {
            reasoning["effort"] = json!(normalize_effort(effort.as_deref()));
        }

        let mut body = json!({
            "model": upstream_model_id,
            "input": input_items,
            "stream": true,
            "store": false,
            "reasoning": reasoning,
            "include": ["reasoning.encrypted_content"]
        });
        if let Some(tools) = normalize_grok_tools(req.tools.as_ref()) {
            body["tools"] = tools.clone();
            if let Some(choice) = normalize_tool_choice(req.tool_choice.as_ref(), &tools) {
                body["tool_choice"] = choice;
            }
            if let Some(ptc) = req.parallel_tool_calls.clone() {
                body["parallel_tool_calls"] = ptc;
            }
        }

        let session_id = Uuid::new_v4().to_string();
        let req_id = Uuid::new_v4().to_string();

        let mut req_builder = client
            .post(RESPONSES_URL)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("User-Agent", USER_AGENT)
            .header("x-xai-token-auth", TOKEN_AUTH)
            .header("x-grok-client-identifier", CLIENT_IDENTIFIER)
            .header("x-grok-client-version", CLIENT_VERSION)
            .header("x-authenticateresponse", "authenticate-response")
            .header("x-compaction-at", COMPACTION_AT)
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
            let (usage_tx, usage_rx) = oneshot::channel::<Option<StreamUsage>>();
            let mut upstream_stream = resp.bytes_stream();

            tokio::spawn(async move {
                let mut buffer = String::new();
                let mut resp_id = format!("chatcmpl-{}", Uuid::new_v4().simple());
                let mut first_chunk_sent = false;
                let mut prompt_tokens: i64 = 0;
                let mut completion_tokens: i64 = 0;
                let mut total_tokens: i64 = 0;
                let mut accumulated_text = String::new();
                let mut tool_state = ToolCallStreamState::default();
                let mut usage_tx = Some(usage_tx);

                while let Some(chunk_res) = upstream_stream.next().await {
                    let chunk = match chunk_res {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = tx
                                .send(Err(std::io::Error::new(std::io::ErrorKind::Other, e)))
                                .await;
                            if let Some(txu) = usage_tx.take() {
                                let _ = txu.send(None);
                            }
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
                                emit_stream_end(
                                    &tx,
                                    &resp_id,
                                    &req_model,
                                    prompt_tokens,
                                    completion_tokens,
                                    total_tokens,
                                    &accumulated_text,
                                    tool_state.any_tool_calls,
                                    &mut usage_tx,
                                )
                                .await;
                                return;
                            }

                            if let Ok(v) = serde_json::from_str::<Value>(data_str) {
                                let event_type =
                                    v.get("type").and_then(|s| s.as_str()).unwrap_or("");

                                match event_type {
                                    "response.created" | "response.in_progress" => {
                                        if let Some(id) =
                                            v.pointer("/response/id").and_then(|s| s.as_str())
                                        {
                                            resp_id = format!("chatcmpl-{id}");
                                        }
                                        if !first_chunk_sent {
                                            first_chunk_sent = true;
                                            if send_sse_json(
                                                &tx,
                                                &openai_role_chunk(&resp_id, &req_model),
                                            )
                                            .await
                                            .is_err()
                                            {
                                                if let Some(txu) = usage_tx.take() {
                                                    let _ = txu.send(None);
                                                }
                                                return;
                                            }
                                        }
                                    }
                                    "response.output_text.delta" => {
                                        if let Some(delta) =
                                            v.get("delta").and_then(|s| s.as_str())
                                        {
                                            if !delta.is_empty() {
                                                accumulated_text.push_str(delta);
                                                if send_sse_json(
                                                    &tx,
                                                    &openai_content_chunk(
                                                        &resp_id,
                                                        &req_model,
                                                        delta,
                                                    ),
                                                )
                                                .await
                                                .is_err()
                                                {
                                                    if let Some(txu) = usage_tx.take() {
                                                        let _ = txu.send(None);
                                                    }
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                    "response.reasoning_summary_text.delta"
                                    | "response.reasoning_text.delta" => {
                                        if let Some(delta) =
                                            v.get("delta").and_then(|s| s.as_str())
                                        {
                                            if !delta.is_empty() {
                                                if send_sse_json(
                                                    &tx,
                                                    &openai_reasoning_chunk(
                                                        &resp_id,
                                                        &req_model,
                                                        delta,
                                                    ),
                                                )
                                                .await
                                                .is_err()
                                                {
                                                    if let Some(txu) = usage_tx.take() {
                                                        let _ = txu.send(None);
                                                    }
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                    "response.output_item.added" => {
                                        if let Some(chunk_json) =
                                            tool_state.on_output_item_added(&v, &resp_id, &req_model)
                                        {
                                            if send_sse_json(&tx, &chunk_json).await.is_err() {
                                                if let Some(txu) = usage_tx.take() {
                                                    let _ = txu.send(None);
                                                }
                                                return;
                                            }
                                        }
                                    }
                                    "response.function_call_arguments.delta"
                                    | "response.custom_tool_call_input.delta" => {
                                        if let Some(chunk_json) = tool_state
                                            .on_function_args_delta(&v, &resp_id, &req_model)
                                        {
                                            if send_sse_json(&tx, &chunk_json).await.is_err() {
                                                if let Some(txu) = usage_tx.take() {
                                                    let _ = txu.send(None);
                                                }
                                                return;
                                            }
                                        }
                                    }
                                    "response.output_item.done" => {
                                        tool_state.on_output_item_done(&v);
                                    }
                                    "response.completed" => {
                                        if let Some(u) = v
                                            .pointer("/response/usage")
                                            .or_else(|| v.get("usage"))
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
                                        emit_stream_end(
                                            &tx,
                                            &resp_id,
                                            &req_model,
                                            prompt_tokens,
                                            completion_tokens,
                                            total_tokens,
                                            &accumulated_text,
                                            tool_state.any_tool_calls,
                                            &mut usage_tx,
                                        )
                                        .await;
                                        return;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }

                emit_stream_end(
                    &tx,
                    &resp_id,
                    &req_model,
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                    &accumulated_text,
                    tool_state.any_tool_calls,
                    &mut usage_tx,
                )
                .await;
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
            Ok(ChatOutcome::Stream {
                response: Response::from_parts(parts, body),
                usage_rx,
            })
        } else {
            let mut buffer = String::new();
            let mut resp_id = format!("chatcmpl-{}", Uuid::new_v4().simple());
            let mut accumulated_text = String::new();
            let mut prompt_tokens: i64 = 0;
            let mut completion_tokens: i64 = 0;
            let mut total_tokens: i64 = 0;
            let mut tool_state = ToolCallStreamState::default();
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
                                    if let Some(id) =
                                        v.pointer("/response/id").and_then(|s| s.as_str())
                                    {
                                        resp_id = format!("chatcmpl-{id}");
                                    }
                                }
                                "response.output_text.delta" => {
                                    if let Some(delta) = v.get("delta").and_then(|s| s.as_str()) {
                                        accumulated_text.push_str(delta);
                                    }
                                }
                                "response.output_item.added" => {
                                    let _ = tool_state.on_output_item_added(
                                        &v,
                                        &resp_id,
                                        &req_model,
                                    );
                                }
                                "response.function_call_arguments.delta"
                                | "response.custom_tool_call_input.delta" => {
                                    let _ = tool_state.on_function_args_delta(
                                        &v,
                                        &resp_id,
                                        &req_model,
                                    );
                                }
                                "response.function_call_arguments.done"
                                | "response.custom_tool_call_input.done" => {
                                    tool_state.on_function_args_done(&v);
                                }
                                "response.output_item.done" => {
                                    tool_state.on_output_item_done(&v);
                                }
                                "response.completed" => {
                                    if let Some(u) =
                                        v.pointer("/response/usage").or_else(|| v.get("usage"))
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

            let filled_tools = tool_state.filled_tool_calls();
            let any_tools = !filled_tools.is_empty();
            let mut message = json!({
                "role": "assistant",
                "content": if accumulated_text.is_empty() && any_tools {
                    Value::Null
                } else {
                    Value::String(accumulated_text)
                }
            });
            if any_tools {
                message["tool_calls"] = json!(filled_tools);
            }

            let result_json = json!({
                "id": resp_id,
                "object": "chat.completion",
                "created": Utc::now().timestamp(),
                "model": req_model,
                "choices": [{
                    "index": 0,
                    "message": message,
                    "finish_reason": if any_tools { "tool_calls" } else { "stop" }
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

fn supports_reasoning_effort(model: &str) -> bool {
    model == "grok-4.5" || model.starts_with("grok-4.5-")
}

fn normalize_effort(raw: Option<&str>) -> String {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("max") | Some("xhigh") => "xhigh".into(),
        Some("high") => "high".into(),
        Some("medium") => "medium".into(),
        Some("low") => "low".into(),
        Some("none") => "none".into(),
        _ => "high".into(),
    }
}

fn resolve_model_and_effort(model_name: &str) -> (String, Option<String>) {
    let (model, effort) = if let Some(base) = model_name.strip_suffix("-xhigh") {
        (base, Some("xhigh".to_string()))
    } else if let Some(base) = model_name.strip_suffix("-high") {
        (base, Some("high".to_string()))
    } else if let Some(base) = model_name.strip_suffix("-medium") {
        (base, Some("medium".to_string()))
    } else if let Some(base) = model_name.strip_suffix("-low") {
        (base, Some("low".to_string()))
    } else {
        (model_name, None)
    };

    let resolved = match model {
        "gb" | "grok-build" => "grok-build",
        m => m,
    };
    (resolved.to_string(), effort)
}

fn content_to_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for p in parts {
                if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                    out.push_str(t);
                } else if let Some(t) = p.as_str() {
                    out.push_str(t);
                } else if p.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                        out.push_str(t);
                    }
                }
            }
            out
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn normalize_image_detail(raw: Option<&str>) -> String {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") => "auto".into(),
        Some("auto") | Some("low") | Some("high") => raw.unwrap().trim().to_ascii_lowercase(),
        Some("original") => "high".into(),
        Some(other) => other.to_string(),
    }
}

fn extract_image_url_from_part(part: &Value) -> Option<String> {
    if let Some(s) = part.get("image_url").and_then(|v| v.as_str()) {
        let s = s.trim();
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    if let Some(obj) = part.get("image_url").and_then(|v| v.as_object()) {
        if let Some(s) = obj.get("url").and_then(|v| v.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    if let Some(s) = part.get("url").and_then(|v| v.as_str()) {
        let s = s.trim();
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    if let Some(s) = part.get("image").and_then(|v| v.as_str()) {
        let s = s.trim();
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    None
}

fn extract_image_detail_from_part(part: &Value) -> String {
    if let Some(d) = part.get("detail").and_then(|v| v.as_str()) {
        return normalize_image_detail(Some(d));
    }
    if let Some(obj) = part.get("image_url").and_then(|v| v.as_object()) {
        if let Some(d) = obj.get("detail").and_then(|v| v.as_str()) {
            return normalize_image_detail(Some(d));
        }
    }
    normalize_image_detail(None)
}

fn part_is_image(part: &Value) -> bool {
    match part.get("type").and_then(|v| v.as_str()) {
        Some("image_url") | Some("input_image") | Some("image") => true,
        _ => extract_image_url_from_part(part).is_some()
            && part.get("text").and_then(|v| v.as_str()).is_none(),
    }
}

fn content_array_has_image(parts: &[Value]) -> bool {
    parts.iter().any(part_is_image)
}

fn text_part_type_for_role(role: &str) -> &'static str {
    if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    }
}

fn part_to_input_image(part: &Value) -> Option<Value> {
    let url = extract_image_url_from_part(part)?;
    let detail = extract_image_detail_from_part(part);
    Some(json!({
        "type": "input_image",
        "image_url": url,
        "detail": detail
    }))
}

fn part_text(part: &Value) -> Option<String> {
    if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
        return Some(t.to_string());
    }
    if let Some(t) = part.get("refusal").and_then(|v| v.as_str()) {
        return Some(t.to_string());
    }
    if let Some(t) = part.as_str() {
        return Some(t.to_string());
    }
    None
}

fn content_to_message_content(content: &Value, role: &str) -> Value {
    match content {
        Value::String(s) => Value::String(s.clone()),
        Value::Null => Value::String(String::new()),
        Value::Array(parts) => {
            if !content_array_has_image(parts) {
                return Value::String(content_to_text(content));
            }
            let text_ty = text_part_type_for_role(role);
            let mut out = Vec::new();
            for p in parts {
                let ty = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match ty {
                    "image_url" | "input_image" | "image" => {
                        if let Some(img) = part_to_input_image(p) {
                            out.push(img);
                        }
                    }
                    "text" | "input_text" | "output_text" | "refusal" | "" => {
                        if part_is_image(p) {
                            if let Some(img) = part_to_input_image(p) {
                                out.push(img);
                            }
                        } else if let Some(t) = part_text(p) {
                            out.push(json!({ "type": text_ty, "text": t }));
                        }
                    }
                    _ => {
                        if let Some(img) = part_to_input_image(p) {
                            out.push(img);
                        } else if let Some(t) = part_text(p) {
                            out.push(json!({ "type": text_ty, "text": t }));
                        }
                    }
                }
            }
            Value::Array(out)
        }
        other => Value::String(other.to_string()),
    }
}

fn build_responses_input(messages: &[crate::openai::ChatMessage]) -> Vec<Value> {
    let mut input = Vec::new();

    for msg in messages {
        let role = msg.role.as_str();
        if role == "tool" {
            let call_id = msg
                .tool_call_id
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "call_unknown".into());
            let output = content_to_message_content(&msg.content, "user");
            input.push(json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": output
            }));
            continue;
        }

        if role == "assistant" {
            if let Some(tool_calls) = msg.tool_calls.as_ref().and_then(|v| v.as_array()) {
                let content = content_to_message_content(&msg.content, role);
                let empty = match &content {
                    Value::String(s) => s.is_empty(),
                    Value::Array(a) => a.is_empty(),
                    Value::Null => true,
                    _ => false,
                };
                if !empty {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": content
                    }));
                }
                for tc in tool_calls {
                    let call_id = tc
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let fn_obj = tc.get("function").cloned().unwrap_or(Value::Null);
                    let name = fn_obj
                        .get("name")
                        .or_else(|| tc.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if call_id.is_empty() || name.is_empty() {
                        continue;
                    }
                    let arguments = match fn_obj.get("arguments").or_else(|| tc.get("arguments")) {
                        Some(Value::String(s)) => s.clone(),
                        Some(other) => other.to_string(),
                        None => "{}".into(),
                    };
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call_id,
                        "name": name,
                        "arguments": arguments
                    }));
                }
                continue;
            }
        }

        let content = content_to_message_content(&msg.content, role);
        input.push(json!({
            "type": "message",
            "role": role,
            "content": content
        }));
    }

    let call_ids: std::collections::HashSet<String> = input
        .iter()
        .filter(|i| i.get("type").and_then(|t| t.as_str()) == Some("function_call"))
        .filter_map(|i| i.get("call_id").and_then(|c| c.as_str()).map(|s| s.to_string()))
        .collect();
    input.retain(|item| {
        if item.get("type").and_then(|t| t.as_str()) == Some("function_call_output") {
            item.get("call_id")
                .and_then(|c| c.as_str())
                .map(|id| call_ids.contains(id))
                .unwrap_or(false)
        } else {
            true
        }
    });

    if input.is_empty() {
        input.push(json!({
            "type": "message",
            "role": "user",
            "content": "..."
        }));
    }
    input
}

const HOSTED_TOOL_TYPES: &[&str] = &[
    "web_search",
    "x_search",
    "web_search_preview",
    "file_search",
    "image_generation",
    "code_interpreter",
    "mcp",
    "local_shell",
];

fn normalize_grok_tools(tools: Option<&Value>) -> Option<Value> {
    let arr = tools?.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let freeform_params = json!({
        "type": "object",
        "properties": { "input": { "type": "string" } },
        "required": ["input"]
    });
    let mut out = Vec::new();
    for tool in arr {
        if !tool.is_object() {
            continue;
        }
        let type_s = tool.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if HOSTED_TOOL_TYPES.contains(&type_s) {
            out.push(tool.clone());
            continue;
        }
        let fn_obj = tool.get("function");
        let raw_name = tool
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| fn_obj.and_then(|f| f.get("name")).and_then(|v| v.as_str()))
            .unwrap_or("")
            .trim();
        if raw_name.is_empty() && type_s != "function" && type_s != "custom" && fn_obj.is_none() {
            continue;
        }
        if raw_name.is_empty() {
            continue;
        }
        let name: String = raw_name.chars().take(128).collect();
        let description = tool
            .get("description")
            .and_then(|v| v.as_str())
            .or_else(|| {
                fn_obj
                    .and_then(|f| f.get("description"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("");
        let parameters = if type_s == "custom" {
            freeform_params.clone()
        } else if let Some(p) = tool
            .get("parameters")
            .filter(|p| p.is_object())
            .cloned()
            .or_else(|| {
                fn_obj
                    .and_then(|f| f.get("parameters"))
                    .filter(|p| p.is_object())
                    .cloned()
            }) {
            p
        } else {
            json!({ "type": "object", "properties": {} })
        };
        let mut item = json!({
            "type": "function",
            "name": name,
            "parameters": parameters
        });
        if !description.is_empty() {
            item["description"] = json!(description);
        }
        out.push(item);
    }
    if out.is_empty() {
        None
    } else {
        Some(Value::Array(out))
    }
}

fn normalize_tool_choice(choice: Option<&Value>, tools: &Value) -> Option<Value> {
    let choice = choice?;
    let valid_names: std::collections::HashSet<&str> = tools
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .collect();
    let hosted: std::collections::HashSet<&str> = tools
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t.get("type").and_then(|n| n.as_str()))
        .filter(|t| HOSTED_TOOL_TYPES.contains(t))
        .collect();

    match choice {
        Value::String(s) => Some(Value::String(s.clone())),
        Value::Object(_) => {
            let choice_type = choice.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if choice_type == "function" || choice_type == "custom" {
                let name = choice
                    .get("name")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        choice
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or("")
                    .trim();
                let name: String = name.chars().take(128).collect();
                if name.is_empty() || !valid_names.contains(name.as_str()) {
                    None
                } else {
                    Some(json!({ "type": "function", "name": name }))
                }
            } else if hosted.contains(choice_type) {
                Some(choice.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

#[derive(Default)]
struct ToolCallStreamState {
    any_tool_calls: bool,
    next_tool_index: usize,
    names: std::collections::HashMap<usize, String>,
    args: std::collections::HashMap<usize, String>,
    ids: std::collections::HashMap<usize, String>,
    item_id_to_index: std::collections::HashMap<String, usize>,
}

impl ToolCallStreamState {
    fn resolve_tool_index(&self, v: &Value, item: Option<&Value>) -> Option<usize> {
        if let Some(item) = item {
            if let Some(item_id) = item.get("id").and_then(|i| i.as_str()) {
                if let Some(idx) = self.item_id_to_index.get(item_id).copied() {
                    return Some(idx);
                }
            }
            if let Some(call_id) = item.get("call_id").and_then(|c| c.as_str()) {
                if let Some(idx) = self.item_id_to_index.get(call_id).copied() {
                    return Some(idx);
                }
                if let Some(idx) = self
                    .item_id_to_index
                    .get(&format!("fc_{call_id}"))
                    .copied()
                {
                    return Some(idx);
                }
            }
        }
        if let Some(item_id) = v.get("item_id").and_then(|i| i.as_str()) {
            if let Some(idx) = self.item_id_to_index.get(item_id).copied() {
                return Some(idx);
            }
        }
        if let Some(call_id) = v.get("call_id").and_then(|c| c.as_str()) {
            if let Some(idx) = self.item_id_to_index.get(call_id).copied() {
                return Some(idx);
            }
            if let Some(idx) = self
                .item_id_to_index
                .get(&format!("fc_{call_id}"))
                .copied()
            {
                return Some(idx);
            }
        }
        if self.ids.len() == 1 {
            return self.ids.keys().next().copied();
        }
        None
    }

    fn on_output_item_added(
        &mut self,
        v: &Value,
        resp_id: &str,
        req_model: &str,
    ) -> Option<Value> {
        let item = v.get("item")?;
        let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if item_type != "function_call" && item_type != "custom_tool_call" {
            return None;
        }
        let call_id = item
            .get("call_id")
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("call_{}", Uuid::new_v4().simple()));
        let name = item
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        let idx = self.next_tool_index;
        self.next_tool_index += 1;
        self.ids.insert(idx, call_id.clone());
        self.names.insert(idx, name.clone());
        self.args.entry(idx).or_default();
        if let Some(item_id) = item.get("id").and_then(|i| i.as_str()) {
            self.item_id_to_index.insert(item_id.to_string(), idx);
        }
        self.item_id_to_index.insert(call_id.clone(), idx);
        self.item_id_to_index
            .insert(format!("fc_{call_id}"), idx);
        self.any_tool_calls = true;
        Some(openai_tool_call_chunk(
            resp_id,
            req_model,
            idx,
            Some(&call_id),
            Some(&name),
            Some(""),
        ))
    }

    fn on_function_args_delta(
        &mut self,
        v: &Value,
        resp_id: &str,
        req_model: &str,
    ) -> Option<Value> {
        let delta = v.get("delta").and_then(|d| d.as_str()).unwrap_or("");
        if delta.is_empty() {
            return None;
        }
        let idx = self.resolve_tool_index(v, None)?;
        self.args.entry(idx).or_default().push_str(delta);
        self.any_tool_calls = true;
        Some(openai_tool_call_chunk(
            resp_id, req_model, idx, None, None, Some(delta),
        ))
    }

    fn on_function_args_done(&mut self, v: &Value) {
        if let Some(args) = v.get("arguments").and_then(|a| a.as_str()) {
            if let Some(idx) = self.resolve_tool_index(v, None) {
                self.args.insert(idx, args.to_string());
                self.any_tool_calls = true;
            }
        }
    }

    fn on_output_item_done(&mut self, v: &Value) {
        let item = match v.get("item") {
            Some(i) => i,
            None => return,
        };
        let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if item_type != "function_call" && item_type != "custom_tool_call" {
            return;
        }
        let idx = match self.resolve_tool_index(v, Some(item)) {
            Some(i) => i,
            None => {
                let call_id = item
                    .get("call_id")
                    .and_then(|c| c.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("call_{}", Uuid::new_v4().simple()));
                let name = item
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    return;
                }
                let idx = self.next_tool_index;
                self.next_tool_index += 1;
                self.ids.insert(idx, call_id.clone());
                self.names.insert(idx, name);
                if let Some(item_id) = item.get("id").and_then(|i| i.as_str()) {
                    self.item_id_to_index.insert(item_id.to_string(), idx);
                }
                self.item_id_to_index.insert(call_id.clone(), idx);
                self.item_id_to_index
                    .insert(format!("fc_{call_id}"), idx);
                idx
            }
        };
        if let Some(args) = item.get("arguments").and_then(|a| a.as_str()) {
            if !args.is_empty() {
                self.args.insert(idx, args.to_string());
            }
        }
        if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
            if !name.is_empty() {
                self.names.insert(idx, name.to_string());
            }
        }
        if let Some(call_id) = item.get("call_id").and_then(|c| c.as_str()) {
            if !call_id.is_empty() {
                self.ids.insert(idx, call_id.to_string());
            }
        }
        self.any_tool_calls = true;
    }

    fn filled_tool_calls(&self) -> Vec<Value> {
        let mut keys: Vec<usize> = self.ids.keys().copied().collect();
        keys.sort_unstable();
        let mut seen_ids = std::collections::HashSet::new();
        keys.into_iter()
            .filter_map(|idx| {
                let id = self.ids.get(&idx)?;
                if !seen_ids.insert(id.clone()) {
                    return None;
                }
                let name = self.names.get(&idx).cloned().unwrap_or_default();
                if name.is_empty() {
                    return None;
                }
                let arguments = self
                    .args
                    .get(&idx)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "{}".into());
                Some(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments
                    }
                }))
            })
            .collect()
    }
}

fn openai_role_chunk(resp_id: &str, req_model: &str) -> Value {
    json!({
        "id": resp_id,
        "object": "chat.completion.chunk",
        "created": Utc::now().timestamp(),
        "model": req_model,
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": "" },
            "finish_reason": null
        }]
    })
}

fn openai_content_chunk(resp_id: &str, req_model: &str, delta: &str) -> Value {
    json!({
        "id": resp_id,
        "object": "chat.completion.chunk",
        "created": Utc::now().timestamp(),
        "model": req_model,
        "choices": [{
            "index": 0,
            "delta": { "content": delta },
            "finish_reason": null
        }]
    })
}

fn openai_reasoning_chunk(resp_id: &str, req_model: &str, delta: &str) -> Value {
    json!({
        "id": resp_id,
        "object": "chat.completion.chunk",
        "created": Utc::now().timestamp(),
        "model": req_model,
        "choices": [{
            "index": 0,
            "delta": { "reasoning_content": delta },
            "finish_reason": null
        }]
    })
}

fn openai_tool_call_chunk(
    resp_id: &str,
    req_model: &str,
    index: usize,
    id: Option<&str>,
    name: Option<&str>,
    arguments: Option<&str>,
) -> Value {
    let mut tc = json!({ "index": index });
    if let Some(id) = id {
        tc["id"] = json!(id);
        tc["type"] = json!("function");
    }
    let mut function = serde_json::Map::new();
    if let Some(name) = name {
        function.insert("name".into(), json!(name));
    }
    if let Some(arguments) = arguments {
        function.insert("arguments".into(), json!(arguments));
    }
    if !function.is_empty() {
        tc["function"] = Value::Object(function);
    }
    json!({
        "id": resp_id,
        "object": "chat.completion.chunk",
        "created": Utc::now().timestamp(),
        "model": req_model,
        "choices": [{
            "index": 0,
            "delta": { "tool_calls": [tc] },
            "finish_reason": null
        }]
    })
}

async fn send_sse_json(
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    value: &Value,
) -> Result<(), ()> {
    let msg = format!(
        "data: {}\n\n",
        serde_json::to_string(value).unwrap_or_else(|_| "{}".into())
    );
    tx.send(Ok(bytes::Bytes::from(msg))).await.map_err(|_| ())
}

async fn emit_stream_end(
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    resp_id: &str,
    req_model: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    accumulated_text: &str,
    any_tool_calls: bool,
    usage_tx: &mut Option<oneshot::Sender<Option<StreamUsage>>>,
) {
    let p = prompt_tokens;
    let mut c = completion_tokens;
    let mut t = total_tokens;
    if t == 0 && c == 0 && !accumulated_text.is_empty() {
        c = estimate_tokens(accumulated_text);
        t = p + c;
    } else if t == 0 && (p > 0 || c > 0) {
        t = p + c;
    }
    let usage = StreamUsage {
        prompt_tokens: p,
        completion_tokens: c,
        total_tokens: t,
    }
    .normalized();

    let finish_reason = if any_tool_calls {
        "tool_calls"
    } else {
        "stop"
    };
    let finish_json = json!({
        "id": resp_id,
        "object": "chat.completion.chunk",
        "created": Utc::now().timestamp(),
        "model": req_model,
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": finish_reason
        }]
    });
    let _ = tx
        .send(Ok(bytes::Bytes::from(format!(
            "data: {}\n\n",
            serde_json::to_string(&finish_json).unwrap()
        ))))
        .await;

    if !usage.is_empty() {
        let usage_json = json!({
            "id": resp_id,
            "object": "chat.completion.chunk",
            "created": Utc::now().timestamp(),
            "model": req_model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": null
            }],
            "usage": {
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens
            }
        });
        let _ = tx
            .send(Ok(bytes::Bytes::from(format!(
                "data: {}\n\n",
                serde_json::to_string(&usage_json).unwrap()
            ))))
            .await;
    }

    let _ = tx.send(Ok(bytes::Bytes::from("data: [DONE]\n\n"))).await;
    if let Some(txu) = usage_tx.take() {
        let _ = txu.send(if usage.is_empty() {
            None
        } else {
            Some(usage)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::ChatMessage;

    #[test]
    fn resolve_effort_xhigh_before_high() {
        let (m, e) = resolve_model_and_effort("grok-4.5-xhigh");
        assert_eq!(m, "grok-4.5");
        assert_eq!(e.as_deref(), Some("xhigh"));
        let (m, e) = resolve_model_and_effort("grok-4.5-high");
        assert_eq!(m, "grok-4.5");
        assert_eq!(e.as_deref(), Some("high"));
    }

    #[test]
    fn effort_gate_only_grok_45() {
        assert!(supports_reasoning_effort("grok-4.5"));
        assert!(supports_reasoning_effort("grok-4.5-something"));
        assert!(!supports_reasoning_effort("grok-build"));
        assert!(!supports_reasoning_effort("grok-4"));
    }

    #[test]
    fn normalize_effort_max_to_xhigh() {
        assert_eq!(normalize_effort(Some("max")), "xhigh");
        assert_eq!(normalize_effort(Some("XHIGH")), "xhigh");
        assert_eq!(normalize_effort(None), "high");
    }

    #[test]
    fn normalize_tools_flattens_openai_shape() {
        let tools = json!([{
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "weather",
                "parameters": { "type": "object", "properties": { "city": { "type": "string" } } }
            }
        }]);
        let out = normalize_grok_tools(Some(&tools)).unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "function");
        assert_eq!(arr[0]["name"], "get_weather");
        assert!(arr[0].get("function").is_none());
        assert!(arr[0].get("parameters").is_some());
    }

    #[test]
    fn build_input_maps_tool_calls_and_outputs() {
        let messages = vec![
            ChatMessage {
                role: "user".into(),
                content: json!("weather?"),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".into(),
                content: json!(""),
                name: None,
                tool_calls: Some(json!([{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "get_weather", "arguments": "{\"city\":\"NY\"}" }
                }])),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".into(),
                content: json!("sunny"),
                name: None,
                tool_calls: None,
                tool_call_id: Some("call_1".into()),
            },
        ];
        let input = build_responses_input(&messages);
        assert!(
            input
                .iter()
                .any(|i| i.get("type").and_then(|t| t.as_str()) == Some("function_call"))
        );
        assert!(
            input
                .iter()
                .any(|i| i.get("type").and_then(|t| t.as_str()) == Some("function_call_output"))
        );
    }

    #[test]
    fn build_input_preserves_image_url_parts() {
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: json!([
                { "type": "text", "text": "what is this?" },
                {
                    "type": "image_url",
                    "image_url": { "url": "https://example.com/cat.png" }
                }
            ]),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }];
        let input = build_responses_input(&messages);
        assert_eq!(input.len(), 1);
        let content = input[0].get("content").unwrap();
        let parts = content.as_array().expect("multimodal content must be array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "input_text");
        assert_eq!(parts[0]["text"], "what is this?");
        assert_eq!(parts[1]["type"], "input_image");
        assert_eq!(parts[1]["image_url"], "https://example.com/cat.png");
        assert_eq!(parts[1]["detail"], "auto");
    }

    #[test]
    fn build_input_image_url_string_form() {
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: json!([
                { "type": "text", "text": "decode" },
                {
                    "type": "image_url",
                    "image_url": "data:image/png;base64,AA=="
                }
            ]),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }];
        let input = build_responses_input(&messages);
        let parts = input[0]["content"].as_array().unwrap();
        assert_eq!(parts[1]["type"], "input_image");
        assert_eq!(parts[1]["image_url"], "data:image/png;base64,AA==");
        assert_eq!(parts[1]["detail"], "auto");
    }

    #[test]
    fn build_input_input_image_native() {
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: json!([
                { "type": "input_text", "text": "see" },
                {
                    "type": "input_image",
                    "image_url": "https://cdn.example/x.jpg",
                    "detail": "high"
                }
            ]),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }];
        let input = build_responses_input(&messages);
        let parts = input[0]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "input_text");
        assert_eq!(parts[1]["type"], "input_image");
        assert_eq!(parts[1]["image_url"], "https://cdn.example/x.jpg");
        assert_eq!(parts[1]["detail"], "high");
    }

    #[test]
    fn build_input_text_only_stays_string() {
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: json!("hello plain"),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }];
        let input = build_responses_input(&messages);
        assert_eq!(input[0]["content"], "hello plain");
        assert!(input[0]["content"].is_string());
    }

    #[test]
    fn build_input_text_array_no_image_stays_joined_string() {
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: json!([
                { "type": "text", "text": "foo" },
                { "type": "text", "text": "bar" }
            ]),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }];
        let input = build_responses_input(&messages);
        assert!(input[0]["content"].is_string());
        assert_eq!(input[0]["content"], "foobar");
    }

    #[test]
    fn build_input_tool_multimodal_output_array() {
        let messages = vec![
            ChatMessage {
                role: "user".into(),
                content: json!("go"),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: "assistant".into(),
                content: json!(""),
                name: None,
                tool_calls: Some(json!([{
                    "id": "call_img",
                    "type": "function",
                    "function": { "name": "screenshot", "arguments": "{}" }
                }])),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".into(),
                content: json!([
                    { "type": "text", "text": "captured" },
                    {
                        "type": "image_url",
                        "image_url": { "url": "data:image/png;base64,BB==", "detail": "low" }
                    }
                ]),
                name: None,
                tool_calls: None,
                tool_call_id: Some("call_img".into()),
            },
        ];
        let input = build_responses_input(&messages);
        let output = input
            .iter()
            .find(|i| i.get("type").and_then(|t| t.as_str()) == Some("function_call_output"))
            .expect("function_call_output");
        let parts = output["output"]
            .as_array()
            .expect("tool multimodal output must be array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "input_text");
        assert_eq!(parts[0]["text"], "captured");
        assert_eq!(parts[1]["type"], "input_image");
        assert_eq!(parts[1]["image_url"], "data:image/png;base64,BB==");
        assert_eq!(parts[1]["detail"], "low");
    }

    #[test]
    fn stream_tool_state_emits_openai_chunks() {
        let mut st = ToolCallStreamState::default();
        let added = json!({
            "type": "response.output_item.added",
            "item": {
                "id": "fc_call_abc",
                "type": "function_call",
                "call_id": "call_abc",
                "name": "get_weather",
                "arguments": ""
            }
        });
        let chunk = st
            .on_output_item_added(&added, "chatcmpl-1", "gcli/grok-4.5")
            .unwrap();
        let tc = &chunk["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(tc["id"], "call_abc");
        assert_eq!(tc["function"]["name"], "get_weather");
        assert_eq!(tc["index"], 0);

        let delta = json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "fc_call_abc",
            "output_index": 1,
            "delta": "{\"city\":"
        });
        let chunk2 = st
            .on_function_args_delta(&delta, "chatcmpl-1", "gcli/grok-4.5")
            .unwrap();
        assert_eq!(
            chunk2["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
            "{\"city\":"
        );
        assert_eq!(chunk2["choices"][0]["delta"]["tool_calls"][0]["index"], 0);
        assert!(st.any_tool_calls);

        st.on_function_args_done(&json!({
            "type": "response.function_call_arguments.done",
            "item_id": "fc_call_abc",
            "output_index": 1,
            "arguments": "{\"city\":\"Paris\"}"
        }));
        st.on_output_item_done(&json!({
            "type": "response.output_item.done",
            "output_index": 1,
            "item": {
                "id": "fc_call_abc",
                "type": "function_call",
                "call_id": "call_abc",
                "name": "get_weather",
                "arguments": "{\"city\":\"Paris\"}"
            }
        }));

        let filled = st.filled_tool_calls();
        assert_eq!(filled.len(), 1);
        assert_eq!(filled[0]["id"], "call_abc");
        assert_eq!(filled[0]["function"]["name"], "get_weather");
        assert_eq!(filled[0]["function"]["arguments"], "{\"city\":\"Paris\"}");
    }
}
