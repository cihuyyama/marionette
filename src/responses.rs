//! OpenAI Responses API (`POST /v1/responses`) surface.
//!
//! Marionette's pool and providers speak Chat Completions internally, so like
//! the Anthropic surface this is translation only: fold a Responses request
//! into a [`ChatCompletionRequest`], run it through the ordinary pool, then
//! re-shape the result.
//!
//! Note the asymmetry this creates: providers such as grok-cli talk Responses
//! *upstream*, convert to Chat Completions for the pool, and this module
//! converts back to Responses for the client. That round-trip is deliberate —
//! one canonical shape inside the pool is worth more than avoiding a
//! conversion at the edge.

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
pub struct ResponsesRequest {
    pub model: String,
    /// Either a plain string or a list of `{role, content}` items.
    #[serde(default)]
    pub input: Option<ResponsesInput>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub previous_response_id: Option<String>,
    #[serde(flatten)]
    pub extra: Value,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ResponsesInput {
    Text(String),
    Items(Vec<ResponsesItem>),
}

#[derive(Debug, Deserialize)]
pub struct ResponsesItem {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<Value>,
    // tool_call / tool_result / function_call variants are carried through as
    // raw JSON rather than modelled, since only text drives the translation.
    #[serde(flatten)]
    pub rest: Value,
}

impl ResponsesRequest {
    pub fn stream_enabled(&self) -> bool {
        self.stream.unwrap_or(false)
    }

    pub fn to_chat_request(&self) -> AppResult<ChatCompletionRequest> {
        let mut messages: Vec<ChatMessage> = Vec::new();

        if let Some(instructions) = &self.instructions {
            if !instructions.trim().is_empty() {
                messages.push(text_message("system", instructions));
            }
        }

        match &self.input {
            Some(ResponsesInput::Text(t)) => {
                if !t.trim().is_empty() {
                    messages.push(text_message("user", t));
                }
            }
            Some(ResponsesInput::Items(items)) => {
                for item in items {
                    // An absent role means a plain input item; default it to
                    // user. A role that is present but unrecognized is
                    // rejected rather than coerced, matching /v1/messages --
                    // silently turning a typo into a user turn changes request
                    // semantics without telling the caller.
                    let role = item.role.as_deref().unwrap_or("user");
                    let text = extract_text(item.content.as_ref());
                    match role {
                        "system" | "developer" => {
                            if !text.trim().is_empty() {
                                messages.push(text_message("system", &text));
                            }
                        }
                        "user" => messages.push(text_message("user", &text)),
                        "assistant" => messages.push(text_message("assistant", &text)),
                        other => {
                            return Err(AppError::BadRequest(format!(
                                "unsupported role `{other}`"
                            )));
                        }
                    }
                }
            }
            None => {}
        }

        if messages.is_empty() {
            return Err(AppError::BadRequest(
                "input must not be empty".into(),
            ));
        }

        // Responses tools are already function-shaped; pass them through so
        // providers that support tools keep working unchanged.
        Ok(ChatCompletionRequest {
            model: self.model.clone(),
            messages,
            stream: Some(self.stream_enabled()),
            temperature: self.temperature,
            max_tokens: self.max_output_tokens,
            top_p: self.top_p,
            tools: self.tools.clone().map(Value::Array),
            tool_choice: None,
            parallel_tool_calls: None,
            extra: Value::Null,
        })
    }
}

fn text_message(role: &str, text: &str) -> ChatMessage {
    ChatMessage {
        role: role.into(),
        content: Value::String(text.into()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
    }
}

/// Pull display text out of a Responses content value, which may be a bare
/// string or a list of `{type: "input_text"|..., text}` parts.
fn extract_text(content: Option<&Value>) -> String {
    match content {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| match p {
                Value::String(s) => Some(s.clone()),
                other => other.get("text").and_then(|t| t.as_str()).map(str::to_string),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string(),
    }
}

// ── Response translation ───────────────────────────────────────────────

fn map_status(finish: Option<&str>) -> &'static str {
    match finish {
        Some("length") => "incomplete",
        Some("content_filter") => "incomplete",
        Some("tool_calls") => "completed",
        _ => "completed",
    }
}

/// Translate a non-streaming Chat Completions response into a Responses one.
pub fn from_chat_json(chat: &Value, model: &str) -> Value {
    let id = chat
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("resp_{}", uuid::Uuid::new_v4().simple()));
    let choice = chat.get("choices").and_then(|c| c.get(0));
    let message = choice.and_then(|c| c.get("message"));

    let text = message
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

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
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "status": map_status(choice.and_then(|c| c.get("finish_reason")).and_then(|v| v.as_str())),
        "model": model,
        "output": [{
            "type": "message",
            "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
            "role": "assistant",
            "status": "completed",
            "content": [{ "type": "output_text", "text": text, "annotations": [] }]
        }],
        "output_text": text,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    })
}

// ── Streaming translation ──────────────────────────────────────────────

/// Re-shape Chat Completions SSE into the Responses event sequence.
pub fn translate_stream(model: String, upstream: Response) -> Response {
    let resp_id = format!("resp_{}", uuid::Uuid::new_v4().simple());
    let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(64);

    tokio::spawn(async move {
        let mut body = upstream.into_body();
        let mut buf = String::new();
        let mut started = false;
        let mut output_tokens: i64 = 0;
        let mut accumulated = String::new();

        let _ = send(
            &tx,
            "response.created",
            &json!({
                "type": "response.created",
                "response": {
                    "id": resp_id, "object": "response", "status": "in_progress",
                    "model": model, "output": []
                }
            }),
        )
        .await;

        while let Some(frame) = body.frame().await {
            let data = match frame {
                Ok(f) => f.into_data().unwrap_or_default(),
                Err(_) => break,
            };
            buf.push_str(&String::from_utf8_lossy(&data));

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
                let delta = chunk.get("choices").and_then(|c| c.get(0)).and_then(|c| c.get("delta"));

                if let Some(text) = delta
                    .and_then(|d| d.get("content"))
                    .and_then(|v| v.as_str())
                    .filter(|t| !t.is_empty())
                {
                    if !started {
                        let _ = send(
                            &tx,
                            "response.output_item.added",
                            &json!({
                                "type": "response.output_item.added",
                                "output_index": 0,
                                "item": {
                                    "type": "message", "id": msg_id, "role": "assistant",
                                    "status": "in_progress", "content": []
                                }
                            }),
                        )
                        .await;
                        started = true;
                    }
                    accumulated.push_str(text);
                    let _ = send(
                        &tx,
                        "response.output_text.delta",
                        &json!({
                            "type": "response.output_text.delta",
                            "output_index": 0, "item_id": msg_id,
                            "delta": text
                        }),
                    )
                    .await;
                }

                if let Some(n) = chunk
                    .get("usage")
                    .and_then(|u| u.get("completion_tokens"))
                    .and_then(|v| v.as_i64())
                {
                    output_tokens = n;
                }
            }
        }

        if started {
            let _ = send(
                &tx,
                "response.output_text.done",
                &json!({
                    "type": "response.output_text.done",
                    "output_index": 0, "item_id": msg_id, "text": accumulated
                }),
            )
            .await;
            let _ = send(
                &tx,
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "type": "message", "id": msg_id, "role": "assistant",
                        "status": "completed", "content": []
                    }
                }),
            )
            .await;
        }

        let _ = send(
            &tx,
            "response.completed",
            &json!({
                "type": "response.completed",
                "response": {
                    "id": resp_id, "object": "response", "status": "completed",
                    "model": model, "output": [],
                    "usage": {
                        "input_tokens": 0,
                        "output_tokens": output_tokens,
                        "total_tokens": output_tokens
                    }
                }
            }),
        )
        .await;
    });

    sse_response(rx)
}

/// Emit a buffered Responses result as one complete event sequence.
pub fn translate_stream_from_json(chat: &Value, model: &str) -> Response {
    let model = model.to_string();
    let resp = from_chat_json(chat, &model);
    let text = resp.get("output_text").and_then(|t| t.as_str()).unwrap_or_default().to_string();
    let id = resp.get("id").cloned().unwrap_or_else(|| json!("resp_unknown"));
    let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(32);

    tokio::spawn(async move {
        let model = model.clone();
        let _ = send(
            &tx,
            "response.created",
            &json!({
                "type": "response.created",
                "response": { "id": id, "object": "response", "status": "in_progress",
                              "model": model, "output": [] }
            }),
        )
        .await;
        let _ = send(
            &tx,
            "response.output_item.added",
            &json!({
                "type": "response.output_item.added", "output_index": 0,
                "item": { "type": "message", "id": msg_id, "role": "assistant",
                          "status": "in_progress", "content": [] }
            }),
        )
        .await;
        if !text.is_empty() {
            let _ = send(
                &tx,
                "response.output_text.delta",
                &json!({
                    "type": "response.output_text.delta",
                    "output_index": 0, "item_id": msg_id, "delta": text
                }),
            )
            .await;
        }
        let _ = send(
            &tx,
            "response.output_text.done",
            &json!({ "type": "response.output_text.done",
                     "output_index": 0, "item_id": msg_id, "text": text }),
        )
        .await;
        let _ = send(
            &tx,
            "response.output_item.done",
            &json!({
                "type": "response.output_item.done", "output_index": 0,
                "item": { "type": "message", "id": msg_id, "role": "assistant",
                          "status": "completed", "content": [] }
            }),
        )
        .await;
        let _ = send(
            &tx,
            "response.completed",
            &json!({
                "type": "response.completed",
                "response": { "id": id, "object": "response", "status": "completed",
                              "model": model, "output": [],
                              "usage": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0 } }
            }),
        )
        .await;
    });

    sse_response(rx)
}

/// Buffer a provider stream into one Responses JSON body.
pub async fn collect_stream(upstream: Response, model: &str) -> Response {
    let mut body = upstream.into_body();
    let mut text = String::new();
    let mut input_tokens = 0i64;
    let mut output_tokens = 0i64;
    let mut buf = String::new();

    while let Some(frame) = body.frame().await {
        let data = match frame {
            Ok(f) => f.into_data().unwrap_or_default(),
            Err(_) => break,
        };
        buf.push_str(&String::from_utf8_lossy(&data));
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
            if let Some(t) = chunk
                .pointer("/choices/0/delta/content")
                .and_then(|v| v.as_str())
            {
                text.push_str(t);
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

    let chat = json!({
        "id": format!("resp_{}", uuid::Uuid::new_v4().simple()),
        "choices": [{ "finish_reason": "stop", "message": { "role": "assistant", "content": text } }],
        "usage": { "prompt_tokens": input_tokens, "completion_tokens": output_tokens }
    });
    axum::Json(from_chat_json(&chat, model)).into_response()
}

fn sse_response(rx: mpsc::Receiver<Result<Bytes, std::io::Error>>) -> Response {
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

    fn req(v: Value) -> ResponsesRequest {
        serde_json::from_value(v).expect("valid")
    }

    #[test]
    fn plain_string_input_becomes_user_message() {
        let r = req(json!({"model":"m","input":"hello"}));
        let c = r.to_chat_request().unwrap();
        assert_eq!(c.messages.len(), 1);
        assert_eq!(c.messages[0].role, "user");
        assert_eq!(c.messages[0].content, json!("hello"));
    }

    #[test]
    fn instructions_become_system_message() {
        let r = req(json!({"model":"m","instructions":"be brief","input":"hi"}));
        let c = r.to_chat_request().unwrap();
        assert_eq!(c.messages[0].role, "system");
        assert_eq!(c.messages[0].content, json!("be brief"));
    }

    #[test]
    fn item_list_preserves_roles() {
        let r = req(json!({"model":"m","input":[
            {"role":"user","content":"a"},
            {"role":"assistant","content":"b"},
            {"role":"user","content":"c"}
        ]}));
        let c = r.to_chat_request().unwrap();
        let roles: Vec<_> = c.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "assistant", "user"]);
    }

    #[test]
    fn content_parts_are_joined() {
        let r = req(json!({"model":"m","input":[
            {"role":"user","content":[{"type":"input_text","text":"one"},
                                      {"type":"input_text","text":"two"}]}
        ]}));
        let c = r.to_chat_request().unwrap();
        assert_eq!(c.messages[0].content, json!("one\ntwo"));
    }

    #[test]
    fn developer_role_maps_to_system() {
        let r = req(json!({"model":"m","input":[{"role":"developer","content":"x"}]}));
        let c = r.to_chat_request().unwrap();
        assert_eq!(c.messages[0].role, "system");
    }

    #[test]
    fn max_output_tokens_maps_to_max_tokens() {
        let r = req(json!({"model":"m","input":"hi","max_output_tokens":99}));
        assert_eq!(r.to_chat_request().unwrap().max_tokens, Some(99));
    }

    #[test]
    fn empty_input_is_rejected() {
        let r = req(json!({"model":"m"}));
        assert!(r.to_chat_request().is_err());
    }

    #[test]
    fn response_shape() {
        let chat = json!({
            "id":"chatcmpl-1",
            "choices":[{"finish_reason":"stop","message":{"content":"hi"}}],
            "usage":{"prompt_tokens":3,"completion_tokens":4}
        });
        let out = from_chat_json(&chat, "m");
        assert_eq!(out["object"], "response");
        assert_eq!(out["status"], "completed");
        assert_eq!(out["output_text"], "hi");
        assert_eq!(out["output"][0]["content"][0]["type"], "output_text");
        assert_eq!(out["usage"]["input_tokens"], 3);
        assert_eq!(out["usage"]["output_tokens"], 4);
        assert_eq!(out["usage"]["total_tokens"], 7);
    }

    #[test]
    fn length_finish_is_incomplete() {
        let chat = json!({"id":"c","choices":[{"finish_reason":"length","message":{"content":"x"}}]});
        assert_eq!(from_chat_json(&chat, "m")["status"], "incomplete");
    }
}
