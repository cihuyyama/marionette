use super::{ChatOutcome, Provider, StreamUsage, classify_http_status};
use crate::db::Account;
use crate::error::ProviderError;
use crate::openai::ChatCompletionRequest;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};
use tokio::sync::oneshot;

const CHAT_URL: &str = "https://api.blackbox.ai/v1/chat/completions";

/// Static `sk-…` API key provider. Pure OpenAI-compatible upstream: no token
/// refresh, no expiry, no quota sync. Like Qoder, chat uses the provider's own
/// client (the pool-resolved `client` argument is not needed here).
pub struct BlackboxProvider {
    client: Client,
}

impl Default for BlackboxProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl BlackboxProvider {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(15))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(60))
            .tcp_nodelay(true)
            .build()
            .expect("reqwest client");
        Self { client }
    }

    fn api_key(data: &Value) -> Option<String> {
        data.get("apiKey")
            .or_else(|| data.get("api_key"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn build_body(req: &ChatCompletionRequest) -> Value {
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
        if req.has_tools() {
            if let Some(tools) = req.tools.as_ref() {
                body["tools"] = tools.clone();
            }
            if let Some(tc) = req.tool_choice.as_ref() {
                body["tool_choice"] = tc.clone();
            }
            if let Some(ptc) = req.parallel_tool_calls.as_ref() {
                body["parallel_tool_calls"] = ptc.clone();
            }
        }
        body
    }
}

#[async_trait]
impl Provider for BlackboxProvider {
    fn id(&self) -> &'static str {
        "blackbox"
    }

    async fn ensure_fresh_auth(&self, account: &mut Account) -> Result<(), ProviderError> {
        let data = account.data_json();
        if Self::api_key(&data).is_none() {
            return Err(ProviderError::AuthInvalid("missing apiKey".into()));
        }
        Ok(())
    }

    async fn chat(
        &self,
        _client: &Client,
        account: &Account,
        req: &ChatCompletionRequest,
    ) -> Result<ChatOutcome, ProviderError> {
        let data = account.data_json();
        let api_key = Self::api_key(&data)
            .ok_or_else(|| ProviderError::AuthInvalid("missing apiKey".into()))?;

        let resp = self
            .client
            .post(CHAT_URL)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&Self::build_body(req))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_blackbox_status(status, &text));
        }

        let req_model = req.model.clone();

        if req.stream_enabled() {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);
            let (usage_tx, usage_rx) = oneshot::channel::<Option<StreamUsage>>();
            let mut upstream_stream = resp.bytes_stream();

            tokio::spawn(async move {
                let mut buffer = String::new();
                let mut prompt_tokens: i64 = 0;
                let mut completion_tokens: i64 = 0;
                let mut total_tokens: i64 = 0;
                let mut usage_tx = Some(usage_tx);

                loop {
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
                                let _ = tx
                                    .send(Ok(bytes::Bytes::from("data: [DONE]\n\n")))
                                    .await;
                                let usage = StreamUsage {
                                    prompt_tokens,
                                    completion_tokens,
                                    total_tokens,
                                }
                                .normalized();
                                if let Some(txu) = usage_tx.take() {
                                    let _ = txu.send(if usage.is_empty() {
                                        None
                                    } else {
                                        Some(usage)
                                    });
                                }
                                return;
                            }

                            if let Ok(v) = serde_json::from_str::<Value>(data_str) {
                                if let Some(u) = v.get("usage") {
                                    if let Some(p) = usage_i64(u, "prompt_tokens") {
                                        prompt_tokens = p;
                                    }
                                    if let Some(c) = usage_i64(u, "completion_tokens") {
                                        completion_tokens = c;
                                    }
                                    if let Some(t) = usage_i64(u, "total_tokens") {
                                        total_tokens = t;
                                    }
                                }
                            }

                            if tx
                                .send(Ok(bytes::Bytes::from(format!("data: {data_str}\n\n"))))
                                .await
                                .is_err()
                            {
                                if let Some(txu) = usage_tx.take() {
                                    let _ = txu.send(None);
                                }
                                return;
                            }
                        } else if tx
                            .send(Ok(bytes::Bytes::from(format!("{line}\n"))))
                            .await
                            .is_err()
                        {
                            if let Some(txu) = usage_tx.take() {
                                let _ = txu.send(None);
                            }
                            return;
                        }
                    }

                    match upstream_stream.next().await {
                        Some(Ok(chunk)) => buffer.push_str(&String::from_utf8_lossy(&chunk)),
                        Some(Err(e)) => {
                            let _ = tx
                                .send(Err(std::io::Error::new(std::io::ErrorKind::Other, e)))
                                .await;
                            if let Some(txu) = usage_tx.take() {
                                let _ = txu.send(None);
                            }
                            return;
                        }
                        None => {
                            if buffer.is_empty() {
                                break;
                            }
                            buffer.push('\n');
                        }
                    }
                }

                // Upstream EOF without [DONE]: close the client stream cleanly.
                let _ = tx.send(Ok(bytes::Bytes::from("data: [DONE]\n\n"))).await;
                let usage = StreamUsage {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                }
                .normalized();
                if let Some(txu) = usage_tx.take() {
                    let _ = txu.send(if usage.is_empty() { None } else { Some(usage) });
                }
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
            let text = resp
                .text()
                .await
                .map_err(|e| ProviderError::Transport(e.to_string()))?;
            let mut v: Value = serde_json::from_str(&text)
                .map_err(|e| ProviderError::Other(format!("blackbox json: {e}")))?;
            if v.is_object() {
                v["model"] = json!(req_model);
            }
            Ok(ChatOutcome::Json(v))
        }
    }
}

/// Blackbox-scoped status classification. 403 is content moderation (a policy
/// rejection of the prompt), NOT account death: it maps to `Other`, which lands
/// in the pool's fallen branch and keeps the account selectable. It must never
/// be AuthInvalid/AccessDenied/Upstream-403 here — all three cut the account.
pub fn classify_blackbox_status(status: u16, body: &str) -> ProviderError {
    match status {
        401 => ProviderError::AuthInvalid("blackbox key invalid".into()),
        402 => ProviderError::PaymentRequired,
        403 => ProviderError::Other(format!(
            "blackbox 403 moderation: {}",
            body.chars().take(200).collect::<String>()
        )),
        429 => ProviderError::RateLimited {
            retry_after_secs: Some(parse_retry_after_secs(body).unwrap_or(900)),
        },
        _ => classify_http_status(status, body),
    }
}

fn parse_retry_after_secs(body: &str) -> Option<u64> {
    let needle = "try again in ";
    let idx = body.to_lowercase().find(needle)?;
    let rest = &body[idx + needle.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn usage_i64(usage: &Value, key: &str) -> Option<i64> {
    usage
        .get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|n| n as i64)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_401_is_auth_invalid() {
        assert!(matches!(
            classify_blackbox_status(401, r#"{"error":{"code":401,"message":"invalid key"}}"#),
            ProviderError::AuthInvalid(_)
        ));
    }

    #[test]
    fn classify_402_is_payment_required() {
        assert!(matches!(
            classify_blackbox_status(402, "{}"),
            ProviderError::PaymentRequired
        ));
    }

    #[test]
    fn classify_403_moderation_never_cuts() {
        let err = classify_blackbox_status(403, r#"{"error":{"message":"blocked"}}"#);
        assert!(
            matches!(&err, ProviderError::Other(msg) if msg.contains("blackbox 403 moderation")),
            "403 must be Other (fallen), got {err:?}"
        );
        assert!(!matches!(
            err,
            ProviderError::AuthInvalid(_) | ProviderError::AccessDenied
        ));
        if let ProviderError::Upstream { status, .. } = &err {
            panic!("403 must not be Upstream-{status} (would cut)");
        }
    }

    #[test]
    fn classify_429_parses_retry_after() {
        let err = classify_blackbox_status(
            429,
            r#"{"error":{"message":"Rate limit reached. Try again in 45 seconds."}}"#,
        );
        assert!(matches!(
            err,
            ProviderError::RateLimited {
                retry_after_secs: Some(45)
            }
        ));
    }

    #[test]
    fn classify_429_without_parseable_delay_defaults_to_900() {
        let err = classify_blackbox_status(429, r#"{"error":{"message":"slow down"}}"#);
        assert!(matches!(
            err,
            ProviderError::RateLimited {
                retry_after_secs: Some(900)
            }
        ));
    }

    #[test]
    fn classify_other_statuses_fall_through_to_global() {
        assert!(matches!(
            classify_blackbox_status(500, "boom"),
            ProviderError::Upstream { status: 500, .. }
        ));
        assert!(matches!(
            classify_blackbox_status(400, "quota exceeded"),
            ProviderError::RateLimited { .. }
        ));
    }

    #[test]
    fn build_body_strips_bb_prefix_and_passes_optional_fields() {
        let req = ChatCompletionRequest {
            model: "bb/z-ai/glm-5.2".into(),
            messages: vec![crate::openai::ChatMessage {
                role: "user".into(),
                content: json!("hi"),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: Some(false),
            temperature: Some(0.5),
            max_tokens: Some(128),
            top_p: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            extra: Value::Object(Default::default()),
        };
        let body = BlackboxProvider::build_body(&req);
        assert_eq!(body["model"], "z-ai/glm-5.2");
        assert_eq!(body["stream"], false);
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["max_tokens"], 128);
        assert!(body.get("top_p").is_none());
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn build_body_includes_tools_when_present() {
        let req = ChatCompletionRequest {
            model: "bb/blackboxai/blackbox-pro".into(),
            messages: vec![],
            stream: Some(true),
            temperature: None,
            max_tokens: None,
            top_p: None,
            tools: Some(json!([{"type":"function","function":{"name":"f"}}])),
            tool_choice: Some(json!("auto")),
            parallel_tool_calls: None,
            extra: Value::Object(Default::default()),
        };
        let body = BlackboxProvider::build_body(&req);
        assert_eq!(body["stream"], true);
        assert!(body["tools"].is_array());
        assert_eq!(body["tool_choice"], "auto");
        assert!(body.get("parallel_tool_calls").is_none());
    }

    #[tokio::test]
    async fn ensure_fresh_auth_requires_api_key() {
        let provider = BlackboxProvider::new();
        let mut acc = Account {
            id: "bb1".into(),
            provider: "blackbox".into(),
            email: None,
            name: None,
            is_active: 1,
            priority: 0,
            data: "{}".into(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: "t".into(),
            updated_at: "t".into(),
            quota_limit: 0,
            quota_remaining: 0,
        };
        let err = provider.ensure_fresh_auth(&mut acc).await.unwrap_err();
        assert!(matches!(err, ProviderError::AuthInvalid(_)));

        acc.data = json!({"apiKey": "sk-test"}).to_string();
        assert!(provider.ensure_fresh_auth(&mut acc).await.is_ok());

        acc.data = json!({"apiKey": "   "}).to_string();
        assert!(matches!(
            provider.ensure_fresh_auth(&mut acc).await,
            Err(ProviderError::AuthInvalid(_))
        ));
    }
}
