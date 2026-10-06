//! Antigravity provider — Google Cloud Code Assist
//! (`https://daily-cloudcode-pa.googleapis.com`).
//!
//! Ported from Cartethyia `src/providers/integrations/antigravity/`.
//!
//! The wire is Gemini-shaped (`contents` / `systemInstruction` /
//! `generationConfig`) nested inside an Antigravity agent envelope, POSTed to
//! two fixed RPC paths. The model id travels in the envelope, not the URL —
//! unlike real Gemini's `/models/{model}:{action}`.
//!
//! Three things are load-bearing and easy to get wrong:
//!
//! * **The User-Agent gates dispatch.** It must look like
//!   `antigravity/hub/<version> (aidev_client; os_type=...; arch=...; cl=...)`.
//!   Only the version actually gates; the rest are desktop fingerprints.
//! * **The account needs a project, but not one it owns.** `loadCodeAssist`
//!   plus free-tier `onboardUser` provision a `cloudaicompanionProject`
//!   automatically. Without it dispatch is refused with "You do not have a
//!   valid license of this product".
//! * **Google OAuth is loopback PKCE with a fixed redirect.** Google validates
//!   `redirect_uri` against the client allowlist at both authorize and token
//!   time, so the port is part of the client identity.
//!
//! Onboarding is refresh-token paste: the refresh endpoint needs no browser
//! state, so a pasted RT bootstraps the account.
//!
//! Account shape: `provider = "antigravity"`, `data = { accessToken,
//! refreshToken, expiresAt, projectId? }`.

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
use uuid::Uuid;

const PROD_HOST: &str = "https://daily-cloudcode-pa.googleapis.com";
const SANDBOX_HOST: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com";
const GENERATE_PATH: &str = "/v1internal:generateContent";
const STREAM_GENERATE_PATH: &str = "/v1internal:streamGenerateContent?alt=sse";
const FETCH_MODELS_PATH: &str = "/v1internal:fetchAvailableModels";
const LOAD_CODE_ASSIST_PATH: &str = "/v1internal:loadCodeAssist";
const ONBOARD_USER_PATH: &str = "/v1internal:onboardUser";
const QUOTA_SUMMARY_PATH: &str = "/v1internal:retrieveUserQuotaSummary";

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo?alt=json";
/// Environment variables carrying the Google OAuth client identity.
///
/// Deliberately NOT constants. The upstream client is a public installed-app
/// client, so the values are not confidential in the usual sense — but a
/// secret in source is a secret in git history forever, and hosting providers
/// block the push on sight. Supply them via `.env`.
pub const ENV_CLIENT_ID: &str = "MARIONETTE_ANTIGRAVITY_CLIENT_ID";
pub const ENV_CLIENT_SECRET: &str = "MARIONETTE_ANTIGRAVITY_CLIENT_SECRET";
/// Google validates this against the client allowlist at authorize AND token
/// time, so the port is part of the client identity.
pub const REDIRECT_URI: &str = "http://127.0.0.1:51121/oauth-callback";

pub const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];

/// Fallback when the live version scrape is unavailable.
const DEFAULT_HUB_VERSION: &str = "2.19.1";

pub const ANTIGRAVITY_PROVIDER: &str = "antigravity";

const REFRESH_LEAD_SECS: i64 = 300;

/// Identity prefix injected for claude and gemini-3 ids. Rides
/// `systemInstruction`, never a fabricated user turn: a fake history turn
/// pollutes multi-turn context.
const SYSTEM_INSTRUCTION: &str =
    "You are Antigravity, a powerful agentic AI coding assistant designed by the Google Deepmind team working on Advanced Agentic Coding.";

/// Static catalog fallback (mirrors Cartethyia `ANTIGRAVITY_MODELS`).
/// Cross-brand: Google, Anthropic and one GPT-OSS SKU ride the same wire.
pub const ANTIGRAVITY_MODELS: &[(&str, i64, i64, bool)] = &[
    ("claude-sonnet-4-6", 250_000, 64_000, true),
    ("claude-opus-4-6", 250_000, 64_000, true),
    ("gemini-3-flash", 1_048_576, 65_536, false),
    ("gemini-3.1-flash-image", 200_000, 64_000, false),
    ("gemini-3.1-pro", 1_048_576, 65_535, false),
    ("gemini-3.6-flash", 1_048_576, 65_536, false),
    ("gemini-3.7-flash", 1_048_576, 65_536, false),
    ("gemini-3.8-flash", 1_048_576, 65_536, false),
    ("gpt-oss-120b", 131_072, 32_768, false),
];

/// Models the catalog must not serve even if discovery reports them.
const DENYLIST: &[&str] = &["chat_20706", "chat_23310", "gemini-2.5-pro"];

#[derive(Debug, Clone, Deserialize)]
pub struct AvailableModel {
    #[serde(default)]
    #[serde(alias = "modelId")]
    pub id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default, rename = "displayName")]
    pub display_name2: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<i64>,
    #[serde(default, rename = "maxTokens")]
    pub max_tokens2: Option<i64>,
    #[serde(default, rename = "maxOutputTokens")]
    pub max_output_tokens: Option<i64>,
    #[serde(default)]
    pub supports_thinking: Option<bool>,
    #[serde(default, rename = "supportsThinking")]
    pub supports_thinking2: Option<bool>,
    #[serde(default)]
    pub is_internal: Option<bool>,
    #[serde(default, rename = "isInternal")]
    pub is_internal2: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct AvailableModelsPayload {
    #[serde(default)]
    models: Option<Value>,
}

pub struct AntigravityProvider {
    client: Client,
    /// Live-resolved hub version, else the fallback.
    hub_version: std::sync::RwLock<String>,
    /// Google OAuth client identity, read from the environment at startup.
    client_id: String,
    client_secret: String,
}

impl std::fmt::Debug for AntigravityProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AntigravityProvider").finish()
    }
}

impl AntigravityProvider {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
            hub_version: std::sync::RwLock::new(DEFAULT_HUB_VERSION.to_string()),
            client_id: std::env::var(ENV_CLIENT_ID).unwrap_or_default(),
            client_secret: std::env::var(ENV_CLIENT_SECRET).unwrap_or_default(),
        }
    }

    /// `true` when the OAuth client identity was supplied.
    ///
    /// Refresh cannot work without it, so this is checked before attempting a
    /// token exchange rather than sending a request that can only 400.
    pub fn oauth_configured(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty()
    }

    pub fn set_hub_version(&self, v: &str) {
        if let Ok(mut g) = self.hub_version.write() {
            *g = v.to_string();
        }
    }

    /// The dispatch gate: `antigravity/hub/<version> (aidev_client; ...)`.
    pub fn user_agent(&self) -> String {
        let v = self
            .hub_version
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|_| DEFAULT_HUB_VERSION.to_string());
        format!("antigravity/hub/{v} (aidev_client; os_type=darwin; arch=arm64; cl=963137146)")
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

    pub fn project_id_of(data: &Value) -> Option<String> {
        data.get("projectId")
            .or_else(|| data.get("project_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    /// Logical id -> wire id. The effort suffix lives on the wire id, not the
    /// URL, so a wrong mapping is a rejection rather than a degradation.
    pub fn wire_model_id(logical: &str) -> String {
        match logical {
            "claude-opus-4-6" => "claude-opus-4-6-thinking".into(),
            "gemini-3.1-pro" | "gemini-3.1-pro-high" => "gemini-pro-agent".into(),
            "gemini-3.6-flash" => "gemini-3.6-flash-low".into(),
            "gemini-3.7-flash" => "gemini-3.7-flash-low".into(),
            "gemini-3.8-flash" => "gemini-3.8-flash-low".into(),
            "gpt-oss-120b" => "gpt-oss-120b-medium".into(),
            other => other.to_string(),
        }
    }

    /// Strip effort suffixes so one logical id covers one effort family.
    pub fn collapse_variant(wire: &str) -> String {
        let suffixes = [
            "-extra-low", "-low", "-medium", "-high", "-tiered", "-agent",
        ];
        let mut out = wire.to_string();
        for s in suffixes {
            if let Some(stripped) = out.strip_suffix(s) {
                out = stripped.to_string();
                break;
            }
        }
        match out.as_str() {
            "gemini-pro" => "gemini-3.1-pro".into(),
            "claude-opus-4-6-thinking" => "claude-opus-4-6".into(),
            other => other.to_string(),
        }
    }

    /// Per-wire output ceiling. The backend answers 400 above these.
    fn max_output_tokens(wire: &str) -> Option<i64> {
        match wire {
            "claude-opus-4-6-thinking" | "claude-sonnet-4-6" => Some(64_000),
            "gemini-3-flash-agent" => Some(65_536),
            "gemini-3.1-pro-low" => Some(65_535),
            "gemini-pro-agent" => Some(65_535),
            _ => None,
        }
    }

    fn model_enum(wire: &str) -> Option<&'static str> {
        match wire {
            "gemini-3-flash-agent" => Some("MODEL_PLACEHOLDER_M132"),
            "gemini-3.1-pro-low" => Some("MODEL_PLACEHOLDER_M36"),
            "gemini-pro-agent" => Some("MODEL_PLACEHOLDER_M16"),
            _ => None,
        }
    }

    /// Thinking budget ladder. Applies only to claude and gemini-3 ids.
    fn thinking_budget(logical: &str, effort: Option<&str>) -> Option<i64> {
        let lower = logical.to_ascii_lowercase();
        if !lower.contains("claude") && !lower.contains("gemini-3") {
            return None;
        }
        Some(match effort.unwrap_or("high") {
            "low" => 1000,
            "medium" => 4000,
            "high" => 10_000,
            // 3.1-pro nudges one token above the default for reasons the
            // captured traffic shows but does not explain.
            _ if lower.contains("3.1-pro") => 10_001,
            _ => 10_000,
        })
    }

    /// Refresh: `grant_type=refresh_token` with the embedded client identity.
    ///
    /// No browser state, which is what makes refresh-token paste viable.
    async fn refresh_with(&self, refresh_token: &str) -> Result<Value, ProviderError> {
        if !self.oauth_configured() {
            return Err(ProviderError::AuthInvalid(format!(
                "antigravity OAuth client identity missing; set {ENV_CLIENT_ID} and {ENV_CLIENT_SECRET}"
            )));
        }
        let resp = self
            .client
            .post(TOKEN_URL)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(classify_http_status(status, &text));
        }
        serde_json::from_str::<Value>(&text).map_err(|e| ProviderError::Upstream {
            status,
            body: format!("bad token json: {e}"),
        })
    }

    /// Best-effort email lookup for the account label.
    async fn fetch_email(&self, token: &str) -> Option<String> {
        let resp = self
            .client
            .get(USERINFO_URL)
            .bearer_auth(token)
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: Value = resp.json().await.ok()?;
        v.get("email").and_then(|e| e.as_str()).map(|s| s.to_string())
    }

    /// Provision or read the `cloudaicompanionProject`.
    ///
    /// An account does not need its own GCP project, but it does need one;
    /// without it dispatch is refused. Failures are not fatal here so login
    /// never strands an account as stored-but-unusable.
    pub async fn discover_project(&self, token: &str) -> Option<String> {
        let metadata = json!({ "metadata": { "ideType": "ANTIGRAVITY" } });
        let resp = self
            .client
            .post(format!("{PROD_HOST}{LOAD_CODE_ASSIST_PATH}"))
            .bearer_auth(token)
            .header("content-type", "application/json")
            .header("user-agent", self.user_agent())
            .json(&metadata)
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: Value = resp.json().await.ok()?;
        if let Some(p) = extract_project(&v) {
            return Some(p);
        }
        // No currentTier: enroll in the free tier, then re-read.
        if v.get("currentTier").is_none() {
            let onboard = self
                .client
                .post(format!("{PROD_HOST}{ONBOARD_USER_PATH}"))
                .bearer_auth(token)
                .header("content-type", "application/json")
                .header("user-agent", self.user_agent())
                .json(&json!({
                    "tierId": "free-tier",
                    "metadata": { "ideType": "ANTIGRAVITY" }
                }))
                .send()
                .await
                .ok()?;
            if onboard.status().is_success() {
                let _: Value = onboard.json().await.ok()?;
                let again = self
                    .client
                    .post(format!("{PROD_HOST}{LOAD_CODE_ASSIST_PATH}"))
                    .bearer_auth(token)
                    .header("content-type", "application/json")
                    .header("user-agent", self.user_agent())
                    .json(&metadata)
                    .send()
                    .await
                    .ok()?;
                if again.status().is_success() {
                    let v2: Value = again.json().await.ok()?;
                    return extract_project(&v2);
                }
            }
        }
        None
    }

    /// Live catalog. Returns `None` on total failure so static routing holds.
    pub async fn fetch_available_models(&self, token: &str) -> Option<Vec<String>> {
        for host in [PROD_HOST, SANDBOX_HOST] {
            let resp = self
                .client
                .post(format!("{host}{FETCH_MODELS_PATH}"))
                .bearer_auth(token)
                .header("content-type", "application/json")
                .header("user-agent", self.user_agent())
                .json(&json!({}))
                .send()
                .await
                .ok()?;
            if !resp.status().is_success() {
                continue;
            }
            let payload: AvailableModelsPayload = resp.json().await.ok()?;
            if let Some(Value::Object(map)) = payload.models {
                let mut out = Vec::new();
                for (id, entry) in map {
                    if DENYLIST.contains(&id.as_str()) {
                        continue;
                    }
                    let internal = entry
                        .get("isInternal")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if internal {
                        continue;
                    }
                    out.push(id);
                }
                if !out.is_empty() {
                    return Some(out);
                }
            }
        }
        None
    }

    /// Translate a Chat request into the Gemini-shaped inner payload.
    fn build_gemini_request(req: &ChatCompletionRequest, logical: &str, wire: &str) -> Value {
        let mut contents = Vec::new();
        let mut system_parts: Vec<Value> = Vec::new();

        for m in &req.messages {
            let role = m.role.as_str();
            let text = match &m.content {
                Value::String(s) => s.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(""),
                other => other.to_string(),
            };
            match role {
                "system" | "developer" => {
                    if !text.trim().is_empty() {
                        system_parts.push(json!({ "text": text }));
                    }
                }
                "assistant" => {
                    contents.push(json!({ "role": "model", "parts": [{ "text": text }] }));
                }
                _ => {
                    contents.push(json!({ "role": "user", "parts": [{ "text": text }] }));
                }
            }
        }

        let lower = logical.to_ascii_lowercase();
        if lower.contains("claude") || lower.contains("gemini-3") {
            system_parts.insert(0, json!({ "text": SYSTEM_INSTRUCTION }));
        }

        let mut out = json!({ "contents": contents });
        if !system_parts.is_empty() {
            out["systemInstruction"] = json!({ "role": "user", "parts": system_parts });
        }

        if let Some(tools) = &req.tools {
            let mut decls = Vec::new();
            if let Some(arr) = tools.as_array() {
                for t in arr {
                    let name = t
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .or_else(|| t.get("name"))
                        .and_then(|v| v.as_str());
                    let desc = t
                        .get("function")
                        .and_then(|f| f.get("description"))
                        .or_else(|| t.get("description"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let params = t
                        .get("function")
                        .and_then(|f| f.get("parameters"))
                        .or_else(|| t.get("parameters"))
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                    if let Some(n) = name {
                        decls.push(json!({
                            "name": n,
                            "description": desc,
                            "parameters": sanitize_schema(params),
                        }));
                    }
                }
            }
            if !decls.is_empty() {
                out["tools"] = json!([{ "functionDeclarations": decls }]);
            }
        }

        let mut generation_config = json!({});
        if let Some(cap) = Self::max_output_tokens(wire) {
            generation_config["maxOutputTokens"] = json!(cap);
        } else if let Some(m) = req.max_tokens {
            generation_config["maxOutputTokens"] = json!(m);
        }
        let effort = req
            .extra
            .get("reasoning_effort")
            .and_then(|v| v.as_str())
            .or_else(|| req.extra.get("reasoningEffort").and_then(|v| v.as_str()));
        if let Some(budget) = Self::thinking_budget(logical, effort) {
            generation_config["thinkingConfig"] = json!({
                "includeThoughts": true,
                "thinkingBudget": budget,
            });
        }
        if generation_config.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            out["generationConfig"] = generation_config;
        }

        out
    }
}

/// Gemini accepts a restricted JSON-Schema key set; anything else is dropped
/// rather than sent, because the backend rejects unknown keys.
fn sanitize_schema(schema: Value) -> Value {
    const ALLOWED: &[&str] = &[
        "type", "properties", "required", "items", "enum", "description",
        "nullable", "format", "minimum", "maximum", "default", "anyOf",
    ];
    match schema {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if ALLOWED.contains(&k.as_str()) {
                    out.insert(k, sanitize_schema(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sanitize_schema).collect()),
        other => other,
    }
}

fn extract_project(v: &Value) -> Option<String> {
    let p = v.get("cloudaicompanionProject")?;
    match p {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Object(o) => o
            .get("id")
            .and_then(|i| i.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string()),
        _ => None,
    }
}

#[async_trait]
impl Provider for AntigravityProvider {
    fn id(&self) -> &'static str {
        ANTIGRAVITY_PROVIDER
    }

    async fn ensure_fresh_auth(&self, account: &mut Account) -> Result<(), ProviderError> {
        let data = account.data_json();
        let needs_refresh = match data.get("expiresAt").and_then(|v| v.as_str()) {
            Some(exp) => chrono::DateTime::parse_from_rfc3339(exp)
                .map(|t| t.timestamp() - Utc::now().timestamp() < REFRESH_LEAD_SECS)
                .unwrap_or(true),
            None => true,
        };

        if !needs_refresh && Self::access_token_of(&data).is_some() && Self::project_id_of(&data).is_some() {
            account.last_error = None;
            return Ok(());
        }
        if !needs_refresh && Self::access_token_of(&data).is_some() {
            // Token is fine but the project is unknown; fill it best-effort.
            if let Some(token) = Self::access_token_of(&data) {
                if let Some(project) = self.discover_project(&token).await {
                    let mut data = data;
                    data["projectId"] = json!(project);
                    account.set_data_json(&data);
                }
            }
            account.last_error = None;
            return Ok(());
        }

        let refresh_token = Self::refresh_token_of(&data).ok_or(ProviderError::AuthExpired)?;
        let refreshed = self.refresh_with(&refresh_token).await?;

        let access = refreshed
            .get("access_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| ProviderError::AuthInvalid("refresh omitted access_token".into()))?;

        let mut data = data;
        data["accessToken"] = json!(access);
        if let Some(rt) = refreshed.get("refresh_token").and_then(|v| v.as_str()) {
            data["refreshToken"] = json!(rt.to_string());
        }
        if let Some(secs) = refreshed.get("expires_in").and_then(|v| v.as_i64()) {
            data["expiresAt"] = json!((Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339());
        }

        // Project discovery is best-effort and must not fail the refresh.
        if let Some(project) = self.discover_project(&access).await {
            data["projectId"] = json!(project);
        }
        if account.email.is_none() {
            account.email = self.fetch_email(&access).await;
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
        let project = Self::project_id_of(&data);

        let logical = req.upstream_model().trim_start_matches("ag/").to_string();
        let wire = Self::wire_model_id(&logical);
        let mut request_payload = Self::build_gemini_request(req, &logical, &wire);

        let agent_id = Uuid::new_v4().to_string();
        let trajectory_id = Uuid::new_v4().to_string();
        let mut labels = json!({
            "trajectory_id": trajectory_id,
            "last_step_index": "0",
            "used_claude": logical.to_ascii_lowercase().contains("claude").to_string(),
            "used_claude_conservative": logical.to_ascii_lowercase().contains("claude").to_string(),
        });
        if let Some(e) = Self::model_enum(&wire) {
            labels["model_enum"] = json!(e);
        }
        // A negative signed integer string, as the captured traffic sends it.
        let session_id = format!("-{}", &Uuid::new_v4().simple().to_string()[..16]);
        request_payload["labels"] = labels;
        request_payload["sessionId"] = json!(session_id);
        if logical.to_ascii_lowercase().contains("claude") {
            request_payload["toolConfig"] =
                json!({ "functionCallingConfig": { "mode": "VALIDATED" } });
        }

        let mut envelope = json!({
            "requestId": format!("agent/{agent_id}/{}/{trajectory_id}/1", Utc::now().timestamp_millis()),
            "model": wire,
            "userAgent": "antigravity",
            "requestType": "agent",
            "request": request_payload,
        });
        if let Some(p) = project {
            envelope["project"] = json!(p);
        }

        let stream = req.stream_enabled();
        let path = if stream { STREAM_GENERATE_PATH } else { GENERATE_PATH };
        let url = format!("{PROD_HOST}{path}");

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .header("content-type", "application/json")
            .header(
                "accept",
                if stream {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            )
            // The User-Agent gates dispatch; see module docs.
            .header("user-agent", self.user_agent())
            .json(&envelope)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();

        // Retry once against the sandbox twin on rate limit or server error
        // only. Auth and client errors never fail over.
        let resp = if status == 429 || status >= 500 {
            let retry = self
                .client
                .post(format!("{SANDBOX_HOST}{path}"))
                .bearer_auth(&token)
                .header("content-type", "application/json")
                .header(
                    "accept",
                    if stream {
                        "text/event-stream"
                    } else {
                        "application/json"
                    },
                )
                .header("user-agent", self.user_agent())
                .json(&envelope)
                .send()
                .await
                .map_err(|e| ProviderError::Transport(e.to_string()))?;
            // Keep whichever answered; if the sandbox also failed, surface the
            // sandbox response so the error reflects the last attempt made.
            retry
        } else {
            resp
        };

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_http_status(status, &text));
        }

        let req_model = req.model.clone();

        if stream {
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
                    buffer.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].trim_end_matches('\r').to_string();
                        buffer = buffer[pos + 1..].to_string();
                        let Some(rest) = line.strip_prefix("data:") else {
                            continue;
                        };
                        let payload = rest.trim();
                        if let Ok(v) = serde_json::from_str::<Value>(payload) {
                            if let Some(u) = v.get("usageMetadata") {
                                if let Some(p) = u.get("promptTokenCount").and_then(|x| x.as_i64()) {
                                    prompt_tokens = p;
                                }
                                if let Some(c) = u.get("candidatesTokenCount").and_then(|x| x.as_i64()) {
                                    completion_tokens = c;
                                }
                                if let Some(t) = u.get("totalTokenCount").and_then(|x| x.as_i64()) {
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
        let project = Self::project_id_of(&data);

        for host in [PROD_HOST, SANDBOX_HOST] {
            let mut req = self
                .client
                .post(format!("{host}{QUOTA_SUMMARY_PATH}"))
                .bearer_auth(&token)
                .header("content-type", "application/json")
                // retrieveUserQuotaSummary answers 403 without this pair, even
                // when the same token reaches fetchAvailableModels.
                .header("x-client-name", "antigravity")
                .header("x-client-version", self.hub_version.read().map(|g| g.clone()).unwrap_or_default())
                .header("user-agent", self.user_agent());
            req = match &project {
                Some(p) => req.json(&json!({ "project": p })),
                None => req.json(&json!({})),
            };
            let Ok(resp) = req.send().await else { continue };
            if !resp.status().is_success() {
                continue;
            }
            let Ok(v) = resp.json::<Value>().await else { continue };
            let mut data = data.clone();
            if let Some(windows) = v.get("quotaSummary").and_then(|q| q.get("groups")) {
                data["quotaWindows"] = windows.clone();
            }
            if let Some(tier) = v.get("tier").or_else(|| v.get("plan")) {
                data["plan"] = tier.clone();
            }
            account.set_data_json(&data);
            return Ok(());
        }
        // A failed summary downgrades rather than fails the sync.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::ChatMessage;

    #[test]
    fn wire_model_id_maps_effort_families() {
        assert_eq!(AntigravityProvider::wire_model_id("claude-opus-4-6"), "claude-opus-4-6-thinking");
        assert_eq!(AntigravityProvider::wire_model_id("gemini-3.1-pro"), "gemini-pro-agent");
        assert_eq!(AntigravityProvider::wire_model_id("gemini-3.6-flash"), "gemini-3.6-flash-low");
        assert_eq!(AntigravityProvider::wire_model_id("gpt-oss-120b"), "gpt-oss-120b-medium");
        // pass-through
        assert_eq!(AntigravityProvider::wire_model_id("claude-sonnet-4-6"), "claude-sonnet-4-6");
    }

    #[test]
    fn collapse_variant_strips_effort_suffixes() {
        assert_eq!(AntigravityProvider::collapse_variant("gemini-3.6-flash-low"), "gemini-3.6-flash");
        assert_eq!(AntigravityProvider::collapse_variant("gemini-pro-agent"), "gemini-3.1-pro");
        assert_eq!(AntigravityProvider::collapse_variant("claude-opus-4-6-thinking"), "claude-opus-4-6");
    }

    #[test]
    fn claude_output_ceiling_is_enforced() {
        assert_eq!(AntigravityProvider::max_output_tokens("claude-sonnet-4-6"), Some(64_000));
        assert_eq!(AntigravityProvider::max_output_tokens("claude-opus-4-6-thinking"), Some(64_000));
    }

    #[test]
    fn thinking_budget_only_for_claude_and_gemini3() {
        assert_eq!(AntigravityProvider::thinking_budget("claude-sonnet-4-6", Some("low")), Some(1000));
        assert_eq!(AntigravityProvider::thinking_budget("gemini-3-flash", Some("medium")), Some(4000));
        assert_eq!(AntigravityProvider::thinking_budget("gpt-oss-120b", Some("high")), None);
    }

    /// The client identity must come from the environment. A literal here
    /// would be a secret in git history, which is what blocked the push.
    #[test]
    fn oauth_identity_is_not_embedded() {
        let src = include_str!("antigravity.rs");
        // Needles are assembled at runtime so this test's own text is not a
        // match for itself.
        let secret_prefix = ["GOCS", "PX"].concat();
        let client_id_suffix = ["apps.googleusercontent", ".com"].concat();
        assert!(
            !src.contains(&secret_prefix),
            "client secret literal must not appear in source"
        );
        assert!(
            !src.contains(&client_id_suffix),
            "client id literal must not appear in source"
        );
        assert_eq!(ENV_CLIENT_ID, "MARIONETTE_ANTIGRAVITY_CLIENT_ID");
    }

    #[test]
    fn oauth_configured_requires_both_halves() {
        // A provider built without the env vars must report itself unconfigured
        // rather than attempting an exchange that can only fail.
        let p = AntigravityProvider::new();
        let expected = std::env::var(ENV_CLIENT_ID).is_ok()
            && std::env::var(ENV_CLIENT_SECRET).is_ok();
        assert_eq!(p.oauth_configured(), expected);
    }

    #[test]
    fn user_agent_is_the_dispatch_gate() {
        let p = AntigravityProvider::new();
        let ua = p.user_agent();
        assert!(ua.starts_with("antigravity/hub/"), "{ua}");
        assert!(ua.contains("aidev_client"), "{ua}");
    }

    #[test]
    fn schema_sanitize_drops_unknown_keys() {
        let schema = json!({
            "type": "object",
            "properties": { "a": { "type": "string" } },
            "additionalProperties": false,
        });
        let out = sanitize_schema(schema);
        assert!(out.get("type").is_some());
        assert!(out.get("additionalProperties").is_none());
    }

    #[test]
    fn system_instruction_rides_its_own_field() {
        let req = sample_request();
        let out = AntigravityProvider::build_gemini_request(&req, "claude-sonnet-4-6", "claude-sonnet-4-6");
        let si = out.get("systemInstruction").expect("systemInstruction present");
        let text = si["parts"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("You are Antigravity"));
        // The identity must not appear as a conversation turn.
        let contents = out["contents"].as_array().unwrap();
        assert!(contents.iter().all(|c| c["role"] != "user" || !c["parts"][0]["text"].as_str().unwrap().starts_with("You are Antigravity")));
    }

    #[test]
    fn denylist_blocks_withdrawn_models() {
        assert!(DENYLIST.contains(&"gemini-2.5-pro"));
    }

    fn sample_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "ag/claude-sonnet-4-6".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: Value::String("hi".into()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            stream: Some(false),
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
