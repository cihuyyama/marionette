//! Freebuff provider — native Rust port of the freebuff2api session protocol.
//!
//! Upstream: https://www.codebuff.com (session-based agent protocol, NOT a
//! plain OpenAI API). Auth = opaque `cb_…` Bearer token (no refresh, no
//! expiry; dies only on ban). Quota = sessions/day per account.
//!
//! Critical design: creating a session CONSUMES daily quota, so sessions are
//! cached per (account, model) with a 30-min idle TTL and reused across
//! requests. Eviction best-effort DELETEs upstream.
//!
//! Protocol steps (mirrored from refs/freebuff2api/engine.js):
//! 1. Session: GET probe → POST create (x-freebuff-model + x-freebuff-instance-id)
//!    → poll queued up to 8× at 1.5s
//! 2. Runs: POST /api/v1/agent-runs START (model agent) + START context-pruner child
//! 3. Chat: POST /api/v1/chat/completions with mandatory envelope:
//!    - system prompt starts with Buffy marker
//!    - always stream:true upstream
//!    - stop:['"cb_easp"'], provider:{data_collection:"deny"}
//!    - dummy end_turn tool, codebuff_metadata
//! 4. SSE unwrap: upstream wraps chunks as {data:<openai-chunk>}; unwrap + re-emit
//!
//! Known limitations (v1):
//! - No ad-auction/usage/streak camouflage calls (anti-ban heuristic; skipped)
//! - No TLS fingerprint impersonation
//! - No dynamic model-catalog fetch (catalog hardcoded)
//! - No proxy rotation beyond Marionette's client_for_account

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
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

const BASE_URL: &str = "https://www.codebuff.com";
const SESSION_TTL_MS: i64 = 30 * 60 * 1000;
const CONTEXT_PRUNER: &str = "context-pruner";
const BUFFY_MARKER: &str = "You are Buffy, the strategic coding assistant.";
const QUEUED_POLL_MAX: usize = 8;
const QUEUED_POLL_MS: u64 = 1500;

const REASONING_EFFORT_RANK: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max", "ultra"];

const MODEL_EFFORTS: &[(&str, &[&str])] = &[
    ("deepseek/deepseek-v4-flash", &["low", "high", "max"]),
    ("deepseek/deepseek-v4-pro", &["low", "high", "max"]),
    ("openai/gpt-5.6-luna", &["low", "medium", "high", "xhigh", "max"]),
    ("anthropic/claude-fable-5", &["low", "medium", "high", "xhigh", "max"]),
    ("meta/muse-spark-1.2-contributor", &["low", "medium", "high", "xhigh"]),
    ("stealth/ox-alpha", &["low", "medium", "high", "xhigh", "max"]),
];

/// Upstream model → base2 root agent id (free-agents.ts
/// FREEBUFF_ROOT_AGENT_ID_BY_MODEL, verified 2026-08-22 against main). The
/// reviewer child is pinned per model server-side; we only start the root run
/// plus the context-pruner child.
fn agent_id_for_model(model: &str) -> &'static str {
    match model {
        "deepseek/deepseek-v4-flash" => "base2-free-deepseek-flash",
        "deepseek/deepseek-v4-pro" => "base2-free-deepseek",
        "mimo/mimo-v2.5" => "base2-free-mimo",
        "minimax/minimax-m3" => "base2-free-minimax-m3",
        "z-ai/glm-5.2" => "base2-free-glm",
        "openai/gpt-5.6-luna" => "base2-free-luna",
        "anthropic/claude-fable-5" => "base2-free-fable",
        "meta/muse-spark-1.2-contributor" => "base2-free-muse-spark",
        "crof/kimi-k3-eco" => "base2-free-kimi-k3-eco",
        "stealth/ox-alpha" => "base2-free-ox-alpha",
        _ => "base2-free",
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[derive(Debug, Clone)]
struct SessionEntry {
    instance_id: String,
    created_at: i64,
    last_used: i64,
    run_id: Option<String>,
    pruner_run_id: Option<String>,
}

impl SessionEntry {
    fn is_alive(&self, now: i64) -> bool {
        now - self.last_used < SESSION_TTL_MS
    }
}

pub struct FreebuffProvider {
    client: Client,
    sessions: Mutex<HashMap<(String, String), SessionEntry>>,
}

impl Default for FreebuffProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FreebuffProvider {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(15))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(60))
            .tcp_nodelay(true)
            .build()
            .expect("reqwest client");
        Self {
            client,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn token_of(data: &Value) -> Option<String> {
        data.get("token")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn session_key(account: &Account, model: &str) -> (String, String) {
        (account.id.clone(), model.to_string())
    }

    /// One upstream session is shared across model keys of the same account;
    /// deleting it upstream invalidates every cached row for that account.
    fn evict_all_for_account(&self, account_id: &str) {
        if let Ok(mut map) = self.sessions.lock() {
            map.retain(|(acc, _), _| acc != account_id);
        }
    }

    fn evict_expired(&self, key: &(String, String)) -> Option<String> {
        let mut map = self.sessions.lock().ok()?;
        if let Some(entry) = map.get(key) {
            if entry.is_alive(now_ms()) {
                return None;
            }
            let instance = entry.instance_id.clone();
            let age_secs = (now_ms() - entry.created_at) / 1000;
            map.remove(key);
            tracing::debug!(
                account = %key.0,
                model = %key.1,
                age_secs,
                "freebuff session idle-expired"
            );
            return Some(instance);
        }
        None
    }

    fn get_cached_session(&self, key: &(String, String)) -> Option<SessionEntry> {
        let map = self.sessions.lock().ok()?;
        let entry = map.get(key)?;
        if entry.is_alive(now_ms()) {
            Some(entry.clone())
        } else {
            None
        }
    }

    fn cache_session(&self, key: (String, String), instance_id: String) -> SessionEntry {
        let now = now_ms();
        let entry = SessionEntry {
            instance_id,
            created_at: now,
            last_used: now,
            run_id: None,
            pruner_run_id: None,
        };
        if let Ok(mut map) = self.sessions.lock() {
            map.insert(key, entry.clone());
        }
        entry
    }

    fn touch_session(&self, key: &(String, String)) {
        if let Ok(mut map) = self.sessions.lock() {
            if let Some(entry) = map.get_mut(key) {
                entry.last_used = now_ms();
            }
        }
    }

    fn set_run_ids(&self, key: &(String, String), run_id: &str, pruner_run_id: &str) {
        if let Ok(mut map) = self.sessions.lock() {
            if let Some(entry) = map.get_mut(key) {
                entry.run_id = Some(run_id.to_string());
                entry.pruner_run_id = Some(pruner_run_id.to_string());
            }
        }
    }

    async fn get_or_create_session(
        &self,
        client: &Client,
        token: &str,
        account_id: &str,
        model: &str,
    ) -> Result<SessionEntry, ProviderError> {
        let key = (account_id.to_string(), model.to_string());

        if let Some(expired_instance) = self.evict_expired(&key) {
            self.evict_all_for_account(account_id);
            self.delete_upstream_session(client, token, &expired_instance)
                .await;
        }

        if let Some(cached) = self.get_cached_session(&key) {
            return Ok(cached);
        }

        // GET probe does not consume quota; POST create does. Reuse an
        // upstream-active session when the model matches.
        if let Some(instance_id) = self.probe_active_session(client, token, model).await? {
            return Ok(self.cache_session(key, instance_id));
        }

        let instance_id = self.create_session(client, token, model).await?;
        Ok(self.cache_session(key, instance_id))
    }

    /// GET /api/v1/freebuff/session with the include-unused-rate-limits
    /// header (quota snapshot; safe on probes). Returns the instance id of an
    /// active session when its model matches; deletes a model-mismatched
    /// session upstream so the caller creates a fresh one.
    async fn probe_active_session(
        &self,
        client: &Client,
        token: &str,
        model: &str,
    ) -> Result<Option<String>, ProviderError> {
        let resp = client
            .get(format!("{BASE_URL}/api/v1/freebuff/session"))
            .header("Authorization", format!("Bearer {token}"))
            .header("x-freebuff-include-unused-rate-limits", "1")
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(format!("freebuff session probe: {e}")))?;

        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 200 {
            return Err(classify_freebuff_status(status, &text));
        }
        let data: Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Other(format!("freebuff session probe json: {e}")))?;

        if data.get("status").and_then(|v| v.as_str()) != Some("active") {
            return Ok(None);
        }
        let instance_id = data
            .get("instanceId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let Some(instance_id) = instance_id else {
            return Ok(None);
        };
        let upstream_model = data.get("model").and_then(|v| v.as_str());
        if upstream_model.is_none() || upstream_model == Some(model) {
            return Ok(Some(instance_id));
        }
        self.delete_upstream_session(client, token, &instance_id)
            .await;
        Ok(None)
    }

    async fn create_session(
        &self,
        client: &Client,
        token: &str,
        model: &str,
    ) -> Result<String, ProviderError> {
        let inst_id = uuid::Uuid::new_v4().to_string();
        let resp = client
            .post(format!("{BASE_URL}/api/v1/freebuff/session"))
            .header("Authorization", format!("Bearer {token}"))
            .header("x-freebuff-model", model)
            .header("x-freebuff-instance-id", &inst_id)
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(format!("freebuff session create: {e}")))?;

        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();

        if status != 200 {
            return Err(classify_freebuff_status(status, &text));
        }

        let data: Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Other(format!("freebuff session json: {e}")))?;

        if data.get("status").and_then(|v| v.as_str()) == Some("active") {
            let inst = data
                .get("instanceId")
                .and_then(|v| v.as_str())
                .unwrap_or(&inst_id)
                .to_string();
            return Ok(inst);
        }

        if data.get("status").and_then(|v| v.as_str()) == Some("queued") {
            let inst = data
                .get("instanceId")
                .and_then(|v| v.as_str())
                .unwrap_or(&inst_id)
                .to_string();
            return self.poll_queued_session(client, token, &inst).await;
        }

        Err(ProviderError::Other(format!(
            "freebuff session create unexpected: {}",
            text.chars().take(200).collect::<String>()
        )))
    }

    async fn poll_queued_session(
        &self,
        client: &Client,
        token: &str,
        instance_id: &str,
    ) -> Result<String, ProviderError> {
        for _ in 0..QUEUED_POLL_MAX {
            tokio::time::sleep(std::time::Duration::from_millis(QUEUED_POLL_MS)).await;
            let resp = client
                .get(format!("{BASE_URL}/api/v1/freebuff/session"))
                .header("Authorization", format!("Bearer {token}"))
                .header("x-freebuff-instance-id", instance_id)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| ProviderError::Transport(format!("freebuff session poll: {e}")))?;

            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            if status != 200 {
                return Err(classify_freebuff_status(status, &text));
            }
            let data: Value = serde_json::from_str(&text)
                .map_err(|e| ProviderError::Other(format!("freebuff session poll json: {e}")))?;
            if data.get("status").and_then(|v| v.as_str()) == Some("active") {
                return Ok(instance_id.to_string());
            }
        }
        Err(ProviderError::Other(
            "freebuff session stayed queued (retry later)".into(),
        ))
    }

    async fn delete_upstream_session(&self, client: &Client, token: &str, instance_id: &str) {
        let _ = client
            .delete(format!("{BASE_URL}/api/v1/freebuff/session"))
            .header("Authorization", format!("Bearer {token}"))
            .header("x-freebuff-instance-id", instance_id)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;
    }

    async fn ensure_run_chain(
        &self,
        client: &Client,
        token: &str,
        model: &str,
        session_entry: &SessionEntry,
        key: &(String, String),
    ) -> Result<String, ProviderError> {
        if let Some(ref run_id) = session_entry.run_id {
            return Ok(run_id.clone());
        }
        let agent = agent_id_for_model(model);
        let run_id = self.start_run(client, token, agent, &[]).await?;
        let pruner_id = self
            .start_run(client, token, CONTEXT_PRUNER, &[run_id.as_str()])
            .await?;
        self.set_run_ids(key, &run_id, &pruner_id);
        Ok(run_id)
    }

    async fn start_run(
        &self,
        client: &Client,
        token: &str,
        agent_id: &str,
        ancestors: &[&str],
    ) -> Result<String, ProviderError> {
        let body = json!({
            "action": "START",
            "agentId": agent_id,
            "ancestorRunIds": ancestors,
        });
        let resp = client
            .post(format!("{BASE_URL}/api/v1/agent-runs"))
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(format!("freebuff start_run: {e}")))?;

        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 200 {
            return Err(classify_freebuff_status(status, &text));
        }
        let data: Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Other(format!("freebuff run json: {e}")))?;
        data.get("runId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                ProviderError::Other(format!(
                    "freebuff start_run no runId: {}",
                    text.chars().take(200).collect::<String>()
                ))
            })
    }

    pub async fn fetch_user(
        &self,
        token: &str,
    ) -> Result<(Option<String>, Option<String>), ProviderError> {
        let resp = self
            .client
            .get(format!("{BASE_URL}/api/v1/me"))
            .header("Authorization", format!("Bearer {token}"))
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(format!("freebuff /me: {e}")))?;

        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 200 {
            return Err(classify_freebuff_status(status, &text));
        }
        let data: Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Other(format!("freebuff /me json: {e}")))?;
        let uid = data
            .get("id")
            .or_else(|| data.get("uid"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let email = data
            .get("email")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Ok((uid, email))
    }
}

#[async_trait]
impl Provider for FreebuffProvider {
    fn id(&self) -> &'static str {
        "freebuff"
    }

    async fn ensure_fresh_auth(&self, account: &mut Account) -> Result<(), ProviderError> {
        let data = account.data_json();
        if Self::token_of(&data).is_none() {
            return Err(ProviderError::AuthInvalid("missing freebuff token".into()));
        }
        Ok(())
    }

    async fn chat(
        &self,
        client: &Client,
        account: &Account,
        req: &ChatCompletionRequest,
    ) -> Result<ChatOutcome, ProviderError> {
        let data = account.data_json();
        let token = Self::token_of(&data)
            .ok_or_else(|| ProviderError::AuthInvalid("missing freebuff token".into()))?;

        let model = req.upstream_model().to_string();
        let key = Self::session_key(account, &model);

        let session = self
            .get_or_create_session(client, &token, &account.id, &model)
            .await?;

        let run_id = self
            .ensure_run_chain(client, &token, &model, &session, &key)
            .await?;

        match self
            .chat_attempt(client, &token, req, &model, &session.instance_id, &run_id)
            .await
        {
            Ok(outcome) => {
                self.touch_session(&key);
                Ok(outcome)
            }
            Err(e) if is_session_gate_error(&e) => {
                self.evict_all_for_account(&account.id);
                self.delete_upstream_session(client, &token, &session.instance_id)
                    .await;
                let instance_id = self.create_session(client, &token, &model).await?;
                let fresh = self.cache_session(key.clone(), instance_id);
                let fresh_run = self
                    .ensure_run_chain(client, &token, &model, &fresh, &key)
                    .await?;
                match self
                    .chat_attempt(client, &token, req, &model, &fresh.instance_id, &fresh_run)
                    .await
                {
                    Ok(outcome) => {
                        self.touch_session(&key);
                        Ok(outcome)
                    }
                    Err(e2) => {
                        if is_session_gate_error(&e2) {
                            self.evict_all_for_account(&account.id);
                        }
                        Err(e2)
                    }
                }
            }
            Err(e) => Err(e),
        }
    }
}

impl FreebuffProvider {
    async fn chat_attempt(
        &self,
        client: &Client,
        token: &str,
        req: &ChatCompletionRequest,
        model: &str,
        instance_id: &str,
        run_id: &str,
    ) -> Result<ChatOutcome, ProviderError> {
        let payload = build_upstream_payload(req, model, instance_id, run_id);

        let resp = client
            .post(format!("{BASE_URL}/api/v1/chat/completions"))
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("x-freebuff-instance-id", instance_id)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(300))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(format!("freebuff chat: {e}")))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_freebuff_status(status, &text));
        }

        let req_model = req.model.clone();
        let is_stream = req.stream_enabled();

        if is_stream {
            Ok(relay_freebuff_stream(resp))
        } else {
            aggregate_stream_to_json(resp, &req_model).await
        }
    }
}

fn relay_freebuff_stream(resp: reqwest::Response) -> ChatOutcome {
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
                        return;
                    }

                    let unwrapped = unwrap_sse_data(data_str);

                    if let Ok(v) = serde_json::from_str::<Value>(&unwrapped) {
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
                        .send(Ok(bytes::Bytes::from(format!("data: {unwrapped}\n\n"))))
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
        .expect("response builder");
    let (mut parts, body) = response.into_parts();
    parts.headers = headers;
    ChatOutcome::Stream {
        response: Response::from_parts(parts, body),
        usage_rx,
    }
}

async fn aggregate_stream_to_json(
    resp: reqwest::Response,
    req_model: &str,
) -> Result<ChatOutcome, ProviderError> {
    let mut upstream_stream = resp.bytes_stream();
    let mut buffer = String::new();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut finish_reason: Option<String> = None;
    let mut id = String::new();
    let mut usage: Option<Value> = None;

    loop {
        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim_end_matches('\r').to_string();
            buffer = buffer[pos + 1..].to_string();

            let trimmed = line.trim();
            if !trimmed.starts_with("data:") {
                continue;
            }
            let payload = trimmed[5..].trim();
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            let unwrapped = unwrap_sse_data(payload);
            if let Ok(obj) = serde_json::from_str::<Value>(&unwrapped) {
                if let Some(choices) = obj.get("choices").and_then(|c| c.as_array()) {
                    if let Some(choice) = choices.first() {
                        let delta = choice.get("delta");
                        if let Some(d) = delta {
                            if let Some(c) = d.get("content").and_then(|v| v.as_str()) {
                                content.push_str(c);
                            }
                            if let Some(r) = d.get("reasoning_content").and_then(|v| v.as_str()) {
                                reasoning.push_str(r);
                            }
                        }
                        if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                            finish_reason = Some(fr.to_string());
                        }
                    }
                }
                if let Some(i) = obj.get("id").and_then(|v| v.as_str()) {
                    id = i.to_string();
                }
                if let Some(u) = obj.get("usage") {
                    usage = Some(u.clone());
                }
            }
        }

        match upstream_stream.next().await {
            Some(Ok(chunk)) => buffer.push_str(&String::from_utf8_lossy(&chunk)),
            Some(Err(e)) => {
                return Err(ProviderError::Transport(format!(
                    "freebuff stream read: {e}"
                )));
            }
            None => break,
        }
    }

    let msg = if reasoning.is_empty() || !content.is_empty() {
        let mut m = json!({ "role": "assistant", "content": content });
        if !reasoning.is_empty() {
            m["reasoning_content"] = json!(reasoning);
        }
        m
    } else {
        json!({ "role": "assistant", "content": reasoning })
    };

    let result = json!({
        "id": if id.is_empty() { format!("gen_{}", now_ms()) } else { id },
        "object": "chat.completion",
        "created": now_ms() / 1000,
        "model": req_model,
        "choices": [{
            "index": 0,
            "message": msg,
            "finish_reason": finish_reason.unwrap_or_else(|| "stop".into()),
            "logprobs": null,
        }],
        "usage": usage.unwrap_or_else(|| json!({"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0})),
    });
    Ok(ChatOutcome::Json(result))
}

/// Unwrap the freebuff SSE `{data: <openai-chunk>}` envelope. If the parsed
/// JSON has a `.data` object with choices/id/usage, emit the inner object.
fn unwrap_sse_data(payload: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(payload) {
        if let Some(inner) = v.get("data") {
            if inner.is_object()
                && (inner.get("choices").is_some()
                    || inner.get("id").is_some()
                    || inner.get("usage").is_some())
            {
                return inner.to_string();
            }
        }
        return v.to_string();
    }
    payload.to_string()
}

pub fn build_upstream_payload(
    req: &ChatCompletionRequest,
    model: &str,
    instance_id: &str,
    run_id: &str,
) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    let mut has_system = false;

    for m in &req.messages {
        let mut msg = serde_json::to_value(m).unwrap_or(json!({}));
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        if role == "system" || role == "developer" {
            if role == "developer" {
                msg["role"] = json!("system");
            }
            has_system = true;
            msg["cache_control"] = json!({"type": "ephemeral"});
            inject_buffy_marker(&mut msg);
        }
        messages.push(msg);
    }

    if !has_system {
        messages.insert(
            0,
            json!({
                "role": "system",
                "content": BUFFY_MARKER,
                "cache_control": {"type": "ephemeral"},
            }),
        );
    }

    let mut payload = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "provider": { "data_collection": "deny" },
        "codebuff_metadata": {
            "freebuff_instance_id": instance_id,
            "trace_session_id": uuid::Uuid::new_v4().to_string(),
            "run_id": run_id,
            "client_id": random_client_id(),
            "cost_mode": "free",
        },
    });

    if let Some(t) = req.temperature {
        payload["temperature"] = json!(t);
    }
    if let Some(m) = req.max_tokens {
        payload["max_tokens"] = json!(m);
    }
    if let Some(p) = req.top_p {
        payload["top_p"] = json!(p);
    }

    if let Some(stop) = req.extra.get("stop") {
        payload["stop"] = stop.clone();
    } else {
        payload["stop"] = json!(["\"cb_easp\""]);
    }

    if let Some(ref effort) = req.extra.get("reasoning_effort") {
        if let Some(e) = effort.as_str() {
            payload["reasoning_effort"] = json!(clamp_reasoning_effort(model, e));
        }
    }

    if req.has_tools() {
        let mut tools = req.tools.as_ref().map(|t| t.clone()).unwrap_or(json!([]));
        if let Some(arr) = tools.as_array_mut() {
            let has_end_turn = arr.iter().any(|t| {
                t.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    == Some("end_turn")
            });
            if !has_end_turn {
                arr.push(json!({
                    "type": "function",
                    "function": {
                        "name": "end_turn",
                        "description": "Signal the end of the current task.",
                        "parameters": { "type": "object", "properties": {} },
                    }
                }));
            }
        }
        payload["tools"] = tools;
        if let Some(tc) = req.tool_choice.as_ref() {
            payload["tool_choice"] = tc.clone();
        }
        if let Some(ptc) = req.parallel_tool_calls.as_ref() {
            payload["parallel_tool_calls"] = ptc.clone();
        }
    }

    payload
}

fn inject_buffy_marker(msg: &mut Value) {
    if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
        if !content.starts_with(BUFFY_MARKER) {
            msg["content"] = json!(format!("{BUFFY_MARKER}{content}"));
        }
    } else if let Some(arr) = msg.get("content").and_then(|v| v.as_array()).cloned() {
        let mut modified = arr.clone();
        let mut injected = false;
        for part in modified.iter_mut() {
            if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                {
                    if !text.starts_with(BUFFY_MARKER) {
                        part["text"] = json!(format!("{BUFFY_MARKER}{text}"));
                    }
                    injected = true;
                    break;
                }
            }
        }
        if !injected {
            modified.insert(0, json!({"type": "text", "text": BUFFY_MARKER}));
        }
        msg["content"] = json!(modified);
    }
}

fn clamp_reasoning_effort(model: &str, requested: &str) -> String {
    let allowed = MODEL_EFFORTS
        .iter()
        .find(|(m, _)| *m == model)
        .map(|(_, a)| *a);
    let Some(allowed) = allowed else {
        return requested.to_string();
    };
    let wanted = REASONING_EFFORT_RANK.iter().position(|r| *r == requested);
    let Some(wanted) = wanted else {
        return requested.to_string();
    };
    let mut best: Option<&str> = None;
    let mut best_rank: usize = 0;
    for cand in allowed {
        if let Some(rank) = REASONING_EFFORT_RANK.iter().position(|r| r == cand) {
            if rank <= wanted && (best.is_none() || rank > best_rank) {
                best = Some(cand);
                best_rank = rank;
            }
        }
    }
    best.unwrap_or(allowed.iter().copied().min_by_key(|c| {
        REASONING_EFFORT_RANK.iter().position(|r| r == c).unwrap_or(99)
    }).unwrap_or(requested))
    .to_string()
}

fn random_client_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let charset = b"0123456789abcdefghijklmnopqrstuvwxyz";
    (0..13).map(|_| charset[rng.gen_range(0..charset.len())] as char).collect()
}

fn is_session_gate_error(e: &ProviderError) -> bool {
    matches!(e, ProviderError::Other(msg) if msg.starts_with("freebuff session gate:"))
}

pub fn classify_freebuff_status(status: u16, body: &str) -> ProviderError {
    let lower = body.to_lowercase();
    match status {
        401 => ProviderError::AuthInvalid("freebuff token invalid".into()),
        403 => {
            if lower.contains("banned") || lower.contains("country_blocked") {
                ProviderError::AccessDenied
            } else if lower.contains("free_mode_cli_required") {
                ProviderError::Other(format!(
                    "freebuff 403 marker: {}",
                    body.chars().take(200).collect::<String>()
                ))
            } else {
                ProviderError::Other(format!(
                    "freebuff 403: {}",
                    body.chars().take(200).collect::<String>()
                ))
            }
        }
        429 => ProviderError::RateLimited {
            retry_after_secs: Some(parse_freebuff_retry_after_secs(body).unwrap_or(900)),
        },
        428 | 410 | 409 => {
            if lower.contains("waiting_room_required")
                || lower.contains("session_expired")
                || lower.contains("session_superseded")
                || lower.contains("session_model_mismatch")
            {
                ProviderError::Other(format!(
                    "freebuff session gate: {}",
                    body.chars().take(200).collect::<String>()
                ))
            } else {
                classify_http_status(status, body)
            }
        }
        _ => classify_http_status(status, body),
    }
}

fn parse_freebuff_retry_after_secs(body: &str) -> Option<u64> {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        if let Some(ms) = v.get("retryAfterMs").and_then(|x| x.as_u64()) {
            if ms > 0 {
                return Some(std::cmp::min(ms / 1000, 6 * 3600));
            }
        }
    }
    let lower = body.to_lowercase();
    let needle = "try again in ";
    let idx = lower.find(needle)?;
    let rest = &lower[idx + needle.len()..];

    let mut total: u64 = 0;
    let mut num: Option<u64> = None;
    let mut matched_unit = false;
    for c in rest.chars() {
        if c.is_ascii_digit() {
            num = Some(num.unwrap_or(0) * 10 + c.to_digit(10).unwrap() as u64);
        } else if c.is_ascii_whitespace() || c == ',' {
            continue;
        } else if c == 'h' {
            if let Some(n) = num.take() {
                total += n * 3600;
                matched_unit = true;
            }
        } else if c == 'm' {
            if let Some(n) = num.take() {
                total += n * 60;
                matched_unit = true;
            }
        } else if c == 's' {
            if let Some(n) = num.take() {
                total += n;
                matched_unit = true;
            }
        } else {
            break;
        }
    }
    if matched_unit && total > 0 {
        Some(std::cmp::min(total, 6 * 3600))
    } else {
        None
    }
}

fn usage_i64(usage: &Value, key: &str) -> Option<i64> {
    usage
        .get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|n| n as i64)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::ChatMessage;

    fn fb_req(model: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: json!("hi"),
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

    #[test]
    fn classify_401_is_auth_invalid() {
        assert!(matches!(
            classify_freebuff_status(401, r#"{"error":"token_invalid"}"#),
            ProviderError::AuthInvalid(_)
        ));
    }

    #[test]
    fn classify_403_banned_is_access_denied() {
        assert!(matches!(
            classify_freebuff_status(403, r#"{"status":"banned"}"#),
            ProviderError::AccessDenied
        ));
        assert!(matches!(
            classify_freebuff_status(403, r#"{"status":"country_blocked"}"#),
            ProviderError::AccessDenied
        ));
    }

    #[test]
    fn classify_403_marker_is_other_not_cut() {
        let err = classify_freebuff_status(403, r#"{"error":"free_mode_cli_required"}"#);
        assert!(
            matches!(&err, ProviderError::Other(msg) if msg.contains("freebuff 403 marker")),
            "free_mode_cli_required must be Other (fallen), got {err:?}"
        );
        assert!(!matches!(
            err,
            ProviderError::AuthInvalid(_) | ProviderError::AccessDenied
        ));
    }

    #[test]
    fn classify_429_parses_retry_after_ms() {
        let err = classify_freebuff_status(429, r#"{"retryAfterMs": 15506639}"#);
        assert!(matches!(
            err,
            ProviderError::RateLimited {
                retry_after_secs: Some(15506)
            }
        ));
    }

    #[test]
    fn classify_429_bare_defaults_900() {
        let err = classify_freebuff_status(429, r#"{"error":"rate limited"}"#);
        assert!(matches!(
            err,
            ProviderError::RateLimited {
                retry_after_secs: Some(900)
            }
        ));
    }

    #[test]
    fn classify_session_gate_errors() {
        let err = classify_freebuff_status(428, r#"{"error":"waiting_room_required"}"#);
        assert!(
            matches!(&err, ProviderError::Other(msg) if msg.starts_with("freebuff session gate:")),
            "428 waiting_room must be session gate Other, got {err:?}"
        );
        let err = classify_freebuff_status(409, r#"{"error":"session_superseded"}"#);
        assert!(matches!(&err, ProviderError::Other(msg) if msg.starts_with("freebuff session gate:")));
        let err = classify_freebuff_status(410, r#"{"error":"session_expired"}"#);
        assert!(matches!(&err, ProviderError::Other(msg) if msg.starts_with("freebuff session gate:")));
        let err = classify_freebuff_status(409, r#"{"error":"session_model_mismatch"}"#);
        assert!(matches!(&err, ProviderError::Other(msg) if msg.starts_with("freebuff session gate:")));
    }

    #[test]
    fn classify_500_falls_through_to_global() {
        assert!(matches!(
            classify_freebuff_status(500, "boom"),
            ProviderError::Upstream { status: 500, .. }
        ));
    }

    #[test]
    fn envelope_prepends_buffy_when_missing() {
        let req = fb_req("fb/deepseek/deepseek-v4-flash");
        let payload = build_upstream_payload(&req, "deepseek/deepseek-v4-flash", "inst-1", "run-1");
        let messages = payload["messages"].as_array().unwrap();
        let system = messages.iter().find(|m| m["role"] == "system").unwrap();
        assert_eq!(system["content"], BUFFY_MARKER);
    }

    #[test]
    fn envelope_no_double_prepend_when_present() {
        let mut req = fb_req("fb/deepseek/deepseek-v4-flash");
        req.messages.insert(
            0,
            ChatMessage {
                role: "system".into(),
                content: json!(format!("{BUFFY_MARKER} Be concise.")),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
        );
        let payload = build_upstream_payload(&req, "deepseek/deepseek-v4-flash", "inst-1", "run-1");
        let messages = payload["messages"].as_array().unwrap();
        let system = messages.iter().find(|m| m["role"] == "system").unwrap();
        let content = system["content"].as_str().unwrap();
        assert!(content.starts_with(BUFFY_MARKER));
        assert_eq!(content.matches(BUFFY_MARKER).count(), 1);
    }

    #[test]
    fn envelope_always_stream_true_and_stop() {
        let req = fb_req("fb/mimo/mimo-v2.5");
        let payload = build_upstream_payload(&req, "mimo/mimo-v2.5", "inst-1", "run-1");
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["stop"], json!(["\"cb_easp\""]));
        assert_eq!(payload["provider"]["data_collection"], "deny");
    }

    #[test]
    fn envelope_dummy_tool_appended() {
        let mut req = fb_req("fb/mimo/mimo-v2.5");
        req.tools = Some(json!([{"type":"function","function":{"name":"my_tool"}}]));
        let payload = build_upstream_payload(&req, "mimo/mimo-v2.5", "inst-1", "run-1");
        let tools = payload["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[1]["function"]["name"], "end_turn");
    }

    #[test]
    fn envelope_codebuff_metadata_fields() {
        let req = fb_req("fb/deepseek/deepseek-v4-flash");
        let payload = build_upstream_payload(&req, "deepseek/deepseek-v4-flash", "inst-42", "run-99");
        let meta = &payload["codebuff_metadata"];
        assert_eq!(meta["freebuff_instance_id"], "inst-42");
        assert_eq!(meta["run_id"], "run-99");
        assert_eq!(meta["cost_mode"], "free");
        assert!(meta["client_id"].as_str().unwrap().len() == 13);
        assert!(meta["trace_session_id"].as_str().unwrap().len() >= 32);
    }

    #[test]
    fn unwrap_sse_data_single_chunk() {
        let wrapped = r#"{"data":{"id":"chatcmpl-1","choices":[{"delta":{"content":"hi"}}],"model":"m"}}"#;
        let unwrapped = unwrap_sse_data(wrapped);
        let v: Value = serde_json::from_str(&unwrapped).unwrap();
        assert_eq!(v["id"], "chatcmpl-1");
        assert_eq!(v["choices"][0]["delta"]["content"], "hi");
    }

    #[test]
    fn unwrap_sse_data_plain_chunk_passthrough() {
        let plain = r#"{"id":"chatcmpl-2","choices":[{"delta":{"content":"x"}}]}"#;
        let unwrapped = unwrap_sse_data(plain);
        let v: Value = serde_json::from_str(&unwrapped).unwrap();
        assert_eq!(v["id"], "chatcmpl-2");
    }

    #[test]
    fn unwrap_sse_data_done_passthrough() {
        let result = unwrap_sse_data("[DONE]");
        assert_eq!(result, "[DONE]");
    }

    #[test]
    fn reasoning_effort_clamped() {
        assert_eq!(
            clamp_reasoning_effort("deepseek/deepseek-v4-flash", "medium"),
            "low"
        );
        assert_eq!(
            clamp_reasoning_effort("deepseek/deepseek-v4-flash", "max"),
            "max"
        );
        assert_eq!(
            clamp_reasoning_effort("openai/gpt-5.6-luna", "ultra"),
            "max"
        );
        assert_eq!(
            clamp_reasoning_effort("unknown/model", "high"),
            "high"
        );
    }

    #[test]
    fn session_cache_eviction_logic() {
        let provider = FreebuffProvider::new();
        let key = ("acc1".to_string(), "m1".to_string());
        let entry = provider.cache_session(key.clone(), "inst-1".to_string());
        assert!(entry.is_alive(now_ms()));

        let cached = provider.get_cached_session(&key);
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().instance_id, "inst-1");

        provider.evict_all_for_account("acc1");
        assert!(provider.get_cached_session(&key).is_none());
    }

    #[test]
    fn session_entry_expires_after_ttl() {
        let entry = SessionEntry {
            instance_id: "x".into(),
            created_at: now_ms() - SESSION_TTL_MS - 1000,
            last_used: now_ms() - SESSION_TTL_MS - 1000,
            run_id: None,
            pruner_run_id: None,
        };
        assert!(!entry.is_alive(now_ms()));
    }

    #[test]
    fn agent_id_mapping() {
        assert_eq!(agent_id_for_model("deepseek/deepseek-v4-flash"), "base2-free-deepseek-flash");
        assert_eq!(agent_id_for_model("deepseek/deepseek-v4-pro"), "base2-free-deepseek");
        assert_eq!(agent_id_for_model("mimo/mimo-v2.5"), "base2-free-mimo");
        assert_eq!(agent_id_for_model("minimax/minimax-m3"), "base2-free-minimax-m3");
        assert_eq!(agent_id_for_model("z-ai/glm-5.2"), "base2-free-glm");
        assert_eq!(agent_id_for_model("openai/gpt-5.6-luna"), "base2-free-luna");
        assert_eq!(agent_id_for_model("other/model"), "base2-free");
    }

    #[test]
    fn token_uid_parse() {
        let data = json!({"token": "cb_abc123", "uid": "u-1"});
        assert_eq!(FreebuffProvider::token_of(&data), Some("cb_abc123".into()));

        let data = json!({"token": "  "});
        assert_eq!(FreebuffProvider::token_of(&data), None);

        let data = json!({});
        assert_eq!(FreebuffProvider::token_of(&data), None);
    }

    #[tokio::test]
    async fn ensure_fresh_auth_requires_token() {
        let provider = FreebuffProvider::new();
        let mut acc = Account {
            id: "fb1".into(),
            provider: "freebuff".into(),
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

        acc.data = json!({"token": "cb_test-token-123"}).to_string();
        assert!(provider.ensure_fresh_auth(&mut acc).await.is_ok());
    }
}
