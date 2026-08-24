//! Command Code provider â€” `https://api.commandcode.ai/alpha/generate`.
//!
//! NDJSON (Vercel AI SDK) streaming gateway authenticated with a bearer API
//! key (`user_â€¦`). Ported from Cartethyia `src/providers/commandcode.ts`.
//! Unlike the `/provider/v1` OpenAI-compatible surface (which the Go plan
//! blocks with `upgrade_required`), `/alpha/generate` is available on the Go
//! plan, so this provider gives Go-plan keys real model access.
//!
//! Account shape: `provider = "commandcode"`, `data = { apiKey }`.
//! Public model ids are `cmc/<upstream-id>`.

use super::{classify_http_status, ChatOutcome, Provider, StreamUsage};
use crate::error::ProviderError;
use crate::openai::ChatCompletionRequest;
use async_trait::async_trait;
use axum::body::Body;
use axum::response::Response;
use chrono::Utc;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::oneshot;
use uuid::Uuid;

const GENERATE_URL: &str = "https://api.commandcode.ai/alpha/generate";
const MODELS_URL: &str = "https://api.commandcode.ai/provider/v1/models";
const CC_VERSION: &str = "1.4.4";
const DEFAULT_MAX_TOKENS: u32 = 4096;

pub const COMMANDCODE_PROVIDER: &str = "commandcode";

/// One upstream catalog row (`GET /provider/v1/models`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CcModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<i64>,
}

pub fn fmt_context_length(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        let m = tokens / 1_000_000;
        if tokens % 1_000_000 == 0 {
            format!("{m}M")
        } else {
            format!("{}K", tokens / 1000)
        }
    } else if tokens >= 1000 {
        format!("{}K", tokens / 1000)
    } else {
        format!("{tokens}")
    }
}

/// Static catalog (mirrors Cartethyia `COMMANDCODE_MODELS`). Upstream ids keep
/// their own slashes; public ids are `cmc/<upstream-id>`.
pub const COMMANDCODE_MODELS: &[(&str, &str)] = &[
    ("moonshotai/Kimi-K2.6", "Kimi K2.6"),
    ("moonshotai/Kimi-K3", "Kimi K3"),
    ("moonshotai/Kimi-K2.7-Code", "Kimi K2.7 Code"),
    ("qwen/qwen3.5-plus", "Qwen 3.5 Plus"),
    ("Qwen/Qwen3.6-Plus", "Qwen 3.6 Plus"),
    ("Qwen/Qwen3.7-Max", "Qwen 3.7 Max"),
    ("minimax/minimax-m2.7-highspeed", "MiniMax M2.7"),
    ("MiniMaxAI/MiniMax-M3", "MiniMax M3"),
    ("z-ai/glm-5.1", "GLM 5.1"),
    ("zai-org/GLM-5.2", "GLM 5.2"),
    ("zai-org/GLM-5.2-Fast", "GLM 5.2 Fast"),
    ("deepseek/deepseek-v4-pro", "DeepSeek V4 Pro"),
    ("deepseek/deepseek-v4-flash", "DeepSeek V4 Flash"),
    ("xiaomi/mimo-v2.5-pro", "Xiaomi MiMo v2.5 Pro"),
    ("xiaomi/mimo-v2.5", "Xiaomi MiMo v2.5"),
    ("stealth/ox-alpha", "Ox Alpha"),
    ("poolside/laguna-s-2.1-free", "Poolside Laguna S 2.1 Free"),
    ("nvidia/nemotron-3-ultra-550b-a55b", "Nemotron 3 Ultra"),
];

pub struct CommandCodeProvider {
    client: Client,
}

impl Default for CommandCodeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandCodeProvider {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        Self { client }
    }

    pub fn api_key_of(data: &Value) -> Option<String> {
        data.get("apiKey")
            .or_else(|| data.get("api_key"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.trim().is_empty())
    }

    /// Fetch the live model catalog. The Go plan blocks chat on
    /// `/provider/v1` but the models list is open, so the catalog stays
    /// current (Ox Alpha etc. appear as soon as upstream lists them).
    pub async fn fetch_models(&self, api_key: &str) -> Result<Vec<CcModelInfo>, ProviderError> {
        let resp = self
            .client
            .get(MODELS_URL)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("x-command-code-version", CC_VERSION)
            .header("x-cli-environment", "cli")
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_http_status(status, &text));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let mut out = Vec::new();
        if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
            for m in arr {
                let Some(id) = m.get("id").and_then(|s| s.as_str()) else {
                    continue;
                };
                let id = id.trim();
                if id.is_empty() {
                    continue;
                }
                out.push(CcModelInfo {
                    id: id.to_string(),
                    name: m
                        .get("name")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string()),
                    context_length: m.get("context_length").and_then(|v| v.as_i64()),
                });
            }
        }
        Ok(out)
    }
}

fn message_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for p in parts {
                if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                    out.push_str(t);
                }
            }
            out
        }
        _ => String::new(),
    }
}

/// Convert OpenAI messages â†’ Command Code `{messages, system}` envelope.
fn convert_messages(req: &ChatCompletionRequest) -> (Vec<Value>, Option<String>) {
    let mut messages: Vec<Value> = Vec::new();
    let mut system: Option<String> = None;
    for m in &req.messages {
        let role = m.role.as_str();
        if role == "system" || role == "developer" {
            let text = message_text(&m.content);
            if !text.is_empty() {
                system = Some(match system {
                    Some(s) => format!("{s}\n{text}"),
                    None => text,
                });
            }
            continue;
        }
        let role_out = match role {
            "assistant" => "assistant",
            "tool" => "tool",
            _ => "user",
        };
        messages.push(json!({
            "role": role_out,
            "content": [{ "type": "text", "text": message_text(&m.content) }]
        }));
    }
    (messages, system)
}

/// Convert OpenAI tools â†’ Command Code (Anthropic-shaped) tools.
fn convert_tools(req: &ChatCompletionRequest) -> Option<Vec<Value>> {
    let tools = req.tools.as_ref()?.as_array()?;
    let mut out = Vec::new();
    for t in tools {
        let f = t.get("function")?;
        out.push(json!({
            "name": f.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            "description": f.get("description").and_then(|v| v.as_str()),
            "input_schema": f.get("parameters").cloned().unwrap_or(json!({ "type": "object" })),
        }));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn build_body(req: &ChatCompletionRequest, upstream_model: &str, thread_id: &str) -> Value {
    let (messages, system) = convert_messages(req);
    let mut params = json!({
        "model": upstream_model,
        "messages": messages,
        "stream": true,
        "max_tokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "temperature": 0.3,
    });
    if let Some(system) = system {
        params["system"] = json!(system);
    }
    if let Some(tools) = convert_tools(req) {
        params["tools"] = json!(tools);
    }
    json!({
        "threadId": thread_id,
        "memory": "",
        "config": {
            "workingDir": "",
            "date": Utc::now().format("%Y-%m-%d").to_string(),
            "environment": "",
            "structure": [],
            "isGitRepo": false,
            "currentBranch": "",
            "mainBranch": "",
            "gitStatus": "",
            "recentCommits": [],
        },
        "params": params,
    })
}

fn estimate_tokens(text: &str) -> i64 {
    (text.len() as f64 / 4.0).ceil() as i64
}

fn usage_i64(u: &Value, key: &str) -> Option<i64> {
    u.get(key).and_then(|v| v.as_i64())
}

fn openai_role_chunk(resp_id: &str, model: &str) -> Value {
    json!({
        "id": resp_id, "object": "chat.completion.chunk", "created": Utc::now().timestamp(), "model": model,
        "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "" }, "finish_reason": null }]
    })
}

fn openai_content_chunk(resp_id: &str, model: &str, delta: &str) -> Value {
    json!({
        "id": resp_id, "object": "chat.completion.chunk", "created": Utc::now().timestamp(), "model": model,
        "choices": [{ "index": 0, "delta": { "content": delta }, "finish_reason": null }]
    })
}

fn openai_reasoning_chunk(resp_id: &str, model: &str, delta: &str) -> Value {
    json!({
        "id": resp_id, "object": "chat.completion.chunk", "created": Utc::now().timestamp(), "model": model,
        "choices": [{ "index": 0, "delta": { "reasoning_content": delta }, "finish_reason": null }]
    })
}

fn openai_tool_chunk(
    resp_id: &str,
    model: &str,
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
        "id": resp_id, "object": "chat.completion.chunk", "created": Utc::now().timestamp(), "model": model,
        "choices": [{ "index": 0, "delta": { "tool_calls": [tc] }, "finish_reason": null }]
    })
}

async fn send_sse(
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    value: &Value,
) -> Result<(), ()> {
    let msg = format!(
        "data: {}\n\n",
        serde_json::to_string(value).unwrap_or_else(|_| "{}".into())
    );
    tx.send(Ok(bytes::Bytes::from(msg))).await.map_err(|_| ())
}

/// Decoder state for the NDJSON stream.
#[derive(Default)]
struct DecoderState {
    tool_index: std::collections::HashMap<String, usize>,
    next_tool_index: usize,
    finish_reason: Option<String>,
    usage: Option<Value>,
}

fn map_finish(reason: &str) -> &'static str {
    match reason {
        "length" => "length",
        "tool_calls" | "tool-calls" => "tool_calls",
        "content_filter" => "content_filter",
        _ => "stop",
    }
}

/// Decode one NDJSON line into zero or more OpenAI SSE chunks.
fn decode_line(
    line: &str,
    state: &mut DecoderState,
    resp_id: &str,
    model: &str,
) -> Result<Vec<Value>, ProviderError> {
    let event: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Ok(Vec::new()),
    };
    let etype = event.get("type").and_then(|s| s.as_str()).unwrap_or("");
    let s = |k: &str| event.get(k).and_then(|v| v.as_str()).map(|s| s.to_string());

    let mut out = Vec::new();
    match etype {
        "text-delta" | "reasoning-delta" => {
            let text = s("text").or_else(|| s("delta")).unwrap_or_default();
            if !text.is_empty() {
                if etype == "reasoning-delta" {
                    out.push(openai_reasoning_chunk(resp_id, model, &text));
                } else {
                    out.push(openai_content_chunk(resp_id, model, &text));
                }
            }
        }
        "tool-input-start" => {
            let id = s("id").or_else(|| s("toolCallId")).unwrap_or_default();
            if !id.is_empty() && !state.tool_index.contains_key(&id) {
                state.tool_index.insert(id.clone(), state.next_tool_index);
                state.next_tool_index += 1;
                out.push(openai_tool_chunk(
                    resp_id,
                    model,
                    state.tool_index[&id],
                    Some(&id),
                    Some(&s("toolName").unwrap_or_default()),
                    None,
                ));
            }
        }
        "tool-input-delta" => {
            let id = s("id").or_else(|| s("toolCallId")).unwrap_or_default();
            let delta = s("delta").or_else(|| s("inputTextDelta")).unwrap_or_default();
            if let Some(&idx) = state.tool_index.get(&id) {
                if !delta.is_empty() {
                    out.push(openai_tool_chunk(resp_id, model, idx, None, None, Some(&delta)));
                }
            }
        }
        "tool-call" => {
            let id = s("toolCallId").unwrap_or_default();
            if !id.is_empty() && !state.tool_index.contains_key(&id) {
                state.tool_index.insert(id.clone(), state.next_tool_index);
                state.next_tool_index += 1;
                let input = match event.get("input") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => serde_json::to_string(v).unwrap_or_default(),
                    None => "{}".into(),
                };
                let idx = state.tool_index[&id];
                out.push(openai_tool_chunk(
                    resp_id,
                    model,
                    idx,
                    Some(&id),
                    Some(&s("toolName").unwrap_or_default()),
                    Some(&input),
                ));
            }
        }
        "finish-step" => {
            if let Some(r) = s("finishReason") {
                state.finish_reason = Some(r);
            }
            if let Some(u) = event.get("usage") {
                state.usage = Some(u.clone());
            }
        }
        "finish" => {
            if state.finish_reason.is_none() {
                state.finish_reason = Some(s("finishReason").unwrap_or_else(|| "stop".into()));
            }
            if let Some(u) = event.get("usage") {
                state.usage = Some(u.clone());
            }
        }
        "error" => {
            let msg = event
                .get("error")
                .or_else(|| event.get("message"))
                .map(|v| v.to_string())
                .unwrap_or_else(|| "unknown".into());
            return Err(ProviderError::Upstream {
                status: 502,
                body: format!("commandcode stream error: {msg}"),
            });
        }
        _ => {}
    }
    Ok(out)
}

#[async_trait]
impl Provider for CommandCodeProvider {
    fn id(&self) -> &'static str {
        COMMANDCODE_PROVIDER
    }

    async fn ensure_fresh_auth(&self, account: &mut crate::db::Account) -> Result<(), ProviderError> {
        let data = account.data_json();
        if Self::api_key_of(&data).is_none() {
            return Err(ProviderError::AuthInvalid("missing apiKey".into()));
        }
        Ok(())
    }

    async fn chat(
        &self,
        _client: &Client,
        account: &crate::db::Account,
        req: &ChatCompletionRequest,
    ) -> Result<ChatOutcome, ProviderError> {
        let data = account.data_json();
        let api_key = Self::api_key_of(&data)
            .ok_or_else(|| ProviderError::AuthInvalid("missing apiKey".into()))?;

        // Strip the leading `cmc/` prefix to get the upstream model id.
        let upstream_model = req
            .upstream_model()
            .trim_start_matches("cmc/")
            .to_string();

        let thread_id = Uuid::new_v4().to_string();
        let body = build_body(req, &upstream_model, &thread_id);

        let resp = self
            .client
            .post(GENERATE_URL)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("x-command-code-version", CC_VERSION)
            .header("x-cli-environment", "cli")
            .header("x-session-id", &thread_id)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_http_status(status, &text));
        }

        let req_model = req.model.clone();

        if req.stream_enabled() {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);
            let (usage_tx, usage_rx) = oneshot::channel::<Option<StreamUsage>>();
            let mut upstream_stream = resp.bytes_stream();

            tokio::spawn(async move {
                let mut buffer = String::new();
                let resp_id = format!("chatcmpl-{}", Uuid::new_v4().simple());
                let mut first_chunk_sent = false;
                let mut state = DecoderState::default();
                let mut accumulated_text = String::new();
                let mut any_tool_calls = false;
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
                        let trimmed = line.trim().to_string();
                        if trimmed.is_empty() {
                            continue;
                        }

                        let events = match decode_line(&trimmed, &mut state, &resp_id, &req_model) {
                            Ok(ev) => ev,
                            Err(e) => {
                                let _ = tx
                                    .send(Err(std::io::Error::new(
                                        std::io::ErrorKind::Other,
                                        e.to_string(),
                                    )))
                                    .await;
                                if let Some(txu) = usage_tx.take() {
                                    let _ = txu.send(None);
                                }
                                return;
                            }
                        };

                        for ev in events {
                            // Track text + tool calls for usage estimation.
                            if let Some(delta) =
                                ev.pointer("/choices/0/delta/content").and_then(|v| v.as_str())
                            {
                                accumulated_text.push_str(delta);
                            }
                            if ev.pointer("/choices/0/delta/tool_calls").is_some() {
                                any_tool_calls = true;
                            }
                            if !first_chunk_sent {
                                first_chunk_sent = true;
                                if send_sse(&tx, &openai_role_chunk(&resp_id, &req_model))
                                    .await
                                    .is_err()
                                {
                                    if let Some(txu) = usage_tx.take() {
                                        let _ = txu.send(None);
                                    }
                                    return;
                                }
                            }
                            if send_sse(&tx, &ev).await.is_err() {
                                if let Some(txu) = usage_tx.take() {
                                    let _ = txu.send(None);
                                }
                                return;
                            }
                        }
                    }
                }

                // Stream ended: emit final chunk + usage.
                let mut prompt_tokens = 0i64;
                let mut completion_tokens = 0i64;
                let mut total_tokens = 0i64;
                if let Some(u) = state.usage.as_ref() {
                    prompt_tokens = usage_i64(u, "promptTokens")
                        .or_else(|| usage_i64(u, "input_tokens"))
                        .unwrap_or(0);
                    completion_tokens = usage_i64(u, "completionTokens")
                        .or_else(|| usage_i64(u, "output_tokens"))
                        .unwrap_or(0);
                    total_tokens = prompt_tokens + completion_tokens;
                }
                if total_tokens == 0 && completion_tokens == 0 && !accumulated_text.is_empty() {
                    completion_tokens = estimate_tokens(&accumulated_text);
                    total_tokens = prompt_tokens + completion_tokens;
                }
                let usage = StreamUsage {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                }
                .normalized();

                let finish_reason = if any_tool_calls {
                    "tool_calls"
                } else {
                    map_finish(state.finish_reason.as_deref().unwrap_or("stop"))
                };
                let finish = json!({
                    "id": resp_id, "object": "chat.completion.chunk", "created": Utc::now().timestamp(), "model": req_model,
                    "choices": [{ "index": 0, "delta": {}, "finish_reason": finish_reason }]
                });
                let _ = send_sse(&tx, &finish).await;
                let _ = tx.send(Ok(bytes::Bytes::from("data: [DONE]\n\n"))).await;
                if let Some(txu) = usage_tx.take() {
                    let _ = txu.send(if usage.is_empty() { None } else { Some(usage) });
                }
            });

            let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
            let response = Response::builder()
                .status(200)
                .header("Content-Type", "text/event-stream")
                .header("Cache-Control", "no-cache")
                .body(body)
                .map_err(|e| ProviderError::Transport(e.to_string()))?;

            Ok(ChatOutcome::Stream {
                response,
                usage_rx,
            })
        } else {
            // Non-stream: accumulate text from the NDJSON stream.
            let mut upstream_stream = resp.bytes_stream();
            let mut buffer = String::new();
            let mut state = DecoderState::default();
            let mut text = String::new();
            while let Some(chunk_res) = upstream_stream.next().await {
                let chunk = chunk_res.map_err(|e| ProviderError::Transport(e.to_string()))?;
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(pos) = buffer.find('\n') {
                    let line = buffer[..pos].trim_end_matches('\r').to_string();
                    buffer = buffer[pos + 1..].to_string();
                    let trimmed = line.trim().to_string();
                    if trimmed.is_empty() {
                        continue;
                    }
                    for ev in decode_line(&trimmed, &mut state, "resp", &req_model)? {
                        if let Some(d) = ev.pointer("/choices/0/delta/content").and_then(|v| v.as_str())
                        {
                            text.push_str(d);
                        }
                    }
                }
            }
            let resp_id = format!("chatcmpl-{}", Uuid::new_v4().simple());
            Ok(ChatOutcome::Json(json!({
                "id": resp_id,
                "object": "chat.completion",
                "created": Utc::now().timestamp(),
                "model": req_model,
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": text },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 0,
                    "completion_tokens": estimate_tokens(&text),
                    "total_tokens": estimate_tokens(&text)
                }
            })))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_context_length_units() {
        assert_eq!(fmt_context_length(1_000_000), "1M");
        assert_eq!(fmt_context_length(1_048_576), "1048K");
        assert_eq!(fmt_context_length(256_000), "256K");
        assert_eq!(fmt_context_length(262_144), "262K");
        assert_eq!(fmt_context_length(900), "900");
    }

    #[test]
    fn decode_text_delta() {
        let mut state = DecoderState::default();
        let evs = decode_line(
            r#"{"type":"text-delta","text":"hello"}"#,
            &mut state,
            "id",
            "cmc/x",
        )
        .unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0]["choices"][0]["delta"]["content"], "hello");
    }

    #[test]
    fn decode_reasoning_delta() {
        let mut state = DecoderState::default();
        let evs = decode_line(
            r#"{"type":"reasoning-delta","text":"think"}"#,
            &mut state,
            "id",
            "cmc/x",
        )
        .unwrap();
        assert_eq!(evs[0]["choices"][0]["delta"]["reasoning_content"], "think");
    }

    #[test]
    fn decode_tool_flow() {
        let mut state = DecoderState::default();
        let start = decode_line(
            r#"{"type":"tool-input-start","id":"t1","toolName":"read"}"#,
            &mut state,
            "id",
            "cmc/x",
        )
        .unwrap();
        assert_eq!(start.len(), 1);
        assert_eq!(start[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["name"], "read");
        let delta = decode_line(
            r#"{"type":"tool-input-delta","id":"t1","delta":"{\"a\":1}"}"#,
            &mut state,
            "id",
            "cmc/x",
        )
        .unwrap();
        assert_eq!(delta[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"], "{\"a\":1}");
    }

    #[test]
    fn decode_finish_captures_usage() {
        let mut state = DecoderState::default();
        let _ = decode_line(
            r#"{"type":"finish","finishReason":"stop","usage":{"promptTokens":10,"completionTokens":5}}"#,
            &mut state,
            "id",
            "cmc/x",
        )
        .unwrap();
        assert_eq!(state.finish_reason.as_deref(), Some("stop"));
        assert!(state.usage.is_some());
    }

    #[test]
    fn build_body_shape() {
        let req: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "cmc/xiaomi/mimo-v2.5",
            "messages": [
                { "role": "system", "content": "sys" },
                { "role": "user", "content": "hi" }
            ]
        }))
        .unwrap();
        let body = build_body(&req, "xiaomi/mimo-v2.5", "tid");
        assert_eq!(body["threadId"], "tid");
        assert_eq!(body["params"]["model"], "xiaomi/mimo-v2.5");
        assert_eq!(body["params"]["system"], "sys");
        assert_eq!(body["params"]["messages"][0]["role"], "user");
    }
}

