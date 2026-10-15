//! 旧请求快照与 format-1 插件的私有 wire DTO。
//!
//! 这些字段不属于当前运行时请求模型。入口读取旧快照时将其消费为唯一
//! canonical 参数；SDK 出站时只向匹配协议的旧 wire 形状投影已支持的参数。
//! 新参数必须添加到 `RequestParameters`，不能在此建立第二份运行时语义。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// 旧 wire 协议族载体；只供历史导入与 SDK 格式桥接使用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProtocolExt {
    OpenAiChat(OpenAIChatExt),
    OpenResponses(Box<OpenResponsesExt>),
    Anthropic(AnthropicExt),
    Google(GoogleExt),
}

// ── OpenAI Chat Completions ────────────────────────────────────────────────

/// Extension fields for `POST /v1/chat/completions`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenAIChatExt {
    /// Audio output configuration (`audio.voice`, `audio.format`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<Value>,
    /// Token logit biases (map from token ID to bias value −100..100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logit_bias: Option<HashMap<String, f64>>,
    /// Whether to return log-probabilities of output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<bool>,
    /// Number of top log-probabilities per token position (requires `logprobs = true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u32>,
    /// Output modalities (`["text"]`, `["text", "audio"]`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modalities: Option<Vec<String>>,
    /// Number of candidate completions to generate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    /// Speculative decoding prediction content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prediction: Option<Value>,
    /// Extended prompt-cache retention (`"in_memory"` | `"24h"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_retention: Option<String>,
    /// Streaming options (e.g. `include_usage`, `include_obfuscation`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<Value>,
    /// Response verbosity hint (`"low"` | `"medium"` | `"high"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<String>,
    /// Web search tool options.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_search_options: Option<Value>,
}

// ── Open Responses 2026-04-24 ─────────────────────────────────────────────

/// Fields owned by the dated Open Responses `2026-04-24` request contract.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenResponsesExt {
    /// Whether `instructions` was present, including an explicit `null`.
    #[serde(skip)]
    pub instructions_present: bool,
    /// Read-only old snapshot carrier. Import consumes only named official
    /// controls; unknown values never become runtime request parameters.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub passthrough_body: HashMap<String, Value>,
    /// 旧原生工具载体；导入时仅保留能够映射到强类型工具的声明。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passthrough_tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// 旧 Web Search 声明；导入时与其它原生搜索工具合并。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_web_search: Option<Value>,
    /// 旧工具选择载体；无法映射到统一选择的值在导入时丢弃。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice_ext: Option<Value>,
}

// ── Anthropic Messages ────────────────────────────────────────────────────

/// Extension fields for `POST /v1/messages`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnthropicExt {
    /// Top-k nucleus sampling (Anthropic-specific).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Code execution container configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<Value>,
    /// Inference geo-routing preference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_geo: Option<String>,
    /// Output configuration (`effort` + `format: json_schema`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<Value>,
    /// Anthropic service tier (different enum space from OAI).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// 旧服务器工具载体；不再允许将未知供应商工具原样透传到上游。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_tools: Option<Vec<Value>>,
}

// ── Google GenAI ──────────────────────────────────────────────────────────

/// Extension fields for `POST /v1beta/{model}:generateContent`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoogleExt {
    /// Top-k token sampling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Number of candidate responses to generate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_count: Option<u32>,
    /// Whether to include token log-probabilities in the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_logprobs: Option<bool>,
    /// Number of top log-probabilities per token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<u32>,
    /// Output MIME type (`"text/plain"` | `"application/json"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_mime_type: Option<String>,
    /// JSON Schema alternative to `response_format` (full JSON Schema standard).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_json_schema: Option<Value>,
    /// Function calling mode configuration (`toolConfig`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<Value>,
    /// Server-side cached content resource name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_content: Option<String>,
    /// Requested response modalities (`["TEXT"]`, `["IMAGE"]`, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_modalities: Option<Vec<String>>,
    /// Thinking / reasoning budget configuration (`thinkingBudget`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boxed_legacy_responses_preserves_snapshot_wire_shape() {
        let wire = serde_json::json!({
            "OpenResponses": {
                "previous_response_id": "response-parent",
                "top_logprobs": 2,
                "passthrough_tools": [{"type": "web_search"}],
            }
        });
        let extension: ProtocolExt = serde_json::from_value(wire.clone()).unwrap();
        let ProtocolExt::OpenResponses(responses) = &extension else {
            panic!("expected legacy Responses extension");
        };
        assert_eq!(responses.previous_response_id.as_deref(), Some("response-parent"));
        assert_eq!(serde_json::to_value(extension).unwrap(), wire);
    }
}
