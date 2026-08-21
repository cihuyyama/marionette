//! BYOK (bring your own key) provider: arbitrary user-supplied OpenAI-compatible
//! endpoints. One account row per endpoint — `provider="byok"`, `email=<slug>`
//! (model prefix), `data = {slug, baseUrl, apiKey, models, modelsFetchedAt}`.
//!
//! Policy differs from the built-in providers because these are the USER's own
//! keys, not farmed accounts — never kill or seal them for billing/moderation:
//!   - 401            → AuthInvalid (dead key → cut; correct)
//!   - 429            → RateLimited (parse "try again in N" from body, else 900s)
//!   - 402 | 403      → Upstream (→ fallen only; account stays selectable)
//!   - everything else → global classify_http_status
//!
//! No token refresh, no expiry, no quota (quota kind "none").

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

/// Reserved BYOK slugs: any of these would collide with the static routing
/// arms in `openai::provider_id_for_model` (checked case-insensitively).
const RESERVED_BYOK_SLUGS: &[&str] = &["bb", "gcli", "qd", "combo"];

/// A BYOK endpoint is one `accounts` row; no quota budget (kind "none").
pub const BYOK_PROVIDER: &str = "byok";

/// Static API-key passthrough provider for user-supplied OpenAI-compatible
/// endpoints. Like BlackboxProvider: no token refresh, no expiry; chat uses
/// the provider's own client (the pool-resolved `client` argument is unused).
pub struct ByokProvider {
    client: Client,
}

impl Default for ByokProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ByokProvider {
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

    pub fn api_key_of(data: &Value) -> Option<String> {
        data.get("apiKey")
            .or_else(|| data.get("api_key"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn base_url_of(data: &Value) -> Option<String> {
        data.get("baseUrl")
            .or_else(|| data.get("base_url"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Copy of BlackboxProvider::build_body: upstream model = the request
    /// model with the first `<slug>/` segment stripped (upstream_model),
    /// inner slashes preserved.
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

    /// Fetch the upstream model catalog: `GET {base}/models` with the same
    /// `/v1` normalization as chat. Accepts OpenAI `{"data":[{"id":...}]}`,
    /// a bare array of `{"id"}` objects, or an array of strings. Ids are
    /// deduped and capped at 500.
    pub async fn fetch_models(
        &self,
        base_url: &str,
        api_key: &str,
    ) -> Result<Vec<String>, ProviderError> {
        let url = models_url(base_url);
        let resp = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(20))
            .header("Authorization", format!("Bearer {api_key}"))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(format!("byok models fetch: {e}")))?;
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream {
                status,
                body: format!(
                    "byok models endpoint {url} returned {status}: {}",
                    text.chars().take(300).collect::<String>()
                ),
            });
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Other(format!("byok models json: {e}")))?;
        parse_models_response(&v).ok_or_else(|| {
            ProviderError::Other(
                "byok models: unrecognized payload (expected OpenAI {data:[{id}]} or array)"
                    .into(),
            )
        })
    }
}

#[async_trait]
impl Provider for ByokProvider {
    fn id(&self) -> &'static str {
        BYOK_PROVIDER
    }

    async fn ensure_fresh_auth(&self, account: &mut Account) -> Result<(), ProviderError> {
        let data = account.data_json();
        if Self::api_key_of(&data).is_none() {
            return Err(ProviderError::AuthInvalid("missing apiKey".into()));
        }
        if Self::base_url_of(&data).is_none() {
            return Err(ProviderError::AuthInvalid("missing baseUrl".into()));
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
        let api_key = Self::api_key_of(&data)
            .ok_or_else(|| ProviderError::AuthInvalid("missing apiKey".into()))?;
        let base = Self::base_url_of(&data)
            .ok_or_else(|| ProviderError::AuthInvalid("missing baseUrl".into()))?;

        let resp = self
            .client
            .post(chat_url(&base))
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&Self::build_body(req))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_byok_status(status, &text));
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
                .map_err(|e| ProviderError::Other(format!("byok json: {e}")))?;
            if v.is_object() {
                v["model"] = json!(req_model);
            }
            Ok(ChatOutcome::Json(v))
        }
    }
}

/// Validate a BYOK endpoint slug: `^[a-z0-9][a-z0-9_-]{0,31}$` plus reserved
/// names that would shadow the static routing arms.
pub fn validate_byok_slug(slug: &str) -> Result<(), String> {
    let bytes = slug.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return Err("slug must be 1-32 characters".into());
    }
    let first = bytes[0];
    let starts_alnum = first.is_ascii_lowercase() || first.is_ascii_digit();
    if !starts_alnum {
        return Err("slug must start with a lowercase letter or digit".into());
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
    {
        return Err(
            "slug may only contain lowercase letters, digits, '-' and '_'".into(),
        );
    }
    // Case-insensitive reserved check (slug grammar already forced lowercase,
    // but keep the explicit fold for clarity/future-proofing).
    let lower = slug.to_ascii_lowercase();
    if RESERVED_BYOK_SLUGS.contains(&lower.as_str()) {
        return Err(format!("slug '{slug}' is reserved for a built-in provider"));
    }
    if lower.starts_with("blackbox") || lower.starts_with("qoder") || lower.contains("grok") {
        return Err(format!(
            "slug '{slug}' is reserved (must not reference a built-in provider)"
        ));
    }
    Ok(())
}

/// Normalize a user base URL into the chat completions endpoint: strip
/// trailing '/', then `/v1/chat/completions` unless the base already ends
/// with `/v1`.
pub fn chat_url(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

/// Same `/v1` normalization for the models catalog endpoint.
pub fn models_url(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    }
}

/// A base URL must be absolute http/https (no url crate: minimal structural
/// check — scheme + non-empty host).
pub fn is_valid_byok_base_url(s: &str) -> bool {
    let rest = s
        .trim()
        .strip_prefix("https://")
        .or_else(|| s.trim().strip_prefix("http://"));
    let Some(rest) = rest else {
        return false;
    };
    let host_part = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host_part.split('@').next_back().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    !host.is_empty()
}

/// BYOK-scoped status classification. The user's own keys must never be cut
/// or sealed for billing/permission rejections: 402/403 map to `Upstream`,
/// which lands in the pool's fallen branch and keeps the account selectable.
pub fn classify_byok_status(status: u16, body: &str) -> ProviderError {
    match status {
        401 => ProviderError::AuthInvalid("byok key invalid".into()),
        402 | 403 => ProviderError::Upstream {
            status,
            body: body.chars().take(2000).collect(),
        },
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

/// Extract model ids from an upstream models payload; dedupe, cap 500.
/// Returns None when the shape is unrecognized.
fn parse_models_response(v: &Value) -> Option<Vec<String>> {
    let arr = if let Some(a) = v.get("data").and_then(|d| d.as_array()) {
        a
    } else if let Some(a) = v.as_array() {
        a
    } else {
        return None;
    };
    let mut out: Vec<String> = Vec::new();
    for item in arr {
        let id = if let Some(s) = item.as_str() {
            s.to_string()
        } else if let Some(s) = item.get("id").and_then(|x| x.as_str()) {
            s.to_string()
        } else {
            continue;
        };
        if id.is_empty() || out.contains(&id) {
            continue;
        }
        out.push(id);
        if out.len() >= 500 {
            break;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn byok_account(data: Value) -> Account {
        Account {
            id: "byok1".into(),
            provider: "byok".into(),
            email: Some("openrouter".into()),
            name: Some("OpenRouter".into()),
            is_active: 1,
            priority: 0,
            data: data.to_string(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: "t".into(),
            updated_at: "t".into(),
            quota_limit: 0,
            quota_remaining: 0,
        }
    }

    #[test]
    fn slug_validation_accepts_valid_slugs() {
        for slug in ["openrouter", "my-api", "a1_b-c"] {
            assert!(validate_byok_slug(slug).is_ok(), "slug '{slug}' must pass");
        }
    }

    #[test]
    fn slug_validation_rejects_reserved_and_invalid() {
        for slug in [
            "BB",
            "bb",
            "gcli",
            "qd",
            "combo",
            "blackbox-x",
            "qoder",
            "my-grok-api",
            "has space",
            "-lead",
            "_lead",
            &"a".repeat(33),
        ] {
            assert!(validate_byok_slug(slug).is_err(), "slug '{slug}' must fail");
        }
    }

    #[test]
    fn classify_401_is_auth_invalid() {
        assert!(matches!(
            classify_byok_status(401, r#"{"error":{"message":"invalid key"}}"#),
            ProviderError::AuthInvalid(_)
        ));
    }

    #[test]
    fn classify_402_is_upstream_not_payment() {
        let err = classify_byok_status(402, r#"{"error":{"message":"billing"}}"#);
        assert!(
            matches!(&err, ProviderError::Upstream { status: 402, .. }),
            "402 must be Upstream{{402}} (fallen), got {err:?}"
        );
        assert!(!matches!(err, ProviderError::PaymentRequired));
    }

    #[test]
    fn classify_403_is_upstream_not_access_denied() {
        let err = classify_byok_status(403, r#"{"error":{"message":"forbidden"}}"#);
        assert!(
            matches!(&err, ProviderError::Upstream { status: 403, .. }),
            "403 must be Upstream{{403}} (fallen), got {err:?}"
        );
        assert!(!matches!(err, ProviderError::AccessDenied));
    }

    #[test]
    fn classify_429_parses_retry_after() {
        let err = classify_byok_status(
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
        let err = classify_byok_status(429, r#"{"error":{"message":"slow down"}}"#);
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
            classify_byok_status(500, "boom"),
            ProviderError::Upstream { status: 500, .. }
        ));
    }

    #[test]
    fn build_body_strips_slug_prefix_and_passes_optional_fields() {
        let req = ChatCompletionRequest {
            model: "openrouter/anthropic/claude-x".into(),
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
        let body = ByokProvider::build_body(&req);
        assert_eq!(
            body["model"], "anthropic/claude-x",
            "only the first slug segment is stripped"
        );
        assert_eq!(body["stream"], false);
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["max_tokens"], 128);
        assert!(body.get("top_p").is_none());
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn build_body_includes_tools_when_present() {
        let req = ChatCompletionRequest {
            model: "my-api/some-model".into(),
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
        let body = ByokProvider::build_body(&req);
        assert_eq!(body["stream"], true);
        assert!(body["tools"].is_array());
        assert_eq!(body["tool_choice"], "auto");
        assert!(body.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn chat_url_normalization() {
        assert_eq!(
            chat_url("https://x.ai/api/v1/"),
            "https://x.ai/api/v1/chat/completions"
        );
        assert_eq!(
            chat_url("https://x.ai/api"),
            "https://x.ai/api/v1/chat/completions"
        );
        assert_eq!(
            chat_url("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            models_url("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1/models"
        );
        assert_eq!(
            models_url("https://x.ai/api"),
            "https://x.ai/api/v1/models"
        );
    }

    #[test]
    fn base_url_validation() {
        assert!(is_valid_byok_base_url("https://openrouter.ai/api/v1"));
        assert!(is_valid_byok_base_url("http://127.0.0.1:8080"));
        assert!(!is_valid_byok_base_url("ftp://x.ai"));
        assert!(!is_valid_byok_base_url("not a url"));
        assert!(!is_valid_byok_base_url("https://"));
        assert!(!is_valid_byok_base_url(""));
    }

    #[tokio::test]
    async fn ensure_fresh_auth_requires_api_key_and_base_url() {
        let provider = ByokProvider::new();

        let mut acc = byok_account(json!({}));
        assert!(matches!(
            provider.ensure_fresh_auth(&mut acc).await,
            Err(ProviderError::AuthInvalid(_))
        ));

        acc.data = json!({"apiKey": "sk-test"}).to_string();
        assert!(
            matches!(
                provider.ensure_fresh_auth(&mut acc).await,
                Err(ProviderError::AuthInvalid(_))
            ),
            "baseUrl missing → invalid"
        );

        acc.data = json!({"apiKey": "sk-test", "baseUrl": "https://openrouter.ai/api/v1"})
            .to_string();
        assert!(provider.ensure_fresh_auth(&mut acc).await.is_ok());

        acc.data = json!({"apiKey": "   ", "baseUrl": "https://openrouter.ai/api/v1"})
            .to_string();
        assert!(matches!(
            provider.ensure_fresh_auth(&mut acc).await,
            Err(ProviderError::AuthInvalid(_))
        ));
    }

    #[test]
    fn parse_models_accepts_all_shapes_dedupes_and_caps() {
        // OpenAI shape
        let v = json!({"object":"list","data":[{"id":"a"},{"id":"b"},{"id":"a"}]});
        assert_eq!(parse_models_response(&v), Some(vec!["a".into(), "b".into()]));
        // bare array of objects
        let v = json!([{"id":"x"},{"id":"y"}]);
        assert_eq!(parse_models_response(&v), Some(vec!["x".into(), "y".into()]));
        // array of strings
        let v = json!(["m1", "m2", "m1"]);
        assert_eq!(parse_models_response(&v), Some(vec!["m1".into(), "m2".into()]));
        // unrecognized
        assert_eq!(parse_models_response(&json!({"weird": true})), None);
        // cap at 500
        let ids: Vec<Value> = (0..600).map(|i| json!(format!("m{i}"))).collect();
        let v = Value::Array(ids);
        assert_eq!(parse_models_response(&v).unwrap().len(), 500);
    }
}
