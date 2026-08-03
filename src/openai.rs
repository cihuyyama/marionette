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
        if let Some((_, rest)) = self.model.split_once('/') {
            rest
        } else {
            &self.model
        }
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

/// Returns `None` for combo ids on purpose: combos are expanded in the pool
/// before concrete routing, so a combo must never resolve to a single provider.
pub fn provider_id_for_model(model: &str) -> Option<&'static str> {
    if model.starts_with(COMBO_PREFIX) {
        None
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

pub fn default_models() -> ModelsResponse {
    ModelsResponse {
        object: "list",
        data: vec![
            gcli("gcli/grok-build", "grok-build", "Grok Build"),
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
        assert_eq!(qoder.len(), 14);

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
