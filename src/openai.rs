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
    pub display_name: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_usage_rate: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelsResponse {
    pub object: &'static str,
    pub data: Vec<ModelObject>,
}

fn model(
    id: &'static str,
    owned_by: &'static str,
    display_name: Option<&'static str>,
    credit_usage_rate: Option<&'static str>,
) -> ModelObject {
    ModelObject {
        id: id.into(),
        object: "model",
        owned_by,
        display_name,
        credit_usage_rate,
    }
}

pub fn default_models() -> ModelsResponse {
    ModelsResponse {
        object: "list",
        data: vec![
            model("gcli/grok-build", "grok-cli", Some("Grok Build"), None),
            model("gcli/grok-4.5", "grok-cli", Some("Grok 4.5"), None),
            model("gcli/grok-4.5-xhigh", "grok-cli", Some("Grok 4.5 xHigh"), None),
            model("gcli/grok-4.5-high", "grok-cli", Some("Grok 4.5 High"), None),
            model("gcli/grok-4.5-medium", "grok-cli", Some("Grok 4.5 Medium"), None),
            model("gcli/grok-4.5-low", "grok-cli", Some("Grok 4.5 Low"), None),
            model("gcli/grok-4", "grok-cli", Some("Grok 4"), None),
            model(
                "gcli/grok-4-fast-reasoning",
                "grok-cli",
                Some("Grok 4 Fast Reasoning"),
                None,
            ),
            model("gcli/grok-code-fast-1", "grok-cli", Some("Grok Code Fast 1"), None),
            model("gcli/grok-3", "grok-cli", Some("Grok 3"), None),
            model("qd/auto", "qoder", Some("Auto"), Some("~1.0x")),
            model("qd/ultimate", "qoder", Some("Ultimate"), Some("~1.6x")),
            model("qd/performance", "qoder", Some("Performance"), Some("~1.1x")),
            model("qd/efficient", "qoder", Some("Efficient"), Some("~0.3x")),
            model("qd/lite", "qoder", Some("Lite"), Some("Free")),
            model(
                "qd/qmodel_preview",
                "qoder",
                Some("Qwen3.8-Max-Preview"),
                Some("0.5x"),
            ),
            model("qd/qmodel_latest", "qoder", Some("Qwen3.7-Max"), Some("0.5x")),
            model("qd/qmodel1", "qoder", Some("Qwen3.7-Plus"), Some("0.1x")),
            model("qd/kmodel_latest", "qoder", Some("Kimi-K3"), Some("0.8x")),
            model("qd/kmodel1", "qoder", Some("Kimi-K2.7-Code"), Some("0.3x")),
            model("qd/gm51model1", "qoder", Some("GLM-5.2"), Some("0.6x")),
            model("qd/dmodel1", "qoder", Some("DeepSeek-V4-Pro"), Some("0.5x")),
            model("qd/dfmodel1", "qoder", Some("DeepSeek-V4-Flash"), Some("0.1x")),
            model("qd/mmodel", "qoder", Some("MiniMax-M3"), Some("0.2x")),
        ],
    }
}
