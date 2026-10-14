pub(crate) mod bedrock;
pub(crate) mod cohere;
pub(crate) mod gateway;
pub(crate) mod watsonx;

use stravia_protocol_codec::transform::ProtocolAdapter;

pub(crate) use bedrock::BedrockConverseV1;
pub(crate) use cohere::CohereChatV2;
pub(crate) use gateway::GatewayLanguageModelV4;
pub(crate) use watsonx::WatsonxTextChatV1;

/// Project only text carriers from a content-block array, never business JSON.
fn tool_result_block_text(block: &serde_json::Value) -> Option<&str> {
    match block.get("type").and_then(serde_json::Value::as_str) {
        Some("text" | "input_text" | "output_text") | None => {
            block.get("text").and_then(serde_json::Value::as_str)
        }
        _ => None,
    }
}

fn tool_result_text_value(
    content: &serde_json::Value,
    kind: Option<stravia_runtime_contract::protocol::ir::ToolResultContentKind>,
) -> anyhow::Result<String> {
    use stravia_runtime_contract::protocol::ir::ToolResultContentKind;
    if let Some(text) = content.as_str() {
        return Ok(text.to_owned());
    }
    if kind == Some(ToolResultContentKind::ContentBlocks) {
        return Ok(content
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(tool_result_block_text)
            .collect());
    }
    Ok(serde_json::to_string(content)?)
}

pub(crate) fn adapter(protocol: &str) -> Option<&'static dyn ProtocolAdapter> {
    match protocol.trim().to_ascii_lowercase().as_str() {
        "bedrock" | "bedrock-converse" | "bedrock-converse/converse/v1" => Some(&BedrockConverseV1),
        "cohere" | "cohere-chat" | "cohere-chat/chat/v2" => Some(&CohereChatV2),
        "gateway" | "gateway-language-model" | "gateway-language-model/language-model/v4" => {
            Some(&GatewayLanguageModelV4)
        }
        "watsonx" | "watsonx-text-chat" | "watsonx-text-chat/chat/v1" => Some(&WatsonxTextChatV1),
        _ => None,
    }
}
