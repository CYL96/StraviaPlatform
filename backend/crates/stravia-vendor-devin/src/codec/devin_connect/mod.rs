//! Devin Connect-RPC egress codec — `ApiServerService/GetChatMessage`.
//!
//! Egress-only, stream-only: the upstream is a server-streaming Connect-RPC
//! endpoint that exchanges protobuf frames (`application/connect+proto`),
//! not JSON. Request encoding is byte-oriented and embeds the session
//! credential inside `ClientMetadata`. The vendor owns the binary encoder
//! and stream decoder; the plugin projects its private wire capabilities before I/O.

pub(crate) mod connect;
pub mod proto;
pub mod request;
pub(crate) mod sanitize;
pub(crate) mod stream;
mod tool_description;

pub use connect::wrap_request;
pub use request::ASSIGN_MODEL_PATH;
pub use request::DevinClientPlatform;
pub use request::DevinModelConfig;
pub use request::GET_CHAT_MESSAGE_PATH;
pub use request::ModelAssignment;
pub use request::decode_assign_model_response;
pub use request::decode_cli_model_configs;
pub use request::devin_upstream_provider_name;
pub use request::encode_assign_model_request_with_platform;
pub use request::encode_client_metadata_request_with_platform;
pub use request::session_shape;
pub use stream::DevinConnectStreamParser;

/// 使用现有 opaque signature carrier 保留 Devin 响应状态；
/// output_id 仅随历史保留，不回填到原生请求的 prompt。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct ThinkingReplay {
    pub signature: String,
    pub signature_type: String,
    pub output_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub redacted_text: String,
}

impl ThinkingReplay {
    const PREFIX: &str = "devin-thinking-v1:";

    fn encode(&self) -> String {
        format!(
            "{}{}",
            Self::PREFIX,
            serde_json::to_string(self).expect("string-only thinking state")
        )
    }

    fn decode(value: &str) -> anyhow::Result<Self> {
        match value.strip_prefix(Self::PREFIX) {
            Some(json) => Ok(serde_json::from_str(json)?),
            None => Ok(Self {
                signature: value.to_owned(),
                ..Self::default()
            }),
        }
    }
}

const CUSTOM_TOOL_META: &str = "__devin_custom_tool";

use stravia_runtime_contract::protocol::ir::{AiRequest, ProtocolExt};
use stravia_runtime_contract::thinking::TargetThinkingControl;

/// 私有协议能力只在插件中投影；正文编码器逐项省略不支持的内容。
pub(crate) fn prepare_request(request: &mut AiRequest) {
    request.embedding = None;
    // 私有 wire 没有这些控制；正文与受保护思考由本插件的编码器投影。
    request.tool_choice = None;
    request.parallel_tool_calls = None;
    request.disable_parallel_tool_calls = None;
    request.generation.seed = None;
    request.generation.stop = None;
    request.generation.presence_penalty = None;
    request.generation.frequency_penalty = None;
    request.response_format = None;
    // Effort is consumed by the provider's model selector, not sent as a wire control.
    if request
        .reasoning
        .target_control
        .as_ref()
        .is_some_and(|control| !matches!(control, TargetThinkingControl::Effort { .. }))
    {
        request.reasoning.target_control = None;
    }
    match request.ext.as_mut() {
        Some(ProtocolExt::OpenAiChat(extension)) => {
            extension.n = None;
            extension.logit_bias = None;
            extension.logprobs = None;
            extension.top_logprobs = None;
            extension.prediction = None;
            extension.prompt_cache_retention = None;
            extension.verbosity = None;
        }
        Some(ProtocolExt::OpenResponses(extension)) => {
            extension.background = None;
            extension.max_tool_calls = None;
            extension.top_logprobs = None;
            extension.truncation = None;
            extension.text = None;
            extension.tool_choice_ext = None;
            extension.passthrough_body.remove("context_management");
        }
        Some(ProtocolExt::Anthropic(extension)) => {
            extension.top_k = None;
            extension.output_config = None;
            extension.service_tier = None;
        }
        Some(ProtocolExt::Google(extension)) => {
            extension.top_k = None;
            extension.candidate_count = None;
            extension.response_logprobs = None;
            extension.logprobs = None;
            extension.response_mime_type = None;
            extension.response_json_schema = None;
            extension.tool_config = None;
            extension.thinking_config = None;
        }
        None => {}
    }
}
