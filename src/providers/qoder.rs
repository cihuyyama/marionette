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

const COSY_VERSION: &str = "1.1.7";
const COSY_MACHINE_OS: &str = "x86_64_win32";
const APPCODE: &str = "cosy";
const SIG_SECRET: &str = "d2FyLCB3YXIgbmV2ZXIgY2hhbmdlcw==";
const JOB_TOKEN_URL: &str = "https://center.qoder.sh/algo/api/v3/user/jobToken?Encode=1";
const CHAT_URL: &str = "https://api2.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1";
const QUOTA_USAGE_URL: &str = "https://openapi.qoder.sh/api/v2/quota/usage";
const USER_PLAN_URL: &str = "https://openapi.qoder.sh/api/v2/user/plan";
const ACTIVITY_URL: &str = "https://openapi.qoder.sh/algo/api/v2/activity";
const TRIAL_URL: &str = "https://openapi.qoder.sh/api/v3/user/trial";

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
            max_input_tokens: 1_000_000,
            is_vl: true,
            is_reasoning: true,
        },
        "performance" => ModelCfg {
            key: "performance",
            display_name: "Performance",
            max_input_tokens: 1_000_000,
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
        "qmodel_preview" | "qwen3.8-max-preview" | "qwen3.8-preview" => ModelCfg {
            key: "qmodel_preview",
            display_name: "Qwen3.8-Max-Preview",
            max_input_tokens: 180_000,
            is_vl: true,
            is_reasoning: true,
        },
        "qmodel_38max" | "qwen3.8-max" | "qwen3.8max" => ModelCfg {
            key: "qmodel_38max",
            display_name: "Qwen3.8-Max",
            max_input_tokens: 1_000_000,
            is_vl: true,
            is_reasoning: true,
        },
        "qmodel_latest" | "qwen3.7-max" => ModelCfg {
            key: "qmodel_latest",
            display_name: "Qwen3.7-Max",
            max_input_tokens: 1_000_000,
            is_vl: true,
            is_reasoning: false,
        },
        "qmodel1" | "qmodel" | "qwen3.6-plus" | "qwen3.7-plus" => ModelCfg {
            key: "qmodel1",
            display_name: "Qwen3.7-Plus",
            max_input_tokens: 1_000_000,
            is_vl: true,
            is_reasoning: false,
        },
        "kmodel_latest" | "kimi-k3" | "kimi_k3" => ModelCfg {
            key: "kmodel_latest",
            display_name: "Kimi-K3",
            max_input_tokens: 180_000,
            is_vl: true,
            is_reasoning: false,
        },
        "kmodel1" | "kmodel" | "kimi-k2.7-code" | "kimi-k2.6" | "kimi-k2.7" => ModelCfg {
            key: "kmodel1",
            display_name: "Kimi-K2.7-Code",
            max_input_tokens: 256_000,
            is_vl: true,
            is_reasoning: false,
        },
        "gm51model1" | "gm51model" | "glm-5.2" | "glm-5.1" | "glm5.2" => ModelCfg {
            key: "gm51model1",
            display_name: "GLM-5.2",
            max_input_tokens: 1_000_000,
            is_vl: true,
            is_reasoning: true,
        },
        "dmodel1" | "dmodel" | "deepseek-v4-pro" => ModelCfg {
            key: "dmodel1",
            display_name: "DeepSeek-V4-Pro",
            max_input_tokens: 1_000_000,
            is_vl: true,
            is_reasoning: true,
        },
        "dfmodel1" | "dfmodel" | "deepseek-v4-flash" => ModelCfg {
            key: "dfmodel1",
            display_name: "DeepSeek-V4-Flash",
            max_input_tokens: 1_000_000,
            is_vl: true,
            is_reasoning: true,
        },
        "mmodel" | "minimax-m3" | "minimax-m2.7" => ModelCfg {
            key: "mmodel",
            display_name: "MiniMax-M3",
            max_input_tokens: 1_000_000,
            is_vl: true,
            is_reasoning: false,
        },
        "lite" => ModelCfg {
            key: "lite",
            display_name: "Lite",
            max_input_tokens: 180_000,
            is_vl: false,
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

    pub async fn claim_pro_trial(
        &self,
        personal_token: &str,
    ) -> Result<(u16, Value), ProviderError> {
        let pat = personal_token.trim();
        if pat.is_empty() {
            return Err(ProviderError::AuthInvalid("empty personalToken".into()));
        }
        let resp = self
            .client
            .post(TRIAL_URL)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {pat}"),
            )
            .header(reqwest::header::USER_AGENT, "QoderCLI/1.0")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&json!({}))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let body: Value = serde_json::from_str(&text).unwrap_or_else(|_| {
            json!({ "raw": text.chars().take(2000).collect::<String>() })
        });
        Ok((status, body))
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
            return Err(classify_qoder_status(status, &text));
        }
        serde_json::from_str(&text)
            .map_err(|e| ProviderError::Transport(format!("jobToken parse: {e}")))
    }

    async fn apply_job_token(&self, tokens: &mut QoderTokens) -> Result<(), ProviderError> {
        let job = self.do_job_token(tokens).await?;
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
        Ok(())
    }

    fn quota_headers(sot: &str) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {sot}").parse().unwrap(),
        );
        h.insert("cosy-clienttype", "5".parse().unwrap());
        h.insert("cosy-version", COSY_VERSION.parse().unwrap());
        h.insert(
            reqwest::header::USER_AGENT,
            format!("qodercli/{COSY_VERSION}").parse().unwrap(),
        );
        h.insert(reqwest::header::ACCEPT, "application/json".parse().unwrap());
        h
    }

    async fn fetch_quota_usage(&self, sot: &str) -> Result<(i64, i64), ProviderError> {
        let resp = self
            .client
            .get(QUOTA_USAGE_URL)
            .headers(Self::quota_headers(sot))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 200 {
            return Err(classify_qoder_status(status, &text));
        }
        parse_quota_usage(&text)
    }

    async fn fetch_user_plan(&self, sot: &str) -> Result<UserPlanInfo, ProviderError> {
        let resp = self
            .client
            .get(USER_PLAN_URL)
            .headers(Self::quota_headers(sot))
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 200 {
            return Err(classify_qoder_status(status, &text));
        }
        parse_user_plan(&text)
    }

    async fn fetch_activity(&self, tokens: &QoderTokens) -> Result<ActivitySnapshot, ProviderError> {
        let session = build_cosy_session(tokens)?;
        let payload_b64 = build_payload_b64(&session.info);
        let cosy_date = format!("{}", chrono::Utc::now().timestamp());
        let body_encoded = "";
        let path_sig = path_sig_from_url(ACTIVITY_URL);
        let bearer_sig = sign_bearer_request(
            &payload_b64,
            &session.cosy_key,
            &cosy_date,
            body_encoded,
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
        headers.insert("accept", "application/json".parse().unwrap());
        headers.insert(
            "authorization",
            format!("Bearer {bearer}").parse().unwrap(),
        );
        headers.insert("accept-encoding", "identity".parse().unwrap());
        headers.insert("cosy-version", COSY_VERSION.parse().unwrap());
        headers.insert("cosy-machineid", tokens.machine_id.parse().unwrap());
        headers.insert("cosy-machineos", COSY_MACHINE_OS.parse().unwrap());
        headers.insert("cosy-machinetoken", tokens.machine_token.parse().unwrap());
        headers.insert("login-version", "v2".parse().unwrap());
        headers.insert("user-agent", "Bun/1.3.14".parse().unwrap());

        let resp = self
            .client
            .get(ACTIVITY_URL)
            .headers(headers)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if status != 200 {
            return Err(classify_qoder_status(status, &text));
        }
        parse_activity_body(&text)
    }
}

fn floor_credit(v: f64) -> i64 {
    if !v.is_finite() || v <= 0.0 {
        0
    } else {
        v.floor() as i64
    }
}

fn parse_quota_usage(body: &str) -> Result<(i64, i64), ProviderError> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| ProviderError::Transport(format!("quota usage parse: {e}")))?;
    let uq = v
        .get("userQuota")
        .ok_or_else(|| ProviderError::Transport("quota usage missing userQuota".into()))?;
    let total = uq
        .get("total")
        .and_then(|x| x.as_f64().or_else(|| x.as_i64().map(|n| n as f64)))
        .unwrap_or(0.0);
    let remaining = uq
        .get("remaining")
        .and_then(|x| x.as_f64().or_else(|| x.as_i64().map(|n| n as f64)))
        .unwrap_or(0.0);
    let mut limit = floor_credit(total);
    let mut rem = floor_credit(remaining);
    if v.get("isQuotaExceeded")
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
    {
        rem = 0;
    }
    if rem > limit && limit > 0 {
        rem = limit;
    }
    if limit < 0 {
        limit = 0;
    }
    if rem < 0 {
        rem = 0;
    }
    Ok((limit, rem))
}

#[derive(Debug, Clone, PartialEq)]
struct UserPlanInfo {
    plan_tier_name: String,
    user_type: String,
    is_paid_plan: bool,
    is_highest_tier: bool,
    start_date_ms: i64,
    end_date_ms: i64,
    feature_allowed: Value,
}

fn json_i64_ms(v: &Value) -> i64 {
    v.as_i64()
        .or_else(|| v.as_u64().map(|n| n as i64))
        .or_else(|| v.as_f64().map(|f| f as i64))
        .unwrap_or(0)
}

fn parse_user_plan(body: &str) -> Result<UserPlanInfo, ProviderError> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| ProviderError::Transport(format!("user plan parse: {e}")))?;
    let plan_tier_name = v
        .get("plan_tier_name")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if plan_tier_name.is_empty() {
        return Err(ProviderError::Transport(
            "user plan missing plan_tier_name".into(),
        ));
    }
    let user_type = v
        .get("user_type")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let is_paid_plan = v
        .get("is_paid_plan")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let is_highest_tier = v
        .get("is_highest_tier")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let start_date_ms = v.get("start_date").map(json_i64_ms).unwrap_or(0);
    let end_date_ms = v.get("end_date").map(json_i64_ms).unwrap_or(0);
    let feature_allowed = v
        .get("feature_allowed")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));
    Ok(UserPlanInfo {
        plan_tier_name,
        user_type,
        is_paid_plan,
        is_highest_tier,
        start_date_ms,
        end_date_ms,
        feature_allowed,
    })
}

fn ms_to_rfc3339(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn apply_user_plan_to_account(account: &mut Account, plan: &UserPlanInfo) {
    let mut data = account.data_json();
    let obj = match data.as_object_mut() {
        Some(o) => o,
        None => return,
    };
    obj.insert(
        "plan_tier_name".into(),
        Value::String(plan.plan_tier_name.clone()),
    );
    obj.insert("plan_user_type".into(), Value::String(plan.user_type.clone()));
    obj.insert("plan_is_paid".into(), Value::Bool(plan.is_paid_plan));
    obj.insert(
        "plan_is_highest_tier".into(),
        Value::Bool(plan.is_highest_tier),
    );
    obj.insert("plan_start_ms".into(), json!(plan.start_date_ms));
    obj.insert("plan_end_ms".into(), json!(plan.end_date_ms));
    if let Some(s) = ms_to_rfc3339(plan.start_date_ms) {
        obj.insert("plan_start_at".into(), Value::String(s));
    } else {
        obj.insert("plan_start_at".into(), Value::Null);
    }
    if let Some(s) = ms_to_rfc3339(plan.end_date_ms) {
        obj.insert("plan_end_at".into(), Value::String(s));
    } else {
        obj.insert("plan_end_at".into(), Value::Null);
    }
    obj.insert("plan_feature_allowed".into(), plan.feature_allowed.clone());
    obj.insert(
        "plan_synced_at".into(),
        Value::String(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    );
    account.set_data_json(&data);
}

#[derive(Debug, Clone, PartialEq)]
struct ActivityFreeBucket {
    activity_id: String,
    model_name: String,
    model_keys: Vec<String>,
    limit: i64,
    used: i64,
    remaining: i64,
    reset_at_ms: i64,
    reset_strategy: String,
    eligible: bool,
    activity_end_at_ms: i64,
    status_text: Option<String>,
    cli_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct ActivitySnapshot {
    activities: Vec<ActivityFreeBucket>,
    query_at_ms: i64,
    fetched_at: String,
}

fn json_i64_any(v: &Value) -> i64 {
    v.as_i64()
        .or_else(|| v.as_u64().map(|n| n as i64))
        .or_else(|| v.as_f64().map(|f| f.floor() as i64))
        .unwrap_or(0)
}

fn parse_activity_body(body: &str) -> Result<ActivitySnapshot, ProviderError> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| ProviderError::Transport(format!("activity parse: {e}")))?;
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
    if code != 0 {
        let msg = v
            .get("msg")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown");
        return Err(ProviderError::Transport(format!(
            "activity code={code} msg={msg}"
        )));
    }
    let data = v.get("data").cloned().unwrap_or(Value::Null);
    let query_at_ms = data
        .get("queryAt")
        .map(json_i64_any)
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let raw_acts = data
        .get("activities")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let mut activities = Vec::new();
    for row in raw_acts {
        let typ = row
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        if !typ.is_empty() && typ != "MODEL_FREE_QUOTA" {
            continue;
        }
        let model_keys: Vec<String> = row
            .get("modelKeys")
            .and_then(|k| k.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let limit = row.get("limit").map(json_i64_any).unwrap_or(0).max(0);
        let used = row.get("used").map(json_i64_any).unwrap_or(0).max(0);
        let mut remaining = row
            .get("remaining")
            .map(json_i64_any)
            .unwrap_or_else(|| (limit - used).max(0));
        if remaining < 0 {
            remaining = 0;
        }
        if remaining > limit && limit > 0 {
            remaining = limit;
        }
        activities.push(ActivityFreeBucket {
            activity_id: row
                .get("activityId")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            model_name: row
                .get("modelName")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            model_keys,
            limit,
            used,
            remaining,
            reset_at_ms: row.get("resetAt").map(json_i64_any).unwrap_or(0),
            reset_strategy: row
                .get("resetStrategy")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            eligible: row
                .get("eligible")
                .and_then(|x| x.as_bool())
                .unwrap_or(true),
            activity_end_at_ms: row.get("activityEndAt").map(json_i64_any).unwrap_or(0),
            status_text: row
                .get("statusText")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
            cli_text: row
                .get("cliText")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
        });
    }
    Ok(ActivitySnapshot {
        activities,
        query_at_ms,
        fetched_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    })
}

fn activity_upstream_key(model: &str) -> String {
    let bare = model.rsplit('/').next().unwrap_or(model).trim();
    model_cfg(bare).key.to_string()
}

fn find_free_bucket_for_model<'a>(
    snap: &'a ActivitySnapshot,
    model: &str,
) -> Option<&'a ActivityFreeBucket> {
    let key = activity_upstream_key(model);
    let key_l = key.to_ascii_lowercase();
    snap.activities.iter().find(|b| {
        b.eligible
            && b.model_keys
                .iter()
                .any(|k| k.eq_ignore_ascii_case(&key_l))
    })
}

fn free_remaining_for_model(snap: &ActivitySnapshot, model: &str) -> i64 {
    find_free_bucket_for_model(snap, model)
        .map(|b| b.remaining.max(0))
        .unwrap_or(0)
}

pub fn model_has_free_activity_path(account: &Account, model: &str) -> bool {
    free_remaining_from_account_data(&account.data_json(), model) > 0
}

pub fn free_remaining_for_account_model(account: &Account, model: &str) -> i64 {
    free_remaining_from_account_data(&account.data_json(), model)
}

pub fn is_ultimate_free_activity_model(model: &str) -> bool {
    let bare = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .trim()
        .to_ascii_lowercase();
    matches!(
        bare.as_str(),
        "ultimate" | "quest-ultimate" | "experts-ultimate" | "qmodel_38max" | "qwen3.8-max"
    ) || matches!(
        activity_upstream_key(model).as_str(),
        "ultimate" | "qmodel_38max"
    )
}

/// True only for the Qwen3.8-Max free bucket (`qwen38_800_invoke` /
/// `qmodel_38max`). Used by the Qwen3.8-Max-free-only pick mode so rotation is
/// scoped to that promo and never the retired Ultimate bucket.
pub fn is_qwen38_free_activity_model(model: &str) -> bool {
    let bare = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .trim()
        .to_ascii_lowercase();
    matches!(bare.as_str(), "qmodel_38max" | "qwen3.8-max" | "qwen3.8max")
        || activity_upstream_key(model) == "qmodel_38max"
}

fn write_free_remaining_used(account: &mut Account, remaining: i64, used: i64) {
    let mut data = account.data_json();
    let obj = match data.as_object_mut() {
        Some(o) => o,
        None => return,
    };
    let rem = remaining.max(0);
    let used = used.max(0);
    obj.insert("free_remaining".into(), json!(rem));
    obj.insert("free_used".into(), json!(used));
    if let Some(Value::Array(acts)) = obj.get_mut("activity_free") {
        for row in acts.iter_mut() {
            let keys_hit = row
                .get("modelKeys")
                .and_then(|k| k.as_array())
                .map(|arr| {
                    arr.iter().any(|x| {
                        x.as_str()
                            .map(|s| {
                                let sl = s.to_ascii_lowercase();
                                sl == "ultimate"
                                    || sl == "quest-ultimate"
                                    || sl == "experts-ultimate"
                                    || sl == "qmodel_38max"
                            })
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            let id_hit = row
                .get("activityId")
                .and_then(|x| x.as_str())
                .map(|s| s == "ultimate_200_free_invoke" || s == "qwen38_800_invoke")
                .unwrap_or(false);
            if keys_hit || id_hit {
                if let Some(map) = row.as_object_mut() {
                    map.insert("remaining".into(), json!(rem));
                    map.insert("used".into(), json!(used));
                }
            }
        }
    }
    account.set_data_json(&data);
}

pub fn optimistic_consume_free_call(account: &mut Account, model: &str) -> Option<i64> {
    if !is_ultimate_free_activity_model(model) {
        return None;
    }
    if free_remaining_from_account_data(&account.data_json(), model) <= 0 {
        return None;
    }
    let data = account.data_json();
    let rem = data
        .get("free_remaining")
        .map(json_i64_any)
        .unwrap_or(0)
        .max(0);
    if rem <= 0 {
        return None;
    }
    let used = data
        .get("free_used")
        .map(json_i64_any)
        .unwrap_or(0)
        .max(0);
    let new_rem = (rem - 1).max(0);
    let new_used = used + 1;
    write_free_remaining_used(account, new_rem, new_used);
    Some(new_rem)
}

pub fn cap_free_remaining_after_sync(account: &mut Account, model: &str, max_remaining: i64) {
    if !is_ultimate_free_activity_model(model) {
        return;
    }
    let cur = free_remaining_from_account_data(&account.data_json(), model);
    if cur <= max_remaining {
        return;
    }
    let data = account.data_json();
    let limit = data
        .get("free_limit")
        .map(json_i64_any)
        .unwrap_or(0)
        .max(0);
    let used = if limit > 0 {
        (limit - max_remaining).max(0)
    } else {
        data.get("free_used")
            .map(json_i64_any)
            .unwrap_or(0)
            .max(0)
    };
    write_free_remaining_used(account, max_remaining.max(0), used);
}

fn free_remaining_from_account_data(data: &Value, model: &str) -> i64 {
    let key = activity_upstream_key(model);
    let bare = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .trim()
        .to_ascii_lowercase();
    if let Some(keys) = data
        .get("free_model_keys")
        .and_then(|k| k.as_array())
    {
        let hit = keys.iter().any(|k| {
            k.as_str()
                .map(|s| {
                    let sl = s.to_ascii_lowercase();
                    sl == key || sl == bare
                })
                .unwrap_or(false)
        });
        if hit {
            return data
                .get("free_remaining")
                .map(json_i64_any)
                .unwrap_or(0)
                .max(0);
        }
    }
    let acts = match data.get("activity_free") {
        Some(v) => v,
        None => return 0,
    };
    let snap = ActivitySnapshot {
        activities: parse_stored_activities(acts),
        query_at_ms: 0,
        fetched_at: String::new(),
    };
    free_remaining_for_model(&snap, model)
}

fn parse_stored_activities(v: &Value) -> Vec<ActivityFreeBucket> {
    let arr = match v.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|row| {
            let model_keys: Vec<String> = row
                .get("modelKeys")
                .and_then(|k| k.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if model_keys.is_empty() && row.get("activityId").is_none() {
                return None;
            }
            Some(ActivityFreeBucket {
                activity_id: row
                    .get("activityId")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                model_name: row
                    .get("modelName")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                model_keys,
                limit: row.get("limit").map(json_i64_any).unwrap_or(0).max(0),
                used: row.get("used").map(json_i64_any).unwrap_or(0).max(0),
                remaining: row.get("remaining").map(json_i64_any).unwrap_or(0).max(0),
                reset_at_ms: row.get("resetAt").map(json_i64_any).unwrap_or(0),
                reset_strategy: row
                    .get("resetStrategy")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                eligible: row
                    .get("eligible")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(true),
                activity_end_at_ms: row.get("activityEndAt").map(json_i64_any).unwrap_or(0),
                status_text: row
                    .get("statusText")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string()),
                cli_text: row
                    .get("cliText")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string()),
            })
        })
        .collect()
}

fn apply_activity_to_account(account: &mut Account, snap: &ActivitySnapshot) {
    let mut data = account.data_json();
    let obj = match data.as_object_mut() {
        Some(o) => o,
        None => return,
    };
    let acts_json: Vec<Value> = snap
        .activities
        .iter()
        .map(|b| {
            json!({
                "activityId": b.activity_id,
                "modelName": b.model_name,
                "modelKeys": b.model_keys,
                "limit": b.limit,
                "used": b.used,
                "remaining": b.remaining,
                "resetAt": b.reset_at_ms,
                "resetStrategy": b.reset_strategy,
                "eligible": b.eligible,
                "activityEndAt": b.activity_end_at_ms,
                "statusText": b.status_text,
                "cliText": b.cli_text,
            })
        })
        .collect();
    obj.insert("activity_free".into(), Value::Array(acts_json));
    obj.insert("activity_query_at_ms".into(), json!(snap.query_at_ms));
    obj.insert(
        "activity_synced_at".into(),
        Value::String(snap.fetched_at.clone()),
    );
    obj.remove("activity_error");

    let primary = snap
        .activities
        .iter()
        .find(|b| {
            b.activity_id == "ultimate_200_free_invoke"
                || b.activity_id == "qwen38_800_invoke"
                || b.model_keys
                    .iter()
                    .any(|k| k.eq_ignore_ascii_case("ultimate") || k.eq_ignore_ascii_case("qmodel_38max"))
        })
        .or_else(|| snap.activities.first());

    if let Some(b) = primary {
        obj.insert("free_limit".into(), json!(b.limit));
        obj.insert("free_remaining".into(), json!(b.remaining));
        obj.insert("free_used".into(), json!(b.used));
        obj.insert("free_activity_id".into(), Value::String(b.activity_id.clone()));
        obj.insert(
            "free_model_keys".into(),
            json!(b.model_keys.clone()),
        );
        obj.insert(
            "free_reset_strategy".into(),
            Value::String(b.reset_strategy.clone()),
        );
        if let Some(s) = ms_to_rfc3339(b.activity_end_at_ms) {
            obj.insert("free_end_at".into(), Value::String(s));
        } else {
            obj.insert("free_end_at".into(), Value::Null);
        }
        obj.insert("free_end_ms".into(), json!(b.activity_end_at_ms));
        if let Some(ref t) = b.cli_text {
            obj.insert("free_cli_text".into(), Value::String(t.clone()));
        } else if let Some(ref t) = b.status_text {
            obj.insert("free_cli_text".into(), Value::String(t.clone()));
        }
    } else {
        obj.insert("free_limit".into(), json!(0));
        obj.insert("free_remaining".into(), json!(0));
        obj.insert("free_used".into(), json!(0));
        obj.insert("free_activity_id".into(), Value::Null);
        obj.insert("free_model_keys".into(), json!([]));
        obj.insert("free_reset_strategy".into(), Value::Null);
        obj.insert("free_end_at".into(), Value::Null);
        obj.insert("free_end_ms".into(), json!(0));
        obj.insert("free_cli_text".into(), Value::Null);
    }
    account.set_data_json(&data);
}

fn apply_activity_error_to_account(account: &mut Account, err: &str) {
    let mut data = account.data_json();
    if let Some(obj) = data.as_object_mut() {
        obj.insert(
            "activity_error".into(),
            Value::String(err.chars().take(200).collect()),
        );
        obj.insert(
            "activity_synced_at".into(),
            Value::String(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        );
        account.set_data_json(&data);
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
            self.apply_job_token(&mut tokens).await?;
            dirty = true;
        }
        if dirty {
            account.data = tokens.to_data().to_string();
        }
        Ok(())
    }

    async fn force_refresh(&self, account: &mut Account) -> Result<(), ProviderError> {
        let data: Value = serde_json::from_str(&account.data).unwrap_or(Value::Null);
        let mut tokens = QoderTokens::from_data(&data)?;
        tokens.security_oauth_token = None;
        tokens.user_id = None;
        self.apply_job_token(&mut tokens).await?;
        account.data = tokens.to_data().to_string();
        Ok(())
    }

    async fn sync_quota(&self, account: &mut Account) -> Result<(), ProviderError> {
        let data: Value = serde_json::from_str(&account.data).unwrap_or(Value::Null);
        let mut tokens = QoderTokens::from_data(&data)?;
        let sot = match tokens
            .security_oauth_token
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            Some(s) => s.to_string(),
            None => {
                self.apply_job_token(&mut tokens).await?;
                account.data = tokens.to_data().to_string();
                tokens
                    .security_oauth_token
                    .clone()
                    .filter(|s| !s.is_empty())
                    .ok_or(ProviderError::AuthExpired)?
            }
        };
        let (limit, remaining) = self.fetch_quota_usage(&sot).await?;
        account.quota_limit = limit;
        account.quota_remaining = remaining;

        match self.fetch_user_plan(&sot).await {
            Ok(plan) => apply_user_plan_to_account(account, &plan),
            Err(e) => {
                tracing::warn!(
                    account = %account.id,
                    error = %e,
                    "qoder user/plan sync failed (credits still updated)"
                );
            }
        }

        let data: Value = serde_json::from_str(&account.data).unwrap_or(Value::Null);
        match QoderTokens::from_data(&data) {
            Ok(tokens) => match self.fetch_activity(&tokens).await {
                Ok(snap) => apply_activity_to_account(account, &snap),
                Err(e) => {
                    tracing::warn!(
                        account = %account.id,
                        error = %e,
                        "qoder activity free-quota sync failed (credits/plan still updated)"
                    );
                    apply_activity_error_to_account(account, &e.to_string());
                }
            },
            Err(e) => {
                tracing::warn!(
                    account = %account.id,
                    error = %e,
                    "qoder activity skipped: tokens unreadable after plan sync"
                );
            }
        }
        Ok(())
    }

    async fn chat(
        &self,
        _client: &Client,
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
            headers.insert("cosy-machineos", COSY_MACHINE_OS.parse().unwrap());
            headers.insert("cosy-machinetoken", tokens.machine_token.parse().unwrap());
            headers.insert("login-version", "v2".parse().unwrap());
            headers.insert("user-agent", "Bun/1.3.14".parse().unwrap());
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
                return Err(classify_qoder_status(status, &text));
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
                        if let Some(err) = first_chunk_service_error(&text) {
                            tracing::warn!(
                                error = %err,
                                "qoder service error in first SSE chunk; returning to pool for rotate"
                            );
                            return Err(err);
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
                    let mut any_tool_calls = false;
                    let mut prompt_tokens: i64 = 0;
                    let mut completion_tokens: i64 = 0;
                    let mut total_tokens: i64 = 0;
                    let mut accumulated_text = String::new();
                    let mut usage_tx = Some(usage_tx);
                    let mut tool_state = ToolCallStreamState::default();
                    let mut upstream_finish: Option<String> = None;

                    loop {
                        while let Some(pos) = buffer.find('\n') {
                            let line = buffer[..pos].trim_end_matches('\r').to_string();
                            buffer = buffer[pos + 1..].to_string();
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                continue;
                            }
                            // NOTE (P0 limitation): mid-stream Qoder 403 cannot trigger pool cooldown because Ok(Stream) was already returned; surfaced as client stream error only. See plan AC#9.
                            if let Some((_svc_status, svc_msg)) = parse_qoder_service_error(trimmed) {
                                tracing::warn!(error = %svc_msg, "qoder upstream error in SSE");
                                let _ = tx
                                    .send(Err(std::io::Error::new(
                                        std::io::ErrorKind::Other,
                                        svc_msg,
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

                                let choice = inner
                                    .get("choices")
                                    .and_then(|c| c.as_array())
                                    .and_then(|a| a.first());
                                let delta = choice.and_then(|c| c.get("delta"));
                                let finish_reason = choice.and_then(|c| c.get("finish_reason"));
                                let is_finish = finish_reason
                                    .map(|v| !v.is_null())
                                    .unwrap_or(false);
                                if is_finish {
                                    if let Some(fr) = finish_reason.and_then(|v| v.as_str()) {
                                        upstream_finish = Some(fr.to_string());
                                    }
                                }

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

                                if let Some(tcs) = delta
                                    .and_then(|d| d.get("tool_calls"))
                                    .and_then(|v| v.as_array())
                                {
                                    if !tcs.is_empty() {
                                        let remapped = tool_state.ingest(tcs);
                                        if !remapped.is_empty() {
                                            delta_out["tool_calls"] = json!(remapped);
                                            has_delta = true;
                                            any_tool_calls = true;
                                        }
                                    }
                                }

                                if has_delta {
                                    if !first_chunk_sent {
                                        first_chunk_sent = true;
                                        delta_out["role"] = json!("assistant");
                                    }
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
                                    let fr = resolve_qoder_finish_reason(
                                        upstream_finish.as_deref(),
                                        any_content,
                                        any_tool_calls,
                                    );
                                    if !any_content && !any_tool_calls {
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
                                        fr,
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
                                    any_tool_calls,
                                    "qoder stream body decode failed"
                                );
                                if first_chunk_sent || any_content || any_tool_calls {
                                    let eof_finish = resolve_qoder_finish_reason(
                                        upstream_finish.as_deref(),
                                        any_content,
                                        any_tool_calls,
                                    );
                                    if !any_content && !any_tool_calls {
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

                    let eof_finish = resolve_qoder_finish_reason(
                        upstream_finish.as_deref(),
                        any_content,
                        any_tool_calls,
                    );
                    if !any_content && !any_tool_calls {
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

            let mut tool_state = ToolCallStreamState::default();
            let mut upstream_finish: Option<String> = None;

            for line in buffer.lines() {
                let trimmed = line.trim().trim_end_matches('\r');
                if trimmed.is_empty() {
                    continue;
                }
                if let Some((svc_status, svc_msg)) = parse_qoder_service_error(trimmed) {
                    return Err(provider_error_from_qoder_service(svc_status, &svc_msg));
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
                    if let Some(fr) = choice
                        .and_then(|c| c.get("finish_reason"))
                        .and_then(|v| v.as_str())
                    {
                        upstream_finish = Some(fr.to_string());
                    }
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
                    if let Some(tcs) = delta
                        .and_then(|d| d.get("tool_calls"))
                        .and_then(|v| v.as_array())
                    {
                        let _ = tool_state.ingest(tcs);
                    }
                }
            }

            let _ = stream_eof_partial;

            let filled_tools = tool_state.snapshot_filled();
            let any_tool_calls = !filled_tools.is_empty();
            let final_text = if !accumulated_text.is_empty() {
                accumulated_text
            } else if any_tool_calls {
                String::new()
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

            let finish_reason_str = resolve_qoder_finish_reason(
                upstream_finish.as_deref(),
                !final_text.is_empty(),
                any_tool_calls,
            );
            if final_text.is_empty() && !any_tool_calls {
                tracing::warn!("Qoder silent reject mid-non-stream (no content generated). Marking finish_reason as 'length'.");
            }

            let mut message = serde_json::json!({
                "role": "assistant",
                "content": final_text
            });
            if any_tool_calls {
                message["tool_calls"] = json!(filled_tools);
            }

            let result_json = serde_json::json!({
                "id": resp_id,
                "object": "chat.completion",
                "created": chrono::Utc::now().timestamp(),
                "model": req_model,
                "choices": [{
                    "index": 0,
                    "message": message,
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
    if content.is_null() {
        return String::new();
    }
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for item in arr {
            if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                parts.push(t.to_string());
            } else if item.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                || item.get("type").and_then(|t| t.as_str()) == Some("tool_result")
            {
                continue;
            } else if let Some(s) = item.as_str() {
                parts.push(s.to_string());
            }
        }
        return parts.join("\n");
    }
    String::new()
}

fn tool_arguments_to_string(args: &Value) -> String {
    if let Some(s) = args.as_str() {
        s.to_string()
    } else if args.is_null() {
        String::new()
    } else {
        serde_json::to_string(args).unwrap_or_default()
    }
}

fn normalize_openai_tool_calls(raw: &Value) -> Vec<Value> {
    let Some(arr) = raw.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|tc| {
            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let type_ = tc
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("function")
                .to_string();
            let func = tc.get("function")?;
            let name = func
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = func
                .get("arguments")
                .map(tool_arguments_to_string)
                .unwrap_or_default();
            if name.is_empty() && arguments.is_empty() && id.is_empty() {
                return None;
            }
            Some(json!({
                "id": id,
                "type": type_,
                "function": { "name": name, "arguments": arguments }
            }))
        })
        .collect()
}

fn generate_openai_tool_id() -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!("call_{}", &hex[..24.min(hex.len())])
}

fn normalize_tool_call_id(id: Option<&str>, _index: usize) -> String {
    let Some(mut id) = id.filter(|s| !s.is_empty()).map(|s| s.to_string()) else {
        return generate_openai_tool_id();
    };
    if let Some(rest) = id.strip_prefix("toolu_") {
        id = rest.to_string();
    }
    if id.len() < 20 {
        return generate_openai_tool_id();
    }
    id
}

fn resolve_qoder_finish_reason(
    upstream: Option<&str>,
    any_content: bool,
    any_tool_calls: bool,
) -> &'static str {
    match upstream {
        Some("tool_calls") => "tool_calls",
        Some("length") => "length",
        Some("content_filter") => "content_filter",
        Some("stop") => {
            if any_tool_calls {
                "tool_calls"
            } else if any_content {
                "stop"
            } else {
                "length"
            }
        }
        Some(_) => {
            if any_tool_calls {
                "tool_calls"
            } else if any_content {
                "stop"
            } else {
                "length"
            }
        }
        None => {
            if any_tool_calls {
                "tool_calls"
            } else if any_content {
                "stop"
            } else {
                "length"
            }
        }
    }
}

#[derive(Default)]
struct ToolCallStreamState {
    index_map: std::collections::HashMap<String, usize>,
    next_idx: usize,
    pending: std::collections::HashMap<usize, (String, String, String)>,
}

impl ToolCallStreamState {
    fn ingest(&mut self, tcs: &[Value]) -> Vec<Value> {
        let mut remapped = Vec::new();
        for tc in tcs {
            let key = if let Some(i) = tc.get("index").and_then(|v| v.as_u64()) {
                format!("idx-{i}")
            } else if let Some(id) = tc.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
            {
                id.to_string()
            } else {
                format!("tool-{}", self.next_idx)
            };
            let idx = if let Some(&existing) = self.index_map.get(&key) {
                existing
            } else {
                let i = self.next_idx;
                self.next_idx += 1;
                self.index_map.insert(key, i);
                let stable = normalize_tool_call_id(tc.get("id").and_then(|v| v.as_str()), i);
                self.pending
                    .insert(i, (stable, String::new(), String::new()));
                i
            };
            if let Some(entry) = self.pending.get_mut(&idx) {
                if let Some(name) = tc
                    .pointer("/function/name")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    entry.1 = name.to_string();
                }
                if let Some(args) = tc.pointer("/function/arguments") {
                    entry.2.push_str(&tool_arguments_to_string(args));
                }
            }
            let (stable_id, name, _args) = self
                .pending
                .get(&idx)
                .cloned()
                .unwrap_or_else(|| (generate_openai_tool_id(), String::new(), String::new()));
            let type_ = tc
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("function");
            let chunk_name = tc
                .pointer("/function/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let chunk_args = tc
                .pointer("/function/arguments")
                .map(tool_arguments_to_string)
                .unwrap_or_default();
            let mut f = serde_json::Map::new();
            if !chunk_name.is_empty() {
                f.insert("name".into(), json!(chunk_name));
            }
            if tc.pointer("/function/arguments").is_some() {
                f.insert("arguments".into(), json!(chunk_args));
            }
            let mut piece = json!({
                "index": idx,
                "id": stable_id,
                "type": type_,
            });
            if !f.is_empty() {
                piece["function"] = Value::Object(f);
            } else if !name.is_empty() {
                piece["function"] = json!({ "name": name, "arguments": "" });
            }
            remapped.push(piece);
        }
        remapped
    }

    fn snapshot_filled(&self) -> Vec<Value> {
        let mut keys: Vec<usize> = self.pending.keys().copied().collect();
        keys.sort_unstable();
        keys.into_iter()
            .filter_map(|idx| {
                let (id, name, args) = self.pending.get(&idx)?;
                if id.is_empty() {
                    return None;
                }
                Some(json!({
                    "index": idx,
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": args
                    }
                }))
            })
            .collect()
    }
}

fn build_qoder_messages(req: &ChatCompletionRequest) -> Vec<Value> {
    let has_incoming_tools = req.has_tools();
    let incoming_has_system = req.messages.iter().any(|m| m.role == "system");
    let mut result: Vec<Value> = Vec::new();

    if has_incoming_tools && !incoming_has_system {
        let tools = req.tools.as_ref().and_then(|v| v.as_array());
        let mut descriptions = Vec::new();
        let mut names = Vec::new();
        if let Some(tools) = tools {
            for t in tools {
                let name = t
                    .pointer("/function/name")
                    .or_else(|| t.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                names.push(name.to_string());
                let desc = t
                    .pointer("/function/description")
                    .or_else(|| t.get("description"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("No description");
                let param_names: Vec<&str> = t
                    .pointer("/function/parameters/properties")
                    .or_else(|| t.pointer("/parameters/properties"))
                    .and_then(|v| v.as_object())
                    .map(|o| o.keys().map(|k| k.as_str()).collect())
                    .unwrap_or_default();
                let param_info = if param_names.is_empty() {
                    String::new()
                } else {
                    format!(" Parameters: {}", param_names.join(", "))
                };
                descriptions.push(format!("- {name}: {desc}{param_info}"));
            }
        }
        let tool_descriptions = descriptions.join("\n");
        let tool_names = names.join(", ");
        let sys = format!(
            "You are a helpful assistant with access to the following tools:\n\n\
             {tool_descriptions}\n\n\
             ## Tool Usage Guidelines:\n\n\
             1. **When to use tools**: When the user's request requires information retrieval, file operations, code execution, or any action that these tools can perform, you MUST call the appropriate tool. Do not say you cannot help; instead, invoke the tool with the correct arguments.\n\n\
             2. **Trust tool results**: After calling a tool, you will receive the tool result in the conversation. The tool result contains the actual data or outcome of the tool execution. Use this information to formulate your response. Do not claim you didn't receive file contents or data if the tool result was provided.\n\n\
             3. **Multi-turn workflows**: For complex tasks requiring multiple tool calls:\n\
                - Call tools sequentially as needed\n\
                - Use information from previous tool results to inform subsequent calls\n\
                - Only respond with your final answer after you have gathered all necessary information\n\n\
             4. **Error handling**: If a tool returns an error or empty result, acknowledge this to the user and suggest alternatives or next steps.\n\n\
             5. **Text-only responses**: Only respond with plain text (without tool calls) when:\n\
                - No available tool can address the user's request\n\
                - You already have all the information needed from previous tool results\n\
                - The user is asking for clarification or a simple answer\n\n\
             Available tools: {tool_names}"
        );
        result.push(json!({
            "role": "system",
            "content": sys
        }));
    } else if !has_incoming_tools && !incoming_has_system {
        let sys = "You are a helpful AI assistant. Answer the user's questions clearly and concisely. Maintain context from earlier turns in the conversation.";
        result.push(json!({
            "role": "system",
            "content": sys
        }));
    }

    for m in &req.messages {
        if m.role == "assistant" {
            if let Some(tcs) = m.tool_calls.as_ref() {
                let tool_calls = normalize_openai_tool_calls(tcs);
                if !tool_calls.is_empty() {
                    let content = content_to_text(&m.content);
                    let contents = if content.is_empty() {
                        json!([])
                    } else {
                        json!([{ "type": "text", "text": content }])
                    };
                    result.push(json!({
                        "role": "assistant",
                        "content": content,
                        "contents": contents,
                        "tool_calls": tool_calls
                    }));
                    continue;
                }
            }
        }

        if m.content.is_null() || m.content.as_str().is_some() {
            let content = content_to_text(&m.content);
            let mut msg = json!({
                "role": m.role,
                "content": content,
                "contents": [{ "type": "text", "text": content }]
            });
            if m.role == "tool" {
                if let Some(id) = m.tool_call_id.as_ref() {
                    msg["tool_call_id"] = json!(id);
                }
            }
            result.push(msg);
            continue;
        }

        if let Some(blocks) = m.content.as_array() {
            let mut text_parts = Vec::new();
            let mut image_parts = Vec::new();
            let mut tool_calls = Vec::new();
            let mut tool_results: Vec<(String, String)> = Vec::new();

            for b in blocks {
                let Some(obj) = b.as_object() else { continue };
                let ty = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match ty {
                    "text" => {
                        if let Some(t) = obj.get("text").and_then(|v| v.as_str()) {
                            text_parts.push(t.to_string());
                        }
                    }
                    "image_url" | "image" | "input_image" => {
                        image_parts.push(b.clone());
                    }
                    "tool_use" => {
                        let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let arguments = obj
                            .get("input")
                            .map(tool_arguments_to_string)
                            .unwrap_or_default();
                        tool_calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": arguments }
                        }));
                    }
                    "tool_result" => {
                        let tool_use_id = obj
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let mut content = String::new();
                        if let Some(s) = obj.get("content").and_then(|v| v.as_str()) {
                            content = s.to_string();
                        } else if let Some(arr) = obj.get("content").and_then(|v| v.as_array()) {
                            content = arr
                                .iter()
                                .filter_map(|inner| {
                                    inner.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                        }
                        if obj.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false) {
                            content = format!("[ERROR] {content}");
                        }
                        tool_results.push((tool_use_id, content));
                    }
                    _ => {
                        if let Some(t) = obj.get("text").and_then(|v| v.as_str()) {
                            text_parts.push(t.to_string());
                        }
                    }
                }
            }

            let text_content = text_parts.join("\n");
            let mut contents_arr = Vec::new();
            if !text_content.is_empty() {
                contents_arr.push(json!({ "type": "text", "text": text_content }));
            }
            contents_arr.extend(image_parts);

            if m.role == "assistant" && !tool_calls.is_empty() {
                result.push(json!({
                    "role": "assistant",
                    "content": text_content,
                    "contents": if text_content.is_empty() {
                        json!([])
                    } else {
                        json!([{ "type": "text", "text": text_content }])
                    },
                    "tool_calls": tool_calls
                }));
                continue;
            }

            if m.role == "user" && !tool_results.is_empty() {
                for (tool_call_id, content) in tool_results {
                    result.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": content
                    }));
                }
                if !contents_arr.is_empty() {
                    result.push(json!({
                        "role": "user",
                        "content": text_content,
                        "contents": contents_arr
                    }));
                }
                continue;
            }

            result.push(json!({
                "role": m.role,
                "content": text_content,
                "contents": contents_arr
            }));
            continue;
        }

        result.push(json!({
            "role": m.role,
            "content": "",
            "contents": []
        }));
    }

    result
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

/// Qoder-scoped status classification. Unlike Grok, Qoder 403/402 are
/// treated as TEMPORARY exhaustion (RateLimited -> cooldown), not a permanent cut,
/// because Qoder free-tier quota surfaces as 403/402 and recovers after cooldown.
fn classify_qoder_status(status: u16, body: &str) -> ProviderError {
    match status {
        403 | 402 => ProviderError::RateLimited { retry_after_secs: None },
        401 => ProviderError::AuthExpired,
        _ => classify_http_status(status, body),
    }
}

fn parse_qoder_service_error(line: &str) -> Option<(u16, String)> {
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
    Some((
        svc as u16,
        format!(
            "Qoder HTTP {svc} {err_status}: {}",
            if err_msg.is_empty() {
                "rate limited or quota exceeded"
            } else {
                &err_msg[..err_msg.len().min(200)]
            }
        ),
    ))
}

fn provider_error_from_qoder_service(svc_status: u16, svc_msg: &str) -> ProviderError {
    match svc_status {
        402 | 403 => ProviderError::RateLimited {
            retry_after_secs: None,
        },
        400 | 429 => {
            let lower = svc_msg.to_ascii_lowercase();
            if lower.contains("quota")
                || lower.contains("rate")
                || lower.contains("limit")
                || lower.contains("exceed")
                || lower.contains("pricing")
                || lower.contains("balance")
                || lower.contains("credit")
            {
                ProviderError::RateLimited {
                    retry_after_secs: None,
                }
            } else {
                ProviderError::Transport(svc_msg.to_string())
            }
        }
        _ => ProviderError::Transport(svc_msg.to_string()),
    }
}

fn first_chunk_service_error(text: &str) -> Option<ProviderError> {
    for line in text.lines() {
        let trimmed = line.trim().trim_end_matches('\r');
        if trimmed.is_empty() {
            continue;
        }
        if let Some((status, msg)) = parse_qoder_service_error(trimmed) {
            return Some(provider_error_from_qoder_service(status, &msg));
        }
    }
    None
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

    let messages = build_qoder_messages(req);
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
    if req.has_tools() {
        body["tools"] = req.tools.clone().unwrap_or_else(|| json!([]));
    } else {
        body["tools"] = json!([]);
    }
    if let Some(obj) = body.as_object_mut() {
        obj.remove("tool_choice");
        obj.remove("parallel_tool_calls");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_cfg_qmodel_preview_maps_qwen38() {
        let cfg = model_cfg("qmodel_preview");
        assert_eq!(cfg.key, "qmodel_preview");
        assert_eq!(cfg.display_name, "Qwen3.8-Max-Preview");
        assert_eq!(cfg.max_input_tokens, 180_000);
        assert!(cfg.is_reasoning);
        assert!(cfg.is_vl);

        let alias = model_cfg("qwen3.8-max-preview");
        assert_eq!(alias.key, "qmodel_preview");
        let alias2 = model_cfg("qwen3.8-preview");
        assert_eq!(alias2.key, "qmodel_preview");
    }

    #[test]
    fn model_cfg_live_upstream_keys_and_legacy_aliases() {
        assert_eq!(model_cfg("qmodel1").key, "qmodel1");
        assert_eq!(model_cfg("qmodel").key, "qmodel1");
        assert_eq!(model_cfg("qwen3.7-plus").key, "qmodel1");
        assert_eq!(model_cfg("qmodel1").max_input_tokens, 1_000_000);

        assert_eq!(model_cfg("kmodel_latest").key, "kmodel_latest");
        assert_eq!(model_cfg("kimi-k3").display_name, "Kimi-K3");
        assert_eq!(model_cfg("kmodel_latest").max_input_tokens, 180_000);

        assert_eq!(model_cfg("kmodel1").key, "kmodel1");
        assert_eq!(model_cfg("kmodel").key, "kmodel1");
        assert_eq!(model_cfg("kimi-k2.7-code").display_name, "Kimi-K2.7-Code");
        assert_eq!(model_cfg("kmodel1").max_input_tokens, 256_000);

        assert_eq!(model_cfg("gm51model1").key, "gm51model1");
        assert_eq!(model_cfg("gm51model").key, "gm51model1");
        assert_eq!(model_cfg("glm-5.2").display_name, "GLM-5.2");
        assert!(model_cfg("gm51model1").is_reasoning);
        assert_eq!(model_cfg("gm51model1").max_input_tokens, 1_000_000);

        assert_eq!(model_cfg("dmodel1").key, "dmodel1");
        assert_eq!(model_cfg("dmodel").key, "dmodel1");
        assert_eq!(model_cfg("dfmodel1").key, "dfmodel1");
        assert_eq!(model_cfg("dfmodel").key, "dfmodel1");
        assert_eq!(model_cfg("dmodel1").max_input_tokens, 1_000_000);

        assert_eq!(model_cfg("mmodel").display_name, "MiniMax-M3");
        assert_eq!(model_cfg("minimax-m3").key, "mmodel");

        assert_eq!(model_cfg("ultimate").max_input_tokens, 1_000_000);
        assert_eq!(model_cfg("performance").max_input_tokens, 1_000_000);

        assert_eq!(model_cfg("lite").key, "lite");
        assert!(!model_cfg("lite").is_vl);
    }

    #[test]
    fn classify_qoder_status_403_returns_rate_limited() {
        let err = classify_qoder_status(403, "");
        assert!(
            matches!(err, ProviderError::RateLimited { .. }),
            "expected RateLimited for 403, got: {:?}",
            err
        );
    }

    #[test]
    fn classify_qoder_status_402_returns_rate_limited() {
        let err = classify_qoder_status(402, "");
        assert!(
            matches!(err, ProviderError::RateLimited { .. }),
            "expected RateLimited for 402, got: {:?}",
            err
        );
    }

    #[test]
    fn classify_qoder_status_401_returns_auth_expired() {
        let err = classify_qoder_status(401, "");
        assert!(
            matches!(err, ProviderError::AuthExpired),
            "expected AuthExpired for 401, got: {:?}",
            err
        );
    }

    #[test]
    fn classify_qoder_status_500_returns_upstream() {
        let err = classify_qoder_status(500, "boom");
        assert!(
            matches!(err, ProviderError::Upstream { .. }),
            "expected Upstream for 500, got: {:?}",
            err
        );
    }

    #[test]
    fn parse_qoder_service_error_403_returns_tuple_with_status() {
        let line = r#"data: {"statusCodeValue":403,"statusCode":"FORBIDDEN","message":"quota"}"#;
        let result = parse_qoder_service_error(line);
        assert!(result.is_some(), "expected Some for 403 error line");
        let (status, _msg) = result.unwrap();
        assert_eq!(status, 403, "expected tuple first element == 403");
    }

    #[test]
    fn parse_qoder_service_error_500_returns_tuple_with_status() {
        let line = r#"data: {"statusCodeValue":500,"statusCode":"ERR","message":"x"}"#;
        let result = parse_qoder_service_error(line);
        assert!(result.is_some(), "expected Some for 500 error line");
        let (status, _msg) = result.unwrap();
        assert_eq!(status, 500, "expected tuple first element == 500");
    }

    #[test]
    fn first_chunk_400_empty_msg_is_rate_limited() {
        let chunk = "data: {\"statusCodeValue\":400,\"statusCode\":\"BAD_REQUEST\",\"message\":\"\"}\n\n";
        let err = first_chunk_service_error(chunk).expect("service error");
        assert!(
            matches!(err, ProviderError::RateLimited { .. }),
            "expected RateLimited, got: {err:?}"
        );
    }

    #[test]
    fn first_chunk_403_is_rate_limited() {
        let chunk =
            "data: {\"statusCodeValue\":403,\"statusCode\":\"FORBIDDEN\",\"message\":\"quota\"}\n";
        let err = first_chunk_service_error(chunk).expect("service error");
        assert!(matches!(err, ProviderError::RateLimited { .. }));
    }

    #[test]
    fn first_chunk_400_non_quota_is_transport() {
        let chunk =
            "data: {\"statusCodeValue\":400,\"statusCode\":\"BAD_REQUEST\",\"message\":\"invalid schema\"}\n";
        let err = first_chunk_service_error(chunk).expect("service error");
        assert!(
            matches!(err, ProviderError::Transport(_)),
            "expected Transport for non-quota 400, got: {err:?}"
        );
    }

    #[test]
    fn first_chunk_normal_sse_is_ok() {
        let chunk = "data: {\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"hi\\\"}}]}\"}\n";
        assert!(first_chunk_service_error(chunk).is_none());
    }

    #[test]
    fn provider_error_from_qoder_service_400_quota_wording() {
        let err = provider_error_from_qoder_service(
            400,
            "Qoder HTTP 400 BAD_REQUEST: rate limited or quota exceeded",
        );
        assert!(matches!(err, ProviderError::RateLimited { .. }));
    }

    #[test]
    fn cosy_version_matches_live_cli_free_path() {
        assert_eq!(COSY_VERSION, "1.1.7");
        assert_eq!(COSY_MACHINE_OS, "x86_64_win32");
    }

    #[test]
    fn build_chat_body_ultimate_free_shape() {
        let req: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "qd/ultimate",
            "messages": [{"role":"user","content":"hai"}],
            "stream": true,
            "max_tokens": 32000
        }))
        .expect("chat req");
        let cfg = model_cfg(req.upstream_model());
        let body = build_chat_body(&req, &cfg);
        assert_eq!(body["aliyun_user_type"], "");
        assert_eq!(body["model_config"]["key"], "ultimate");
        assert_eq!(body["model_config"]["max_input_tokens"], 1_000_000);
        assert_eq!(body["business"]["product"], "cli");
        assert_eq!(body["business"]["type"], "agent");
        assert_eq!(body["business"]["version"], COSY_VERSION);
        assert_eq!(body["business"]["stage"], "start");
        assert_eq!(body["chat_task"], "FREE_INPUT");
        assert_eq!(body["session_type"], "qodercli");
    }

    #[test]
    fn parse_quota_usage_happy_path() {
        let body = r#"{
          "userId": "u1",
          "userType": "personal_professional_trial",
          "usageType": "credits",
          "isQuotaExceeded": false,
          "expiresAt": 1786332776366,
          "userQuota": {
            "total": 300.0,
            "used": 0.0,
            "remaining": 300.0,
            "percentage": 0.0,
            "unit": "credits"
          }
        }"#;
        let (limit, rem) = parse_quota_usage(body).unwrap();
        assert_eq!(limit, 300);
        assert_eq!(rem, 300);
    }

    #[test]
    fn parse_quota_usage_floors_floats() {
        let body = r#"{"userQuota":{"total":300.9,"remaining":12.9,"used":1.0,"unit":"credits"}}"#;
        let (limit, rem) = parse_quota_usage(body).unwrap();
        assert_eq!(limit, 300);
        assert_eq!(rem, 12);
    }

    #[test]
    fn parse_quota_usage_exceeded_zeros_remaining() {
        let body = r#"{"isQuotaExceeded":true,"userQuota":{"total":300.0,"remaining":5.0,"used":295.0,"unit":"credits"}}"#;
        let (limit, rem) = parse_quota_usage(body).unwrap();
        assert_eq!(limit, 300);
        assert_eq!(rem, 0);
    }

    #[test]
    fn parse_quota_usage_missing_user_quota_errors() {
        let err = parse_quota_usage(r#"{"userId":"x"}"#).unwrap_err();
        assert!(matches!(err, ProviderError::Transport(_)));
    }

    #[test]
    fn quota_usage_url_is_openapi() {
        assert_eq!(
            QUOTA_USAGE_URL,
            "https://openapi.qoder.sh/api/v2/quota/usage"
        );
    }

    #[test]
    fn parse_user_plan_pro_trial() {
        let body = r#"{
          "user_type": "personal_professional_trial",
          "plan_tier_name": "Pro Trial",
          "is_personal_version": true,
          "is_paid_plan": false,
          "is_highest_tier": false,
          "feature_allowed": {
            "wiki": true,
            "quest": true,
            "code_review": true,
            "commit_indexing": true
          },
          "start_date": 1785223050869,
          "end_date": 1786432650869
        }"#;
        let p = parse_user_plan(body).unwrap();
        assert_eq!(p.plan_tier_name, "Pro Trial");
        assert_eq!(p.user_type, "personal_professional_trial");
        assert!(!p.is_paid_plan);
        assert_eq!(p.start_date_ms, 1785223050869);
        assert_eq!(p.end_date_ms, 1786432650869);
        assert_eq!(p.feature_allowed["wiki"], true);
        let iso = ms_to_rfc3339(p.end_date_ms).unwrap();
        assert!(iso.starts_with("20"));
    }

    #[test]
    fn parse_user_plan_free_zero_end() {
        let body = r#"{
          "user_type": "personal_standard",
          "plan_tier_name": "Free",
          "is_personal_version": true,
          "is_paid_plan": false,
          "is_highest_tier": false,
          "feature_allowed": {"wiki": false, "quest": true},
          "start_date": 1785195612646,
          "end_date": 0
        }"#;
        let p = parse_user_plan(body).unwrap();
        assert_eq!(p.plan_tier_name, "Free");
        assert_eq!(p.end_date_ms, 0);
        assert!(ms_to_rfc3339(p.end_date_ms).is_none());
    }

    #[test]
    fn parse_user_plan_missing_tier_errors() {
        let err = parse_user_plan(r#"{"user_type":"x"}"#).unwrap_err();
        assert!(matches!(err, ProviderError::Transport(_)));
    }

    #[test]
    fn apply_user_plan_writes_data_fields() {
        let mut acc = Account {
            id: "a1".into(),
            provider: "qoder".into(),
            email: Some("t@example.com".into()),
            name: None,
            is_active: 1,
            priority: 0,
            data: r#"{"personalToken":"pt"}"#.into(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            quota_limit: 0,
            quota_remaining: 0,
        };
        let plan = UserPlanInfo {
            plan_tier_name: "Pro Trial".into(),
            user_type: "personal_professional_trial".into(),
            is_paid_plan: false,
            is_highest_tier: false,
            start_date_ms: 1785223050869,
            end_date_ms: 1786432650869,
            feature_allowed: json!({"wiki": true}),
        };
        apply_user_plan_to_account(&mut acc, &plan);
        let d = acc.data_json();
        assert_eq!(d["plan_tier_name"], "Pro Trial");
        assert_eq!(d["plan_end_ms"], 1786432650869_i64);
        assert!(d["plan_end_at"].as_str().unwrap().len() > 10);
        assert_eq!(d["personalToken"], "pt");

        let free = UserPlanInfo {
            plan_tier_name: "Free".into(),
            user_type: "personal_standard".into(),
            is_paid_plan: false,
            is_highest_tier: false,
            start_date_ms: 1,
            end_date_ms: 0,
            feature_allowed: json!({}),
        };
        apply_user_plan_to_account(&mut acc, &free);
        let d2 = acc.data_json();
        assert_eq!(d2["plan_tier_name"], "Free");
        assert!(d2["plan_end_at"].is_null());
        assert_eq!(d2["plan_end_ms"], 0);
    }

    #[test]
    fn user_plan_url_is_openapi() {
        assert_eq!(USER_PLAN_URL, "https://openapi.qoder.sh/api/v2/user/plan");
    }

    #[test]
    fn activity_url_is_openapi_algo() {
        assert_eq!(
            ACTIVITY_URL,
            "https://openapi.qoder.sh/algo/api/v2/activity"
        );
    }

    #[test]
    fn parse_activity_empty_unclaimed() {
        let body = r#"{
          "code": 0,
          "msg": "ok",
          "data": { "activities": [], "queryAt": 1785329743398 }
        }"#;
        let snap = parse_activity_body(body).unwrap();
        assert!(snap.activities.is_empty());
        assert_eq!(snap.query_at_ms, 1785329743398);
    }

    #[test]
    fn parse_activity_ultimate_200_free() {
        let body = r#"{
          "code": 0,
          "msg": "ok",
          "data": {
            "activities": [{
              "type": "MODEL_FREE_QUOTA",
              "activityId": "ultimate_200_free_invoke",
              "modelName": "Ultimate free",
              "modelKeys": ["ultimate", "quest-ultimate", "experts-ultimate"],
              "limit": 200,
              "used": 3,
              "remaining": 197,
              "resetAt": 0,
              "resetStrategy": "NEVER_EXPIRE",
              "eligible": true,
              "activityEndAt": 1785513540000,
              "statusText": "197 left",
              "cliText": "Used 3 / 200"
            }],
            "queryAt": 1785381043819
          }
        }"#;
        let snap = parse_activity_body(body).unwrap();
        assert_eq!(snap.activities.len(), 1);
        let b = &snap.activities[0];
        assert_eq!(b.activity_id, "ultimate_200_free_invoke");
        assert_eq!(b.limit, 200);
        assert_eq!(b.used, 3);
        assert_eq!(b.remaining, 197);
        assert_eq!(b.model_keys.len(), 3);
        assert_eq!(free_remaining_for_model(&snap, "qd/ultimate"), 197);
        assert_eq!(free_remaining_for_model(&snap, "ultimate"), 197);
        assert_eq!(free_remaining_for_model(&snap, "qd/auto"), 0);
        assert_eq!(free_remaining_for_model(&snap, "qd/lite"), 0);
        assert_eq!(activity_upstream_key("qd/ultimate"), "ultimate");
    }

    #[test]
    fn parse_activity_nonzero_code_errors() {
        let err = parse_activity_body(r#"{"code":1,"msg":"nope"}"#).unwrap_err();
        assert!(matches!(err, ProviderError::Transport(_)));
    }

    #[test]
    fn parse_activity_skips_non_free_type() {
        let body = r#"{
          "code": 0,
          "data": {
            "activities": [
              {"type": "OTHER", "activityId": "x", "modelKeys": ["ultimate"], "limit": 10, "remaining": 10, "eligible": true},
              {"type": "MODEL_FREE_QUOTA", "activityId": "u", "modelKeys": ["ultimate"], "limit": 5, "remaining": 4, "eligible": true}
            ],
            "queryAt": 1
          }
        }"#;
        let snap = parse_activity_body(body).unwrap();
        assert_eq!(snap.activities.len(), 1);
        assert_eq!(snap.activities[0].remaining, 4);
    }

    #[test]
    fn apply_activity_writes_compact_free_fields() {
        let mut acc = Account {
            id: "a1".into(),
            provider: "qoder".into(),
            email: Some("t@example.com".into()),
            name: None,
            is_active: 1,
            priority: 0,
            data: r#"{"personalToken":"pt"}"#.into(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            quota_limit: 0,
            quota_remaining: 0,
        };
        let snap = ActivitySnapshot {
            activities: vec![ActivityFreeBucket {
                activity_id: "ultimate_200_free_invoke".into(),
                model_name: "Ultimate".into(),
                model_keys: vec![
                    "ultimate".into(),
                    "quest-ultimate".into(),
                    "experts-ultimate".into(),
                ],
                limit: 200,
                used: 1,
                remaining: 199,
                reset_at_ms: 0,
                reset_strategy: "NEVER_EXPIRE".into(),
                eligible: true,
                activity_end_at_ms: 1785513540000,
                status_text: Some("199 left".into()),
                cli_text: Some("Used 1 / 200".into()),
            }],
            query_at_ms: 99,
            fetched_at: "2026-07-30T00:00:00.000Z".into(),
        };
        apply_activity_to_account(&mut acc, &snap);
        let d = acc.data_json();
        assert_eq!(d["free_limit"], 200);
        assert_eq!(d["free_remaining"], 199);
        assert_eq!(d["free_used"], 1);
        assert_eq!(d["free_activity_id"], "ultimate_200_free_invoke");
        assert_eq!(d["personalToken"], "pt");
        assert!(model_has_free_activity_path(&acc, "qd/ultimate"));
        assert!(!model_has_free_activity_path(&acc, "qd/auto"));
        assert_eq!(free_remaining_for_account_model(&acc, "ultimate"), 199);

        let empty = ActivitySnapshot {
            activities: vec![],
            query_at_ms: 1,
            fetched_at: "t".into(),
        };
        apply_activity_to_account(&mut acc, &empty);
        let d2 = acc.data_json();
        assert_eq!(d2["free_remaining"], 0);
        assert_eq!(d2["free_limit"], 0);
        assert!(!model_has_free_activity_path(&acc, "qd/ultimate"));
    }

    #[test]
    fn is_ultimate_free_activity_model_keys() {
        assert!(is_ultimate_free_activity_model("qd/ultimate"));
        assert!(is_ultimate_free_activity_model("ultimate"));
        assert!(is_ultimate_free_activity_model("quest-ultimate"));
        assert!(!is_ultimate_free_activity_model("qd/lite"));
        assert!(!is_ultimate_free_activity_model("qd/auto"));
    }

    #[test]
    fn optimistic_consume_and_cap_free() {
        let mut acc = Account {
            id: "t".into(),
            provider: "qoder".into(),
            email: None,
            name: None,
            is_active: 1,
            priority: 0,
            data: r#"{
              "personalToken":"pt",
              "free_limit":200,
              "free_remaining":3,
              "free_used":197,
              "free_model_keys":["ultimate","quest-ultimate","experts-ultimate"],
              "activity_free":[{
                "activityId":"ultimate_200_free_invoke",
                "modelKeys":["ultimate"],
                "limit":200,"used":197,"remaining":3,"eligible":true
              }]
            }"#
            .into(),
            cooldown_until: None,
            last_error: None,
            last_used_at: None,
            created_at: "t".into(),
            updated_at: "t".into(),
            quota_limit: 0,
            quota_remaining: 0,
        };
        assert_eq!(
            optimistic_consume_free_call(&mut acc, "qd/ultimate"),
            Some(2)
        );
        assert_eq!(free_remaining_for_account_model(&acc, "ultimate"), 2);
        assert_eq!(acc.data_json()["free_used"], 198);
        assert_eq!(acc.data_json()["activity_free"][0]["remaining"], 2);

        assert!(optimistic_consume_free_call(&mut acc, "qd/auto").is_none());
        assert!(optimistic_consume_free_call(&mut acc, "qd/lite").is_none());

        acc.data = r#"{
          "free_limit":200,"free_remaining":5,"free_used":195,
          "free_model_keys":["ultimate"]
        }"#
        .into();
        cap_free_remaining_after_sync(&mut acc, "qd/ultimate", 2);
        assert_eq!(free_remaining_for_account_model(&acc, "ultimate"), 2);
        assert_eq!(acc.data_json()["free_used"], 198);

        acc.data = r#"{
          "free_limit":200,"free_remaining":1,"free_used":199,
          "free_model_keys":["ultimate"]
        }"#
        .into();
        cap_free_remaining_after_sync(&mut acc, "qd/ultimate", 5);
        assert_eq!(
            free_remaining_for_account_model(&acc, "ultimate"),
            1,
            "cap must not raise remaining"
        );
    }

    #[tokio::test]
    #[ignore = "live: needs QODER_PROBE_PAT; hits real Qoder /activity"]
    async fn live_probe_activity_raw() {
        let pat = std::env::var("QODER_PROBE_PAT").expect("set QODER_PROBE_PAT");
        let provider = QoderProvider::new();
        let data = json!({ "personalToken": pat });
        let mut tokens = QoderTokens::from_data(&data).expect("tokens from pat");
        provider
            .apply_job_token(&mut tokens)
            .await
            .expect("jobToken exchange");

        async fn fetch_activity_raw(provider: &QoderProvider, tokens: &QoderTokens) -> Value {
            let session = build_cosy_session(tokens).expect("cosy session");
            let payload_b64 = build_payload_b64(&session.info);
            let cosy_date = format!("{}", chrono::Utc::now().timestamp());
            let path_sig = path_sig_from_url(ACTIVITY_URL);
            let bearer_sig =
                sign_bearer_request(&payload_b64, &session.cosy_key, &cosy_date, "", &path_sig);
            let bearer = format!("COSY.{payload_b64}.{bearer_sig}");
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert("cosy-data-policy", "agree".parse().unwrap());
            headers.insert("cosy-machinetype", "5".parse().unwrap());
            headers.insert("cosy-clienttype", "5".parse().unwrap());
            headers.insert("cosy-date", cosy_date.parse().unwrap());
            headers.insert(
                "cosy-user",
                tokens.user_id.as_deref().unwrap_or("").parse().unwrap(),
            );
            headers.insert("cosy-key", session.cosy_key.parse().unwrap());
            headers.insert("cache-control", "no-cache".parse().unwrap());
            headers.insert("cosy-business-product", "cli".parse().unwrap());
            headers.insert("cosy-business-type", "agent".parse().unwrap());
            headers.insert("cosy-scene", "assistant".parse().unwrap());
            headers.insert("accept", "application/json".parse().unwrap());
            headers.insert("authorization", format!("Bearer {bearer}").parse().unwrap());
            headers.insert("accept-encoding", "identity".parse().unwrap());
            headers.insert("cosy-version", COSY_VERSION.parse().unwrap());
            headers.insert("cosy-machineid", tokens.machine_id.parse().unwrap());
            headers.insert("cosy-machineos", COSY_MACHINE_OS.parse().unwrap());
            headers.insert("cosy-machinetoken", tokens.machine_token.parse().unwrap());
            headers.insert("login-version", "v2".parse().unwrap());
            headers.insert("user-agent", "Bun/1.3.14".parse().unwrap());
            let resp = provider
                .client
                .get(ACTIVITY_URL)
                .headers(headers)
                .send()
                .await
                .expect("activity request");
            let body = resp.text().await.unwrap_or_default();
            serde_json::from_str::<Value>(&body).unwrap_or(Value::Null)
        }

        fn bucket_38max(v: &Value) -> Option<(i64, i64)> {
            v.get("data")?
                .get("activities")?
                .as_array()?
                .iter()
                .find(|r| {
                    r.get("activityId").and_then(|x| x.as_str()) == Some("qwen38_800_invoke")
                })
                .map(|r| {
                    (
                        r.get("used").and_then(|x| x.as_i64()).unwrap_or(-1),
                        r.get("remaining").and_then(|x| x.as_i64()).unwrap_or(-1),
                    )
                })
        }

        let pre = fetch_activity_raw(&provider, &tokens).await;
        let pre_uw = bucket_38max(&pre);
        println!("=== PRE qwen38_800_invoke (used, remaining) = {pre_uw:?} ===");

        let account: Account = serde_json::from_value(json!({
            "id": "probe",
            "provider": "qoder",
            "email": null,
            "name": null,
            "is_active": 1,
            "priority": 0,
            "data": tokens.to_data().to_string(),
            "cooldown_until": null,
            "last_error": null,
            "last_used_at": null,
            "created_at": "",
            "updated_at": "",
            "quota_limit": 0,
            "quota_remaining": 0
        }))
        .expect("account");
        let req: ChatCompletionRequest = serde_json::from_value(json!({
            "model": "qd/qmodel_38max",
            "messages": [{ "role": "user", "content": "Reply with exactly: OK" }],
            "stream": false
        }))
        .expect("req");
        match provider.chat(&provider.client, &account, &req).await {
            Ok(ChatOutcome::Json(v)) => {
                let reply = v
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("message"))
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("<no content>");
                println!("=== CHAT ok, reply: {reply} ===");
            }
            Ok(_) => println!("=== CHAT ok (stream) ==="),
            Err(e) => println!("=== CHAT err: {e} ==="),
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let post = fetch_activity_raw(&provider, &tokens).await;
        let post_uw = bucket_38max(&post);
        println!("=== POST qwen38_800_invoke (used, remaining) = {post_uw:?} ===");
        println!(
            "{}",
            serde_json::to_string_pretty(&post).unwrap_or_default()
        );
    }
}
