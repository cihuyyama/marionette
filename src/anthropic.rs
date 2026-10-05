//! Anthropic Messages API (`POST /v1/messages`) surface.
//!
//! Marionette's pool and providers speak OpenAI Chat Completions internally,
//! so this module is a translation layer and nothing more: it folds the
//! Anthropic request shape into a [`ChatCompletionRequest`], lets the pool do
//! its normal routing/failover/accounting, then re-shapes the result back into
//! the contract an Anthropic client expects.
//!
//! Deliberately narrow: it covers what Claude Code and similar clients
//! actually send (text, images, tool use, tool results, streaming). Anything
//! outside that is rejected with a clear error rather than silently mangled.

use axum::body::Body;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use http_body_util::BodyExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::error::{AppError, AppResult};
use crate::openai::{ChatCompletionRequest, ChatMessage};

// ── Request ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    pub messages: Vec<AnthropicMessage>,
    #[serde(default)]
    pub system: Option<SystemPrompt>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub tools: Option<Vec<AnthropicTool>>,
    #[serde(default)]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: Value,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SystemPrompt {
    Text(String),
    Blocks(Vec<TextBlock>),
}

#[derive(Debug, Deserialize)]
pub struct TextBlock {
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct AnthropicMessage {
    pub role: String,
    #[serde(default)]
    pub content: MessageContent,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl Default for MessageContent {
    fn default() -> Self {
        Self::Blocks(Vec::new())
    }
}

impl MessageContent {
    fn blocks(&self) -> Vec<ContentBlock> {
        match self {
            Self::Text(t) => vec![ContentBlock::Text { text: t.clone() }],
            Self::Blocks(b) => b.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: ImageSource },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: Option<ToolResultContent>,
    },
    // Anything else (thinking, redacted_thinking, ...) is passed over: folding
    // it into the OpenAI shape would invent semantics we cannot verify.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageSource {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub media_type: Option<String>,
    #[serde(default)]
    pub data: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl ToolResultContent {
    fn to_text(&self) -> String {
        match self {
            Self::Text(t) => t.clone(),
            Self::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub input_schema: Option<Value>,
}

// ── Request translation ────────────────────────────────────────────────

impl MessagesRequest {
    pub fn stream_enabled(&self) -> bool {
        self.stream.unwrap_or(false)
    }

    pub fn to_chat_request(&self) -> AppResult<ChatCompletionRequest> {
        if self.messages.is_empty() {
            return Err(AppError::BadRequest("messages must not be empty".into()));
        }

        let mut messages: Vec<ChatMessage> = Vec::new();

        if let Some(system) = &self.system {
            let text = match system {
                SystemPrompt::Text(t) => t.clone(),
                SystemPrompt::Blocks(blocks) => blocks
                    .iter()
                    .map(|b| b.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            if !text.trim().is_empty() {
                messages.push(ChatMessage {
                    role: "system".into(),
                    content: Value::String(text),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
        }

        for msg in &self.messages {
            match msg.role.as_str() {
                "user" => push_user(&mut messages, &msg.content.blocks())?,
                "assistant" => push_assistant(&mut messages, &msg.content.blocks())?,
                other => {
                    return Err(AppError::BadRequest(format!("unsupported role `{other}`")));
                }
            }
        }

        let tools = self.tools.as_ref().map(|tools| {
            tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description.clone().unwrap_or_default(),
                            "parameters": t.input_schema.clone().unwrap_or_else(|| json!({"type":"object"})),
                        }
                    })
                })
                .collect::<Vec<_>>()
        });

        Ok(ChatCompletionRequest {
            model: self.model.clone(),
            messages,
            stream: Some(self.stream_enabled()),
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            top_p: self.top_p,
            tools: tools.map(Value::Array),
            tool_choice: None,
            parallel_tool_calls: None,
            extra: Value::Null,
        })
    }
}

fn push_user(out: &mut Vec<ChatMessage>, blocks: &[ContentBlock]) -> AppResult<()> {
    let mut parts: Vec<Value> = Vec::new();
    let mut tool_results: Vec<ChatMessage> = Vec::new();
    let mut text = String::new();

    for block in blocks {
        match block {
            ContentBlock::Text { text: t } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t);
            }
            ContentBlock::Image { source } => {
                let data = source.data.as_deref().unwrap_or_default();
                if data.is_empty() {
                    continue;
                }
                let media = source.media_type.as_deref().unwrap_or("image/png");
                parts.push(json!({
                    "type": "image_url",
                    "image_url": { "url": format!("data:{media};base64,{data}") }
                }));
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
            } => {
                // A tool result is its own OpenAI message; it cannot share a
                // turn with text.
                tool_results.push(ChatMessage {
                    role: "tool".into(),
                    content: Value::String(content.as_ref().map(|c| c.to_text()).unwrap_or_default()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: Some(tool_use_id.clone()),
                });
            }
            ContentBlock::ToolUse { .. } | ContentBlock::Other => {}
        }
    }

    if !parts.is_empty() {
        // Multimodal: text rides along as a content part.
        if !text.is_empty() {
            parts.insert(0, json!({ "type": "text", "text": text }));
        }
        out.push(ChatMessage {
            role: "user".into(),
            content: Value::Array(parts),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        });
    } else if !text.is_empty() {
        out.push(ChatMessage {
            role: "user".into(),
            content: Value::String(text),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        });
    }
    out.extend(tool_results);
    Ok(())
}

fn push_assistant(out: &mut Vec<ChatMessage>, blocks: &[ContentBlock]) -> AppResult<()> {
    let mut text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    for block in blocks {
        match block {
            ContentBlock::Text { text: t } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t);
            }
            ContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".into())
                    }
                }));
            }
            _ => {}
        }
    }

    out.push(ChatMessage {
        role: "assistant".into(),
        content: Value::String(text),
        name: None,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(Value::Array(tool_calls))
        },
        tool_call_id: None,
    });
    Ok(())
}

// ── Response translation ───────────────────────────────────────────────

fn map_stop_reason(finish: Option<&str>) -> &'static str {
    match finish {
        Some("length") => "max_tokens",
        Some("tool_calls") => "tool_use",
        Some("stop_sequence") => "stop_sequence",
        _ => "end_turn",
    }
}

/// Translate a non-streaming Chat Completions response into a Messages one.
pub fn from_chat_json(chat: &Value, model: &str) -> Value {
    let id = chat
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("msg_unknown")
        .to_string();
    let choice = chat.get("choices").and_then(|c| c.get(0));

    let mut content: Vec<Value> = Vec::new();
    if let Some(msg) = choice.and_then(|c| c.get("message")) {
        let text = msg
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if !text.is_empty() {
            content.push(json!({ "type": "text", "text": text }));
        }
        if let Some(calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
            for call in calls {
                let name = call
                    .pointer("/function/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let args_raw = call
                    .pointer("/function/arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                let input = serde_json::from_str::<Value>(args_raw)
                    .unwrap_or_else(|_| json!({}));
                content.push(json!({
                    "type": "tool_use",
                    "id": call.get("id").and_then(|v| v.as_str()).unwrap_or_default(),
                    "name": name,
                    "input": input,
                }));
            }
        }
    }
    if content.is_empty() {
        content.push(json!({ "type": "text", "text": "" }));
    }

    let finish = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(|v| v.as_str());
    let usage = chat.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": map_stop_reason(finish),
        "stop_sequence": null,
        "usage": { "input_tokens": input_tokens, "output_tokens": output_tokens }
    })
}

// ── Streaming translation ──────────────────────────────────────────────

/// Re-shape an OpenAI Chat Completions SSE body into Anthropic Messages SSE.
///
/// Reads the provider's `data: {...}` frames, tracks which content blocks are
/// open, and emits the event sequence an Anthropic client expects:
/// `message_start` → `content_block_start` → `content_block_delta`* →
/// `content_block_stop` → `message_delta` → `message_stop`.
pub fn translate_stream(model: String, upstream: Response) -> Response {
    let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(64);

    tokio::spawn(async move {
        let mut body = upstream.into_body();
        let mut buf = String::new();
        let mut text_started = false;
        let mut text_index: usize = 0;
        // OpenAI tool-call index -> Anthropic content-block index.
        let mut tool_blocks: std::collections::HashMap<usize, usize> =
            std::collections::HashMap::new();
        let mut output_tokens: i64 = 0;
        let mut stop_reason: &str = "end_turn";

        let mut start = json!({
            "type": "message_start",
            "message": {
                "id": msg_id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": { "input_tokens": 0, "output_tokens": 0 }
            }
        });
        if let Some(m) = start.get_mut("message") {
            m["model"] = json!(model.clone());
        }
        let _ = send(&tx, "message_start", &start).await;

        while let Some(frame) = body.frame().await {
            let data = match frame {
                Ok(f) => f.into_data().unwrap_or_default(),
                Err(_) => break,
            };
            buf.push_str(&String::from_utf8_lossy(&data));

            // Frames are separated by a blank line; keep any partial tail.
            while let Some(pos) = buf.find("\n\n") {
                let raw: String = buf.drain(..=pos + 1).collect();
                let line = raw.trim();
                let Some(payload) = line.strip_prefix("data:") else {
                    continue;
                };
                let payload = payload.trim();
                if payload == "[DONE]" {
                    continue;
                }
                let Ok(chunk) = serde_json::from_str::<Value>(payload) else {
                    continue;
                };

                let choice = chunk.get("choices").and_then(|c| c.get(0));
                let delta = choice.and_then(|c| c.get("delta"));

                if let Some(text) = delta
                    .and_then(|d| d.get("content"))
                    .and_then(|v| v.as_str())
                    .filter(|t| !t.is_empty())
                {
                    if !text_started {
                        text_index = 0;
                        let _ = send(
                            &tx,
                            "content_block_start",
                            &json!({
                                "type": "content_block_start",
                                "index": text_index,
                                "content_block": { "type": "text", "text": "" }
                            }),
                        )
                        .await;
                        text_started = true;
                    }
                    let _ = send(
                        &tx,
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": text_index,
                            "delta": { "type": "text_delta", "text": text }
                        }),
                    )
                    .await;
                }

                if let Some(calls) = delta
                    .and_then(|d| d.get("tool_calls"))
                    .and_then(|v| v.as_array())
                {
                    for call in calls {
                        let idx = call.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        let anthropic_index = match tool_blocks.get(&idx) {
                            Some(i) => *i,
                            None => {
                                if text_started {
                                    let _ = send(
                                        &tx,
                                        "content_block_stop",
                                        &json!({ "type": "content_block_stop", "index": text_index }),
                                    )
                                    .await;
                                    text_started = false;
                                }
                                let next = text_index + 1 + tool_blocks.len();
                                tool_blocks.insert(idx, next);
                                let name = call
                                    .pointer("/function/name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default();
                                let _ = send(
                                    &tx,
                                    "content_block_start",
                                    &json!({
                                        "type": "content_block_start",
                                        "index": next,
                                        "content_block": {
                                            "type": "tool_use",
                                            "id": call.get("id").and_then(|v| v.as_str()).unwrap_or_default(),
                                            "name": name,
                                            "input": {}
                                        }
                                    }),
                                )
                                .await;
                                next
                            }
                        };
                        if let Some(args) = call
                            .pointer("/function/arguments")
                            .and_then(|v| v.as_str())
                            .filter(|a| !a.is_empty())
                        {
                            let _ = send(
                                &tx,
                                "content_block_delta",
                                &json!({
                                    "type": "content_block_delta",
                                    "index": anthropic_index,
                                    "delta": { "type": "input_json_delta", "partial_json": args }
                                }),
                            )
                            .await;
                        }
                    }
                }

                if let Some(finish) = choice
                    .and_then(|c| c.get("finish_reason"))
                    .and_then(|v| v.as_str())
                {
                    stop_reason = map_stop_reason(Some(finish));
                }

                if let Some(u) = chunk.get("usage") {
                    if let Some(n) = u.get("completion_tokens").and_then(|v| v.as_i64()) {
                        output_tokens = n;
                    }
                }
            }
        }

        // Close whatever is still open, then finish the message.
        if text_started {
            let _ = send(
                &tx,
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": text_index }),
            )
            .await;
        }
        for idx in tool_blocks.values() {
            let _ = send(
                &tx,
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": idx }),
            )
            .await;
        }
        let _ = send(
            &tx,
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                "usage": { "output_tokens": output_tokens }
            }),
        )
        .await;
        let _ = send(&tx, "message_stop", &json!({ "type": "message_stop" })).await;
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| AppError::Internal("stream build".into()).into_response())
}

/// Buffer a provider stream and emit one complete Anthropic event sequence.
///
/// Used when a client asks for `stream: true` but the provider returned a
/// buffered response: emitting raw JSON would break the client's parser.
pub fn translate_stream_from_json(chat: &Value, model: &str) -> Response {
    let model = model.to_string();
    let msg = from_chat_json(chat, &model);
    let blocks = msg.get("content").and_then(|c| c.as_array()).cloned().unwrap_or_default();
    let stop_reason = msg.get("stop_reason").cloned().unwrap_or(json!("end_turn"));

    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(32);
    tokio::spawn(async move {
        let model = model.clone();
        let id = msg.get("id").cloned().unwrap_or_else(|| json!("msg_unknown"));
        let _ = send(
            &tx,
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": id,
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 }
                }
            }),
        )
        .await;

        for (index, block) in blocks.iter().enumerate() {
            let kind = block.get("type").and_then(|t| t.as_str()).unwrap_or("text");
            let mut start_block = json!({ "type": kind });
            if kind == "text" {
                start_block["text"] = json!("");
            } else if kind == "tool_use" {
                start_block["id"] = block.get("id").cloned().unwrap_or(json!(""));
                start_block["name"] = block.get("name").cloned().unwrap_or(json!(""));
                start_block["input"] = json!({});
            }
            let _ = send(
                &tx,
                "content_block_start",
                &json!({ "type": "content_block_start", "index": index, "content_block": start_block }),
            )
            .await;

            if kind == "text" {
                let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if !text.is_empty() {
                    let _ = send(
                        &tx,
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": { "type": "text_delta", "text": text }
                        }),
                    )
                    .await;
                }
            } else if kind == "tool_use" {
                let input = serde_json::to_string(block.get("input").unwrap_or(&json!({})))
                    .unwrap_or_else(|_| "{}".into());
                let _ = send(
                    &tx,
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": { "type": "input_json_delta", "partial_json": input }
                    }),
                )
                .await;
            }

            let _ = send(
                &tx,
                "content_block_stop",
                &json!({ "type": "content_block_stop", "index": index }),
            )
            .await;
        }

        let _ = send(
            &tx,
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                "usage": { "output_tokens": 0 }
            }),
        )
        .await;
        let _ = send(&tx, "message_stop", &json!({ "type": "message_stop" })).await;
    });

    sse_response(rx)
}

/// Buffer a provider stream into one Anthropic JSON message.
///
/// The mirror of [`translate_stream_from_json`]: a client that did not ask for
/// streaming still gets a plain body even though the provider streamed.
pub async fn collect_stream(upstream: Response, model: &str) -> Response {
    use http_body_util::BodyExt;

    let mut body = upstream.into_body();
    let mut text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut finish: Option<String> = None;
    let mut input_tokens = 0i64;
    let mut output_tokens = 0i64;

    while let Some(frame) = body.frame().await {
        let data = match frame {
            Ok(f) => f.into_data().unwrap_or_default(),
            Err(_) => break,
        };
        let raw = String::from_utf8_lossy(&data);
        for line in raw.split("

") {
            let line = line.trim();
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload == "[DONE]" {
                continue;
            }
            let Ok(chunk) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            let choice = chunk.get("choices").and_then(|c| c.get(0));
            let delta = choice.and_then(|c| c.get("delta"));
            if let Some(t) = delta.and_then(|d| d.get("content")).and_then(|v| v.as_str()) {
                text.push_str(t);
            }
            if let Some(calls) = delta
                .and_then(|d| d.get("tool_calls"))
                .and_then(|v| v.as_array())
            {
                for call in calls {
                    let idx = call.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let args = call
                        .pointer("/function/arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let id = call.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let name = call
                        .pointer("/function/name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    while tool_calls.len() <= idx {
                        tool_calls.push(json!({"id":"","type":"function","function":{"name":"","arguments":""}}));
                    }
                    if !id.is_empty() {
                        tool_calls[idx]["id"] = json!(id);
                    }
                    if !name.is_empty() {
                        tool_calls[idx]["function"]["name"] = json!(name);
                    }
                    let existing = tool_calls[idx]
                        .pointer("/function/arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    tool_calls[idx]["function"]["arguments"] =
                        json!(format!("{existing}{args}"));
                }
            }
            if let Some(f) = choice
                .and_then(|c| c.get("finish_reason"))
                .and_then(|v| v.as_str())
            {
                finish = Some(f.to_string());
            }
            if let Some(u) = chunk.get("usage") {
                if let Some(n) = u.get("prompt_tokens").and_then(|v| v.as_i64()) {
                    input_tokens = n;
                }
                if let Some(n) = u.get("completion_tokens").and_then(|v| v.as_i64()) {
                    output_tokens = n;
                }
            }
        }
    }

    let mut message = json!({ "role": "assistant", "content": text });
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }
    let chat = json!({
        "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
        "choices": [{ "finish_reason": finish, "message": message }],
        "usage": { "prompt_tokens": input_tokens, "completion_tokens": output_tokens }
    });
    axum::Json(from_chat_json(&chat, model)).into_response()
}

fn sse_response(rx: mpsc::Receiver<Result<Bytes, std::io::Error>>) -> Response {
    use axum::body::Body;
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| AppError::Internal("stream build".into()).into_response())
}

async fn send(
    tx: &mpsc::Sender<Result<Bytes, std::io::Error>>,
    event: &str,
    data: &Value,
) -> Result<(), ()> {
    let payload = serde_json::to_string(data).unwrap_or_else(|_| "{}".into());
    let frame = format!("event: {event}\ndata: {payload}\n\n");
    tx.send(Ok(Bytes::from(frame))).await.map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(json: Value) -> MessagesRequest {
        serde_json::from_value(json).expect("valid request")
    }

    #[test]
    fn system_prompt_becomes_system_message() {
        let r = req(json!({
            "model": "qd/ultimate",
            "max_tokens": 100,
            "system": "be brief",
            "messages": [{"role":"user","content":"hi"}]
        }));
        let c = r.to_chat_request().unwrap();
        assert_eq!(c.messages[0].role, "system");
        assert_eq!(c.messages[0].content, json!("be brief"));
        assert_eq!(c.messages[1].role, "user");
    }

    #[test]
    fn system_blocks_are_joined() {
        let r = req(json!({
            "model": "qd/ultimate",
            "system": [{"type":"text","text":"a"},{"type":"text","text":"b"}],
            "messages": [{"role":"user","content":"hi"}]
        }));
        let c = r.to_chat_request().unwrap();
        assert_eq!(c.messages[0].content, json!("a\nb"));
    }

    #[test]
    fn tool_definitions_map_to_openai_functions() {
        let r = req(json!({
            "model": "qd/ultimate",
            "messages": [{"role":"user","content":"hi"}],
            "tools": [{"name":"Read","description":"read a file","input_schema":{"type":"object"}}]
        }));
        let c = r.to_chat_request().unwrap();
        let tools = c.tools.as_ref().unwrap().as_array().unwrap();
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "Read");
    }

    #[test]
    fn tool_result_becomes_tool_message() {
        let r = req(json!({
            "model": "qd/ultimate",
            "messages": [
                {"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"file data"}]}
            ]
        }));
        let c = r.to_chat_request().unwrap();
        assert_eq!(c.messages[0].role, "assistant");
        assert!(c.messages[0].tool_calls.is_some());
        assert_eq!(c.messages[1].role, "tool");
        assert_eq!(c.messages[1].tool_call_id.as_deref(), Some("t1"));
        assert_eq!(c.messages[1].content, json!("file data"));
    }

    #[test]
    fn image_block_becomes_data_uri() {
        let r = req(json!({
            "model": "qd/ultimate",
            "messages": [{"role":"user","content":[
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAA="}}
            ]}]
        }));
        let c = r.to_chat_request().unwrap();
        let parts = c.messages[0].content.as_array().unwrap();
        let url = parts[0]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,AAA="), "got {url}");
    }

    #[test]
    fn empty_messages_is_rejected() {
        let r = req(json!({"model":"qd/ultimate","messages":[]}));
        assert!(r.to_chat_request().is_err());
    }

    #[test]
    fn non_stream_response_shape() {
        let chat = json!({
            "id": "chatcmpl-1",
            "choices": [{"finish_reason":"stop","message":{"content":"hello"}}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 7}
        });
        let out = from_chat_json(&chat, "qd/ultimate");
        assert_eq!(out["type"], "message");
        assert_eq!(out["role"], "assistant");
        assert_eq!(out["content"][0]["type"], "text");
        assert_eq!(out["content"][0]["text"], "hello");
        assert_eq!(out["stop_reason"], "end_turn");
        assert_eq!(out["usage"]["input_tokens"], 5);
        assert_eq!(out["usage"]["output_tokens"], 7);
    }

    #[test]
    fn tool_use_response_and_stop_reason() {
        let chat = json!({
            "id": "chatcmpl-2",
            "choices": [{"finish_reason":"tool_calls","message":{"content":"",
                "tool_calls":[{"id":"t9","type":"function",
                    "function":{"name":"Read","arguments":"{\"path\":\"a\"}"}}]}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 2}
        });
        let out = from_chat_json(&chat, "qd/ultimate");
        assert_eq!(out["stop_reason"], "tool_use");
        assert_eq!(out["content"][0]["type"], "tool_use");
        assert_eq!(out["content"][0]["name"], "Read");
        assert_eq!(out["content"][0]["input"]["path"], "a");
    }

    #[test]
    fn max_tokens_maps_to_stop_reason() {
        let chat = json!({
            "id": "c",
            "choices": [{"finish_reason":"length","message":{"content":"x"}}]
        });
        assert_eq!(from_chat_json(&chat, "m")["stop_reason"], "max_tokens");
    }

    #[test]
    fn empty_content_still_emits_a_text_block() {
        // Anthropic clients reject a message with no content blocks at all.
        let chat = json!({"id":"c","choices":[{"finish_reason":"stop","message":{}}]});
        let out = from_chat_json(&chat, "m");
        assert_eq!(out["content"][0]["type"], "text");
    }
}
