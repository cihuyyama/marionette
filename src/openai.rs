use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<Value>,
    #[serde(flatten)]
    pub extra: Value,
}

impl ChatCompletionRequest {
    pub fn stream_enabled(&self) -> bool {
        self.stream.unwrap_or(false)
    }

    pub fn upstream_model(&self) -> &str {
        strip_first_prefix_segment(&self.model)
    }

    pub fn provider_id(&self) -> Option<&'static str> {
        provider_id_for_model(&self.model)
    }

    pub fn has_tools(&self) -> bool {
        self.tools
            .as_ref()
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    }
}

pub const COMBO_PREFIX: &str = "combo/";

/// Strip the first `prefix/` segment; models without a segment are unchanged.
pub fn strip_first_prefix_segment(model: &str) -> &str {
    if let Some((_, rest)) = model.split_once('/') {
        rest
    } else {
        model
    }
}

/// Returns `None` for combo ids on purpose: combos are expanded in the pool
/// before concrete routing, so a combo must never resolve to a single provider.
pub fn provider_id_for_model(model: &str) -> Option<&'static str> {
    if model.starts_with(COMBO_PREFIX) {
        None
    } else if model.starts_with("cmc/") || model.starts_with("commandcode") {
        Some("commandcode")
    } else if model.starts_with("cln/") || model.starts_with("cline") {
        Some("cline")
    } else if model.starts_with("cb/") {
        Some("cb")
    } else if model.starts_with("cbcn/") {
        Some("cbcn")
    } else if model.starts_with("wb/") || model.starts_with("workbuddy") {
        Some("workbuddy")
    } else if model.starts_with("ag/") || model.starts_with("antigravity") {
        Some("antigravity")
    } else if model.starts_with("kr/") || model.starts_with("kiro") {
        Some("kiro")
    } else if model.starts_with("gcli/") || model.starts_with("grok") {
        Some("grok-cli")
    } else if model.starts_with("qd/") || model.starts_with("qoder") {
        Some("qoder")
    } else if model.contains("grok") {
        Some("grok-cli")
    } else {
        None
    }
}

pub fn is_combo_model(model: &str) -> bool {
    model.starts_with(COMBO_PREFIX)
}

pub fn combo_slug(model: &str) -> Option<&str> {
    model.strip_prefix(COMBO_PREFIX).filter(|s| !s.is_empty())
}

/// A combo target must route chat completions only: no nested combos, no
/// image-only models, and it must be a canonical catalog id.
pub fn is_valid_combo_target(model: &str) -> bool {
    if is_combo_model(model) {
        return false;
    }
    if is_image_model(model) {
        return false;
    }
    provider_id_for_model(model).is_some() && is_known_chat_model(model)
}

pub fn is_image_model(model: &str) -> bool {
    model.contains("imagine-image")
}

pub fn is_known_chat_model(model: &str) -> bool {
    default_models()
        .data
        .iter()
        .any(|m| m.id == model && !is_image_model(&m.id))
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelObject {
    pub id: String,
    pub object: &'static str,
    pub owned_by: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_key: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_usage_rate: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input: Option<&'static str>,
    pub reasoning: bool,
    pub vision: bool,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelsResponse {
    pub object: &'static str,
    pub data: Vec<ModelObject>,
}

fn model(
    id: &'static str,
    owned_by: &'static str,
    model_key: Option<&'static str>,
    display_name: Option<&'static str>,
    credit_usage_rate: Option<&'static str>,
    max_input: Option<&'static str>,
    reasoning: bool,
    vision: bool,
    is_default: bool,
) -> ModelObject {
    ModelObject {
        id: id.into(),
        object: "model",
        owned_by,
        model_key,
        display_name,
        credit_usage_rate,
        max_input,
        reasoning,
        vision,
        is_default,
    }
}

fn gcli(id: &'static str, key: &'static str, display: &'static str) -> ModelObject {
    model(
        id,
        "grok-cli",
        Some(key),
        Some(display),
        None,
        Some("256K"),
        false,
        true,
        false,
    )
}

/// Command Code public ids are `cmc/<upstream-id>`; upstream ids keep their
/// own slashes (upstream_model strips only the first `cmc/` segment).
fn cmc(id: &'static str, display: &'static str) -> ModelObject {
    model(
        id,
        "commandcode",
        Some(id),
        Some(display),
        None,
        None,
        true,
        true,
        false,
    )
}

/// Buddy-family public ids are `<prefix>/<upstream-id>`; the upstream id keeps
/// its own slashes (upstream_model strips only the first segment).
fn buddy(id: &'static str, provider: &'static str, display: &'static str) -> ModelObject {
    model(id, provider, Some(id), Some(display), None, None, true, true, false)
}

/// Cline public ids are `cln/<upstream-id>`; upstream ids keep their own
/// slashes (upstream_model strips only the first `cln/` segment).
fn cln(id: &'static str, display: &'static str) -> ModelObject {
    model(
        id,
        "cline",
        Some(id),
        Some(display),
        None,
        None,
        true,
        true,
        false,
    )
}

/// Antigravity public ids are `ag/<logical-id>`; the wire id is derived from
/// the logical id (effort suffix lives on the wire, not the URL).
fn ag(id: &'static str, display: &'static str) -> ModelObject {
    model(
        id,
        "antigravity",
        Some(id),
        Some(display),
        None,
        None,
        true,
        true,
        false,
    )
}

/// Kiro public ids are `kr/<model-id>`.
fn kr(id: &'static str, display: &'static str) -> ModelObject {
    model(
        id,
        "kiro",
        Some(id),
        Some(display),
        None,
        None,
        true,
        false,
        false,
    )
}

pub fn default_models() -> ModelsResponse {
    ModelsResponse {
        object: "list",
        data: vec![
            gcli("gcli/grok-build", "grok-build", "Grok Build"),
            gcli("gcli/grok-4.6", "grok-4.6", "Grok 4.6"),
            gcli("gcli/grok-4.6-xhigh", "grok-4.6-xhigh", "Grok 4.6 xHigh"),
            gcli("gcli/grok-4.6-high", "grok-4.6-high", "Grok 4.6 High"),
            gcli("gcli/grok-4.6-medium", "grok-4.6-medium", "Grok 4.6 Medium"),
            gcli("gcli/grok-4.6-low", "grok-4.6-low", "Grok 4.6 Low"),
            gcli("gcli/grok-4.5", "grok-4.5", "Grok 4.5"),
            gcli("gcli/grok-4.5-xhigh", "grok-4.5-xhigh", "Grok 4.5 xHigh"),
            gcli("gcli/grok-4.5-high", "grok-4.5-high", "Grok 4.5 High"),
            gcli("gcli/grok-4.5-medium", "grok-4.5-medium", "Grok 4.5 Medium"),
            gcli("gcli/grok-4.5-low", "grok-4.5-low", "Grok 4.5 Low"),
            gcli("gcli/grok-4", "grok-4", "Grok 4"),
            gcli(
                "gcli/grok-4-fast-reasoning",
                "grok-4-fast-reasoning",
                "Grok 4 Fast Reasoning",
            ),
            gcli("gcli/grok-code-fast-1", "grok-code-fast-1", "Grok Code Fast 1"),
            gcli("gcli/grok-3", "grok-3", "Grok 3"),
            // Imagine (images API) — Responses path uses grok-4.5 + image_generation tool
            gcli(
                "gcli/grok-imagine-image",
                "grok-imagine-image",
                "Grok Imagine Image",
            ),
            gcli(
                "gcli/grok-imagine-image-quality",
                "grok-imagine-image-quality",
                "Grok Imagine Image Quality",
            ),
            gcli(
                "gcli/grok-imagine-image-edit",
                "grok-imagine-image-edit",
                "Grok Imagine Image Edit",
            ),
            gcli(
                "grok-imagine-image",
                "grok-imagine-image",
                "Grok Imagine Image",
            ),
            model(
                "qd/auto",
                "qoder",
                Some("auto"),
                Some("Auto"),
                Some("1.0x"),
                Some("180K"),
                false,
                true,
                true,
            ),
            model(
                "qd/ultimate",
                "qoder",
                Some("ultimate"),
                Some("Ultimate"),
                Some("0.8x"),
                Some("1M"),
                true,
                true,
                false,
            ),
            model(
                "qd/performance",
                "qoder",
                Some("performance"),
                Some("Performance"),
                Some("1.1x"),
                Some("1M"),
                false,
                true,
                false,
            ),
            model(
                "qd/efficient",
                "qoder",
                Some("efficient"),
                Some("Efficient"),
                Some("0.3x"),
                Some("180K"),
                false,
                true,
                false,
            ),
            model(
                "qd/lite",
                "qoder",
                Some("lite"),
                Some("Lite"),
                Some("0.0x"),
                Some("180K"),
                false,
                false,
                false,
            ),
            model(
                "qd/qmodel_preview",
                "qoder",
                Some("qmodel_preview"),
                Some("Qwen3.8-Max-Preview"),
                Some("0.05x"),
                Some("180K"),
                true,
                true,
                false,
            ),
            model(
                "qd/qmodel_38max",
                "qoder",
                Some("qmodel_38max"),
                Some("Qwen3.8-Max"),
                Some("0.8x"),
                Some("1M"),
                true,
                true,
                false,
            ),
            model(
                "qd/qmodel_latest",
                "qoder",
                Some("qmodel_latest"),
                Some("Qwen3.7-Max"),
                Some("0.25x"),
                Some("1M"),
                false,
                true,
                false,
            ),
            model(
                "qd/qmodel1",
                "qoder",
                Some("qmodel"),
                Some("Qwen3.7-Plus"),
                Some("0.1x"),
                Some("1M"),
                false,
                true,
                false,
            ),
            model(
                "qd/kmodel_latest",
                "qoder",
                Some("kmodel_latest"),
                Some("Kimi-K3"),
                Some("0.8x"),
                Some("180K"),
                false,
                true,
                false,
            ),
            model(
                "qd/kmodel1",
                "qoder",
                Some("kmodel"),
                Some("Kimi-K2.7-Code"),
                Some("0.3x"),
                Some("256K"),
                false,
                true,
                false,
            ),
            model(
                "qd/gm51model1",
                "qoder",
                Some("gm51model"),
                Some("GLM-5.2"),
                Some("0.6x"),
                Some("1M"),
                true,
                true,
                false,
            ),
            model(
                "qd/dmodel1",
                "qoder",
                Some("dmodel"),
                Some("DeepSeek-V4-Pro"),
                Some("0.5x"),
                Some("1M"),
                true,
                true,
                false,
            ),
            model(
                "qd/dfmodel1",
                "qoder",
                Some("dfmodel"),
                Some("DeepSeek-V4-Flash"),
                Some("0.1x"),
                Some("1M"),
                true,
                true,
                false,
            ),
            model(
                "qd/mmodel",
                "qoder",
                Some("mmodel"),
                Some("MiniMax-M3"),
                Some("0.2x"),
                Some("1M"),
                false,
                true,
                false,
            ),
            cmc("cmc/moonshotai/Kimi-K2.6", "Kimi K2.6"),
            cmc("cmc/moonshotai/Kimi-K3", "Kimi K3"),
            cmc("cmc/moonshotai/Kimi-K2.7-Code", "Kimi K2.7 Code"),
            cmc("cmc/qwen/qwen3.5-plus", "Qwen 3.5 Plus"),
            cmc("cmc/Qwen/Qwen3.6-Plus", "Qwen 3.6 Plus"),
            cmc("cmc/Qwen/Qwen3.7-Max", "Qwen 3.7 Max"),
            cmc("cmc/minimax/minimax-m2.7-highspeed", "MiniMax M2.7"),
            cmc("cmc/MiniMaxAI/MiniMax-M3", "MiniMax M3"),
            cmc("cmc/z-ai/glm-5.1", "GLM 5.1"),
            cmc("cmc/zai-org/GLM-5.2", "GLM 5.2"),
            cmc("cmc/zai-org/GLM-5.2-Fast", "GLM 5.2 Fast"),
            cmc("cmc/deepseek/deepseek-v4-pro", "DeepSeek V4 Pro"),
            cmc("cmc/deepseek/deepseek-v4-flash", "DeepSeek V4 Flash"),
            cmc("cmc/xiaomi/mimo-v2.5-pro", "Xiaomi MiMo v2.5 Pro"),
            cmc("cmc/xiaomi/mimo-v2.5", "Xiaomi MiMo v2.5"),
            cmc("cmc/poolside/laguna-s-2.1-free", "Poolside Laguna S 2.1 Free"),
            cmc("cmc/nvidia/nemotron-3-ultra-550b-a55b", "Nemotron 3 Ultra"),
            // cline's live roster (GET /ai/cline/recommended-models) as of
            // 2026-10-06. The free bucket is daily-reset and costs nothing;
            // the recommended bucket bills the one-off $0.5 signup credit,
            // which never refills. Kept separate for that reason.
            cln("cln/cline-free/solar-mini4", "Solar Mini 4 (free)"),
            cln("cln/cline-free/mimo-v2.6-flash", "MiMo v2.6 Flash (free)"),
            cln("cln/cline-free/muse-spark-1.3-contributor", "Muse Spark 1.3 (free)"),
            cln("cln/anthropic/claude-sonnet-5.5", "Claude Sonnet 5.5 (credit)"),
            cln("cln/anthropic/claude-opus-5.5", "Claude Opus 5.5 (credit)"),
            cln("cln/openai/gpt-6-astra", "GPT-6 Astra (credit)"),
            cln("cln/openai/gpt-6.1-sol", "GPT-6.1 Sol (credit)"),
            cln("cln/spacexai/grok-4.7", "Grok 4.7 (credit)"),
            cln("cln/moonshotai/kimi-k3", "Kimi K3 (credit)"),
            // CodeBuddy International and WorkBuddy share one roster; cbcn
            // ships a smaller, partly different one (see below).
            buddy("cb/auto", "cb", "auto"),
            buddy("wb/auto", "workbuddy", "auto"),
            buddy("cb/primary-model", "cb", "primary model"),
            buddy("wb/primary-model", "workbuddy", "primary model"),
            buddy("cb/claude-opus-4.6", "cb", "Claude OPUS 4.6"),
            buddy("wb/claude-opus-4.6", "workbuddy", "Claude OPUS 4.6"),
            buddy("cb/claude-opus-4.7-1m", "cb", "Claude OPUS 4.7 1M"),
            buddy("wb/claude-opus-4.7-1m", "workbuddy", "Claude OPUS 4.7 1M"),
            buddy("cb/claude-sonnet-4.6", "cb", "Claude SONNET 4.6"),
            buddy("wb/claude-sonnet-4.6", "workbuddy", "Claude SONNET 4.6"),
            buddy("cb/deepseek-v4.1-flash", "cb", "Deepseek v4.1 FLASH"),
            buddy("wb/deepseek-v4.1-flash", "workbuddy", "Deepseek v4.1 FLASH"),
            buddy("cb/deepseek-v4.1-flash-sg", "cb", "Deepseek v4.1 FLASH SG"),
            buddy("wb/deepseek-v4.1-flash-sg", "workbuddy", "Deepseek v4.1 FLASH SG"),
            buddy("cb/deepseek-v4.1-pro", "cb", "Deepseek v4.1 PRO"),
            buddy("wb/deepseek-v4.1-pro", "workbuddy", "Deepseek v4.1 PRO"),
            buddy("cb/gemini-2.5-flash-image", "cb", "Gemini 2.5 FLASH IMAGE"),
            buddy("wb/gemini-2.5-flash-image", "workbuddy", "Gemini 2.5 FLASH IMAGE"),
            buddy("cb/gemini-3.0-pro-image", "cb", "Gemini 3.0 PRO IMAGE"),
            buddy("wb/gemini-3.0-pro-image", "workbuddy", "Gemini 3.0 PRO IMAGE"),
            buddy("cb/gemini-3.1-flash-image", "cb", "Gemini 3.1 FLASH IMAGE"),
            buddy("wb/gemini-3.1-flash-image", "workbuddy", "Gemini 3.1 FLASH IMAGE"),
            buddy("cb/gemini-3.1-pro", "cb", "Gemini 3.1 PRO"),
            buddy("wb/gemini-3.1-pro", "workbuddy", "Gemini 3.1 PRO"),
            buddy("cb/gemini-3.5-flash", "cb", "Gemini 3.5 FLASH"),
            buddy("wb/gemini-3.5-flash", "workbuddy", "Gemini 3.5 FLASH"),
            buddy("cb/glm-5.3", "cb", "Glm 5.3"),
            buddy("wb/glm-5.3", "workbuddy", "Glm 5.3"),
            buddy("cb/glm-5.3-flash", "cb", "Glm 5.3 FLASH"),
            buddy("wb/glm-5.3-flash", "workbuddy", "Glm 5.3 FLASH"),
            buddy("cb/glm-5v-turbo", "cb", "Glm 5v TURBO"),
            buddy("wb/glm-5v-turbo", "workbuddy", "Glm 5v TURBO"),
            buddy("cb/gpt-5.3-codex", "cb", "Gpt 5.3 CODEX"),
            buddy("wb/gpt-5.3-codex", "workbuddy", "Gpt 5.3 CODEX"),
            buddy("cb/gpt-5.4", "cb", "Gpt 5.4"),
            buddy("wb/gpt-5.4", "workbuddy", "Gpt 5.4"),
            buddy("cb/gpt-5.5", "cb", "Gpt 5.5"),
            buddy("wb/gpt-5.5", "workbuddy", "Gpt 5.5"),
            buddy("cb/gpt-5.6-luna", "cb", "Gpt 5.6 LUNA"),
            buddy("wb/gpt-5.6-luna", "workbuddy", "Gpt 5.6 LUNA"),
            buddy("cb/gpt-5.6-sol", "cb", "Gpt 5.6 SOL"),
            buddy("wb/gpt-5.6-sol", "workbuddy", "Gpt 5.6 SOL"),
            buddy("cb/gpt-5.6-terra", "cb", "Gpt 5.6 TERRA"),
            buddy("wb/gpt-5.6-terra", "workbuddy", "Gpt 5.6 TERRA"),
            buddy("cb/gpt-6-astra", "cb", "Gpt 6 ASTRA"),
            buddy("wb/gpt-6-astra", "workbuddy", "Gpt 6 ASTRA"),
            buddy("cb/gpt-6.1-sol", "cb", "Gpt 6.1 SOL"),
            buddy("wb/gpt-6.1-sol", "workbuddy", "Gpt 6.1 SOL"),
            buddy("cb/gpt-image-2", "cb", "Gpt IMAGE 2"),
            buddy("wb/gpt-image-2", "workbuddy", "Gpt IMAGE 2"),
            buddy("cb/grok-4.6", "cb", "Grok 4.6"),
            buddy("wb/grok-4.6", "workbuddy", "Grok 4.6"),
            buddy("cb/grok-4.7", "cb", "Grok 4.7"),
            buddy("wb/grok-4.7", "workbuddy", "Grok 4.7"),
            buddy("cb/hy3", "cb", "hy3"),
            buddy("wb/hy3", "workbuddy", "hy3"),
            buddy("cb/hy4-preview", "cb", "hy4 PREVIEW"),
            buddy("wb/hy4-preview", "workbuddy", "hy4 PREVIEW"),
            buddy("cb/hy4-preview-f", "cb", "hy4 PREVIEW f"),
            buddy("wb/hy4-preview-f", "workbuddy", "hy4 PREVIEW f"),
            buddy("cb/kimi-k2.5", "cb", "Kimi k2.5"),
            buddy("wb/kimi-k2.5", "workbuddy", "Kimi k2.5"),
            buddy("cb/kimi-k2.6", "cb", "Kimi k2.6"),
            buddy("wb/kimi-k2.6", "workbuddy", "Kimi k2.6"),
            buddy("cb/kimi-k2.7", "cb", "Kimi k2.7"),
            buddy("wb/kimi-k2.7", "workbuddy", "Kimi k2.7"),
            buddy("cb/kimi-k2.8-preview", "cb", "Kimi k2.8 PREVIEW"),
            buddy("wb/kimi-k2.8-preview", "workbuddy", "Kimi k2.8 PREVIEW"),
            buddy("cb/kimi-k3", "cb", "Kimi k3"),
            buddy("wb/kimi-k3", "workbuddy", "Kimi k3"),
            buddy("cb/kimi-k3.1", "cb", "Kimi k3.1"),
            buddy("wb/kimi-k3.1", "workbuddy", "Kimi k3.1"),
            buddy("cb/minimax-m3", "cb", "Minimax m3"),
            buddy("wb/minimax-m3", "workbuddy", "Minimax m3"),
            // CodeBuddy CN roster.
            buddy("cbcn/glm-5.3", "cbcn", "Glm 5.3"),
            buddy("cbcn/glm-5.3-flash", "cbcn", "Glm 5.3 FLASH"),
            buddy("cbcn/glm-5v-turbo", "cbcn", "Glm 5v TURBO"),
            buddy("cbcn/minimax-m3", "cbcn", "Minimax m3"),
            buddy("cbcn/kimi-k2.8-preview", "cbcn", "Kimi k2.8 PREVIEW"),
            buddy("cbcn/kimi-k2.7", "cbcn", "Kimi k2.7"),
            buddy("cbcn/kimi-k2.6", "cbcn", "Kimi k2.6"),
            buddy("cbcn/kimi-k2.5", "cbcn", "Kimi k2.5"),
            buddy("cbcn/kimi-k3-1", "cbcn", "Kimi k3 1"),
            buddy("cbcn/hy3", "cbcn", "hy3"),
            buddy("cbcn/hy4-preview", "cbcn", "hy4 PREVIEW"),
            buddy("cbcn/deepseek-v4.1-flash", "cbcn", "Deepseek v4.1 FLASH"),
            ag("ag/claude-sonnet-4-6", "Claude Sonnet 4.6"),
            ag("ag/claude-opus-4-6", "Claude Opus 4.6"),
            ag("ag/gemini-3-flash", "Gemini 3 Flash"),
            ag("ag/gemini-3.1-flash-image", "Gemini 3.1 Flash Image"),
            ag("ag/gemini-3.1-pro", "Gemini 3.1 Pro"),
            ag("ag/gemini-3.6-flash", "Gemini 3.6 Flash"),
            ag("ag/gemini-3.7-flash", "Gemini 3.7 Flash"),
            ag("ag/gemini-3.8-flash", "Gemini 3.8 Flash"),
            ag("ag/gpt-oss-120b", "GPT-OSS 120B"),
            kr("kr/claude-opus-5", "Claude Opus 5"),
            kr("kr/claude-opus-4.8", "Claude Opus 4.8"),
            kr("kr/claude-opus-4.7", "Claude Opus 4.7"),
            kr("kr/claude-opus-4.5", "Claude Opus 4.5"),
            kr("kr/claude-haiku-4.5", "Claude Haiku 4.5"),
            kr("kr/claude-sonnet-5", "Claude Sonnet 5"),
            kr("kr/claude-sonnet-4.5", "Claude Sonnet 4.5"),
            kr("kr/gpt-5.6-sol", "GPT-5.6 Sol"),
            kr("kr/gpt-5.6-terra", "GPT-5.6 Terra"),
            kr("kr/gpt-5.6-luna", "GPT-5.6 Luna"),
            kr("kr/deepseek-3.2", "DeepSeek 3.2"),
            kr("kr/qwen3-coder-next", "Qwen3 Coder Next"),
            kr("kr/glm-5", "GLM-5"),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qoder_catalog_matches_reference_table() {
        let models = default_models();
        let qoder: Vec<_> = models
            .data
            .iter()
            .filter(|m| m.owned_by == "qoder")
            .collect();
        assert_eq!(qoder.len(), 15);

        let auto = qoder.iter().find(|m| m.id == "qd/auto").unwrap();
        assert_eq!(auto.model_key, Some("auto"));
        assert_eq!(auto.display_name, Some("Auto"));
        assert_eq!(auto.credit_usage_rate, Some("1.0x"));
        assert_eq!(auto.max_input, Some("180K"));
        assert!(!auto.reasoning);
        assert!(auto.vision);
        assert!(auto.is_default);

        let ultimate = qoder.iter().find(|m| m.id == "qd/ultimate").unwrap();
        assert_eq!(ultimate.credit_usage_rate, Some("0.8x"));
        assert_eq!(ultimate.max_input, Some("1M"));
        assert!(ultimate.reasoning);
        assert!(ultimate.vision);

        let lite = qoder.iter().find(|m| m.id == "qd/lite").unwrap();
        assert_eq!(lite.credit_usage_rate, Some("0.0x"));
        assert!(!lite.vision);
        assert!(!lite.reasoning);

        let preview = qoder
            .iter()
            .find(|m| m.id == "qd/qmodel_preview")
            .unwrap();
        assert_eq!(preview.credit_usage_rate, Some("0.05x"));
        assert_eq!(preview.max_input, Some("180K"));
        assert!(preview.reasoning);

        let max38 = qoder
            .iter()
            .find(|m| m.id == "qd/qmodel_38max")
            .unwrap();
        assert_eq!(max38.model_key, Some("qmodel_38max"));
        assert_eq!(max38.display_name, Some("Qwen3.8-Max"));
        assert_eq!(max38.max_input, Some("1M"));
        assert!(max38.reasoning);
        assert!(max38.vision);

        let plus = qoder.iter().find(|m| m.id == "qd/qmodel1").unwrap();
        assert_eq!(plus.model_key, Some("qmodel"));
        assert_eq!(plus.max_input, Some("1M"));

        let kimi = qoder.iter().find(|m| m.id == "qd/kmodel_latest").unwrap();
        assert_eq!(kimi.max_input, Some("180K"));
        assert_eq!(kimi.credit_usage_rate, Some("0.8x"));
    }

    #[test]
    fn grok_catalog_max_input_is_256k() {
        for m in default_models().data.iter().filter(|m| m.owned_by == "grok-cli") {
            assert_eq!(m.max_input, Some("256K"), "id={}", m.id);
            assert!(m.vision, "id={} should advertise vision", m.id);
        }
    }

    #[test]
    fn combo_ids_do_not_route_to_a_provider() {
        assert_eq!(provider_id_for_model("combo/coding"), None);
        assert!(is_combo_model("combo/coding"));
        assert_eq!(combo_slug("combo/coding"), Some("coding"));
        assert_eq!(combo_slug("combo/"), None);
        assert!(!is_combo_model("qd/auto"));
    }

    #[test]
    fn concrete_models_still_route_after_combo_change() {
        assert_eq!(provider_id_for_model("gcli/grok-4.5"), Some("grok-cli"));
        assert_eq!(provider_id_for_model("qd/ultimate"), Some("qoder"));
        assert_eq!(provider_id_for_model("grok-3"), Some("grok-cli"));
        assert_eq!(provider_id_for_model("unknown-model"), None);
    }

    #[test]
    fn commandcode_models_route_to_commandcode() {
        assert_eq!(
            provider_id_for_model("cmc/xiaomi/mimo-v2.5"),
            Some("commandcode")
        );
        assert_eq!(
            provider_id_for_model("cmc/stealth/ox-alpha"),
            Some("commandcode")
        );
        assert_eq!(provider_id_for_model("commandcode/x"), Some("commandcode"));
        assert_eq!(provider_id_for_model("cc/x"), None);
    }

    #[test]
    fn combo_target_validation_rejects_bad_targets() {
        assert!(is_valid_combo_target("gcli/grok-4.5"));
        assert!(is_valid_combo_target("qd/ultimate"));
        assert!(!is_valid_combo_target("combo/other"));
        assert!(!is_valid_combo_target("gcli/grok-imagine-image"));
        assert!(!is_valid_combo_target("grok-imagine-image"));
        assert!(!is_valid_combo_target("qd/not-a-real-model"));
        assert!(!is_valid_combo_target("totally-unknown"));
    }
}
