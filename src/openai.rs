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
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelsResponse {
    pub object: &'static str,
    pub data: Vec<ModelObject>,
}

pub fn default_models() -> ModelsResponse {
    let ids = [
        ("gcli/grok-build", "grok-cli"),
        ("gcli/grok-4.5", "grok-cli"),
        ("gcli/grok-4.5-high", "grok-cli"),
        ("gcli/grok-4.5-medium", "grok-cli"),
        ("gcli/grok-4.5-low", "grok-cli"),
        ("gcli/grok-4", "grok-cli"),
        ("gcli/grok-4-fast-reasoning", "grok-cli"),
        ("gcli/grok-code-fast-1", "grok-cli"),
        ("gcli/grok-3", "grok-cli"),
        ("qd/lite", "qoder"),
        ("qd/auto", "qoder"),
        ("qd/ultimate", "qoder"),
        ("qd/performance", "qoder"),
        ("qd/efficient", "qoder"),
        ("qd/qmodel_latest", "qoder"),
        ("qd/qwen3.7-max", "qoder"),
        ("qd/qmodel", "qoder"),
        ("qd/qwen3.6-plus", "qoder"),
        ("qd/dmodel", "qoder"),
        ("qd/deepseek-v4-pro", "qoder"),
        ("qd/dfmodel", "qoder"),
        ("qd/deepseek-v4-flash", "qoder"),
        ("qd/gm51model", "qoder"),
        ("qd/glm-5.1", "qoder"),
        ("qd/kmodel", "qoder"),
        ("qd/kimi-k2.6", "qoder"),
        ("qd/mmodel", "qoder"),
        ("qd/minimax-m2.7", "qoder"),
    ];
    ModelsResponse {
        object: "list",
        data: ids
            .into_iter()
            .map(|(id, owner)| ModelObject {
                id: id.into(),
                object: "model",
                owned_by: owner,
            })
            .collect(),
    }
}
