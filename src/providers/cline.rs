//! Cline provider (`https://api.cline.bot/api/v1`).
//!
//! Ported from Cartethyia `src/providers/integrations/cline/`.
//!
//! Cline is the most conventional of the three new providers: the chat wire is
//! plain OpenAI Chat Completions over SSE, so this is mostly header and path
//! fidelity. Two details are easy to get wrong and both are load-bearing:
//!
//! * The path is `/chat/completions` joined onto a base that ALREADY ends in
//!   `/api/v1`. The reflexive `/v1/chat/completions` yields
//!   `/api/v1/v1/chat/completions` and 404s.
//! * An OAuth credential is sent as `Bearer workos:<token>`; an API-key
//!   credential gets a bare bearer. The prefix is not decoration — the
//!   upstream distinguishes the two credential kinds by it.
//!
//! Auth is WorkOS device flow. `/api/v1/auth/register` is NOT a signup
//! endpoint: it exchanges a WorkOS token pair for Cline's own credential.
//!
//! Account shape: `provider = "cline"`, `data = { accessToken, refreshToken,
//! expiresAt, credentialKind }`.

use super::{classify_http_status, ChatOutcome, Provider, StreamUsage};
use crate::db::Account;
use crate::error::ProviderError;
use crate::openai::ChatCompletionRequest;
use async_trait::async_trait;
use axum::body::Body;
use axum::response::Response;
use chrono::Utc;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::oneshot;

const BASE_URL: &str = "https://api.cline.bot/api/v1";
/// Joined onto a base that already ends in `/api/v1`. See module docs.
const CHAT_PATH: &str = "/chat/completions";
const REFRESH_URL: &str = "https://api.cline.bot/api/v1/auth/refresh";
const ME_URL: &str = "https://api.cline.bot/api/v1/users/me";
const PLAN_URL: &str = "https://api.cline.bot/api/v1/users/me/plan";
const USAGE_LIMITS_URL: &str = "https://api.cline.bot/api/v1/users/me/plan/usage-limits";
/// Version floors. Cartethyia resolves these live with these as fallback; we
/// pin them rather than scraping two URLs at startup.
const CLINE_CLIENT_VERSION: &str = "4.1.22";
const CLINE_SDK_VERSION: &str = "0.0.90";

pub const CLINE_PROVIDER: &str = "cline";

/// Refresh window: refresh this long before the stated expiry.
const REFRESH_LEAD_SECS: i64 = 300;

/// One billing window from `/users/me/plan/usage-limits`.
///
/// The upstream reports a percentage and a ceiling but no absolute used value,
/// so `used` is derived rather than left unknown.
#[derive(Debug, Clone, Deserialize)]
pub struct ClineUsageLimit {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    #[serde(rename = "percentUsed")]
    pub percent_used: Option<f64>,
    #[serde(rename = "resetsAt")]
    pub resets_at: Option<Value>,
    pub limit: Option<f64>,
    pub entitlement: Option<f64>,
    pub total: Option<f64>,
}

impl ClineUsageLimit {
    /// First present ceiling among the three spellings the upstream has used.
    fn ceiling(&self) -> Option<f64> {
        self.limit.or(self.entitlement).or(self.total)
    }

    fn used(&self) -> Option<f64> {
        let pct = self.percent_used?;
        let ceiling = self.ceiling()?;
        Some(ceiling * pct / 100.0)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ClineUsageLimitsPayload {
    #[serde(default)]
    limits: Vec<ClineUsageLimit>,
}

#[derive(Debug, Clone)]
pub struct ClineProvider {
    client: Client,
}

impl Default for ClineProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ClineProvider {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    pub fn access_token_of(data: &Value) -> Option<String> {
        data.get("accessToken")
            .or_else(|| data.get("access_token"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    pub fn refresh_token_of(data: &Value) -> Option<String> {
        data.get("refreshToken")
            .or_else(|| data.get("refresh_token"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    /// `true` when the row was onboarded as a raw API key rather than OAuth.
    ///
    /// Only then is the bearer sent bare instead of `workos:`-prefixed.
    fn is_api_key(data: &Value) -> bool {
        data.get("credentialKind")
            .or_else(|| data.get("credential_kind"))
            .and_then(|v| v.as_str())
            .map(|s| s == "api_key")
            .unwrap_or(false)
    }

    /// The exact Authorization value. `workos:` for OAuth, bare for api_key.
    fn bearer(data: &Value, token: &str) -> String {
        if Self::is_api_key(data) {
            format!("Bearer {token}")
        } else {
            format!("Bearer workos:{token}")
        }
    }

    /// Dispatch headers, mirroring `clineExtraHeaders` in Cartethyia.
    ///
    /// Both `Referer` spellings are sent deliberately: the upstream has been
    /// observed reading either depending on which edge handled the request.
    fn chat_headers(&self, data: &Value, token: &str) -> Vec<(&'static str, String)> {
        vec![
            ("content-type", "application/json".into()),
            ("authorization", Self::bearer(data, token)),
            ("user-agent", format!("Cline/{CLINE_CLIENT_VERSION}")),
            ("http-referer", "https://cline.bot".into()),
            ("HTTP-Referer", "https://cline.bot".into()),
            ("x-platform", std::env::consts::OS.into()),
            ("x-title", "Cline".into()),
            ("X-Title", "Cline".into()),
            ("x-client-type", "cline-sdk".into()),
            ("x-client-version", CLINE_CLIENT_VERSION.into()),
            ("x-core-version", CLINE_SDK_VERSION.into()),
            ("x-is-multi-root", "false".into()),
            ("accept", "text/event-stream, application/json".into()),
        ]
    }

    /// Quota headers. Note `x-client-type: cline-cli` (not `cline-sdk`) and
    /// `x-is-multiroot` — a different spelling from dispatch's
    /// `x-is-multi-root`. Both are transcribed as the upstream sends them.
    fn quota_headers(&self, token: &str, api_key: bool) -> Vec<(&'static str, String)> {
        let auth = if api_key {
            format!("Bearer {token}")
        } else {
            format!("Bearer workos:{token}")
        };
        vec![
            ("accept", "application/json".into()),
            ("content-type", "application/json".into()),
            ("user-agent", format!("Cline/{CLINE_CLIENT_VERSION}")),
            ("x-platform", "server".into()),
            ("x-platform-version", "1.0.0".into()),
            ("x-client-type", "cline-cli".into()),
            ("x-client-version", CLINE_CLIENT_VERSION.into()),
            ("x-core-version", CLINE_CLIENT_VERSION.into()),
            ("x-is-multiroot", "false".into()),
            ("authorization", auth),
        ]
    }

    pub fn chat_url() -> String {
        format!("{BASE_URL}{CHAT_PATH}")
    }

    /// Build the upstream body.
    ///
    /// Cline speaks stock Chat Completions, so the body is the request's own
    /// shape with two provider-specific repairs applied (see below).
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
        if let Some(t) = &req.tools {
            body["tools"] = t.clone();
        }
        if let Some(t) = &req.tool_choice {
            body["tool_choice"] = t.clone();
        }
        if let Some(p) = &req.parallel_tool_calls {
            body["parallel_tool_calls"] = p.clone();
        }
        // Forward anything the client sent that we do not model explicitly,
        // so provider-specific controls are not silently dropped.
        if let Value::Object(extra) = &req.extra {
            for (k, v) in extra {
                if !body.as_object().unwrap().contains_key(k) {
                    body[k] = v.clone();
                }
            }
        }

        // Cline's upstream rejects an empty or absent system turn, so one is
        // injected rather than letting the request fail on a shape the client
        // was entitled to send.
        if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
            let has_system = messages
                .iter()
                .any(|m| matches!(m.get("role").and_then(|r| r.as_str()), Some("system" | "developer")));
            if !has_system {
                messages.insert(0, json!({"role": "system", "content": "You are a helpful assistant."}));
            }
            for m in messages.iter_mut() {
                if matches!(m.get("role").and_then(|r| r.as_str()), Some("system" | "developer")) {
                    let empty = match m.get("content") {
                        None | Some(Value::Null) => true,
                        Some(Value::String(s)) => s.trim().is_empty(),
                        Some(Value::Array(a)) => a.is_empty(),
                        _ => false,
                    };
                    if empty {
                        m.as_object_mut()
                            .expect("message is an object")
                            .insert("content".into(), json!("You are a helpful assistant."));
                    }
                }
            }
        }

        // Every tool schema must carry an explicit `required` array; the
        // upstream rejects a schema that omits it even when nothing is
        // required.
        if let Some(tools) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
            for tool in tools.iter_mut() {
                complete_required_schema(tool);
            }
        }

        body
    }

    /// Refresh: `POST /auth/refresh` with `{ refreshToken, grantType }`.
    ///
    /// This is a pure POST with no browser state, which is what makes a
    /// refresh-token-only import viable.
    async fn refresh_with(&self, refresh_token: &str) -> Result<Value, ProviderError> {
        let resp = self
            .client
            .post(REFRESH_URL)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .json(&json!({
                "refreshToken": refresh_token,
                "grantType": "refresh_token"
            }))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(classify_http_status(status, &text));
        }
        serde_json::from_str::<Value>(&text)
            .map_err(|e| ProviderError::Upstream { status, body: format!("bad refresh json: {e}") })
    }
}

/// Recursively ensure every JSON-Schema object carries `required`.
fn complete_required_schema(value: &mut Value) {
    match value {
        Value::Object(map) => {
            // Repair the schema-bearing keys, then recurse into everything.
            for key in ["parameters", "schema", "json_schema"] {
                if let Some(inner) = map.get_mut(key) {
                    complete_required_schema(inner);
                }
            }
            if let Some(Value::Object(inner)) = map.get_mut("function") {
                complete_required_schema(&mut Value::Object(inner.clone()));
                if let Some(Value::Object(fixed)) = map.get("function") {
                    let _ = fixed;
                }
            }
            // A schema object is one that declares properties or a type.
            let is_schema = map.contains_key("properties") || map.contains_key("type");
            if is_schema && !map.contains_key("required") {
                map.insert("required".into(), json!([]));
            }
            for (_, v) in map.iter_mut() {
                complete_required_schema(v);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                complete_required_schema(item);
            }
        }
        _ => {}
    }
}

#[async_trait]
impl Provider for ClineProvider {
    fn id(&self) -> &'static str {
        CLINE_PROVIDER
    }

    async fn ensure_fresh_auth(&self, account: &mut Account) -> Result<(), ProviderError> {
        let data = account.data_json();

        // API-key rows are static: no refresh, no expiry.
        if Self::is_api_key(&data) {
            if Self::access_token_of(&data).is_none() {
                return Err(ProviderError::AuthInvalid("missing accessToken".into()));
            }
            account.last_error = None;
            return Ok(());
        }

        let needs_refresh = match data.get("expiresAt").and_then(|v| v.as_str()) {
            Some(exp) => chrono::DateTime::parse_from_rfc3339(exp)
                .map(|t| t.timestamp() - Utc::now().timestamp() < REFRESH_LEAD_SECS)
                .unwrap_or(true),
            None => true,
        };
        if !needs_refresh && Self::access_token_of(&data).is_some() {
            account.last_error = None;
            return Ok(());
        }

        let refresh_token = Self::refresh_token_of(&data)
            .ok_or_else(|| ProviderError::AuthExpired)?;

        let mut refreshed = self.refresh_with(&refresh_token).await?;
        // The endpoint answers with either a bare token object or a
        // `{ success, data }` envelope.
        if refreshed.get("data").is_some() && refreshed.get("accessToken").is_none() {
            refreshed = refreshed.get("data").cloned().unwrap_or(refreshed);
        }

        let access = refreshed
            .get("accessToken")
            .or_else(|| refreshed.get("access_token"))
            .or_else(|| refreshed.get("token"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| ProviderError::AuthInvalid("refresh omitted access token".into()))?;

        let mut data = data;
        data["accessToken"] = json!(access);
        if let Some(rt) = refreshed
            .get("refreshToken")
            .or_else(|| refreshed.get("refresh_token"))
            .and_then(|v| v.as_str())
        {
            data["refreshToken"] = json!(rt.to_string());
        }
        if let Some(exp) = refreshed
            .get("expiresAt")
            .or_else(|| refreshed.get("expires_at"))
            .and_then(|v| v.as_str())
        {
            data["expiresAt"] = json!(exp.to_string());
        } else if let Some(secs) = refreshed
            .get("expiresIn")
            .or_else(|| refreshed.get("expires_in"))
            .and_then(|v| v.as_i64())
        {
            data["expiresAt"] = json!((Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339());
        }
        account.set_data_json(&data);
        account.last_error = None;
        Ok(())
    }

    async fn chat(
        &self,
        _client: &Client,
        account: &Account,
        req: &ChatCompletionRequest,
    ) -> Result<ChatOutcome, ProviderError> {
        let data = account.data_json();
        let token = Self::access_token_of(&data)
            .ok_or_else(|| ProviderError::AuthInvalid("missing accessToken".into()))?;

        let body = Self::build_body(req);

        let mut request = self.client.post(Self::chat_url());
        for (k, v) in self.chat_headers(&data, &token) {
            request = request.header(k, v);
        }

        let resp = request
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
            // Cline's stream is ordinary SSE, so the bytes are passed through
            // and only the usage frames are observed for accounting.
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);
            let (usage_tx, usage_rx) = oneshot::channel::<Option<StreamUsage>>();
            let mut upstream = resp.bytes_stream();

            tokio::spawn(async move {
                let mut buffer = String::new();
                let mut prompt_tokens: i64 = 0;
                let mut completion_tokens: i64 = 0;
                let mut total_tokens: i64 = 0;
                let mut usage_tx = Some(usage_tx);

                while let Some(chunk_res) = upstream.next().await {
                    let chunk = match chunk_res {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = tx
                                .send(Err(std::io::Error::new(std::io::ErrorKind::Other, e)))
                                .await;
                            if let Some(t) = usage_tx.take() {
                                let _ = t.send(None);
                            }
                            return;
                        }
                    };
                    let _ = tx.send(Ok(chunk.clone())).await;

                    // Usage is read out of band so the pool can bill the
                    // request; the client still receives verbatim bytes.
                    buffer.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].trim_end_matches('\r').to_string();
                        buffer = buffer[pos + 1..].to_string();
                        let Some(rest) = line.strip_prefix("data:") else {
                            continue;
                        };
                        let payload = rest.trim();
                        if payload == "[DONE]" {
                            continue;
                        }
                        if let Ok(v) = serde_json::from_str::<Value>(payload) {
                            if let Some(u) = v.get("usage") {
                                if let Some(p) = u.get("prompt_tokens").and_then(|x| x.as_i64()) {
                                    prompt_tokens = p;
                                }
                                if let Some(c) = u.get("completion_tokens").and_then(|x| x.as_i64()) {
                                    completion_tokens = c;
                                }
                                if let Some(t) = u.get("total_tokens").and_then(|x| x.as_i64()) {
                                    total_tokens = t;
                                }
                            }
                        }
                    }
                }

                if let Some(t) = usage_tx.take() {
                    let usage = if prompt_tokens == 0 && completion_tokens == 0 && total_tokens == 0 {
                        None
                    } else {
                        Some(
                            StreamUsage {
                                prompt_tokens,
                                completion_tokens,
                                total_tokens,
                            }
                            .normalized(),
                        )
                    };
                    let _ = t.send(usage);
                }
            });

            return Ok(ChatOutcome::Stream {
                response: Response::builder()
                    .status(200)
                    .header("Content-Type", "text/event-stream")
                    .header("Cache-Control", "no-cache")
                    .body(Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)))
                    .map_err(|e| ProviderError::Transport(e.to_string()))?,
                usage_rx,
            });
        }

        let text = resp.text().await.unwrap_or_default();
        let mut value: Value = serde_json::from_str(&text).map_err(|e| ProviderError::Upstream {
            status,
            body: format!("bad chat json: {e}"),
        })?;

        value = unwrap_data_envelope(value);

        // A JSON error envelope on a 2xx must not decode as an empty answer.
        if value.get("error").is_some()
            && value.get("choices").is_none()
            && value.get("output").is_none()
        {
            return Err(ProviderError::Upstream {
                status,
                body: text.chars().take(2000).collect(),
            });
        }
        if let Some(obj) = value.as_object_mut() {
            if obj.get("model").is_none() {
                obj.insert("model".into(), json!(req_model));
            }
        }
        Ok(ChatOutcome::Json(value))
    }

    async fn sync_quota(&self, account: &mut Account) -> Result<(), ProviderError> {
        let data = account.data_json();
        let token = Self::access_token_of(&data)
            .ok_or_else(|| ProviderError::AuthInvalid("missing accessToken".into()))?;
        let api_key = Self::is_api_key(&data);

        // API-key rows have no OAuth envelope, and upstream exposes no quota
        // surface for them; reaching /users/me with one would only 401.
        if api_key {
            return Ok(());
        }

        let mut request = self.client.get(ME_URL);
        for (k, v) in self.quota_headers(&token, api_key) {
            request = request.header(k, v);
        }
        let resp = request
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        if !resp.status().is_success() {
            // A rejected credential is not a hard failure of the sync.
            return Ok(());
        }
        let me: Value = resp.json().await.unwrap_or(json!({}));

        let mut request = self.client.get(USAGE_LIMITS_URL);
        for (k, v) in self.quota_headers(&token, api_key) {
            request = request.header(k, v);
        }
        let limits_resp = request.send().await;

        let mut data = data;
        if let Some(email) = me.get("email").and_then(|v| v.as_str()) {
            if account.email.is_none() {
                account.email = Some(email.to_string());
            }
        }
        if let Ok(resp) = limits_resp {
            if resp.status().is_success() {
                if let Ok(payload) = resp.json::<ClineUsageLimitsPayload>().await {
                    let windows: Vec<Value> = payload
                        .limits
                        .iter()
                        .filter_map(|l| {
                            let pct = l.percent_used?;
                            json_window(l, pct)
                        })
                        .collect();
                    if !windows.is_empty() {
                        data["quotaWindows"] = Value::Array(windows);
                    }
                }
            }
        }
        // plan name is best-effort and never fails the sync
        let mut request = self.client.get(PLAN_URL);
        for (k, v) in self.quota_headers(&token, api_key) {
            request = request.header(k, v);
        }
        if let Ok(resp) = request.send().await {
            if resp.status().is_success() {
                if let Ok(plan) = resp.json::<Value>().await {
                    let name = plan
                        .get("plan")
                        .and_then(|p| p.get("displayName"))
                        .or_else(|| plan.get("displayName"))
                        .and_then(|v| v.as_str());
                    if let Some(n) = name {
                        data["plan"] = json!(n);
                    }
                }
            }
        }
        account.set_data_json(&data);
        Ok(())
    }
}

fn json_window(l: &ClineUsageLimit, pct: f64) -> Option<Value> {
    let kind = l.kind.clone().unwrap_or_else(|| "window".into());
    let pct = pct.clamp(0.0, 100.0);
    let mut out = serde_json::Map::new();
    out.insert("kind".into(), json!(kind));
    out.insert("usedPercent".into(), json!(pct));
    out.insert("remainingPercent".into(), json!(100.0 - pct));
    if let Some(ceiling) = l.ceiling() {
        out.insert("limit".into(), json!(ceiling));
    }
    if let Some(used) = l.used() {
        out.insert("used".into(), json!(used));
    }
    if let Some(reset) = &l.resets_at {
        out.insert("resetsAt".into(), reset.clone());
    }
    if out.len() <= 3 {
        // kind + percentages only: upstream told us nothing actionable.
        return None;
    }
    Some(Value::Object(out))
}


/// cline wraps the non-stream completion in a `data` envelope
/// (`{"data":{"choices":[...]}}`) while its streaming path emits plain chunks.
///
/// Passing that through would hand an OpenAI-shaped client a body whose
/// `choices` is undefined. Only unwrap when the inner object actually carries a
/// completion or an error, so an unrelated `data` field is left untouched.
pub fn unwrap_data_envelope(mut value: Value) -> Value {
    if value.get("choices").is_some() || value.get("output").is_some() {
        return value;
    }
    let inner = value
        .get("data")
        .filter(|d| {
            d.get("choices").is_some() || d.get("output").is_some() || d.get("error").is_some()
        })
        .cloned();
    if let Some(inner) = inner {
        value = inner;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_url_is_not_doubled() {
        assert_eq!(ClineProvider::chat_url(), "https://api.cline.bot/api/v1/chat/completions");
    }

    #[test]
    fn oauth_bearer_is_workos_prefixed() {
        let data = json!({"accessToken": "tok"});
        assert_eq!(ClineProvider::bearer(&data, "tok"), "Bearer workos:tok");
    }

    #[test]
    fn api_key_bearer_is_bare() {
        let data = json!({"accessToken": "tok", "credentialKind": "api_key"});
        assert_eq!(ClineProvider::bearer(&data, "tok"), "Bearer tok");
    }

    #[test]
    fn system_turn_is_injected_when_absent() {
        let req = sample_request(false);
        let body = ClineProvider::build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
    }

    #[test]
    fn empty_system_turn_is_repaired() {
        let mut req = sample_request(false);
        req.messages[0].role = "system".into();
        req.messages[0].content = Value::String("".into());
        let body = ClineProvider::build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["content"], "You are a helpful assistant.");
    }

    #[test]
    fn tool_schema_gets_required() {
        let mut req = sample_request(false);
        req.tools = Some(json!([
            {"type":"function","function":{"name":"f","parameters":{"type":"object","properties":{"a":{"type":"string"}}}}}
        ]));
        let body = ClineProvider::build_body(&req);
        let tools = body["tools"].as_array().unwrap();
        assert!(tools[0]["function"]["parameters"]["required"].is_array());
    }

    #[test]
    fn used_is_derived_from_percent_and_ceiling() {
        let l = ClineUsageLimit {
            kind: Some("weekly".into()),
            percent_used: Some(40.0),
            resets_at: None,
            limit: Some(500.0),
            entitlement: None,
            total: None,
        };
        assert_eq!(l.used(), Some(200.0));
    }

    #[test]
    fn ceiling_prefers_limit_then_entitlement_then_total() {
        let l = ClineUsageLimit {
            kind: None,
            percent_used: None,
            resets_at: None,
            limit: None,
            entitlement: Some(7.0),
            total: Some(9.0),
        };
        assert_eq!(l.ceiling(), Some(7.0));
    }

    fn sample_request(stream: bool) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "cline/deepseek-v4-flash".into(),
            messages: vec![crate::openai::ChatMessage {
                role: "user".into(),
                content: Value::String("hi".into()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: Some(stream),
            temperature: None,
            max_tokens: None,
            top_p: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            extra: Value::Object(Default::default()),
        }
    }
}
