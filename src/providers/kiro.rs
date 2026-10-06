//! Kiro provider — AWS CodeWhisperer / Amazon Q
//! (`https://q.{region}.amazonaws.com/generateAssistantResponse`).
//!
//! Ported from Cartethyia `src/providers/integrations/kiro/`.
//!
//! This is the least conventional of the three new providers, and three facts
//! about it are load-bearing:
//!
//! * **The response is AWS binary EventStream, not SSE.** Decoded by
//!   [`crate::providers::kiro_event_stream`], which verifies both CRCs.
//! * **The request is not chat-shaped.** It is a `conversationState` envelope
//!   with strict user/assistant alternation, sanitized tool names and repaired
//!   JSON schemas. Malformed shapes are refused locally because a 400 upstream
//!   cools the account.
//! * **A stable machine ID is mandatory.** The upstream binds a credential to
//!   the device it was first seen from; a credential presenting a different
//!   device per request reads as account sharing.
//!
//! Auth has seven methods across three flow families. The browser-dependent
//! families (device flow, social PKCE, Builder ID) require operator
//! interaction and are NOT ported; the four paste-only import paths are, since
//! they match Marionette's import model.
//!
//! Account shape: `provider = "kiro"`, `data = { authMethod, region,
//! accessToken, refreshToken, expiresAt, profileArn?, machineId }`.

use super::kiro_event_stream::decode_messages;
use super::{classify_http_status, ChatOutcome, Provider, StreamUsage};
use crate::db::Account;
use crate::error::ProviderError;
use crate::openai::ChatCompletionRequest;
use crate::providers::client_version::{ClientVersion, Extract, VersionSource};
use async_trait::async_trait;
use axum::body::Body;
use axum::response::Response;
use chrono::Utc;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use uuid::Uuid;

/// The one generation surface. There is no fallback host: replaying a refusal
/// onto `codewhisperer.*` or a path-style gateway is credential probing.
fn generate_url(region: &str) -> String {
    format!("https://q.{region}.amazonaws.com/generateAssistantResponse")
}

fn list_models_url(region: &str) -> String {
    format!("https://q.{region}.amazonaws.com/ListAvailableModels?origin=AI_EDITOR")
}

fn usage_limits_url(region: &str) -> String {
    format!(
        "https://q.{region}.amazonaws.com/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST"
    )
}

const OIDC_HOST_TEMPLATE: &str = "https://oidc.{region}.amazonaws.com";
const SOCIAL_HOST: &str = "https://prod.us-east-1.auth.desktop.kiro.dev";

pub const KIRO_PROVIDER: &str = "kiro";

/// The IDE build stamped into `KiroIDE-<version>-<machine_id>` user-agents.
///
/// Upstream gates on this the same way grok does, so it is resolved at runtime
/// rather than pinned in the binary. The download page publishes the current
/// version; the pinned fallback keeps dispatch working if it is unreachable.
static CLIENT_VERSION: ClientVersion = ClientVersion::new(
    "kiro",
    "1.2.37",
    &[VersionSource {
        url: "https://kiro.dev/downloads/",
        extract: Extract::AfterField("currentVersion"),
    }],
);

/// The resolver behind the KiroIDE user-agent, for the version worker.
pub fn client_version() -> &'static ClientVersion {
    &CLIENT_VERSION
}

/// `KiroIDE-<version>-<machine_id>`.
fn kiro_ide_user_agent(machine_id: &str) -> String {
    format!("KiroIDE-{}-{machine_id}", CLIENT_VERSION.get())
}

const REFRESH_LEAD_SECS: i64 = 300;
const DEFAULT_REGION: &str = "us-east-1";
/// Used to turn `contextUsagePercentage` into a token figure.
const DEFAULT_CONTEXT_WINDOW: i64 = 200_000;

/// Static catalog (mirrors Cartethyia `KIRO_MODELS`).
pub const KIRO_MODELS: &[(&str, i64, i64)] = &[
    ("claude-opus-5", 1_000_000, 128_000),
    ("claude-opus-4.8", 1_000_000, 128_000),
    ("claude-opus-4.7", 1_000_000, 128_000),
    ("claude-opus-4.5", 200_000, 64_000),
    ("claude-haiku-4.5", 200_000, 64_000),
    ("claude-sonnet-5", 1_000_000, 128_000),
    ("claude-sonnet-4.5", 1_000_000, 64_000),
    ("gpt-5.6-sol", 272_000, 128_000),
    ("gpt-5.6-terra", 272_000, 128_000),
    ("gpt-5.6-luna", 272_000, 128_000),
    ("deepseek-3.2", 128_000, 64_000),
    ("qwen3-coder-next", 1_000_000, 64_000),
    ("glm-5", 204_800, 131_000),
];

/// Default profile ARNs. AWS SSO OIDC never reports a profile, so the family
/// default is used; credential-scoped methods send none.
const DEFAULT_PROFILE_ARN_BUILDER_ID: &str =
    "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX";
const DEFAULT_PROFILE_ARN_SOCIAL: &str =
    "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK";

/// Auth methods that reach the same subscription. Only the paste-only ones are
/// ported; the browser families need operator interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KiroAuthMethod {
    /// Refresh token pasted from a Kiro social session.
    Imported,
    /// Identity Center refresh token (needs the issuing client id/secret).
    Idc,
    /// Exported CLIProxyAPI JSON (Microsoft token endpoint).
    ExternalIdp,
    /// Static API key.
    ApiKey,
}

impl KiroAuthMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Imported => "imported",
            Self::Idc => "idc",
            Self::ExternalIdp => "external_idp",
            Self::ApiKey => "api_key",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "imported" => Some(Self::Imported),
            "idc" => Some(Self::Idc),
            "external_idp" | "externalidp" => Some(Self::ExternalIdp),
            "api_key" | "apikey" => Some(Self::ApiKey),
            _ => None,
        }
    }

    /// Credential-scoped methods present no profile at all.
    pub fn default_profile_arn(&self) -> &'static str {
        match self {
            Self::ApiKey | Self::ExternalIdp => "",
            Self::Imported => DEFAULT_PROFILE_ARN_SOCIAL,
            Self::Idc => DEFAULT_PROFILE_ARN_BUILDER_ID,
        }
    }

    pub fn token_type_header(&self) -> Option<&'static str> {
        match self {
            Self::ApiKey => Some("API_KEY"),
            Self::ExternalIdp => Some("EXTERNAL_IDP"),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Machine identity
// ---------------------------------------------------------------------------

/// Prefix the observed client hashes for an OAuth (refresh-token) credential.
const OAUTH_SALT: &str = "KotlinNativeAPI/";
/// Prefix for an API-key credential.
const API_KEY_SALT: &str = "KiroAPIKey/";
/// Prefix for the fallback when neither material is available.
const FALLBACK_SALT: &str = "KiroFallback/";
const MACHINE_ID_LENGTH: usize = 64;

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// A 64-hex value is taken as-is; a 32-hex value (a bare UUID without dashes)
/// is doubled, which is the shape the upstream accepts for that input.
/// Anything else is rejected rather than sent: a malformed device id is itself
/// a signal.
pub fn normalize_machine_id(raw: &str) -> Option<String> {
    let value = raw.trim().to_ascii_lowercase();
    let is_hex = !value.is_empty() && value.chars().all(|c| c.is_ascii_hexdigit());
    if !is_hex {
        return None;
    }
    if value.len() == MACHINE_ID_LENGTH {
        return Some(value);
    }
    if value.len() == MACHINE_ID_LENGTH / 2 {
        return Some(format!("{value}{value}"));
    }
    None
}

pub fn derive_oauth_machine_id(refresh_token: &str) -> String {
    sha256_hex(&format!("{OAUTH_SALT}{refresh_token}"))
}

pub fn derive_api_key_machine_id(api_key: &str) -> String {
    sha256_hex(&format!("{API_KEY_SALT}{api_key}"))
}

pub fn fallback_machine_id(account_id: &str) -> String {
    sha256_hex(&format!("{FALLBACK_SALT}{account_id}"))
}

/// Resolve the machine id for an account.
///
/// The frozen value always wins. This is load-bearing: a refresh rotates the
/// refresh token, so re-deriving at refresh time would move the account to a
/// new device on every refresh. Dispatch only ever sees the access token,
/// which is also replaced on every refresh, so it must never derive from that
/// either.
pub fn machine_id_for(data: &Value, account_id: &str) -> String {
    if let Some(frozen) = data
        .get("machineId")
        .and_then(|v| v.as_str())
        .and_then(normalize_machine_id)
    {
        return frozen;
    }
    // Never a constant and never random: a constant makes every account in
    // every deployment claim one machine, random makes one account move
    // devices every call. Both are worse than a stable per-account fallback.
    fallback_machine_id(account_id)
}

// ---------------------------------------------------------------------------
// Request encoding
// ---------------------------------------------------------------------------

/// Tool names are restricted to `[a-zA-Z0-9_-]` and 64 code points. Over-long
/// names keep a readable prefix plus a hash of the whole original, so two
/// tools differing only past the limit do not collide.
pub fn wire_tool_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    if sanitized.chars().count() <= 64 {
        return sanitized;
    }
    let hash = sha256_hex(name);
    let prefix: String = sanitized.chars().take(51).collect();
    format!("{prefix}_{}", &hash[..12])
}

/// Repair a JSON schema the way the upstream accepts it: drop
/// `additionalProperties`, force the root to an object with properties, and
/// remove an empty or invalid `required`.
fn normalize_json_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if k == "additionalProperties" {
                    continue;
                }
                if k == "required" {
                    if let Some(arr) = v.as_array() {
                        if arr.is_empty() || arr.iter().any(|x| !x.is_string()) {
                            continue;
                        }
                    } else {
                        continue;
                    }
                }
                out.insert(k.clone(), normalize_json_schema(v));
            }
            // The root must be an object with properties.
            if out.contains_key("properties") && !out.contains_key("type") {
                out.insert("type".into(), json!("object"));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize_json_schema).collect()),
        other => other.clone(),
    }
}

/// Enforce strict user/assistant alternation. Adjacent same-role turns are
/// MERGED, not refused; a trailing assistant turn gets a synthetic "continue".
fn split_conversation(req: &ChatCompletionRequest) -> (Vec<Value>, Option<String>, String) {
    let mut history: Vec<Value> = Vec::new();
    let mut system_parts: Vec<String> = Vec::new();
    let mut pending_user: Option<String> = None;
    let mut pending_assistant: Option<String> = None;

    fn text_of(content: &Value) -> String {
        match content {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(""),
            other => other.to_string(),
        }
    }

    for m in &req.messages {
        let text = text_of(&m.content);
        match m.role.as_str() {
            "system" | "developer" => {
                if !text.trim().is_empty() {
                    system_parts.push(text);
                }
            }
            "assistant" => {
                if let Some(u) = pending_user.take() {
                    history.push(json!({ "userInputMessage": {
                        "content": u, "origin": "AI_EDITOR", "modelId": req.upstream_model(),
                    }}));
                }
                match pending_assistant.take() {
                    Some(prev) => pending_assistant = Some(format!("{prev}\n{text}")),
                    None => pending_assistant = Some(text),
                }
            }
            _ => {
                if let Some(a) = pending_assistant.take() {
                    history.push(json!({ "assistantResponseMessage": { "content": a }}));
                }
                match pending_user.take() {
                    Some(prev) => pending_user = Some(format!("{prev}\n{text}")),
                    None => pending_user = Some(text),
                }
            }
        }
    }

    // The final turn must be a user turn; an assistant tail gets a synthetic
    // continue rather than being sent as-is.
    let current = match (pending_user.take(), pending_assistant.take()) {
        (Some(u), None) => u,
        (None, Some(_)) => "continue".to_string(),
        (Some(u), Some(a)) => {
            history.push(json!({ "assistantResponseMessage": { "content": a }}));
            u
        }
        (None, None) => "...".to_string(),
    };

    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n"))
    };
    (history, system, current)
}

/// Build the upstream body.
///
/// There is no top-level system prompt: system/instructions text is prefixed
/// onto the opening user turn.
fn build_wire_request(
    req: &ChatCompletionRequest,
    upstream_model: &str,
    profile_arn: &str,
) -> Value {
    let (history, system, current) = split_conversation(req);
    let model_id = upstream_model.to_string();

    // There is no top-level system prompt: system/instructions text is
    // prefixed onto the opening user turn.
    let user_content = match system {
        Some(s) => format!("{s}\n\n{current}"),
        None => current,
    };

    let has_tools = req.tools.as_ref().and_then(|t| t.as_array()).map(|a| !a.is_empty()).unwrap_or(false);

    let mut current_message = json!({
        "content": user_content,
        "modelId": model_id,
        "origin": "AI_EDITOR",
    });

    if has_tools {
        let mut tools_out = Vec::new();
        if let Some(arr) = req.tools.as_ref().and_then(|t| t.as_array()) {
            for t in arr {
                let name = t
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .or_else(|| t.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
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
                    .unwrap_or_else(|| json!({}));
                if !name.is_empty() {
                    tools_out.push(json!({
                        "toolSpecification": {
                            "name": wire_tool_name(name),
                            "description": desc,
                            "inputSchema": { "json": normalize_json_schema(&params) },
                        }
                    }));
                }
            }
        }
        current_message["userInputMessageContext"] =
            json!({ "tools": tools_out, "toolResults": [] });
    }

    let payload = json!({
        "conversationState": {
            "agentTaskType": if has_tools { "spectask" } else { "vibe" },
            "agentContinuationId": Uuid::new_v4().to_string(),
            "chatTriggerType": "MANUAL",
            "conversationId": Uuid::new_v4().to_string(),
            "currentMessage": { "userInputMessage": current_message },
            "history": history,
        }
    });

    if profile_arn.is_empty() {
        payload
    } else {
        let mut payload = payload;
        payload["profileArn"] = json!(profile_arn);
        payload
    }
}

/// Thinking budget ladder. Requested by a text marker for models that take no
/// structured effort field.
const THINKING_BUDGET_MAX: i64 = 32_000;

fn thinking_budget(effort: Option<&str>) -> i64 {
    match effort.unwrap_or("high") {
        "minimal" => 512,
        "low" => 1024,
        "medium" => 8192,
        "high" => 16_000,
        _ => THINKING_BUDGET_MAX,
    }
}

/// Which structured effort field this model takes. A wrong schema is a
/// rejection, not a degradation, so this never defaults.
fn effort_path(model_id: &str) -> &'static str {
    let lower = model_id.to_ascii_lowercase();
    if lower.starts_with("gpt-5.6") {
        "reasoning"
    } else if let Some(rest) = lower.strip_prefix("claude-") {
        // The version sits after the family name, e.g. claude-opus-4.7 has
        // major 4 minor 7. Claude 4.5 and older take no structured field.
        let mut nums: Vec<f64> = Vec::new();
        for part in rest.split('-') {
            for piece in part.split('.') {
                if let Ok(n) = piece.parse::<f64>() {
                    nums.push(n);
                }
            }
        }
        nums.retain(|n| *n < 1000.0);
        if nums.len() >= 2 {
            let major = nums[nums.len() - 2];
            let minor = nums[nums.len() - 1];
            if major >= 4.0 && minor > 5.0 {
                return "output_config";
            }
        }
        "none"
    } else {
        "none"
    }
}

fn context_window(model_id: &str) -> i64 {
    let lower = model_id.to_ascii_lowercase();
    for (id, ctx, _out) in KIRO_MODELS {
        if lower == id.to_ascii_lowercase() {
            return *ctx;
        }
    }
    // A wrong window beats no figure: every Anthropic row shares one.
    DEFAULT_CONTEXT_WINDOW
}

pub struct KiroProvider {
    client: Client,
}

impl std::fmt::Debug for KiroProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KiroProvider").finish()
    }
}

impl Default for KiroProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl KiroProvider {
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

    /// Region is validated against the AWS pattern; anything else falls back
    /// rather than being interpolated into a URL.
    pub fn region_of(data: &Value) -> String {
        let candidate = data
            .get("region")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_REGION)
            .trim();
        let valid = {
            let parts: Vec<&str> = candidate.split('-').collect();
            parts.len() >= 3
                && parts[0].chars().all(|c| c.is_ascii_lowercase())
                && parts[1].chars().all(|c| c.is_ascii_lowercase())
                && parts[2].chars().all(|c| c.is_ascii_digit())
        };
        if valid {
            candidate.to_string()
        } else {
            DEFAULT_REGION.to_string()
        }
    }

    pub fn auth_method_of(data: &Value) -> KiroAuthMethod {
        data.get("authMethod")
            .or_else(|| data.get("auth_method"))
            .and_then(|v| v.as_str())
            .and_then(KiroAuthMethod::parse)
            .unwrap_or(KiroAuthMethod::Imported)
    }

    fn profile_arn_of(data: &Value) -> String {
        let explicit = data
            .get("profileArn")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        match explicit {
            Some(p) => p.to_string(),
            None => Self::auth_method_of(data).default_profile_arn().to_string(),
        }
    }

    fn oidc_host(region: &str) -> String {
        OIDC_HOST_TEMPLATE.replace("{region}", region)
    }

    fn dispatch_headers(
        &self,
        data: &Value,
        token: &str,
        machine_id: &str,
    ) -> Vec<(String, String)> {
        let method = Self::auth_method_of(data);
        let mut out = vec![
            ("content-type".into(), "application/json".into()),
            ("accept".into(), "application/vnd.amazon.eventstream".into()),
            ("connection".into(), "close".into()),
            ("x-amzn-codewhisperer-optout".into(), "true".into()),
            ("x-amzn-kiro-agent-mode".into(), "spec".into()),
            ("amz-sdk-request".into(), "attempt=1; max=3".into()),
            ("amz-sdk-invocation-id".into(), Uuid::new_v4().to_string()),
            ("user-agent".into(), kiro_ide_user_agent(machine_id)),
            ("x-amz-user-agent".into(), kiro_ide_user_agent(machine_id)),
            ("authorization".into(), format!("Bearer {token}")),
        ];
        // A blank machine id advertises a device with no identity, a shape no
        // real client produces — so it is never sent.
        if !machine_id.is_empty() {
            if let Some(tt) = method.token_type_header() {
                out.push(("tokentype".into(), tt.into()));
            }
        }
        out
    }

    /// Refresh for the two OIDC families (idc) via the AWS SSO OIDC endpoint.
    async fn refresh_oidc(&self, data: &Value, refresh_token: &str) -> Result<Value, ProviderError> {
        let region = Self::region_of(data);
        let client_id = data
            .get("clientId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::AuthInvalid("idc: missing clientId".into()))?;
        let client_secret = data
            .get("clientSecret")
            .or_else(|| data.get("client_secret"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::AuthInvalid("idc: missing clientSecret".into()))?;

        let resp = self
            .client
            .post(format!("{}/token", Self::oidc_host(&region)))
            .header("content-type", "application/json")
            .json(&json!({
                "clientId": client_id,
                "clientSecret": client_secret,
                "grantType": "refresh_token",
                "refreshToken": refresh_token,
            }))
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

    /// Refresh for the social/imported family.
    async fn refresh_social(&self, refresh_token: &str) -> Result<Value, ProviderError> {
        let resp = self
            .client
            .post(format!("{SOCIAL_HOST}/refreshToken"))
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .header("connection", "close")
            .json(&json!({ "refreshToken": refresh_token }))
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

    /// Refresh for external_idp: POST form-encoded to the stored Microsoft
    /// endpoint. The endpoint is validated on import because this posts the
    /// account's refresh token to whatever the JSON names.
    async fn refresh_external_idp(&self, data: &Value, refresh_token: &str) -> Result<Value, ProviderError> {
        let endpoint = data
            .get("tokenEndpoint")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::AuthInvalid("external_idp: missing tokenEndpoint".into()))?;
        if !is_allowed_idp_endpoint(endpoint) {
            return Err(ProviderError::AuthInvalid(
                "external_idp: tokenEndpoint is not an allowed Microsoft host".into(),
            ));
        }
        let client_id = data.get("clientId").and_then(|v| v.as_str()).unwrap_or("");
        let scope = data.get("scope").and_then(|v| v.as_str()).unwrap_or("");

        let resp = self
            .client
            .post(endpoint)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("refresh_token", refresh_token),
                ("scope", scope),
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

    /// Validate an API key against the catalog. A bearer-only profile lookup
    /// answers 200 with an empty list for an arbitrary key, so it proves
    /// nothing — the catalog is the surface inference actually uses.
    pub async fn validate_api_key(&self, region: &str, api_key: &str) -> Result<bool, ProviderError> {
        let resp = self
            .client
            .get(list_models_url(region))
            .header("authorization", format!("Bearer {api_key}"))
            .header("accept", "application/json")
            .header("connection", "close")
            .header("tokentype", "API_KEY")
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        if !resp.status().is_success() {
            return Ok(false);
        }
        let v: Value = resp.json().await.unwrap_or(json!({}));
        let empty = v
            .get("models")
            .and_then(|m| m.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true);
        Ok(!empty)
    }

    pub async fn fetch_models(&self, data: &Value, token: &str) -> Option<Vec<String>> {
        let region = Self::region_of(data);
        let mut req = self
            .client
            .get(list_models_url(&region))
            .header("authorization", format!("Bearer {token}"))
            .header("accept", "application/json")
            .header("connection", "close")
            .header("x-amzn-kiro-agent-mode", "vibe")
            .header("x-amzn-codewhisperer-optout", "true")
            .header("amz-sdk-request", "attempt=1; max=1");
        if let Some(arn) = data.get("profileArn").and_then(|v| v.as_str()) {
            if !arn.trim().is_empty() {
                req = req.query(&[("profileArn", arn)]);
            }
        }
        let Ok(resp) = req.send().await else { return None };
        if !resp.status().is_success() {
            return None;
        }
        let Ok(v) = resp.json::<Value>().await else { return None };
        let arr = v.get("models").and_then(|m| m.as_array())?;
        let mut out = Vec::new();
        for row in arr {
            if let Some(id) = row.get("modelId").or_else(|| row.get("id")).and_then(|x| x.as_str()) {
                if !out.iter().any(|x| x == id) {
                    out.push(id.to_string());
                }
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }
}

/// Only Microsoft login hosts are accepted as an external_idp token endpoint.
pub fn is_allowed_idp_endpoint(endpoint: &str) -> bool {
    let lower = endpoint.trim().to_ascii_lowercase();
    if !lower.starts_with("https://") {
        return false;
    }
    let host = lower
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or("");
    matches!(
        host,
        "login.microsoftonline.com" | "login.microsoft.com" | "login.windows.net"
    )
}

#[async_trait]
impl Provider for KiroProvider {
    fn id(&self) -> &'static str {
        KIRO_PROVIDER
    }

    async fn ensure_fresh_auth(&self, account: &mut Account) -> Result<(), ProviderError> {
        let data = account.data_json();
        let method = Self::auth_method_of(&data);

        // API keys are static: no refresh, no expiry.
        if method == KiroAuthMethod::ApiKey {
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

        let refresh_token = Self::refresh_token_of(&data).ok_or(ProviderError::AuthExpired)?;

        let refreshed = match method {
            KiroAuthMethod::Idc => self.refresh_oidc(&data, &refresh_token).await?,
            KiroAuthMethod::ExternalIdp => self.refresh_external_idp(&data, &refresh_token).await?,
            KiroAuthMethod::Imported | KiroAuthMethod::ApiKey => {
                self.refresh_social(&refresh_token).await?
            }
        };

        // OIDC answers camelCase; Microsoft answers snake_case.
        let access = refreshed
            .get("accessToken")
            .or_else(|| refreshed.get("access_token"))
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
        if let Some(secs) = refreshed
            .get("expiresIn")
            .or_else(|| refreshed.get("expires_in"))
            .and_then(|v| v.as_i64())
        {
            data["expiresAt"] = json!((Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339());
        }

        // Machine id is FROZEN at import and carried forward. Re-deriving here
        // would move the account to a new device on every refresh.
        if data.get("machineId").is_none() {
            let mid = match method {
                KiroAuthMethod::ApiKey => derive_api_key_machine_id(
                    &Self::access_token_of(&data).unwrap_or_default(),
                ),
                _ => derive_oauth_machine_id(&refresh_token),
            };
            data["machineId"] = json!(mid);
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
        let region = Self::region_of(&data);
        let machine_id = machine_id_for(&data, &account.id);
        let profile_arn = Self::profile_arn_of(&data);

        let upstream_model = req.upstream_model().trim_start_matches("kr/").to_string();
        let mut body = build_wire_request(req, &upstream_model, &profile_arn);

        // Effort: a wrong schema is a rejection, so the field is chosen from
        // the model id and never defaulted.
        let effort = req
            .extra
            .get("reasoning_effort")
            .and_then(|v| v.as_str())
            .or_else(|| req.extra.get("reasoningEffort").and_then(|v| v.as_str()));
        match effort_path(&upstream_model) {
            "reasoning" => {
                body["additionalModelRequestFields"] = json!({
                    "reasoning": { "effort": effort.unwrap_or("high") }
                });
            }
            "output_config" => {
                body["additionalModelRequestFields"] = json!({
                    "thinking": { "type": "adaptive", "display": "summarized" },
                    "output_config": { "effort": effort.unwrap_or("high") },
                });
            }
            _ => {
                // Requested by a text marker instead.
                let budget = thinking_budget(effort);
                let marker = format!(
                    "<thinking_mode>enabled</thinking_mode>\n<max_thinking_length>{budget}</max_thinking_length>\n"
                );
                if let Some(msg) = body
                    .pointer_mut("/conversationState/currentMessage/userInputMessage/content")
                    .and_then(|v| v.as_str())
                {
                    body["conversationState"]["currentMessage"]["userInputMessage"]["content"] =
                        json!(format!("{marker}{msg}"));
                }
            }
        }

        let mut request = self.client.post(generate_url(&region));
        for (k, v) in self.dispatch_headers(&data, &token, &machine_id) {
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
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);
            let (usage_tx, usage_rx) = oneshot::channel::<Option<StreamUsage>>();
            let mut upstream = resp.bytes_stream();
            let model_for_usage = upstream_model.clone();

            tokio::spawn(async move {
                let mut buffer: Vec<u8> = Vec::new();
                let mut output_text = String::new();
                let mut context_pct: Option<f64> = None;
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
                    buffer.extend_from_slice(&chunk);

                    let res = decode_messages(&buffer);
                    if let Some((reason, detail)) = &res.failure {
                        let _ = tx
                            .send(Err(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                format!("Kiro EventStream is corrupt: {reason} ({detail})"),
                            )))
                            .await;
                        if let Some(t) = usage_tx.take() {
                            let _ = t.send(None);
                        }
                        return;
                    }
                    buffer.drain(..res.consumed);

                    for msg in &res.messages {
                        if matches!(msg.message_type(), Some("error" | "exception")) {
                            let _ = tx
                                .send(Err(std::io::Error::new(
                                    std::io::ErrorKind::Other,
                                    format!("Kiro upstream error: {}", msg.payload.as_deref().unwrap_or("")),
                                )))
                                .await;
                            if let Some(t) = usage_tx.take() {
                                let _ = t.send(None);
                            }
                            return;
                        }
                        let Some(payload) = msg.payload.as_deref() else { continue };
                        let Ok(v) = serde_json::from_str::<Value>(payload) else { continue };
                        let inner = v
                            .get(msg.event_type())
                            .filter(|x| x.is_object())
                            .unwrap_or(&v);

                        match msg.event_type() {
                            "assistantResponseEvent" | "codeEvent" => {
                                if let Some(t) = inner.get("content").and_then(|x| x.as_str()) {
                                    output_text.push_str(t);
                                    emit_delta(&tx, t).await;
                                }
                            }
                            "contextUsageEvent" => {
                                if let Some(p) = inner
                                    .get("contextUsagePercentage")
                                    .and_then(|x| x.as_f64())
                                {
                                    context_pct = Some(p);
                                }
                            }
                            _ => {}
                        }
                    }
                }

                // No token field is sent by this surface, so input tokens are
                // recovered from the context percentage.
                let completion_tokens = (output_text.len() as f64 / 4.0).floor().max(1.0) as i64;
                let usage = context_pct.map(|pct| {
                    let total = ((context_window(&model_for_usage) as f64) * pct / 100.0).round() as i64;
                    StreamUsage {
                        prompt_tokens: (total - completion_tokens).max(0),
                        completion_tokens,
                        total_tokens: total.max(completion_tokens),
                    }
                    .normalized()
                });
                if let Some(t) = usage_tx.take() {
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

        // Non-stream: the upstream still answers EventStream, so it is drained
        // and folded into one Chat Completions object.
        let mut buffer: Vec<u8> = Vec::new();
        let mut upstream = resp.bytes_stream();
        while let Some(chunk_res) = upstream.next().await {
            let chunk = chunk_res.map_err(|e| ProviderError::Transport(e.to_string()))?;
            buffer.extend_from_slice(&chunk);
        }

        let mut text = String::new();
        let mut context_pct: Option<f64> = None;
        let res = decode_messages(&buffer);
        if let Some((reason, detail)) = &res.failure {
            return Err(ProviderError::Upstream {
                status,
                body: format!("Kiro EventStream is corrupt: {reason} ({detail})"),
            });
        }
        for msg in &res.messages {
            if matches!(msg.message_type(), Some("error" | "exception")) {
                return Err(ProviderError::Upstream {
                    status,
                    body: msg.payload.clone().unwrap_or_default(),
                });
            }
            let Some(payload) = msg.payload.as_deref() else { continue };
            let Ok(v) = serde_json::from_str::<Value>(payload) else { continue };
            let inner = v.get(msg.event_type()).filter(|x| x.is_object()).unwrap_or(&v);
            match msg.event_type() {
                "assistantResponseEvent" | "codeEvent" => {
                    if let Some(t) = inner.get("content").and_then(|x| x.as_str()) {
                        text.push_str(t);
                    }
                }
                "contextUsageEvent" => {
                    if let Some(p) = inner.get("contextUsagePercentage").and_then(|x| x.as_f64()) {
                        context_pct = Some(p);
                    }
                }
                _ => {}
            }
        }

        let completion_tokens = (text.len() as f64 / 4.0).floor().max(1.0) as i64;
        let total = context_pct
            .map(|pct| ((context_window(&upstream_model) as f64) * pct / 100.0).round() as i64)
            .unwrap_or(completion_tokens);
        let prompt_tokens = (total - completion_tokens).max(0);

        Ok(ChatOutcome::Json(json!({
            "id": format!("chatcmpl-{}", Uuid::new_v4().simple()),
            "object": "chat.completion",
            "created": Utc::now().timestamp(),
            "model": req_model,
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": text },
            }],
            "usage": {
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "total_tokens": total.max(completion_tokens),
            },
        })))
    }

    async fn sync_quota(&self, account: &mut Account) -> Result<(), ProviderError> {
        let data = account.data_json();
        let token = Self::access_token_of(&data)
            .ok_or_else(|| ProviderError::AuthInvalid("missing accessToken".into()))?;
        let region = Self::region_of(&data);
        let machine_id = machine_id_for(&data, &account.id);
        let method = Self::auth_method_of(&data);

        let mut url = usage_limits_url(&region);
        if let Some(arn) = data.get("profileArn").and_then(|v| v.as_str()) {
            if !arn.trim().is_empty() {
                url.push_str(&format!("&profileArn={arn}"));
            }
        }

        let mut request = self
            .client
            .get(url)
            .header("authorization", format!("Bearer {token}"))
            .header("accept", "application/json")
            .header("connection", "close")
            .header("x-amzn-kiro-agent-mode", "vibe")
            .header("user-agent", kiro_ide_user_agent(&machine_id))
            .header("x-amz-user-agent", kiro_ide_user_agent(&machine_id));
        if let Some(tt) = method.token_type_header() {
            request = request.header("tokentype", tt);
        }

        let Ok(resp) = request.send().await else { return Ok(()) };
        if !resp.status().is_success() {
            // 401/403 here means the credential was refused, not that the
            // transport failed; report it rather than the last transport error.
            if matches!(resp.status().as_u16(), 401 | 403) {
                return Err(ProviderError::AuthExpired);
            }
            return Ok(());
        }
        let Ok(v) = resp.json::<Value>().await else { return Ok(()) };

        let mut data = data;
        if let Some(title) = v
            .get("subscriptionInfo")
            .and_then(|s| s.get("subscriptionTitle"))
            .and_then(|x| x.as_str())
        {
            data["plan"] = json!(title);
        }
        if let Some(list) = v.get("usageBreakdownList").and_then(|x| x.as_array()) {
            let windows: Vec<Value> = list
                .iter()
                .filter_map(|b| {
                    let kind = b.get("resourceType").and_then(|x| x.as_str())?.to_lowercase();
                    let used = b
                        .get("currentUsageWithPrecision")
                        .or_else(|| b.get("currentUsage"))
                        .and_then(|x| x.as_f64())
                        .unwrap_or(0.0);
                    let limit = b
                        .get("usageLimitWithPrecision")
                        .or_else(|| b.get("usageLimit"))
                        .and_then(|x| x.as_f64())
                        .unwrap_or(0.0);
                    let pct = if limit > 0.0 { (used / limit) * 100.0 } else { 0.0 };
                    Some(json!({
                        "kind": kind,
                        "usedPercent": pct,
                        "remainingPercent": 100.0 - pct,
                        "limit": limit,
                        "used": used,
                    }))
                })
                .collect();
            if !windows.is_empty() {
                data["quotaWindows"] = Value::Array(windows);
            }
        }
        account.set_data_json(&data);
        Ok(())
    }
}

async fn emit_delta(tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, std::io::Error>>, text: &str) {
    let frame = json!({
        "id": format!("chatcmpl-{}", Uuid::new_v4().simple()),
        "object": "chat.completion.chunk",
        "created": Utc::now().timestamp(),
        "choices": [{ "index": 0, "delta": { "content": text }, "finish_reason": null }],
    });
    let _ = tx
        .send(Ok(bytes::Bytes::from(format!("data: {frame}\n\n"))))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_id_is_frozen_across_refresh() {
        let frozen = "a".repeat(64);
        let data = json!({ "machineId": frozen });
        // A different account id must not change a frozen machine id.
        assert_eq!(machine_id_for(&data, "acct-1"), frozen);
        assert_eq!(machine_id_for(&data, "acct-2"), frozen);
    }

    #[test]
    fn machine_id_falls_back_per_account() {
        let data = json!({});
        let a = machine_id_for(&data, "acct-1");
        let b = machine_id_for(&data, "acct-2");
        assert_ne!(a, b, "fallback must be stable but distinct per account");
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn normalize_machine_id_doubles_32_hex() {
        let bare = "0123456789abcdef0123456789abcdef";
        assert_eq!(normalize_machine_id(bare), Some(format!("{bare}{bare}")));
        assert_eq!(normalize_machine_id(&"z".repeat(64)), None);
    }

    #[test]
    fn oauth_and_api_key_salts_differ() {
        // Same secret must not derive the same device id across families.
        assert_ne!(derive_oauth_machine_id("s"), derive_api_key_machine_id("s"));
    }

    #[test]
    fn tool_name_sanitizes_illegal_chars() {
        assert_eq!(wire_tool_name("get-weather"), "get-weather");
        assert_eq!(wire_tool_name("get weather!"), "get_weather_");
    }

    #[test]
    fn overlong_tool_name_keeps_prefix_and_hash() {
        let long = "t".repeat(100);
        let out = wire_tool_name(&long);
        assert!(out.len() <= 64, "{out}");
        // Two names differing only past the limit must not collide.
        let mut other = long.clone();
        other.replace_range(90..91, "x");
        assert_ne!(out, wire_tool_name(&other));
    }

    #[test]
    fn schema_drops_additional_properties_and_empty_required() {
        let schema = json!({
            "type": "object",
            "properties": { "a": { "type": "string" } },
            "additionalProperties": false,
            "required": [],
        });
        let out = normalize_json_schema(&schema);
        assert!(out.get("additionalProperties").is_none());
        assert!(out.get("required").is_none());
        assert_eq!(out["type"], "object");
    }

    #[test]
    fn effort_path_is_chosen_from_model_id() {
        assert_eq!(effort_path("gpt-5.6-sol"), "reasoning");
        assert_eq!(effort_path("claude-opus-4.7"), "output_config");
        assert_eq!(effort_path("claude-sonnet-4.5"), "none");
        assert_eq!(effort_path("deepseek-3.2"), "none");
    }

    #[test]
    fn thinking_budget_ladder() {
        assert_eq!(thinking_budget(Some("minimal")), 512);
        assert_eq!(thinking_budget(Some("low")), 1024);
        assert_eq!(thinking_budget(Some("medium")), 8192);
        assert_eq!(thinking_budget(Some("high")), 16_000);
        assert_eq!(thinking_budget(Some("xhigh")), THINKING_BUDGET_MAX);
    }

    #[test]
    fn idp_endpoint_allowlist_rejects_other_hosts() {
        assert!(is_allowed_idp_endpoint("https://login.microsoftonline.com/x"));
        assert!(is_allowed_idp_endpoint("https://login.windows.net/x"));
        assert!(!is_allowed_idp_endpoint("https://evil.example.com/x"));
        assert!(!is_allowed_idp_endpoint("http://login.microsoftonline.com/x"));
    }

    #[test]
    fn region_is_validated_before_url_interpolation() {
        assert_eq!(KiroProvider::region_of(&json!({"region": "us-east-1"})), "us-east-1");
        assert_eq!(KiroProvider::region_of(&json!({"region": "evil\"host"})), DEFAULT_REGION);
    }

    #[test]
    fn profile_arn_uses_family_default() {
        assert_eq!(
            KiroProvider::auth_method_of(&json!({"authMethod":"imported"})).default_profile_arn(),
            DEFAULT_PROFILE_ARN_SOCIAL
        );
        assert_eq!(
            KiroProvider::auth_method_of(&json!({"authMethod":"api_key"})).default_profile_arn(),
            ""
        );
    }

    #[test]
    fn context_window_falls_back_to_default() {
        assert_eq!(context_window("claude-sonnet-5"), 1_000_000);
        assert_eq!(context_window("totally-unknown"), DEFAULT_CONTEXT_WINDOW);
    }
}
