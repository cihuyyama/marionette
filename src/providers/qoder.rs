//! Qoder provider — full port from etteeum qoder.ts
//! Auth: PAT -> jobToken -> COSY bearer. Chat: SSE -> OpenAI chunks.

use super::{ChatOutcome, Provider, StreamUsage, classify_http_status};
use crate::db::Account;
use crate::error::ProviderError;
use crate::openai::ChatCompletionRequest;
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use md5::{Md5, Digest as Md5Digest};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use uuid::Uuid;
use sha2::Sha256;

const COSY_VERSION: &str = "1.0.22";
const APPCODE: &str = "cosy";
const SIG_SECRET: &str = "d2FyLCB3YXIgbmV2ZXIgY2hhbmdlcw==";
const JOB_TOKEN_URL: &str = "https://center.qoder.sh/algo/api/v3/user/jobToken?Encode=1";
const CHAT_URL: &str = "https://api2.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1";

const SERVER_PUBKEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc
4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l
6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17
XcW+ML9FoCI6AOvOzwIDAQAB
-----END PUBLIC KEY-----";

const CUSTOM_ALPHABET: &[u8] = b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";
const STD_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const CUSTOM_PAD: u8 = b'$';

fn build_s2c() -> [u8; 128] {
    let mut table = [0u8; 128];
    for i in 0..64 {
        let s = STD_ALPHABET[i] as usize;
        table[s] = CUSTOM_ALPHABET[i];
    }
    table[b'=' as usize] = CUSTOM_PAD;
    table
}

pub fn encode_qoder_payload(data: &[u8]) -> String {
    let s2c = build_s2c();
    let std = B64.encode(data);
    let n = std.len();
    let a = n / 3;
    let rearranged = format!("{}{}{}", &std[n - a..], &std[a..n - a], &std[..a]);
    rearranged
        .bytes()
        .map(|b| {
            let idx = b as usize;
            if idx < 128 && s2c[idx] != 0 { s2c[idx] as char } else { b as char }
        })
        .collect()
}

fn md5_hex(s: &str) -> String {
    let mut h = Md5::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

fn rfc1123_date() -> String {
    chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

fn sign_signature_header(date: &str) -> String {
    md5_hex(&format!("{}&{}&{}", APPCODE, SIG_SECRET, date))
}

fn rsa_encrypt_key(temp_key: &[u8]) -> Result<Vec<u8>, ProviderError> {
    use rsa::{RsaPublicKey, Pkcs1v15Encrypt};
    use rsa::pkcs8::DecodePublicKey;
    let pubkey = RsaPublicKey::from_public_key_pem(SERVER_PUBKEY_PEM)
        .map_err(|e| ProviderError::Other(format!("RSA key parse: {e}")))?;
    let mut rng = rand::thread_rng();
    pubkey.encrypt(&mut rng, Pkcs1v15Encrypt, temp_key)
        .map_err(|e| ProviderError::Other(format!("RSA encrypt: {e}")))
}

fn aes_128_cbc_encrypt(plain: &[u8], key: &[u8]) -> Result<Vec<u8>, ProviderError> {
    use aes::Aes128;
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
    type Aes128CbcEnc = cbc::Encryptor<Aes128>;
    if key.len() != 16 {
        return Err(ProviderError::Other("AES key must be 16 bytes".into()));
    }
    let encryptor = Aes128CbcEnc::new_from_slices(key, key)
        .map_err(|e| ProviderError::Other(format!("AES init: {e}")))?;
    Ok(encryptor.encrypt_padded_vec_mut::<Pkcs7>(plain))
}

struct ModelCfg {
    key: &'static str,
    display_name: &'static str,
    max_input_tokens: u64,
    is_vl: bool,
    is_reasoning: bool,
}

fn model_cfg(name: &str) -> ModelCfg {
    match name.to_lowercase().as_str() {
        "auto" => ModelCfg {
            key: "auto",
            display_name: "Auto",
            max_input_tokens: 180_000,
            is_vl: true,
            is_reasoning: false,
        },
        "ultimate" => ModelCfg {
            key: "ultimate",
            display_name: "Ultimate",
            max_input_tokens: 180_000,
            is_vl: true,
            is_reasoning: true,
        },
        "performance" => ModelCfg {
            key: "performance",
            display_name: "Performance",
            max_input_tokens: 272_000,
            is_vl: true,
            is_reasoning: false,
        },
        "efficient" => ModelCfg {
            key: "efficient",
            display_name: "Efficient",
            max_input_tokens: 180_000,
            is_vl: true,
            is_reasoning: false,
        },
        "qmodel_latest" | "qwen3.7-max" => ModelCfg {
            key: "qmodel_latest",
            display_name: "Qwen3.7-Max",
            max_input_tokens: 1_000_000,
            is_vl: true,
            is_reasoning: false,
        },
        "qmodel" | "qwen3.6-plus" => ModelCfg {
            key: "qmodel",
            display_name: "Qwen3.6-Plus",
            max_input_tokens: 180_000,
            is_vl: true,
            is_reasoning: false,
        },
        "dmodel" | "deepseek-v4-pro" => ModelCfg {
            key: "dmodel",
            display_name: "DeepSeek-V4-Pro",
            max_input_tokens: 180_000,
            is_vl: true,
            is_reasoning: true,
        },
        "dfmodel" | "deepseek-v4-flash" => ModelCfg {
            key: "dfmodel",
            display_name: "DeepSeek-V4-Flash",
            max_input_tokens: 180_000,
            is_vl: true,
            is_reasoning: true,
        },
        "gm51model" | "glm-5.1" => ModelCfg {
            key: "gm51model",
            display_name: "GLM-5.1",
            max_input_tokens: 180_000,
            is_vl: true,
            is_reasoning: true,
        },
        "kmodel" | "kimi-k2.6" => ModelCfg {
            key: "kmodel",
            display_name: "Kimi-K2.6",
            max_input_tokens: 256_000,
            is_vl: true,
            is_reasoning: false,
        },
        "mmodel" | "minimax-m2.7" => ModelCfg {
            key: "mmodel",
            display_name: "MiniMax-M2.7",
            max_input_tokens: 180_000,
            is_vl: true,
            is_reasoning: false,
        },
        _ => ModelCfg {
            key: "lite",
            display_name: "Lite",
            max_input_tokens: 180_000,
            is_vl: false,
            is_reasoning: false,
        },
    }
}

#[derive(Debug, Clone)]
struct QoderTokens {
    personal_token: String,
    security_oauth_token: Option<String>,
    refresh_token: Option<String>,
    user_id: Option<String>,
    user_name: Option<String>,
    user_type: Option<String>,
    expire_time: Option<u64>,
    machine_id: String,
    machine_token: String,
    machine_type: String,
    machine_backfilled: bool,
}

impl QoderTokens {
    fn effective_data(data: &Value) -> Value {
        let mut out = serde_json::Map::new();
        if let Some(obj) = data.as_object() {
            for (k, v) in obj {
                if k == "providerSpecificData" {
                    continue;
                }
                out.insert(k.clone(), v.clone());
            }
        }
        if let Some(psd) = data
            .get("providerSpecificData")
            .and_then(|v| v.as_object())
        {
            for (k, v) in psd {
                let overwrite = !out.contains_key(k)
                    || out
                        .get(k)
                        .and_then(|x| x.as_str())
                        .map(|s| s.is_empty())
                        .unwrap_or(true);
                if overwrite {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
        if !out.contains_key("securityOauthToken")
            || out
                .get("securityOauthToken")
                .and_then(|v| v.as_str())
                .map(|s| s.is_empty())
                .unwrap_or(true)
        {
            if let Some(at) = out
                .get("accessToken")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                out.insert("securityOauthToken".into(), json!(at));
            }
        }
        Value::Object(out)
    }

    fn from_data(data: &Value) -> Result<Self, ProviderError> {
        let data = Self::effective_data(data);
        let pt = data
            .get("personalToken")
            .or_else(|| data.get("personal_token"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::AuthInvalid("missing personalToken".into()))?
            .to_string();
        let mid = data
            .get("machineId")
            .or_else(|| data.get("machine_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let mt = data
            .get("machineToken")
            .or_else(|| data.get("machine_token"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| mid.clone());
        let had_machine = data
            .get("machineId")
            .or_else(|| data.get("machine_id"))
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        Ok(Self {
            personal_token: pt,
            security_oauth_token: data
                .get("securityOauthToken")
                .or_else(|| data.get("security_oauth_token"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            refresh_token: data
                .get("refreshToken")
                .or_else(|| data.get("refresh_token"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            user_id: data
                .get("userId")
                .or_else(|| data.get("user_id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            user_name: data
                .get("userName")
                .or_else(|| data.get("user_name"))
                .or_else(|| data.get("displayName"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            user_type: data
                .get("userType")
                .or_else(|| data.get("user_type"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            expire_time: data
                .get("expireTime")
                .or_else(|| data.get("expire_time"))
                .and_then(|v| v.as_u64()),
            machine_id: mid,
            machine_token: mt,
            machine_type: data
                .get("machineType")
                .or_else(|| data.get("machine_type"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "5".into()),
            machine_backfilled: !had_machine,
        })
    }
    fn to_data(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("personalToken".into(), json!(self.personal_token));
        if let Some(ref v) = self.security_oauth_token { m.insert("securityOauthToken".into(), json!(v)); }
        if let Some(ref v) = self.refresh_token { m.insert("refreshToken".into(), json!(v)); }
        if let Some(ref v) = self.user_id { m.insert("userId".into(), json!(v)); }
        if let Some(ref v) = self.user_name { m.insert("userName".into(), json!(v)); }
        if let Some(ref v) = self.user_type { m.insert("userType".into(), json!(v)); }
        if let Some(v) = self.expire_time { m.insert("expireTime".into(), json!(v)); }
        m.insert("machineId".into(), json!(self.machine_id));
        m.insert("machineToken".into(), json!(self.machine_token));
        m.insert("machineType".into(), json!(self.machine_type));
        Value::Object(m)
    }
}

#[derive(Debug, Deserialize)]
struct JobTokenResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, alias = "securityOauthToken")]
    security_oauth_token: Option<String>,
    #[serde(default, alias = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(default, alias = "expireTime")]
    #[allow(dead_code)]
    expire_time: Option<u64>,
    #[serde(default, alias = "userType")]
    user_type: Option<String>,
}

fn signature_headers(tokens: &QoderTokens) -> reqwest::header::HeaderMap {
    let date = rfc1123_date();
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("cosy-machinetoken", tokens.machine_token.parse().unwrap());
    h.insert("cosy-machinetype", tokens.machine_type.parse().unwrap());
    h.insert("login-version", "v2".parse().unwrap());
    h.insert("appcode", APPCODE.parse().unwrap());
    h.insert("accept", "application/json".parse().unwrap());
    h.insert("accept-encoding", "identity".parse().unwrap());
    h.insert("cosy-version", COSY_VERSION.parse().unwrap());
    h.insert("cosy-clienttype", "5".parse().unwrap());
    h.insert("date", date.parse().unwrap());
    h.insert("signature", sign_signature_header(&date).parse().unwrap());
    h.insert("content-type", "application/json".parse().unwrap());
    h.insert("cosy-machineid", tokens.machine_id.parse().unwrap());
    h.insert("user-agent", "Go-http-client/2.0".parse().unwrap());
    h
}

struct CosySession {
    cosy_key: String,
    info: String,
}

fn build_cosy_session(tokens: &QoderTokens) -> Result<CosySession, ProviderError> {
    let temp_key_str = Uuid::new_v4().to_string().replace('-', "");
    let temp_key = &temp_key_str.as_bytes()[..16];
    let cosy_key = B64.encode(rsa_encrypt_key(temp_key)?);
    let identity = json!({
        "name": tokens.user_name.as_deref().unwrap_or(""),
        "aid": tokens.user_id.as_deref().unwrap_or(""),
        "uid": tokens.user_id.as_deref().unwrap_or(""),
        "yx_uid": "",
        "organization_id": "",
        "organization_name": "",
        "user_type": tokens.user_type.as_deref().unwrap_or("personal_standard"),
        "security_oauth_token": tokens.security_oauth_token.as_deref().unwrap_or(""),
        "refresh_token": tokens.refresh_token.as_deref().unwrap_or("")
    });
    let encrypted = aes_128_cbc_encrypt(identity.to_string().as_bytes(), temp_key)?;
    let info = B64.encode(&encrypted);
    Ok(CosySession { cosy_key, info })
}

fn build_payload_b64(info: &str) -> String {
    let m = json!({
        "version": "v1",
        "requestId": Uuid::new_v4().to_string(),
        "info": info,
        "cosyVersion": COSY_VERSION,
        "ideVersion": ""
    });
    B64.encode(m.to_string().as_bytes())
}

fn path_sig_from_url(url: &str) -> String {
    if let Some(idx) = url.find("/algo") {
        url[idx + 5..].split('?').next().unwrap_or("").to_string()
    } else if let Some(idx) = url.find("//") {
        let after_host = &url[idx + 2..];
        if let Some(pidx) = after_host.find('/') {
            after_host[pidx..].split('?').next().unwrap_or("").to_string()
        } else { String::new() }
    } else { String::new() }
}

fn sign_bearer_request(payload_b64: &str, cosy_key: &str, cosy_date: &str, body: &str, path_sig: &str) -> String {
    md5_hex(&format!("{}\n{}\n{}\n{}\n{}", payload_b64, cosy_key, cosy_date, body, path_sig))
}

pub struct QoderProvider {
    /// Default client (HTTP/2 allowed). Used for jobToken + first chat attempt.
    client: Client,
    /// HTTP/1.1-only fallback. Some Qoder SSE paths die mid-stream on H2
    /// (`stream error … unexpected internal error`) while H1 still works (or vice versa).
    client_h1: Client,
}

impl QoderProvider {
    pub fn new() -> Self {
        let common = || {
            Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .connect_timeout(std::time::Duration::from_secs(30))
                // Short idle: stale pooled conns often surface as H2 stream errors on SSE.
                .pool_idle_timeout(std::time::Duration::from_secs(15))
                .pool_max_idle_per_host(2)
                .tcp_keepalive(std::time::Duration::from_secs(30))
                .tcp_nodelay(true)
        };
        // HTTP/2 preferred first: pure H1-only previously hit hyper chunked UnexpectedEof.
        let client = common().build().expect("qoder http client");
        let client_h1 = common()
            .http1_only()
            .build()
            .expect("qoder http1 client");
        Self { client, client_h1 }
    }

    fn format_reqwest_err(e: &reqwest::Error) -> String {
        let mut parts = vec![format!("{e}")];
        let mut src = std::error::Error::source(e);
        while let Some(s) = src {
            parts.push(format!("{s}"));
            src = s.source();
        }
        parts.join(" | ")
    }

    /// Transport deaths while reading Qoder SSE body (H1 chunked EOF or H2 stream reset).
    fn is_sse_transport_error(e: &reqwest::Error) -> bool {
        let full = Self::format_reqwest_err(e).to_ascii_lowercase();
        full.contains("unexpectedeof")
            || full.contains("unexpected eof")
            || full.contains("chunk size line")
            || full.contains("unexpected internal error")
            || full.contains("stream error")
            || full.contains("error decoding response body")
            || full.contains("error reading a body")
            || full.contains("connection reset")
            || full.contains("broken pipe")
    }

    fn chat_clients(&self) -> [&Client; 2] {
        // H2 first, then H1-only retry (covers both failure modes).
        [&self.client, &self.client_h1]
    }

    async fn do_job_token(&self, tokens: &QoderTokens) -> Result<JobTokenResponse, ProviderError> {
        let inner = json!({
            "personalToken": tokens.personal_token,
            "securityOauthToken": tokens.security_oauth_token.as_deref().unwrap_or(""),
            "refreshToken": tokens.refresh_token.as_deref().unwrap_or(""),
            "needRefresh": tokens.refresh_token.is_some(),
            "authInfo": {}
        });
        let outer = json!({
            "payload": inner.to_string(),
            "encodeVersion": "1"
        });
        let body = encode_qoder_payload(outer.to_string().as_bytes());
        let resp = self.client
            .post(JOB_TOKEN_URL)
            .headers(signature_headers(tokens))
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 200 {
            return Err(classify_http_status(status, &text));
        }
        serde_json::from_str(&text)
            .map_err(|e| ProviderError::Transport(format!("jobToken parse: {e}")))
    }
}

#[async_trait]
impl Provider for QoderProvider {
    fn id(&self) -> &'static str {
        "qoder"
    }

    async fn ensure_fresh_auth(&self, account: &mut Account) -> Result<(), ProviderError> {
        let data: Value = serde_json::from_str(&account.data).unwrap_or(Value::Null);
        let mut tokens = QoderTokens::from_data(&data)?;
        let mut dirty = tokens.machine_backfilled;

        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let expired = tokens
            .expire_time
            .map(|exp| exp <= now_ms + 60_000)
            .unwrap_or(false);

        let needs_refresh = tokens.security_oauth_token.is_none()
            || tokens.security_oauth_token.as_deref() == Some("")
            || tokens.user_id.is_none()
            || tokens.user_id.as_deref() == Some("")
            || expired;

        if needs_refresh {
            let job = self.do_job_token(&tokens).await?;
            if let Some(sot) = job.security_oauth_token {
                tokens.security_oauth_token = Some(sot);
            }
            if let Some(rt) = job.refresh_token {
                tokens.refresh_token = Some(rt);
            }
            if let Some(uid) = job.id {
                tokens.user_id = Some(uid);
            }
            if let Some(name) = job.name {
                tokens.user_name = Some(name);
            }
            if let Some(ut) = job.user_type {
                tokens.user_type = Some(ut);
            }
            if let Some(exp) = job.expire_time {
                tokens.expire_time = Some(exp);
            }
            dirty = true;
        }
        if dirty {
            account.data = tokens.to_data().to_string();
        }
        Ok(())
    }

    async fn chat(
        &self,
        account: &Account,
        req: &ChatCompletionRequest,
    ) -> Result<ChatOutcome, ProviderError> {
        let data: serde_json::Value = serde_json::from_str(&account.data).unwrap_or(serde_json::Value::Null);
        let tokens = QoderTokens::from_data(&data)?;
        if tokens.security_oauth_token.is_none()
            || tokens.security_oauth_token.as_deref() == Some("")
        {
            return Err(ProviderError::AuthExpired);
        }

        let req_model = req.model.clone();
        let stream_mode = req.stream_enabled();
        let estimated_prompt = estimate_prompt_tokens(req);
        let mut last_err: Option<ProviderError> = None;

        for (attempt, client) in self.chat_clients().into_iter().enumerate() {
            let attempt_label = if attempt == 0 { "h2-preferred" } else { "http1-only" };

            let session = match build_cosy_session(&tokens) {
                Ok(s) => s,
                Err(e) => return Err(e),
            };
            let payload_b64 = build_payload_b64(&session.info);
            let cosy_date = format!("{}", chrono::Utc::now().timestamp());
            let cfg = model_cfg(req.upstream_model());
            let model = cfg.key.to_string();
            let body = build_chat_body(req, &cfg);
            let body_str = serde_json::to_string(&body).unwrap_or_default();
            let body_encoded = encode_qoder_payload(body_str.as_bytes());
            let path_sig = path_sig_from_url(CHAT_URL);
            let bearer_sig = sign_bearer_request(
                &payload_b64,
                &session.cosy_key,
                &cosy_date,
                &body_encoded,
                &path_sig,
            );
            let bearer = format!("COSY.{payload_b64}.{bearer_sig}");

            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert("cosy-data-policy", "agree".parse().unwrap());
            headers.insert("cosy-machinetype", "5".parse().unwrap());
            headers.insert("cosy-clienttype", "5".parse().unwrap());
            headers.insert("cosy-date", cosy_date.parse().unwrap());
            headers.insert(
                "cosy-user",
                tokens
                    .user_id
                    .as_deref()
                    .unwrap_or("")
                    .parse()
                    .unwrap_or_else(|_| "".parse().unwrap()),
            );
            headers.insert("cosy-key", session.cosy_key.parse().unwrap());
            headers.insert("cache-control", "no-cache".parse().unwrap());
            headers.insert("cosy-business-product", "cli".parse().unwrap());
            headers.insert("cosy-business-type", "agent".parse().unwrap());
            headers.insert("cosy-scene", "assistant".parse().unwrap());
            headers.insert("accept", "text/event-stream".parse().unwrap());
            headers.insert(
                "authorization",
                format!("Bearer {bearer}").parse().unwrap(),
            );
            headers.insert("accept-encoding", "identity".parse().unwrap());
            headers.insert("cosy-version", COSY_VERSION.parse().unwrap());
            headers.insert("cosy-machineid", tokens.machine_id.parse().unwrap());
            headers.insert("cosy-machinetoken", tokens.machine_token.parse().unwrap());
            headers.insert("login-version", "v2".parse().unwrap());
            headers.insert("user-agent", "Go-http-client/2.0".parse().unwrap());
            headers.insert("content-type", "application/json".parse().unwrap());
            headers.insert("x-model-key", model.parse().unwrap());
            headers.insert("x-model-source", "system".parse().unwrap());

            let resp = match client
                .post(CHAT_URL)
                .headers(headers)
                .body(body_encoded)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    let chain = Self::format_reqwest_err(&e);
                    tracing::warn!(attempt = attempt_label, error = %chain, "qoder send failed");
                    last_err = Some(ProviderError::Transport(format!("qoder send: {chain}")));
                    if attempt + 1 < 2 {
                        continue;
                    }
                    return Err(last_err.unwrap());
                }
            };

            let status = resp.status().as_u16();
            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let content_encoding = resp
                .headers()
                .get(reqwest::header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let transfer_encoding = resp
                .headers()
                .get(reqwest::header::TRANSFER_ENCODING)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            if status != 200 {
                let text = resp.text().await.unwrap_or_default();
                return Err(classify_http_status(status, &text));
            }

            let http_version = format!("{:?}", resp.version());
            let decode_ctx = format!(
                "ct={content_type} ce={content_encoding} te={transfer_encoding} http={http_version} attempt={attempt_label}"
            );
            tracing::debug!(
                status,
                content_type = %content_type,
                http_version = %http_version,
                attempt = attempt_label,
                "qoder chat upstream ok"
            );

            if stream_mode {
                let mut upstream_stream = resp.bytes_stream();
                // Probe first body chunk BEFORE returning SSE to client.
                let first = tokio::time::timeout(
                    std::time::Duration::from_secs(45),
                    futures_util::StreamExt::next(&mut upstream_stream),
                )
                .await;

                let first_chunk: Option<bytes::Bytes> = match first {
                    Ok(Some(Ok(c))) => {
                        let text = String::from_utf8_lossy(&c);
                        if text.trim().is_empty() {
                            // If Qoder replies HTTP 200 but sends an empty first chunk right away,
                            // it means silent reject (EOF) due to quota/context limit.
                            tracing::warn!("Qoder silent reject detected (empty first chunk). Returning RateLimited 429 to pool.");
                            return Err(ProviderError::RateLimited { retry_after_secs: None });
                        }
                        Some(c)
                    },
                    Ok(Some(Err(e))) => {
                        let chain = Self::format_reqwest_err(&e);
                        tracing::warn!(
                            attempt = attempt_label,
                            error = %chain,
                            ctx = %decode_ctx,
                            "qoder stream first-chunk decode failed"
                        );
                        if Self::is_sse_transport_error(&e) && attempt + 1 < 2 {
                            last_err = Some(ProviderError::Transport(format!(
                                "qoder sse body decode: {chain} ({decode_ctx})"
                            )));
                            continue;
                        }
                        return Err(ProviderError::Transport(format!(
                            "qoder sse body decode: {chain} ({decode_ctx})"
                        )));
                    }
                    Ok(None) => {
                        // Qoder closed connection with empty body immediately (silent context reject)
                        tracing::warn!("Qoder silent reject detected (EOF no chunks). Returning RateLimited 429 to pool.");
                        return Err(ProviderError::RateLimited { retry_after_secs: None });
                    }
                    Err(_) => {
                        if attempt + 1 < 2 {
                            last_err = Some(ProviderError::Transport(format!(
                                "qoder sse idle timeout waiting first chunk ({decode_ctx})"
                            )));
                            continue;
                        }
                        return Err(ProviderError::Transport(format!(
                            "qoder sse idle timeout waiting first chunk ({decode_ctx})"
                        )));
                    }
                };

                let (tx, rx) =
                    tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);
                let (usage_tx, usage_rx) = oneshot::channel::<Option<StreamUsage>>();
                let decode_ctx_stream = decode_ctx.clone();
                let req_model_stream = req_model.clone();
                let estimated_prompt_stream = estimated_prompt;

                tokio::spawn(async move {
                    let mut buffer = String::new();
                    if let Some(c) = first_chunk {
                        buffer.push_str(&String::from_utf8_lossy(&c));
                    }
                    let resp_id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
                    let mut first_chunk_sent = false;
                    let mut any_content = false;
                    let mut prompt_tokens: i64 = 0;
                    let mut completion_tokens: i64 = 0;
                    let mut total_tokens: i64 = 0;
                    let mut accumulated_text = String::new();
                    let mut usage_tx = Some(usage_tx);

                    loop {
                        while let Some(pos) = buffer.find('\n') {
                            let line = buffer[..pos].trim_end_matches('\r').to_string();
                            buffer = buffer[pos + 1..].to_string();
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                continue;
                            }
                            if let Some(svc_err) = parse_qoder_service_error(trimmed) {
                                tracing::warn!(error = %svc_err, "qoder upstream error in SSE");
                                let _ = tx
                                    .send(Err(std::io::Error::new(
                                        std::io::ErrorKind::Other,
                                        svc_err,
                                    )))
                                    .await;
                                if let Some(txu) = usage_tx.take() {
                                    let _ = txu.send(None);
                                }
                                return;
                            }
                            if let Some(inner) = parse_sse_line(trimmed) {
                                if let Some(u) = inner.get("usage") {
                                    if let Some(p) = usage_i64(u, "prompt_tokens")
                                        .or_else(|| usage_i64(u, "input_tokens"))
                                    {
                                        prompt_tokens = p;
                                    }
                                    if let Some(c) = usage_i64(u, "completion_tokens")
                                        .or_else(|| usage_i64(u, "output_tokens"))
                                    {
                                        completion_tokens = c;
                                    }
                                    if let Some(t) = usage_i64(u, "total_tokens") {
                                        total_tokens = t;
                                    } else if prompt_tokens > 0 || completion_tokens > 0 {
                                        total_tokens = prompt_tokens + completion_tokens;
                                    }
                                }

                                if !first_chunk_sent {
                                    first_chunk_sent = true;
                                    let chunk_json = serde_json::json!({
                                        "id": resp_id,
                                        "object": "chat.completion.chunk",
                                        "created": chrono::Utc::now().timestamp(),
                                        "model": req_model_stream,
                                        "choices": [{
                                            "index": 0,
                                            "delta": { "role": "assistant", "content": "" },
                                            "finish_reason": serde_json::Value::Null
                                        }]
                                    });
                                    let msg = format!(
                                        "data: {}\n\n",
                                        serde_json::to_string(&chunk_json).unwrap()
                                    );
                                    if tx.send(Ok(bytes::Bytes::from(msg))).await.is_err() {
                                        if let Some(txu) = usage_tx.take() {
                                            let _ = txu.send(None);
                                        }
                                        return;
                                    }
                                }

                                let choice = inner
                                    .get("choices")
                                    .and_then(|c| c.as_array())
                                    .and_then(|a| a.first());
                                let delta = choice.and_then(|c| c.get("delta"));
                                let finish_reason = choice.and_then(|c| c.get("finish_reason"));
                                let is_finish = finish_reason
                                    .map(|v| !v.is_null())
                                    .unwrap_or(false);

                                let mut delta_out = serde_json::json!({});
                                let mut has_delta = false;

                                if let Some(content) = delta
                                    .and_then(|d| d.get("content"))
                                    .and_then(|v| v.as_str())
                                {
                                    if !content.is_empty() {
                                        let skip_echo = is_finish
                                            && any_content
                                            && !accumulated_text.is_empty()
                                            && (content == accumulated_text
                                                || content.starts_with(accumulated_text.as_str())
                                                || accumulated_text.starts_with(content));
                                        if !skip_echo {
                                            delta_out["content"] = serde_json::json!(content);
                                            has_delta = true;
                                            any_content = true;
                                            accumulated_text.push_str(content);
                                        }
                                    }
                                }
                                if let Some(reasoning) = delta
                                    .and_then(|d| d.get("reasoning_content"))
                                    .and_then(|v| v.as_str())
                                {
                                    if !reasoning.is_empty() && !(is_finish && any_content) {
                                        delta_out["reasoning_content"] =
                                            serde_json::json!(reasoning);
                                        has_delta = true;
                                        any_content = true;
                                        if accumulated_text.is_empty() {
                                            accumulated_text.push_str(reasoning);
                                        }
                                    }
                                }

                                if has_delta {
                                    let chunk_json = serde_json::json!({
                                        "id": resp_id,
                                        "object": "chat.completion.chunk",
                                        "created": chrono::Utc::now().timestamp(),
                                        "model": req_model_stream,
                                        "choices": [{
                                            "index": 0,
                                            "delta": delta_out,
                                            "finish_reason": serde_json::Value::Null
                                        }]
                                    });
                                    let msg = format!(
                                        "data: {}\n\n",
                                        serde_json::to_string(&chunk_json).unwrap()
                                    );
                                    if tx.send(Ok(bytes::Bytes::from(msg))).await.is_err() {
                                        if let Some(txu) = usage_tx.take() {
                                            let _ = txu.send(None);
                                        }
                                        return;
                                    }
                                }

                                if is_finish {
                                    if !any_content {
                                        tracing::warn!("Qoder silent reject mid-stream (no content generated). Marking finish_reason as 'length'.");
                                    }
                                    qoder_emit_stream_end(
                                        &tx,
                                        &resp_id,
                                        &req_model_stream,
                                        prompt_tokens,
                                        completion_tokens,
                                        total_tokens,
                                        estimated_prompt_stream,
                                        &accumulated_text,
                                        if any_content { "stop" } else { "length" },
                                        &mut usage_tx,
                                    )
                                    .await;
                                    return;
                                }
                            }
                        }

                        match futures_util::StreamExt::next(&mut upstream_stream).await {
                            Some(Ok(c)) => {
                                buffer.push_str(&String::from_utf8_lossy(&c));
                            }
                            Some(Err(e)) => {
                                let chain = QoderProvider::format_reqwest_err(&e);
                                tracing::warn!(
                                    error = %chain,
                                    ctx = %decode_ctx_stream,
                                    first_chunk_sent,
                                    any_content,
                                    "qoder stream body decode failed"
                                );
                                if first_chunk_sent || any_content {
                                    let eof_finish = if any_content { "stop" } else { "length" };
                                    if !any_content {
                                        tracing::warn!("Qoder silent reject at EOF (no content). Marking fallback finish_reason as 'length'.");
                                    }
                                    qoder_emit_stream_end(
                                        &tx,
                                        &resp_id,
                                        &req_model_stream,
                                        prompt_tokens,
                                        completion_tokens,
                                        total_tokens,
                                        estimated_prompt_stream,
                                        &accumulated_text,
                                        eof_finish,
                                        &mut usage_tx,
                                    )
                                    .await;
                                } else {
                                    let _ = tx
                                        .send(Err(std::io::Error::new(
                                            std::io::ErrorKind::UnexpectedEof,
                                            format!(
                                                "qoder sse body decode: {chain} ({decode_ctx_stream})"
                                            ),
                                        )))
                                        .await;
                                    if let Some(txu) = usage_tx.take() {
                                        let _ = txu.send(None);
                                    }
                                }
                                return;
                            }
                            None => break,
                        }
                    }

                    let eof_finish = if any_content { "stop" } else { "length" };
                    if !any_content {
                        tracing::warn!("Qoder silent reject at EOF (no content). Marking fallback finish_reason as 'length'.");
                    }
                    qoder_emit_stream_end(
                        &tx,
                        &resp_id,
                        &req_model_stream,
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                        estimated_prompt_stream,
                        &accumulated_text,
                        eof_finish,
                        &mut usage_tx,
                    )
                    .await;
                });

                let body = axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
                let mut out_headers = axum::http::HeaderMap::new();
                out_headers.insert("content-type", axum::http::HeaderValue::from_static("text/event-stream"));
                out_headers.insert("cache-control", axum::http::HeaderValue::from_static("no-cache"));
                out_headers.insert("connection", axum::http::HeaderValue::from_static("keep-alive"));
                let response = axum::response::Response::builder()
                    .status(axum::http::StatusCode::OK)
                    .body(body)
                    .map_err(|e| ProviderError::Other(e.to_string()))?;
                let (mut parts, body) = response.into_parts();
                parts.headers = out_headers;
                return Ok(ChatOutcome::Stream {
                    response: axum::response::Response::from_parts(parts, body),
                    usage_rx,
                });
            }

            // Non-stream
            let mut buffer = String::new();
            let resp_id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
            let mut accumulated_text = String::new();
            let mut accumulated_reasoning = String::new();
            let mut prompt_tokens: i64 = 0;
            let mut completion_tokens: i64 = 0;
            let mut total_tokens: i64 = 0;
            let mut upstream_stream = resp.bytes_stream();
            let mut stream_eof_partial = false;

            const IDLE_SECS: u64 = 45;
            loop {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(IDLE_SECS),
                    futures_util::StreamExt::next(&mut upstream_stream),
                )
                .await
                {
                    Ok(Some(Ok(chunk))) => {
                        buffer.push_str(&String::from_utf8_lossy(&chunk));
                    }
                    Ok(Some(Err(e))) => {
                        let chain = Self::format_reqwest_err(&e);
                        if Self::is_sse_transport_error(&e) && !buffer.is_empty() {
                            tracing::warn!(
                                error = %chain,
                                ctx = %decode_ctx,
                                buffer_len = buffer.len(),
                                "qoder non-stream body error; using partial SSE body"
                            );
                            stream_eof_partial = true;
                            break;
                        }
                        if Self::is_sse_transport_error(&e) && buffer.is_empty() && attempt + 1 < 2
                        {
                            tracing::warn!(
                                attempt = attempt_label,
                                error = %chain,
                                ctx = %decode_ctx,
                                "qoder non-stream empty-body transport error; retrying"
                            );
                            last_err = Some(ProviderError::Transport(format!(
                                "qoder sse body decode: {chain} ({decode_ctx})"
                            )));
                            buffer.clear();
                            stream_eof_partial = false;
                            break; // breaks inner match, falls through to next iter
                        }
                        return Err(ProviderError::Transport(format!(
                            "qoder sse body decode: {chain} ({decode_ctx})"
                        )));
                    }
                    Ok(None) => break,
                    Err(_) => {
                        if buffer.is_empty() {
                            if attempt + 1 < 2 {
                                last_err = Some(ProviderError::Transport(format!(
                                    "qoder sse idle timeout {IDLE_SECS}s with empty body ({decode_ctx})"
                                )));
                                break;
                            }
                            return Err(ProviderError::Transport(format!(
                                "qoder sse idle timeout {IDLE_SECS}s with empty body ({decode_ctx}) — upstream accepted request but never streamed tokens"
                            )));
                        }
                        tracing::warn!(
                            ctx = %decode_ctx,
                            buffer_len = buffer.len(),
                            "qoder non-stream idle timeout; using partial SSE body"
                        );
                        stream_eof_partial = true;
                        break;
                    }
                }
            }

            // If we broke early for retry, buffer will be empty and we should continue the outer loop
            if buffer.is_empty() && attempt + 1 < 2 {
                continue;
            }

            if buffer.is_empty() {
                return Err(last_err.unwrap_or_else(|| {
                    ProviderError::Transport(format!("qoder sse empty body ({decode_ctx})"))
                }));
            }

            for line in buffer.lines() {
                let trimmed = line.trim().trim_end_matches('\r');
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(svc_err) = parse_qoder_service_error(trimmed) {
                    return Err(ProviderError::Transport(svc_err));
                }
                if let Some(inner) = parse_sse_line(trimmed) {
                    if let Some(u) = inner.get("usage") {
                        if let Some(p) = usage_i64(u, "prompt_tokens")
                            .or_else(|| usage_i64(u, "input_tokens"))
                        {
                            prompt_tokens = p;
                        }
                        if let Some(c) = usage_i64(u, "completion_tokens")
                            .or_else(|| usage_i64(u, "output_tokens"))
                        {
                            completion_tokens = c;
                        }
                        if let Some(t) = usage_i64(u, "total_tokens") {
                            total_tokens = t;
                        } else if prompt_tokens > 0 || completion_tokens > 0 {
                            total_tokens = prompt_tokens + completion_tokens;
                        }
                    }
                    let choice = inner
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first());
                    let delta = choice.and_then(|c| c.get("delta"));
                    if let Some(content) = delta
                        .and_then(|d| d.get("content"))
                        .and_then(|v| v.as_str())
                    {
                        if !content.is_empty() {
                            accumulated_text.push_str(content);
                        }
                    }
                    if let Some(reasoning) = delta
                        .and_then(|d| d.get("reasoning_content"))
                        .and_then(|v| v.as_str())
                    {
                        if !reasoning.is_empty() {
                            accumulated_reasoning.push_str(reasoning);
                        }
                    }
                }
            }

            let _ = stream_eof_partial;

            let final_text = if !accumulated_text.is_empty() {
                accumulated_text
            } else {
                accumulated_reasoning
            };

            let (prompt_tokens, completion_tokens, total_tokens) = fill_missing_usage(
                prompt_tokens,
                completion_tokens,
                total_tokens,
                estimated_prompt,
                &final_text,
            );

            let mut finish_reason_str = "stop";
            if final_text.is_empty() {
                tracing::warn!("Qoder silent reject mid-non-stream (no content generated). Marking finish_reason as 'length'.");
                finish_reason_str = "length";
            }

            let result_json = serde_json::json!({
                "id": resp_id,
                "object": "chat.completion",
                "created": chrono::Utc::now().timestamp(),
                "model": req_model,
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": final_text
                    },
                    "finish_reason": finish_reason_str
                }],
                "usage": {
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "total_tokens": total_tokens
                }
            });

            return Ok(ChatOutcome::Json(result_json));
        }

        Err(last_err.unwrap_or_else(|| {
            ProviderError::Transport("qoder chat failed after H2/H1 attempts".into())
        }))
    }
}

fn extract_user_text(req: &ChatCompletionRequest) -> String {
    for m in req.messages.iter().rev() {
        if m.role == "user" {
            return content_to_text(&m.content);
        }
    }
    String::new()
}

fn content_to_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for item in arr {
            if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                parts.push(t.to_string());
            } else if let Some(s) = item.as_str() {
                parts.push(s.to_string());
            }
        }
        return parts.join("\n");
    }
    content.to_string()
}

fn derive_session_id(messages: &[crate::openai::ChatMessage]) -> String {
    let mut hasher = Sha256::new();
    let mut first_user_seen = false;
    for m in messages {
        if m.role == "system" {
            hasher.update(b"system:");
            hasher.update(content_to_text(&m.content).as_bytes());
            hasher.update(b"\n");
        } else if m.role == "user" && !first_user_seen {
            hasher.update(b"user:");
            hasher.update(content_to_text(&m.content).as_bytes());
            hasher.update(b"\n");
            first_user_seen = true;
            break;
        }
    }
    if !first_user_seen {
        hasher.update(b"__no_user__");
    }
    let hash_result = hasher.finalize();
    let hex = hex::encode(hash_result);
    format!(
        "{}-{}-4{}-a{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

fn parse_qoder_service_error(line: &str) -> Option<String> {
    if !line.starts_with("data:") {
        return None;
    }
    let data_str = line["data:".len()..].trim();
    if data_str.is_empty() || data_str == "[DONE]" {
        return None;
    }
    let wrapper: Value = serde_json::from_str(data_str).ok()?;
    let svc = wrapper
        .get("statusCodeValue")
        .and_then(|v| v.as_u64())
        .or_else(|| wrapper.get("statusCodeValue").and_then(|v| v.as_i64()).map(|n| n as u64))?;
    if svc < 400 {
        return None;
    }
    let err_status = wrapper
        .get("statusCode")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let mut err_msg = wrapper
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if err_msg.starts_with('{') {
        if let Ok(p) = serde_json::from_str::<Value>(&err_msg) {
            if let Some(url) = p.get("pricingUrl").and_then(|v| v.as_str()) {
                err_msg = url.to_string();
            }
        }
    }
    Some(format!(
        "Qoder HTTP {svc} {err_status}: {}",
        if err_msg.is_empty() {
            "rate limited or quota exceeded"
        } else {
            &err_msg[..err_msg.len().min(200)]
        }
    ))
}

fn parse_sse_line(line: &str) -> Option<Value> {
    if !line.starts_with("data:") {
        return None;
    }
    let data_str = line["data:".len()..].trim();
    if data_str.is_empty() || data_str == "[DONE]" {
        return None;
    }
    if let Ok(wrapper) = serde_json::from_str::<Value>(data_str) {
        if let Some(inner_str) = wrapper.get("body").and_then(|b| b.as_str()) {
            if inner_str == "[DONE]" {
                return None;
            }
            if let Ok(inner) = serde_json::from_str::<Value>(inner_str) {
                return Some(inner);
            }
        }
        if wrapper.get("choices").is_some() || wrapper.get("usage").is_some() {
            if let Some(svc) = wrapper
                .get("statusCodeValue")
                .and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|n| n as u64)))
            {
                if svc >= 400 {
                    return None;
                }
            }
            return Some(wrapper);
        }
    }
    None
}

fn usage_i64(usage: &Value, key: &str) -> Option<i64> {
    usage
        .get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|n| n as i64)))
}

fn estimate_tokens(text: &str) -> i64 {
    if text.is_empty() {
        return 0;
    }
    let n = (text.chars().count() as f64 / 4.0).ceil() as i64;
    n.max(1)
}

fn estimate_prompt_tokens(req: &ChatCompletionRequest) -> i64 {
    req.messages.iter().fold(0i64, |acc, m| {
        acc + estimate_tokens(&content_to_text(&m.content)) + 4
    })
}

fn fill_missing_usage(
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    estimated_prompt: i64,
    completion_text: &str,
) -> (i64, i64, i64) {
    let mut p = prompt_tokens;
    let mut c = completion_tokens;
    let mut t = total_tokens;
    if p <= 0 && estimated_prompt > 0 {
        p = estimated_prompt;
    }
    if c <= 0 && !completion_text.is_empty() {
        c = estimate_tokens(completion_text);
    }
    if t <= 0 {
        t = p.saturating_add(c);
    }
    (p, c, t)
}

async fn qoder_emit_stream_end(
    tx: &tokio::sync::mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    resp_id: &str,
    req_model: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    estimated_prompt: i64,
    accumulated_text: &str,
    finish_reason: &str,
    usage_tx: &mut Option<oneshot::Sender<Option<StreamUsage>>>,
) {
    let (p, c, t) = fill_missing_usage(
        prompt_tokens,
        completion_tokens,
        total_tokens,
        estimated_prompt,
        accumulated_text,
    );
    let usage = StreamUsage {
        prompt_tokens: p,
        completion_tokens: c,
        total_tokens: t,
    }
    .normalized();

    let finish_json = serde_json::json!({
        "id": resp_id,
        "object": "chat.completion.chunk",
        "created": chrono::Utc::now().timestamp(),
        "model": req_model,
        "choices": [{
            "index": 0,
            "delta": serde_json::json!({}),
            "finish_reason": finish_reason
        }]
    });
    let _ = tx
        .send(Ok(bytes::Bytes::from(format!(
            "data: {}\n\n",
            serde_json::to_string(&finish_json).unwrap_or_default()
        ))))
        .await;

    if !usage.is_empty() {
        let usage_json = serde_json::json!({
            "id": resp_id,
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": req_model,
            "choices": [{
                "index": 0,
                "delta": serde_json::json!({}),
                "finish_reason": serde_json::Value::Null
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
                serde_json::to_string(&usage_json).unwrap_or_default()
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

fn load_chat_template() -> Option<Value> {
    const RAW: &str = include_str!("qoder-baseprompt.json");
    let mut s = RAW.to_string();
    for _ in 0..5 {
        s = s.replacen("{UUID1}", &Uuid::new_v4().to_string(), 1);
        s = s.replacen("{UUID2}", &Uuid::new_v4().to_string(), 1);
        s = s.replacen("{UUID3}", &Uuid::new_v4().to_string(), 1);
        s = s.replacen("{UUID4}", &Uuid::new_v4().to_string(), 1);
        s = s.replacen("{UUID5}", &Uuid::new_v4().to_string(), 1);
    }
    s = s.replace("{TIME1}", &format!("{}", chrono::Utc::now().timestamp_millis()));
    serde_json::from_str(&s).ok()
}

fn build_chat_body(req: &ChatCompletionRequest, cfg: &ModelCfg) -> Value {
    let prompt = extract_user_text(req);
    let mut body = load_chat_template().unwrap_or_else(|| json!({}));

    let mut messages: Vec<Value> = Vec::new();
    let has_system = req.messages.iter().any(|m| m.role == "system");
    if !has_system {
        let sys = "You are a helpful AI assistant. Answer the user's questions clearly and concisely. Maintain context from earlier turns in the conversation.";
        messages.push(json!({
            "role": "system",
            "content": sys,
            "contents": [{ "type": "text", "text": sys }]
        }));
    }
    for m in &req.messages {
        let content_str = content_to_text(&m.content);
        messages.push(json!({
            "role": m.role,
            "content": content_str,
            "contents": [{ "type": "text", "text": content_str }]
        }));
    }
    let system_text = messages
        .iter()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        .unwrap_or("")
        .to_string();
    let max_tokens = req.max_tokens.unwrap_or(32_768);
    let session_id = derive_session_id(&req.messages);

    body["request_id"] = json!(Uuid::new_v4().to_string());
    body["request_set_id"] = json!(Uuid::new_v4().to_string());
    body["chat_record_id"] = json!(Uuid::new_v4().to_string());
    body["session_id"] = json!(session_id);
    body["stream"] = json!(true);
    body["chat_task"] = json!("FREE_INPUT");
    body["is_reply"] = json!(true);
    body["is_retry"] = json!(false);
    body["source"] = json!(1);
    body["version"] = json!("3");
    body["session_type"] = json!("qodercli");
    body["agent_id"] = json!("agent_common");
    body["task_id"] = json!("common");
    body["code_language"] = json!("");
    body["chat_prompt"] = json!("");
    body["image_urls"] = Value::Null;
    body["aliyun_user_type"] = json!("");
    body["system"] = json!(system_text);
    body["messages"] = json!(messages);
    body["tools"] = json!([]);
    body["parameters"] = json!({ "max_tokens": max_tokens });

    if !body.get("chat_context").map(|v| v.is_object()).unwrap_or(false) {
        body["chat_context"] = json!({});
    }
    body["chat_context"]["chatPrompt"] = json!("");
    body["chat_context"]["imageUrls"] = Value::Null;
    body["chat_context"]["features"] = json!([]);
    body["chat_context"]["text"] = json!({ "type": "text", "text": prompt });
    if !body["chat_context"]
        .get("extra")
        .map(|v| v.is_object())
        .unwrap_or(false)
    {
        body["chat_context"]["extra"] = json!({});
    }
    body["chat_context"]["extra"]["context"] = json!([]);
    body["chat_context"]["extra"]["originalContent"] =
        json!({ "type": "text", "text": prompt });
    body["chat_context"]["extra"]["modelConfig"] = json!({
        "key": cfg.key,
        "is_reasoning": cfg.is_reasoning
    });

    body["model_config"] = json!({
        "key": cfg.key,
        "display_name": cfg.display_name,
        "model": "",
        "format": "openai",
        "is_vl": cfg.is_vl,
        "is_reasoning": cfg.is_reasoning,
        "api_key": "",
        "url": "",
        "source": "system",
        "max_input_tokens": cfg.max_input_tokens
    });
    body["business"] = json!({
        "product": "cli",
        "version": COSY_VERSION,
        "type": "agent",
        "id": Uuid::new_v4().to_string(),
        "name": prompt.chars().take(30).collect::<String>(),
        "begin_at": chrono::Utc::now().timestamp_millis(),
        "stage": "start"
    });
    body
}
