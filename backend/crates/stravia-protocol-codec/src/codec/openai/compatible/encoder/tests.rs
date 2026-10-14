use super::*;

#[test]
fn json_schema_description_survives_without_overriding_canonical_controls() {
    use crate::transform::ProtocolTransform;
    use stravia_runtime_contract::protocol::ids::OPENAI_COMPATIBLE_CHAT_COMPLETIONS_V1;
    use stravia_runtime_contract::protocol::ir::ResponseFormat;

    let pair = ProtocolTransform::global()
        .bind(
            OPENAI_COMPATIBLE_CHAT_COMPLETIONS_V1,
            OPENAI_COMPATIBLE_CHAT_COMPLETIONS_V1,
        )
        .expect("bind Chat schema round trip");
    let mut request = pair
        .decode_request(serde_json::json!({
            "model": "model",
            "messages": [{"role": "user", "content": "hello"}],
            "response_format": {"type": "json_schema", "json_schema": {
                "name": "original", "description": "Describe the answer",
                "schema": {"type": "object"}, "strict": false
            }}
        }))
        .expect("decode described schema");
    let schema =
        serde_json::json!({"type": "object", "properties": {"answer": {"type": "string"}}});
    request.response_format = Some(ResponseFormat::JsonSchema {
        name: "changed".into(),
        schema: schema.clone(),
        strict: Some(true),
    });
    let body = pair
        .encode_request(request)
        .expect("encode canonical schema changes")
        .body;
    assert_eq!(
        body["response_format"]["json_schema"],
        serde_json::json!({
            "name": "changed", "description": "Describe the answer", "schema": schema, "strict": true
        })
    );
}

#[test]
fn anthropic_controls_convert_to_chat_with_exact_tool_strict_values() {
    use crate::transform::ProtocolTransform;
    use stravia_runtime_contract::protocol::ids::{
        ANTHROPIC_MESSAGES_2023_06_01, OPENAI_COMPATIBLE_CHAT_COMPLETIONS_V1,
    };

    let schema = serde_json::json!({
        "type": "object", "properties": {"answer": {"type": "string"}},
        "required": ["answer"], "additionalProperties": false
    });
    let pair = ProtocolTransform::global()
        .bind(
            ANTHROPIC_MESSAGES_2023_06_01,
            OPENAI_COMPATIBLE_CHAT_COMPLETIONS_V1,
        )
        .expect("bind reverse controls");
    for disabled in [false, true] {
        let request = pair
            .decode_request(serde_json::json!({
                "model": "model", "max_tokens": 1024,
                "messages": [{"role": "user", "content": "hello"}],
                "tool_choice": {"type": "auto", "disable_parallel_tool_use": disabled},
                "stop_sequences": ["END", "DONE"],
                "output_config": {"format": {"type": "json_schema", "schema": schema}},
                "tools": [
                    {"name": "strict", "input_schema": schema, "strict": true},
                    {"name": "non_strict", "input_schema": schema, "strict": false},
                    {"name": "unspecified", "input_schema": schema}
                ]
            }))
            .expect("decode Anthropic controls");
        let body = pair
            .encode_request(request)
            .expect("encode Chat controls")
            .body;
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["parallel_tool_calls"], !disabled);
        assert_eq!(body["stop"], serde_json::json!(["END", "DONE"]));
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["schema"], schema);
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        assert_eq!(body["tools"][0]["function"]["strict"], true);
        assert_eq!(body["tools"][1]["function"]["strict"], false);
        assert!(body["tools"][2]["function"].get("strict").is_none());
    }
}

#[test]
fn tool_result_payload_kind_controls_chat_text_projection() {
    use stravia_runtime_contract::protocol::ir::ToolResultContentKind;

    let business_array = serde_json::json!([
        {"type": "text", "text": "business", "extra": [1, 2]},
        {"type": "image", "source": {"type": "url", "url": "https://example.invalid/business"}},
        {"type": "unrecognized", "payload": true}
    ]);
    for (content, kind, expected) in [
        (
            serde_json::json!([
                {"type": "text", "text": "first"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "image-data"}},
                {"type": "unknown", "raw": {"private": "discard"}},
                {"type": "unrecognized", "payload": "discard"},
                {"type": "text", "text": "second"}
            ]),
            Some(ToolResultContentKind::ContentBlocks),
            Some("firstsecond".to_owned()),
        ),
        (
            business_array.clone(),
            Some(ToolResultContentKind::Json),
            Some(business_array.to_string()),
        ),
        (
            serde_json::json!([
                {"type": "image", "source": {"type": "url", "url": "https://example.invalid/image"}},
                {"type": "unknown", "raw": {"private": "discard"}}
            ]),
            Some(ToolResultContentKind::ContentBlocks),
            None,
        ),
        (
            serde_json::json!("literal result"),
            Some(ToolResultContentKind::ContentBlocks),
            Some("literal result".into()),
        ),
        (
            serde_json::json!([]),
            Some(ToolResultContentKind::ContentBlocks),
            None,
        ),
    ] {
        let request = AiRequest::new(
            "model",
            vec![
                AiItem {
                    role: Role::Assistant,
                    content: MessageContent::Text("".to_owned().into()),
                    tool_calls: Some(vec![ToolCall {
                        id: "call_lookup".into(),
                        name: "lookup".into(),
                        arguments: "{}".into(),
                    }]),
                    tool_call_id: None,
                    meta: None,
                },
                AiItem {
                    role: Role::Tool,
                    content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: "call_lookup".into(),
                        content,
                        content_kind: kind,
                        is_error: None,
                        cache_control: None,
                    }]),
                    tool_calls: None,
                    tool_call_id: Some("call_lookup".into()),
                    meta: None,
                },
            ],
        );
        let (body, _) = OpenAIEncoder
            .encode_request(request)
            .expect("encode tool result");
        let results: Vec<_> = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "tool")
            .collect();
        if let Some(expected) = expected {
            assert_eq!(results.len(), 1);
            assert_eq!(results[0]["tool_call_id"], "call_lookup");
            assert_eq!(results[0]["content"], expected);
        } else {
            assert!(
                results.is_empty(),
                "unsupported tool results must not become JSON text"
            );
        }
    }
}

#[test]
fn request_roundtrip_preserves_escaped_text_and_multimodal_parts() {
    use crate::codec::openai::compatible::chat_completions::OpenAIChatCompletionsV1;
    use crate::transform::{ProtocolAdapter, ProtocolTransform};

    for content in [
        serde_json::json!(""),
        serde_json::json!("原文 🐟\n\"quote\"\\path\tend"),
        serde_json::json!([
            {"type": "text", "text": "原文 🐟\n"},
            {"type": "image_url", "image_url": {"url": "https://example.invalid/image", "detail": "auto"}},
            {"type": "input_audio", "input_audio": {"data": "YWJj", "format": "wav"}}
        ]),
    ] {
        let request = OpenAIChatCompletionsV1
            .decode_request(serde_json::json!({
                "model": "model",
                "messages": [{"role": "user", "content": content}]
            }))
            .expect("decode content");
        let encoded = ProtocolTransform::encode_request_with(&OpenAIChatCompletionsV1, request)
            .expect("encode content");
        assert_eq!(encoded.body["messages"][0]["role"], "user");
        assert_eq!(encoded.body["messages"][0]["content"], content);
    }
}

#[test]
fn request_decode_rejects_invalid_text_or_parts() {
    use crate::codec::openai::compatible::chat_completions::OpenAIChatCompletionsV1;
    use crate::transform::ProtocolAdapter;

    for content in [
        serde_json::json!(false),
        serde_json::json!(42),
        serde_json::json!({"type": "text", "text": "not an array"}),
        serde_json::json!(["not a part"]),
        serde_json::json!([{"type": "text", "text": 42}]),
        serde_json::json!([{"type": "unknown_part"}]),
    ] {
        assert!(
            OpenAIChatCompletionsV1
                .decode_request(serde_json::json!({
                    "model": "model",
                    "messages": [{"role": "user", "content": content}]
                }))
                .is_err()
        );
    }
}

#[test]
fn owned_encoding_preserves_malformed_legacy_metadata_extensions() {
    use crate::codec::openai::compatible::chat_completions::OpenAIChatCompletionsV1;
    use crate::transform::{ProtocolAdapter, ProtocolTransform};

    let message = serde_json::json!({
        "role": "user",
        "content": "hello",
        "id": null,
        "status": "legacy-status",
        "provenance": 42,
        "audience": ["legacy-audience"],
        "__open_responses_item_reference": null,
        "vendor_extra": {"value": "preserved"}
    });
    let request = OpenAIChatCompletionsV1
        .decode_request(serde_json::json!({
            "model": "model",
            "messages": [message]
        }))
        .expect("decode legacy metadata");
    let encoded = ProtocolTransform::encode_request_with(&OpenAIChatCompletionsV1, request)
        .expect("encode legacy metadata");
    assert_eq!(
        encoded.body["messages"],
        serde_json::json!([{
            "role": "user",
            "content": "hello",
            "id": null,
            "status": "legacy-status",
            "provenance": 42,
            "audience": ["legacy-audience"],
            "__open_responses_item_reference": null,
            "vendor_extra": {"value": "preserved"}
        }])
    );
}

#[test]
fn owned_encoding_preserves_shared_text_and_visible_block_shapes() {
    let shared = Arc::new(" shared payload \n".to_string());
    let item = |role, content| AiItem {
        role,
        content,
        tool_calls: None,
        tool_call_id: None,
        meta: None,
    };
    let request = AiRequest::new(
        "model",
        vec![
            item(Role::User, MessageContent::Text(Arc::clone(&shared))),
            item(Role::Assistant, MessageContent::Text(Arc::clone(&shared))),
            item(
                Role::User,
                MessageContent::Blocks(vec![
                    ContentBlock::Text {
                        text: Arc::clone(&shared),
                        cache_control: None,
                    },
                    ContentBlock::Text {
                        text: "tail".to_string().into(),
                        cache_control: None,
                    },
                ]),
            ),
            item(
                Role::Assistant,
                MessageContent::Blocks(vec![ContentBlock::Text {
                    text: Arc::clone(&shared),
                    cache_control: None,
                }]),
            ),
        ],
    );
    let (body, _) = OpenAIEncoder.encode_request(request).expect("encode");
    assert_eq!(shared.as_str(), " shared payload \n");
    assert_eq!(body["messages"][0]["content"], shared.as_str());
    assert_eq!(body["messages"][1]["content"], shared.as_str());
    assert_eq!(
        body["messages"][2]["content"],
        serde_json::json!([
            {"type": "text", "text": shared.as_str()},
            {"type": "text", "text": "tail"},
        ])
    );
    assert_eq!(body["messages"][3]["content"], shared.as_str());
}

#[test]
fn tool_result_uses_first_block_hint_and_keeps_result_order() {
    let shared = Arc::new("first result".to_string());
    let request = AiRequest::new(
        "model",
        vec![
            AiItem {
                role: Role::Assistant,
                content: MessageContent::Text(String::new().into()),
                tool_calls: Some(vec![
                    ToolCall {
                        id: "first".into(),
                        name: "first_tool".into(),
                        arguments: "{}".into(),
                    },
                    ToolCall {
                        id: "second".into(),
                        name: "second_tool".into(),
                        arguments: "{}".into(),
                    },
                ]),
                tool_call_id: None,
                meta: None,
            },
            AiItem {
                role: Role::Tool,
                content: MessageContent::Blocks(vec![
                    ContentBlock::ToolResult {
                        tool_use_id: String::new().into(),
                        content: Value::String("first result".into()),
                        content_kind: None,
                        is_error: None,
                        cache_control: None,
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "second".into(),
                        content: Value::String("ignored second block".into()),
                        content_kind: None,
                        is_error: None,
                        cache_control: None,
                    },
                ]),
                tool_calls: None,
                tool_call_id: None,
                meta: None,
            },
            AiItem {
                role: Role::Tool,
                content: MessageContent::Text(Arc::clone(&shared)),
                tool_calls: None,
                tool_call_id: Some("second".into()),
                meta: None,
            },
        ],
    );
    let (body, _) = OpenAIEncoder.encode_request(request).expect("encode");
    let results: Vec<_> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["tool_call_id"], "first");
    assert_eq!(results[0]["content"], "first result");
    assert_eq!(results[1]["tool_call_id"], "second");
    assert_eq!(results[1]["content"], shared.as_str());
    assert_eq!(shared.as_str(), "first result");
}

/// Regression: Open Responses replays history as separate items — a standalone
/// reasoning item, one item per function_call, then the outputs. The reasoning
/// item must not be encoded as an assistant message with neither `content` nor
/// `tool_calls`: DeepSeek (and strict chat-completions upstreams) reject it with
/// 400 "Invalid assistant message: content or tool_calls must be set". Its
/// reasoning text must merge onto the following assistant turn instead.
#[test]
fn standalone_reasoning_item_does_not_become_contentless_assistant_message() {
    let assistant_call = |id: &str, name: &str| AiItem {
        role: Role::Assistant,
        content: MessageContent::Text(String::new().into()),
        tool_calls: Some(vec![ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: "{}".into(),
        }]),
        tool_call_id: None,
        meta: None,
    };
    let tool_output = |id: &str, text: &str| AiItem {
        role: Role::Tool,
        content: MessageContent::Text(text.to_owned().into()),
        tool_calls: None,
        tool_call_id: Some(id.into()),
        meta: None,
    };
    let request = AiRequest::new(
        "deepseek-flash",
        vec![
            AiItem {
                role: Role::User,
                content: MessageContent::Text("修复这个问题".to_owned().into()),
                tool_calls: None,
                tool_call_id: None,
                meta: None,
            },
            AiItem::reasoning(Vec::new(), vec!["inspect the repo first".into()], None),
            assistant_call("call_00", "bash"),
            assistant_call("call_01", "glob"),
            tool_output("call_01", "scratch listing"),
            tool_output("call_00", "git status output"),
        ],
    );

    let (body, _) = OpenAIEncoder.encode_request(request).expect("encode");
    assert_replayed_reasoning_is_valid(body);

    // Release shape: replay keeps preserved native reasoning as Thinking
    // blocks (see `transform/replay.rs`), which `encode_message` strips from
    // content — the exact message DeepSeek rejected in production.
    let request = AiRequest::new(
        "deepseek-flash",
        vec![
            AiItem {
                role: Role::User,
                content: MessageContent::Text("修复这个问题".to_owned().into()),
                tool_calls: None,
                tool_call_id: None,
                meta: None,
            },
            AiItem::thinking("inspect the repo first", None),
            assistant_call("call_00", "bash"),
            assistant_call("call_01", "glob"),
            tool_output("call_01", "scratch listing"),
            tool_output("call_00", "git status output"),
        ],
    );

    let (body, _) = OpenAIEncoder.encode_request(request).expect("encode");
    assert_replayed_reasoning_is_valid(body);
}

fn assert_replayed_reasoning_is_valid(body: serde_json::Value) {
    let messages = body["messages"].as_array().expect("messages array");

    let mut reasoning_merged = false;
    for message in messages {
        if message["role"] != "assistant" {
            continue;
        }
        let has_content = message.get("content").is_some_and(|c| !c.is_null());
        let has_calls = message
            .get("tool_calls")
            .is_some_and(|c| c.as_array().is_some_and(|a| !a.is_empty()));
        assert!(
            has_content || has_calls,
            "assistant message without content or tool_calls: {message}"
        );
        if message.get("reasoning_content").and_then(|c| c.as_str())
            == Some("inspect the repo first")
        {
            reasoning_merged = true;
        }
    }
    assert!(
        reasoning_merged,
        "standalone reasoning text must survive on a following assistant message: {messages:?}"
    );
}

#[test]
fn canonical_system_is_encoded_as_a_system_message() {
    let mut request = AiRequest::new(
        "model",
        vec![AiItem {
            role: Role::User,
            content: MessageContent::Text("hello".to_owned().into()),
            tool_calls: None,
            tool_call_id: None,
            meta: None,
        }],
    );
    request.instructions = Some("hook-system".into());

    let (body, _) = OpenAIEncoder.encode_request(request).unwrap();

    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], "hook-system");
    assert_eq!(body["messages"][1]["role"], "user");
}

#[test]
fn internal_artifact_identity_is_not_sent_upstream() {
    let request = AiRequest::new(
        "model",
        vec![AiItem {
            role: Role::User,
            content: MessageContent::Text("hello".to_owned().into()),
            tool_calls: None,
            tool_call_id: None,
            meta: Some(
                stravia_runtime_contract::protocol::ir::AiItemMetadata::boxed(serde_json::json!({
                    "__stravia_artifact_references": [{
                        "block_index": 0,
                        "artifact_id": "artifact_secret"
                    }],
                    "reasoning_content": "visible reasoning"
                })),
            ),
        }],
    );

    let (body, _) = OpenAIEncoder.encode_request(request).expect("encode");
    assert_eq!(
        body["messages"][0]["reasoning_content"],
        "visible reasoning"
    );
    assert!(
        body["messages"][0]
            .get("__stravia_artifact_references")
            .is_none()
    );
    assert!(!body.to_string().contains("artifact_secret"));
}

#[test]
fn gateway_request_state_is_not_passed_through_upstream() {
    let mut request = AiRequest::new(
        "model",
        vec![AiItem {
            role: Role::User,
            content: MessageContent::Text("hello".to_owned().into()),
            tool_calls: None,
            tool_call_id: None,
            meta: None,
        }],
    );
    let ingress = &mut request.meta.vendor.ingress;
    ingress.insert(
        "__stravia_generation_session_id".into(),
        serde_json::json!("session-secret"),
    );
    ingress.insert(
        stravia_runtime_contract::protocol::ir::request::VERIFIED_HISTORY_REPLAY_META.into(),
        serde_json::Value::Bool(true),
    );
    ingress.insert("client_extension".into(), serde_json::json!("kept"));

    let (body, _) = OpenAIEncoder.encode_request(request).expect("encode");
    assert_eq!(body["client_extension"], "kept");
    assert!(!body.to_string().contains("__stravia_"), "{body}");
}

#[test]
fn synthetic_tool_ids_are_distinct_correlated_and_skip_supplied_ids() {
    let request = AiRequest::new(
        "model",
        vec![
            AiItem {
                role: Role::Assistant,
                content: MessageContent::Text(String::new().into()),
                tool_calls: Some(vec![
                    ToolCall {
                        id: (String::new()).into(),
                        name: "first".into(),
                        arguments: "{}".into(),
                    },
                    ToolCall {
                        id: "tc_1".into(),
                        name: "external".into(),
                        arguments: "{}".into(),
                    },
                    ToolCall {
                        id: (String::new()).into(),
                        name: "second".into(),
                        arguments: "{}".into(),
                    },
                ]),
                tool_call_id: None,
                meta: None,
            },
            AiItem {
                role: Role::Tool,
                content: MessageContent::Text("first result".to_owned().into()),
                tool_calls: None,
                tool_call_id: None,
                meta: None,
            },
            AiItem {
                role: Role::Tool,
                content: MessageContent::Text("external result".to_owned().into()),
                tool_calls: None,
                tool_call_id: None,
                meta: None,
            },
            AiItem {
                role: Role::Tool,
                content: MessageContent::Text("second result".to_owned().into()),
                tool_calls: None,
                tool_call_id: None,
                meta: None,
            },
        ],
    );

    let (first, _) = OpenAIEncoder
        .encode_request(request.clone())
        .expect("encode");
    let (repeated, _) = OpenAIEncoder.encode_request(request).expect("encode");
    let result_ids: Vec<&str> = first["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| message["tool_call_id"].as_str().unwrap())
        .collect();

    assert_eq!(result_ids, ["tc_2", "tc_1", "tc_3"]);
    assert_eq!(repeated["messages"], first["messages"]);
}

#[test]
fn duplicate_external_tool_calls_remain_distinct_and_correlated() {
    let request = AiRequest::new(
        "model",
        vec![
            AiItem {
                role: Role::Assistant,
                content: MessageContent::Text(String::new().into()),
                tool_calls: Some(vec![
                    ToolCall {
                        id: "external".into(),
                        name: "first".into(),
                        arguments: "{}".into(),
                    },
                    ToolCall {
                        id: "external".into(),
                        name: "second".into(),
                        arguments: "{}".into(),
                    },
                ]),
                tool_call_id: None,
                meta: None,
            },
            AiItem {
                role: Role::Tool,
                content: MessageContent::Text("second result".to_owned().into()),
                tool_calls: None,
                tool_call_id: Some("external".into()),
                meta: None,
            },
            AiItem {
                role: Role::Tool,
                content: MessageContent::Text("first result".to_owned().into()),
                tool_calls: None,
                tool_call_id: Some("external".into()),
                meta: None,
            },
        ],
    );

    let (body, _) = OpenAIEncoder.encode_request(request).expect("encode");
    let result_ids: Vec<&str> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| message["tool_call_id"].as_str().unwrap())
        .collect();

    assert_eq!(result_ids, ["tc_1", "external"]);
}

#[test]
fn responses_reasoning_effort_maps_to_chat_without_loss() {
    use crate::codec::open_responses::decoder::ResponsesDecoder;

    let mut request = ResponsesDecoder
        .decode_request(serde_json::json!({
            "model": "gpt",
            "input": "hello",
            "reasoning": { "effort": "xhigh", "summary": "auto" }
        }))
        .unwrap();
    request.reasoning.target_control = Some(
        stravia_runtime_contract::thinking::TargetThinkingControl::Effort {
            value: "xhigh".into(),
        },
    );

    let (body, _) = OpenAIEncoder.encode_request(request).unwrap();

    assert_eq!(body["reasoning_effort"], "xhigh");
    assert!(body.get("reasoning").is_none());
}

#[test]
fn generic_toggle_without_provider_adapter_is_omitted() {
    let mut request = AiRequest::new(
        "custom-model",
        vec![AiItem {
            role: Role::User,
            content: MessageContent::Text("hello".to_owned().into()),
            tool_calls: None,
            tool_call_id: None,
            meta: None,
        }],
    );
    request.reasoning.target_control =
        Some(stravia_runtime_contract::thinking::TargetThinkingControl::Enabled);
    request.meta.source_protocol =
        Some(stravia_runtime_contract::protocol::ids::OPENAI_COMPATIBLE_CHAT_COMPLETIONS_V1);

    let encoded = crate::transform::ProtocolTransform::global()
        .bind(
            stravia_runtime_contract::protocol::ids::OPENAI_COMPATIBLE_CHAT_COMPLETIONS_V1,
            stravia_runtime_contract::protocol::ids::OPENAI_COMPATIBLE_CHAT_COMPLETIONS_V1,
        )
        .unwrap()
        .encode_request(request)
        .unwrap();
    assert!(encoded.body.get("reasoning").is_none());
    assert!(encoded.body.get("thinking").is_none());
    assert!(encoded.body.get("enable_thinking").is_none());
    assert_eq!(encoded.body["messages"][0]["content"], "hello");
}

#[test]
fn unknown_reasoning_effort_is_rejected() {
    use crate::codec::openai::compatible::decoder::OpenAIDecoder;

    for value in ["future_7", "not safe"] {
        assert!(
            OpenAIDecoder
                .decode_request(serde_json::json!({
                "model": "gpt",
                "messages": [{"role": "user", "content": "hello"}],
                "reasoning_effort": value
                    }))
                .is_err()
        );
    }
}

#[test]
fn thinking_and_reasoning_plaintext_joins_into_reasoning_content() {
    let request = AiRequest::new(
        "deepseek-flash",
        vec![
            AiItem {
                role: Role::User,
                content: MessageContent::Text("hello".to_owned().into()),
                tool_calls: None,
                tool_call_id: None,
                meta: None,
            },
            AiItem {
                role: Role::Assistant,
                content: MessageContent::Blocks(vec![
                    ContentBlock::Thinking {
                        thinking: "first thought".into(),
                        signature: Some("opaque-sig".into()),
                    },
                    ContentBlock::Reasoning {
                        summary: vec!["summary".into()],
                        content: vec!["detail".into()],
                        encrypted_content: Some("cipher-text".into()),
                    },
                    ContentBlock::RedactedThinking {
                        data: "redacted-payload".into(),
                    },
                    ContentBlock::Text {
                        text: "answer".to_owned().into(),
                        cache_control: None,
                    },
                ]),
                tool_calls: None,
                tool_call_id: None,
                meta: None,
            },
        ],
    );

    let (body, _) = OpenAIEncoder.encode_request(request).expect("encode");

    let assistant = &body["messages"][1];
    // 明文推理按块顺序进入 reasoning_content；受保护载荷没有任何载体。
    assert_eq!(
        assistant["reasoning_content"],
        "first thought\nsummary\ndetail"
    );
    let wire = body.to_string();
    assert!(!wire.contains("opaque-sig"), "{wire}");
    assert!(!wire.contains("cipher-text"), "{wire}");
    assert!(!wire.contains("redacted-payload"), "{wire}");
    assert!(!wire.contains("reasoning\""), "{wire}");
}

#[test]
fn protected_only_assistant_item_is_dropped_entirely() {
    for blocks in [
        vec![ContentBlock::RedactedThinking {
            data: "redacted-payload".into(),
        }],
        vec![ContentBlock::Reasoning {
            summary: Vec::new(),
            content: Vec::new(),
            encrypted_content: Some("cipher-text".into()),
        }],
        vec![ContentBlock::Thinking {
            thinking: String::new(),
            signature: Some("opaque-sig".into()),
        }],
    ] {
        let request = AiRequest::new(
            "deepseek-flash",
            vec![
                AiItem {
                    role: Role::User,
                    content: MessageContent::Text("hello".to_owned().into()),
                    tool_calls: None,
                    tool_call_id: None,
                    meta: None,
                },
                AiItem {
                    role: Role::Assistant,
                    content: MessageContent::Blocks(blocks),
                    tool_calls: None,
                    tool_call_id: None,
                    meta: None,
                },
                AiItem {
                    role: Role::User,
                    content: MessageContent::Text("continue".to_owned().into()),
                    tool_calls: None,
                    tool_call_id: None,
                    meta: None,
                },
            ],
        );

        let (body, _) = OpenAIEncoder.encode_request(request).expect("encode");

        // 条目只剩承载不了的受保护载荷：整条跳过，不能发出空 assistant 消息。
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(messages.iter().all(|m| m["role"] == "user"));
        let wire = body.to_string();
        assert!(!wire.contains("redacted-payload"), "{wire}");
        assert!(!wire.contains("cipher-text"), "{wire}");
        assert!(!wire.contains("opaque-sig"), "{wire}");
    }
}

#[test]
fn signed_thinking_with_tool_calls_keeps_reasoning_content_and_drops_signature() {
    let request = AiRequest::new(
        "deepseek-flash",
        vec![
            AiItem {
                role: Role::User,
                content: MessageContent::Text("hello".to_owned().into()),
                tool_calls: None,
                tool_call_id: None,
                meta: None,
            },
            AiItem {
                role: Role::Assistant,
                content: MessageContent::Blocks(vec![ContentBlock::Thinking {
                    thinking: "inspect the repo".into(),
                    signature: Some("opaque-sig".into()),
                }]),
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".into(),
                    name: "glob".into(),
                    arguments: "{}".into(),
                }]),
                tool_call_id: None,
                meta: None,
            },
            AiItem {
                role: Role::Tool,
                content: MessageContent::Text("listing".to_owned().into()),
                tool_calls: None,
                tool_call_id: Some("call_1".into()),
                meta: None,
            },
        ],
    );

    let (body, _) = OpenAIEncoder.encode_request(request).expect("encode");

    let assistant = &body["messages"][1];
    assert_eq!(assistant["reasoning_content"], "inspect the repo");
    assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
    // 有 tool_calls 时 content 可缺省，但绝不能把签名写进任何字段。
    assert!(!body.to_string().contains("opaque-sig"));
}
