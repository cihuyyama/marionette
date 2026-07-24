//! Qoder provider — full port from etteeum qoder.ts
//! Auth: PAT -> jobToken -> COSY bearer. Chat: SSE -> OpenAI chunks.

use super::{ChatOutcome, Provider, classify_http_status};
use crate::db::Account;
use crate::error::ProviderError;
use crate::openai::ChatCompletionRequest;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use aes::cipher::{BlockEncrypt, KeyInit};
use md5::{Md5, Digest as Md5Digest};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};


use uuid::Uuid;

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
    use aes::cipher::{BlockEncrypt, KeyInit};
    use aes::Aes128;
    if key.len() != 16 {
        return Err(ProviderError::Other("AES key must be 16 bytes".into()));
    }
    let mut padded = plain.to_vec();
    let pad_len = 16 - (padded.len() % 16);
    padded.extend(std::iter::repeat(pad_len as u8).take(pad_len));
    let cipher = Aes128::new_from_slice(key).map_err(|e| ProviderError::Other(format!("AES init: {e}")))?;
    let mut result = Vec::with_capacity(padded.len());
    let mut prev = [0u8; 16];
    for chunk in padded.chunks(16) {
        let mut block = [0u8; 16];
        block.copy_from_slice(chunk);
        for i in 0..16 { block[i] ^= prev[i]; }
        let mut gblock = aes::cipher::generic_array::GenericArray::from_mut_slice(&mut block); cipher.encrypt_block(&mut gblock);
        result.extend_from_slice(&block);
        prev = block;
    }
    Ok(result)
}

fn map_model(name: &str) -> &str {
    match name.to_lowercase().as_str() {
        "lite" => "lite",
        "auto" => "auto",
        "ultimate" => "ultimate",
        "performance" => "performance",
        "efficient" => "efficient",
        "qmodel_latest" | "qwen3.7-max" => "qmodel_latest",
        "qmodel" | "qwen3.6-plus" => "qmodel",
        "dmodel" | "deepseek-v4-pro" => "dmodel",
        "dfmodel" | "deepseek-v4-flash" => "dfmodel",
        "gm51model" | "glm-5.1" => "gm51model",
        "kmodel" | "kimi-k2.6" => "kmodel",
        "mmodel" | "minimax-m2.7" => "mmodel",
        _ => "lite",
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
}

impl QoderTokens {
    fn from_data(data: &Value) -> Result<Self, ProviderError> {
        let pt = data.get("personalToken")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::AuthInvalid("missing personalToken".into()))?
            .to_string();
        let mid = data.get("machineId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let mt = data.get("machineToken")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| mid.clone());
        Ok(Self {
            personal_token: pt,
            security_oauth_token: data.get("securityOauthToken").and_then(|v| v.as_str()).map(|s| s.to_string()),
            refresh_token: data.get("refreshToken").and_then(|v| v.as_str()).map(|s| s.to_string()),
            user_id: data.get("userId").and_then(|v| v.as_str()).map(|s| s.to_string()),
            user_name: data.get("userName").and_then(|v| v.as_str()).map(|s| s.to_string()),
            user_type: data.get("userType").and_then(|v| v.as_str()).map(|s| s.to_string()),
            expire_time: data.get("expireTime").and_then(|v| v.as_u64()),
            machine_id: mid,
            machine_token: mt,
            machine_type: data.get("machineType").and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_else(|| "5".into()),
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
    #[serde(default)]
    security_oauth_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expire_time: Option<u64>,
    #[serde(default)]
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
    client: Client,
}

impl QoderProvider {
    pub fn new(client: Client) -> Self {
        Self { client }
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
        if tokens.security_oauth_token.is_none() || tokens.security_oauth_token.as_deref() == Some("") {
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
            account.data = tokens.to_data().to_string();
        }
        Ok(())
    }

    async fn chat(
        &self,
        account: &Account,
        req: &ChatCompletionRequest,
    ) -> Result<ChatOutcome, ProviderError> {
        let data: Value = serde_json::from_str(&account.data).unwrap_or(Value::Null);
        let tokens = QoderTokens::from_data(&data)?;
        if tokens.security_oauth_token.is_none() || tokens.security_oauth_token.as_deref() == Some("") {
            return Err(ProviderError::AuthExpired);
        }
        let session = build_cosy_session(&tokens)?;
        let payload_b64 = build_payload_b64(&session.info);
        let cosy_date = rfc1123_date();
        let body = build_chat_body(req);
        let body_str = serde_json::to_string(&body).unwrap_or_default();
        let path_sig = path_sig_from_url(CHAT_URL);
        let bearer_sig = sign_bearer_request(&payload_b64, &session.cosy_key, &cosy_date, &body_str, &path_sig);
        let bearer = format!("{}.{}.{}.{}", payload_b64, session.cosy_key, cosy_date, bearer_sig);
        let mut headers = signature_headers(&tokens);
        headers.insert("authorization", format!("Bearer {}", bearer).parse().unwrap());
        headers.insert("cosy-payload", payload_b64.parse().unwrap());
        headers.insert("cosy-key", session.cosy_key.clone().parse().unwrap());
        headers.insert("cosy-date", cosy_date.parse().unwrap());
        headers.insert("cosy-bearer-signature", bearer_sig.parse().unwrap());
        let resp = self.client
            .post(CHAT_URL)
            .headers(headers)
            .body(body_str)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if status != 200 {
            let text = resp.text().await.unwrap_or_default();
            return Err(classify_http_status(status, &text));
        }
        if req.stream_enabled() {
            let byte_stream = resp.bytes_stream();
            let body = Body::from_stream(byte_stream);
            let response = Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .body(body)
                .map_err(|e| ProviderError::Transport(format!("body build: {e}")))?;
            Ok(ChatOutcome::Stream(response))
        } else {
            let text = resp.text().await.unwrap_or_default();
            let json_val = parse_nonstream_response(&text)?;
            Ok(ChatOutcome::Json(json_val))
        }
    }
}

fn build_chat_body(req: &ChatCompletionRequest) -> Value {
    let messages: Vec<Value> = req.messages.iter().map(|m| {
        json!({
            "role": m.role,
            "content": m.content
        })
    }).collect();
    let model = map_model(req.upstream_model()).to_string();
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": req.stream_enabled(),
        "business": {
            "product": "cli",
            "type": "agent",
            "version": "1.0.22"
        },
        "scene": "assistant"
    });
    if let Some(max) = req.max_tokens {
        body["max_tokens"] = json!(max);
    }
    if let Some(temp) = req.temperature {
        body["temperature"] = json!(temp);
    }
    body
}

fn parse_nonstream_response(text: &str) -> Result<Value, ProviderError> {
    let mut last_data: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("data:") {
            let rest = rest.trim();
            if rest == "[DONE]" { continue; }
            last_data = Some(rest);
        }
    }
    let raw = last_data.unwrap_or(text);
    if let Ok(wrapper) = serde_json::from_str::<Value>(raw) {
        if let Some(inner_str) = wrapper.get("body").and_then(|b| b.as_str()) {
            if let Ok(inner) = serde_json::from_str::<Value>(inner_str) {
                return Ok(inner);
            }
        }
        return Ok(wrapper);
    }
    if let Ok(val) = serde_json::from_str::<Value>(raw) {
        return Ok(val);
    }
    Err(ProviderError::Transport(format!("qoder parse: no JSON in {} bytes", text.len())))
}
