//! OpenAI-compatible image generations / edits (P1 + P2).
//!
//! Upstream path (grok-cli OAuth only — **no** Web Imagine / cookie SSO):
//! `POST https://cli-chat-proxy.grok.com/v1/responses` with
//! `tools: [{"type":"image_generation"}]`, `stream: true`.
//!
//! Imagine model ids (`grok-imagine-image*`) are mapped to Responses model
//! `grok-4.5` because cli-chat-proxy does not expose a separate Imagine model
//! id; generation is the hosted `image_generation` tool on that chat model.
//!
//! ## Limitations (honest)
//! - `response_format=url` returns a `data:image/...;base64,...` URL (no object
//!   storage / CDN in Marionette v1). Prefer `b64_json` for clients that save files.
//! - Mid-stream upstream failure after headers are already OK is rare here
//!   because we buffer the full SSE before responding (unlike chat stream).
//! - Qoder is not involved; unknown / non-imagine models → 400.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Default catalog model when client omits `model`.
pub const DEFAULT_IMAGE_MODEL: &str = "grok-imagine-image";

/// Responses API model used for all imagine tools (see module docs).
pub const IMAGINE_RESPONSES_MODEL: &str = "grok-4.5";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ImageGenerationRequest {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub response_format: Option<String>,
    /// Single ref: data URL, https URL, or bare base64 (OpenAI / server.mjs).
    #[serde(default)]
    pub image: Option<Value>,
    /// Multiple refs (server.mjs bulk).
    #[serde(default)]
    pub images: Option<Value>,
    #[serde(flatten)]
    pub extra: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ImageEditRequest {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub response_format: Option<String>,
    #[serde(default)]
    pub image: Option<Value>,
    #[serde(default)]
    pub images: Option<Value>,
    #[serde(default)]
    pub mask: Option<Value>,
    #[serde(flatten)]
    pub extra: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageResponseFormat {
    B64Json,
    Url,
}

impl ImageResponseFormat {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("url") => Self::Url,
            _ => Self::B64Json,
        }
    }
}

/// Strip optional `gcli/` prefix.
pub fn image_upstream_model_id(model: &str) -> &str {
    if let Some((_, rest)) = model.split_once('/') {
        rest
    } else {
        model
    }
}

/// True for grok-cli imagine models (with or without `gcli/` prefix).
pub fn is_imagine_model(model: &str) -> bool {
    let id = image_upstream_model_id(model);
    matches!(
        id,
        "grok-imagine-image"
            | "grok-imagine-image-quality"
            | "grok-imagine-image-edit"
            | "grok-imagine"
    ) || id.starts_with("grok-imagine-image")
}

/// Provider for image routes — currently only grok-cli.
pub fn image_provider_id(model: &str) -> Option<&'static str> {
    if is_imagine_model(model) {
        Some("grok-cli")
    } else if model.starts_with("gcli/") || model.starts_with("grok") || model.contains("grok")
    {
        // Non-imagine grok ids still route to grok-cli (tool path uses grok-4.5).
        Some("grok-cli")
    } else {
        None
    }
}

/// Map client imagine model → Responses API model id.
///
/// All imagine variants use `grok-4.5` + hosted `image_generation` tool.
pub fn imagine_to_responses_model(model: &str) -> &'static str {
    let _ = model;
    IMAGINE_RESPONSES_MODEL
}

/// Normalize one image ref from string | `{url}` | `{image_url}` | `{b64_json}`.
pub fn normalize_image_ref(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        }
        Value::Object(obj) => {
            for key in ["url", "image_url", "b64_json", "image"] {
                if let Some(s) = obj.get(key).and_then(|x| x.as_str()) {
                    let s = s.trim();
                    if !s.is_empty() {
                        return Some(s.to_string());
                    }
                }
                // nested { image_url: { url: "..." } } (OpenAI chat style)
                if let Some(inner) = obj.get(key).and_then(|x| x.as_object()) {
                    if let Some(s) = inner.get("url").and_then(|x| x.as_str()) {
                        let s = s.trim();
                        if !s.is_empty() {
                            return Some(s.to_string());
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Collect refs from `image` (singular) and/or `images` (array or single).
/// Order: all `images` entries first, then singular `image` (matches server.mjs push order loosely).
pub fn collect_image_refs(image: Option<&Value>, images: Option<&Value>) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: String| {
        if !out.iter().any(|e| e == &s) {
            out.push(s);
        }
    };

    if let Some(imgs) = images {
        match imgs {
            Value::Array(arr) => {
                for item in arr {
                    if let Some(s) = normalize_image_ref(item) {
                        push(s);
                    }
                }
            }
            other => {
                if let Some(s) = normalize_image_ref(other) {
                    push(s);
                }
            }
        }
    }

    if let Some(img) = image {
        if let Some(s) = normalize_image_ref(img) {
            push(s);
        }
    }

    out
}

/// Ensure ref is a data URL or http(s) URL suitable for Responses `input_image`.
pub fn coerce_image_url(raw: &str) -> String {
    let s = raw.trim();
    if s.starts_with("data:") || s.starts_with("http://") || s.starts_with("https://") {
        return s.to_string();
    }
    // bare base64 → assume png
    format!("data:image/png;base64,{s}")
}

/// Strip `data:image/...;base64,` prefix if present; return raw b64.
pub fn strip_to_raw_b64(s: &str) -> String {
    let s = s.trim();
    if let Some(idx) = s.find("base64,") {
        s[idx + "base64,".len()..].trim().to_string()
    } else {
        s.to_string()
    }
}

/// Build OpenAI Images response: `{ created, data: [{ b64_json | url }] }`.
pub fn build_images_response(created: i64, b64_list: &[String], format: ImageResponseFormat) -> Value {
    let data: Vec<Value> = b64_list
        .iter()
        .map(|b64| {
            let raw = strip_to_raw_b64(b64);
            match format {
                ImageResponseFormat::B64Json => json!({ "b64_json": raw }),
                ImageResponseFormat::Url => {
                    let url = if b64.trim().starts_with("data:") {
                        b64.trim().to_string()
                    } else {
                        format!("data:image/png;base64,{raw}")
                    };
                    json!({ "url": url })
                }
            }
        })
        .collect();
    json!({
        "created": created,
        "data": data
    })
}

/// Extract base64 image payloads from a Responses SSE stream body (full text).
///
/// Primary: `response.output_item.done` with `item.type == "image_generation_call"`
/// and `item.result` (raw b64 or data URL).
/// Fallback: `data:image/...;base64,...` substrings in accumulated text.
pub fn parse_image_b64_from_sse(sse_body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    fn push_unique(out: &mut Vec<String>, s: String) {
        let raw = strip_to_raw_b64(&s);
        if raw.len() < 32 {
            return;
        }
        if !out.iter().any(|e| e == &raw) {
            out.push(raw);
        }
    }

    for line in sse_body.lines() {
        let trimmed = line.trim();
        let Some(data_str) = trimmed.strip_prefix("data:") else {
            continue;
        };
        let data_str = data_str.trim();
        if data_str.is_empty() || data_str == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(data_str) else {
            // bare data line might be a data-url fragment
            if data_str.contains("base64,") {
                if let Some(b64) = extract_data_url_b64(data_str) {
                    push_unique(&mut out, b64);
                }
            }
            continue;
        };

        let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

        // Primary path: completed image_generation_call
        if event_type == "response.output_item.done" || event_type == "response.output_item.added" {
            if let Some(item) = v.get("item") {
                let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if item_type == "image_generation_call" {
                    if let Some(result) = item.get("result").and_then(|r| r.as_str()) {
                        if !result.is_empty() {
                            push_unique(&mut out, result.to_string());
                        }
                    }
                }
            }
        }

        // Some streams put result on the event root
        if event_type.contains("image_generation") {
            if let Some(result) = v.get("result").and_then(|r| r.as_str()) {
                if !result.is_empty() {
                    push_unique(&mut out, result.to_string());
                }
            }
            if let Some(b64) = v.get("b64_json").and_then(|r| r.as_str()) {
                if !b64.is_empty() {
                    push_unique(&mut out, b64.to_string());
                }
            }
            if let Some(partial) = v.get("partial_image_b64").and_then(|r| r.as_str()) {
                if !partial.is_empty() {
                    // keep last partial; only commit if nothing better — still collect
                    push_unique(&mut out, partial.to_string());
                }
            }
        }

        // Text delta may embed a markdown data URL
        if let Some(delta) = v.get("delta").and_then(|d| d.as_str()) {
            if let Some(b64) = extract_data_url_b64(delta) {
                push_unique(&mut out, b64);
            }
        }
        if let Some(text) = v
            .pointer("/item/content")
            .and_then(|c| c.as_str())
            .or_else(|| v.get("text").and_then(|t| t.as_str()))
        {
            if let Some(b64) = extract_data_url_b64(text) {
                push_unique(&mut out, b64);
            }
        }
    }

    // Whole-body fallback (in case lines were concatenated oddly)
    if out.is_empty() {
        if let Some(b64) = extract_data_url_b64(sse_body) {
            push_unique(&mut out, b64);
        }
    }

    out
}

fn extract_data_url_b64(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let marker = "data:image/";
    let start = lower.find(marker)?;
    let slice = &text[start..];
    let b64_marker = "base64,";
    let b64_pos = slice.to_ascii_lowercase().find(b64_marker)?;
    let after = &slice[b64_pos + b64_marker.len()..];
    // stop at whitespace, quote, paren, or markdown close
    let end = after
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')' || c == ']')
        .unwrap_or(after.len());
    let b64 = after[..end].trim_end_matches('`').trim().to_string();
    if b64.len() >= 32 {
        Some(b64)
    } else {
        None
    }
}

/// Build Responses API `input` message content: prompt text + optional ref images.
pub fn build_imagine_input_content(prompt: &str, refs: &[String]) -> Value {
    let mut parts = vec![json!({
        "type": "input_text",
        "text": prompt
    })];
    for r in refs {
        parts.push(json!({
            "type": "input_image",
            "image_url": coerce_image_url(r),
            "detail": "auto"
        }));
    }
    Value::Array(parts)
}

/// Validate prompt non-empty after trim.
pub fn require_prompt(prompt: Option<&str>) -> Result<String, String> {
    let p = prompt.map(str::trim).unwrap_or("");
    if p.is_empty() {
        Err("Missing required field: prompt".into())
    } else {
        Ok(p.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_image_string_and_array() {
        let image = json!("data:image/png;base64,aaa");
        let images = json!(["https://ex.com/a.png", "data:image/jpeg;base64,bbb"]);
        let refs = collect_image_refs(Some(&image), Some(&images));
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0], "https://ex.com/a.png");
        assert_eq!(refs[1], "data:image/jpeg;base64,bbb");
        assert_eq!(refs[2], "data:image/png;base64,aaa");
    }

    #[test]
    fn collect_image_object_url() {
        let image = json!({ "url": "https://ex.com/x.png" });
        let refs = collect_image_refs(Some(&image), None);
        assert_eq!(refs, vec!["https://ex.com/x.png".to_string()]);
    }

    #[test]
    fn collect_images_mixed_string_and_object() {
        let images = json!([
            "data:image/png;base64,abc",
            { "url": "https://ex.com/y.png" }
        ]);
        let refs = collect_image_refs(None, Some(&images));
        assert_eq!(refs.len(), 2);
        assert!(refs[0].starts_with("data:"));
        assert_eq!(refs[1], "https://ex.com/y.png");
    }

    #[test]
    fn model_routing_imagine() {
        assert!(is_imagine_model("grok-imagine-image"));
        assert!(is_imagine_model("gcli/grok-imagine-image"));
        assert!(is_imagine_model("grok-imagine-image-quality"));
        assert!(is_imagine_model("grok-imagine-image-edit"));
        assert_eq!(image_provider_id("grok-imagine-image"), Some("grok-cli"));
        assert_eq!(
            image_provider_id("gcli/grok-imagine-image-quality"),
            Some("grok-cli")
        );
        assert_eq!(image_provider_id("qd/lite"), None);
    }

    #[test]
    fn imagine_maps_to_grok_45() {
        assert_eq!(
            imagine_to_responses_model("grok-imagine-image"),
            "grok-4.5"
        );
        assert_eq!(
            imagine_to_responses_model("gcli/grok-imagine-image-edit"),
            "grok-4.5"
        );
    }

    #[test]
    fn response_shape_b64() {
        let v = build_images_response(1_700_000_000, &["QUJD".into()], ImageResponseFormat::B64Json);
        assert_eq!(v["created"], 1_700_000_000);
        assert_eq!(v["data"][0]["b64_json"], "QUJD");
        assert!(v["data"][0].get("url").is_none());
    }

    #[test]
    fn response_shape_url() {
        let v = build_images_response(42, &["QUJD".into()], ImageResponseFormat::Url);
        assert_eq!(v["created"], 42);
        let url = v["data"][0]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
        assert!(url.ends_with("QUJD"));
    }

    #[test]
    fn parse_sse_image_generation_call() {
        let sse = r#"
data: {"type":"response.created","response":{"id":"resp_1"}}

data: {"type":"response.output_item.done","item":{"type":"image_generation_call","result":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB"}}

data: {"type":"response.completed"}

data: [DONE]
"#;
        let imgs = parse_image_b64_from_sse(sse);
        assert_eq!(imgs.len(), 1);
        assert!(imgs[0].starts_with("iVBORw0KGgo"));
    }

    #[test]
    fn parse_sse_fallback_data_url_in_text() {
        let sse = r#"
data: {"type":"response.output_text.delta","delta":"here ![img](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJ) done"}

data: [DONE]
"#;
        let imgs = parse_image_b64_from_sse(sse);
        assert_eq!(imgs.len(), 1);
        assert!(imgs[0].starts_with("iVBORw0KGgo"));
    }

    #[test]
    fn require_prompt_validation() {
        assert!(require_prompt(None).is_err());
        assert!(require_prompt(Some("  ")).is_err());
        assert_eq!(require_prompt(Some(" cat ")).unwrap(), "cat");
    }

    #[test]
    fn coerce_bare_b64() {
        let u = coerce_image_url("AAAA");
        assert_eq!(u, "data:image/png;base64,AAAA");
        assert_eq!(
            coerce_image_url("https://x.com/a.png"),
            "https://x.com/a.png"
        );
    }

    #[test]
    fn build_input_content_with_refs() {
        let c = build_imagine_input_content("draw a cat", &["https://x.com/a.png".into()]);
        let arr = c.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "input_text");
        assert_eq!(arr[1]["type"], "input_image");
        assert_eq!(arr[1]["image_url"], "https://x.com/a.png");
    }
}
