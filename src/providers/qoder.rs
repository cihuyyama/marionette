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
use md5::{Md5, Digest as Md5Digest};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;
use futures_util::StreamExt;
use axum::http::{HeaderMap, HeaderValue};
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
    use aes::cipher::{BlockEncrypt, KeyInit};
    use aes::Aes128;
    if key.len() != 16 {
        return Err(ProviderError::Other("AES key must be 16 bytes".into()));
    }
    let mut padded = plain.to_vec();
    let pad_len = 16 - (padded.len() % 16);
    padded.extend(std::iter::repeat(pad_len as u8).take(pad_len));
    let cipher = Aes128::new_from_slice(key)
        .map_err(|e| ProviderError::Other(format!("AES init: {e}")))?;
    let mut result = Vec::with_capacity(padded.len());
    // qodercli: IV == key (not zero IV)
    let mut prev = [0u8; 16];
    prev.copy_from_slice(key);
    for chunk in padded.chunks(16) {
        let mut block = [0u8; 16];
        block.copy_from_slice(chunk);
        for i in 0..16 {
            block[i] ^= prev[i];
        }
        let mut gblock = aes::cipher::generic_array::GenericArray::from_mut_slice(&mut block);
        cipher.encrypt_block(&mut gblock);
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
    client: Client,
}

impl QoderProvider {
    pub fn new() -> Self {
        // HTTP/2 preferred: HTTP/1.1-only hit hyper chunked UnexpectedEof on Qoder SSE.
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .connect_timeout(std::time::Duration::from_secs(30))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .build()
            .expect("qoder http client");
        Self { client }
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

    fn is_chunked_eof(e: &reqwest::Error) -> bool {
        let full = Self::format_reqwest_err(e);
        full.contains("UnexpectedEof")
            || full.contains("unexpected EOF")
            || full.contains("chunk size line")
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
        if tokens.security_oauth_token.is_none()
            || tokens.security_oauth_token.as_deref() == Some("")
        {
            return Err(ProviderError::AuthExpired);
        }
        let session = build_cosy_session(&tokens)?;
        let payload_b64 = build_payload_b64(&session.info);
        let cosy_date = format!("{}", chrono::Utc::now().timestamp());
        let model = map_model(req.upstream_model()).to_string();
        let body = build_chat_body(req, &model);
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

        let resp = self
            .client
            .post(CHAT_URL)
            .headers(headers)
            .body(body_encoded)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(format!("qoder send: {e:?}")))?;
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
        tracing::debug!(
            status,
            content_type = %content_type,
            content_encoding = %content_encoding,
            transfer_encoding = %transfer_encoding,
            "qoder chat upstream ok"
        );

        let req_model = req.model.clone();
        let http_version = format!("{:?}", resp.version());
        let decode_ctx = format!(
            "ct={content_type} ce={content_encoding} te={transfer_encoding} http={http_version}"
        );

        if req.stream_enabled() {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(32);
            let mut upstream_stream = resp.bytes_stream();
            let decode_ctx_stream = decode_ctx.clone();

            tokio::spawn(async move {
                let mut buffer = String::new();
                let resp_id = format!("chatcmpl-{}", Uuid::new_v4().simple());
                let mut first_chunk_sent = false;
                let mut any_content = false;

                while let Some(chunk_res) = upstream_stream.next().await {
                    let chunk = match chunk_res {
                        Ok(c) => c,
                        Err(e) => {
                            let chain = QoderProvider::format_reqwest_err(&e);
                            tracing::warn!(
                                error = %chain,
                                ctx = %decode_ctx_stream,
                                first_chunk_sent,
                                any_content,
                                "qoder stream body decode failed"
                            );
                            if first_chunk_sent || any_content {
                                let chunk_json = json!({
                                    "id": resp_id,
                                    "object": "chat.completion.chunk",
                                    "created": chrono::Utc::now().timestamp(),
                                    "model": req_model,
                                    "choices": [{
                                        "index": 0,
                                        "delta": {},
                                        "finish_reason": "stop"
                                    }]
                                });
                                let msg = format!(
                                    "data: {}\n\ndata: [DONE]\n\n",
                                    serde_json::to_string(&chunk_json).unwrap_or_default()
                                );
                                let _ = tx.send(Ok(bytes::Bytes::from(msg))).await;
                            } else {
                                let _ = tx
                                    .send(Err(std::io::Error::new(
                                        std::io::ErrorKind::UnexpectedEof,
                                        format!(
                                            "qoder sse body decode: {chain} ({decode_ctx_stream})"
                                        ),
                                    )))
                                    .await;
                            }
                            return;
                        }
                    };

                    buffer.push_str(&String::from_utf8_lossy(&chunk));

                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].trim_end_matches('\r').to_string();
                        buffer = buffer[pos + 1..].to_string();

                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }

                        if let Some(svc_err) = parse_qoder_service_error(&trimmed) {
                            tracing::warn!(error = %svc_err, "qoder upstream error in SSE");
                            let _ = tx
                                .send(Err(std::io::Error::new(
                                    std::io::ErrorKind::Other,
                                    svc_err,
                                )))
                                .await;
                            return;
                        }

                        if let Some(inner) = parse_sse_line(&trimmed) {
                            if !first_chunk_sent {
                                first_chunk_sent = true;
                                let chunk_json = json!({
                                    "id": resp_id,
                                    "object": "chat.completion.chunk",
                                    "created": chrono::Utc::now().timestamp(),
                                    "model": req_model,
                                    "choices": [{
                                        "index": 0,
                                        "delta": { "role": "assistant", "content": "" },
                                        "finish_reason": null
                                    }]
                                });
                                let msg = format!(
                                    "data: {}\n\n",
                                    serde_json::to_string(&chunk_json).unwrap()
                                );
                                if tx.send(Ok(bytes::Bytes::from(msg))).await.is_err() {
                                    return;
                                }
                            }

                            let choice = inner
                                .get("choices")
                                .and_then(|c| c.as_array())
                                .and_then(|a| a.first());
                            let delta = choice.and_then(|c| c.get("delta"));
                            let finish_reason = choice.and_then(|c| c.get("finish_reason"));

                            let mut delta_out = json!({});
                            let mut has_delta = false;

                            if let Some(content) = delta
                                .and_then(|d| d.get("content"))
                                .and_then(|v| v.as_str())
                            {
                                if !content.is_empty() {
                                    delta_out["content"] = json!(content);
                                    has_delta = true;
                                    any_content = true;
                                }
                            }
                            if let Some(reasoning) = delta
                                .and_then(|d| d.get("reasoning_content"))
                                .and_then(|v| v.as_str())
                            {
                                if !reasoning.is_empty() {
                                    delta_out["reasoning_content"] = json!(reasoning);
                                    has_delta = true;
                                    any_content = true;
                                }
                            }

                            if has_delta || finish_reason.is_some() {
                                let chunk_json = json!({
                                    "id": resp_id,
                                    "object": "chat.completion.chunk",
                                    "created": chrono::Utc::now().timestamp(),
                                    "model": req_model,
                                    "choices": [{
                                        "index": 0,
                                        "delta": delta_out,
                                        "finish_reason": finish_reason
                                    }]
                                });
                                let msg = format!(
                                    "data: {}\n\n",
                                    serde_json::to_string(&chunk_json).unwrap()
                                );
                                if tx.send(Ok(bytes::Bytes::from(msg))).await.is_err() {
                                    return;
                                }
                            }

                            if finish_reason.is_some() {
                                let _ = tx.send(Ok(bytes::Bytes::from("data: [DONE]\n\n"))).await;
                                return;
                            }
                        }
                    }
                }

                let chunk_json = json!({
                    "id": resp_id,
                    "object": "chat.completion.chunk",
                    "created": chrono::Utc::now().timestamp(),
                    "model": req_model,
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": "stop"
                    }]
                });
                let msg = format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    serde_json::to_string(&chunk_json).unwrap()
                );
                let _ = tx.send(Ok(bytes::Bytes::from(msg))).await;
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
            Ok(ChatOutcome::Stream(Response::from_parts(parts, body)))
        } else {
            let mut buffer = String::new();
            let resp_id = format!("chatcmpl-{}", Uuid::new_v4().simple());
            let mut accumulated_text = String::new();
            let mut prompt_tokens: i64 = 0;
            let mut completion_tokens: i64 = 0;
            let mut total_tokens: i64 = 0;
            let mut upstream_stream = resp.bytes_stream();
            let mut stream_eof_partial = false;

            while let Some(chunk_res) = upstream_stream.next().await {
                let chunk = match chunk_res {
                    Ok(c) => c,
                    Err(e) => {
                        let chain = Self::format_reqwest_err(&e);
                        if Self::is_chunked_eof(&e) && !buffer.is_empty() {
                            tracing::warn!(
                                error = %chain,
                                ctx = %decode_ctx,
                                buffer_len = buffer.len(),
                                "qoder non-stream chunked EOF; using partial SSE body"
                            );
                            stream_eof_partial = true;
                            break;
                        }
                        return Err(ProviderError::Transport(format!(
                            "qoder sse body decode: {chain} ({decode_ctx})"
                        )));
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));
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
                        accumulated_text.push_str(content);
                    }
                }
            }

            let _ = stream_eof_partial;

            if total_tokens == 0 && completion_tokens == 0 && !accumulated_text.is_empty() {
                completion_tokens = estimate_tokens(&accumulated_text);
                total_tokens = prompt_tokens + completion_tokens;
            }

            let result_json = json!({
                "id": resp_id,
                "object": "chat.completion",
                "created": chrono::Utc::now().timestamp(),
                "model": req_model,
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": accumulated_text
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "total_tokens": total_tokens
                }
            });

            Ok(ChatOutcome::Json(result_json))
        }
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
        if wrapper.get("statusCodeValue").is_some() {
            return None;
        }
        if let Some(inner_str) = wrapper.get("body").and_then(|b| b.as_str()) {
            if inner_str == "[DONE]" {
                return None;
            }
            if let Ok(inner) = serde_json::from_str::<Value>(inner_str) {
                return Some(inner);
            }
        }
        if wrapper.get("choices").is_some() || wrapper.get("usage").is_some() {
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
    let n = (text.chars().count() as f64 / 4.0).ceil() as i64;
    n.max(1)
}

fn build_chat_body(req: &ChatCompletionRequest, model: &str) -> Value {
    let prompt = extract_user_text(req);
    let mut messages: Vec<Value> = Vec::new();
    let has_system = req.messages.iter().any(|m| m.role == "system");
    if !has_system {
        messages.push(json!({
            "role": "system",
            "content": "You are a helpful AI assistant. Answer the user's questions clearly and concisely.",
            "contents": [
                {
                    "type": "text",
                    "text": "You are a helpful AI assistant. Answer the user's questions clearly and concisely."
                }
            ]
        }));
    }
    for m in &req.messages {
        let content_str = content_to_text(&m.content);
        messages.push(json!({
            "role": m.role,
            "content": content_str,
            "contents": [
                {
                    "type": "text",
                    "text": content_str
                }
            ]
        }));
    }
    let system_text = messages
        .iter()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        .unwrap_or("")
        .to_string();
    let is_reasoning = matches!(
        model,
        "ultimate" | "dmodel" | "dfmodel" | "gm51model"
    );
    let max_tokens = req.max_tokens.unwrap_or(32_768);
    let req_id = Uuid::new_v4().to_string();
    let chat_record_id = Uuid::new_v4().to_string();
    let session_id = derive_session_id(&req.messages);
    json!({
        "request_id": req_id,
        "request_set_id": Uuid::new_v4().to_string(),
        "chat_record_id": chat_record_id,
        "session_id": session_id,
        "stream": true,
        "chat_task": "FREE_INPUT",
        "is_reply": true,
        "is_retry": false,
        "source": 1,
        "version": "3",
        "session_type": "qodercli",
        "agent_id": "agent_common",
        "task_id": "common",
        "code_language": "",
        "chat_prompt": "",
        "image_urls": null,
        "aliyun_user_type": "",
        "system": system_text,
        "messages": messages,
        "tools": [],
        "parameters": { "max_tokens": max_tokens },
        "chat_context": {
            "chatPrompt": "",
            "imageUrls": null,
            "extra": {
                "context": [],
                "modelConfig": { "key": model, "is_reasoning": is_reasoning },
                "originalContent": { "type": "text", "text": prompt }
            },
            "features": [],
            "text": { "type": "text", "text": prompt }
        },
        "model_config": {
            "key": model,
            "display_name": model,
            "is_vl": true,
            "is_reasoning": is_reasoning,
            "max_input_tokens": 180000,
            "format": "openai",
            "source": "system"
        },
        "business": {
            "product": "cli",
            "version": COSY_VERSION,
            "type": "agent",
            "stage": "start",
            "id": Uuid::new_v4().to_string(),
            "name": prompt.chars().take(30).collect::<String>(),
            "begin_at": chrono::Utc::now().timestamp_millis()
        }
    })
}
