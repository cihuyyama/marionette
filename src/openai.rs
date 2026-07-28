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
        if self.model.starts_with("gcli/") || self.model.starts_with("grok") {
            Some("grok-cli")
        } else if self.model.starts_with("qd/") || self.model.starts_with("qoder") {
            Some("qoder")
        } else if self.model.contains("grok") {
            Some("grok-cli")
        } else {
            None
        }
    }

    pub fn has_tools(&self) -> bool {
        self.tools
            .as_ref()
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    }
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
        false,
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
        }
    }
}
