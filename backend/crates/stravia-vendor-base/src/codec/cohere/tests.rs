use super::*;

#[test]
fn projects_nested_cohere_results_without_disguising_media() {
    use stravia_runtime_contract::protocol::ir::ToolResultContentKind;
    let mixed = json!([
        {"type": "text", "text": "keep"},
        {"type": "input_text", "text": " more"},
        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aW1n"}},
        {"type": "unknown", "text": "drop"}
    ]);
    for (kind, content, expected) in [
        (
            ToolResultContentKind::ContentBlocks,
            mixed.clone(),
            Some("keep more".to_owned()),
        ),
        (
            ToolResultContentKind::Json,
            mixed.clone(),
            Some(serde_json::to_string(&mixed).unwrap()),
        ),
        (
            ToolResultContentKind::Json,
            json!({"type": "image", "business": true}),
            Some("{\"type\":\"image\",\"business\":true}".to_owned()),
        ),
        (
            ToolResultContentKind::ContentBlocks,
            json!([{"type": "image"}, {"type": "unknown", "text": "drop"}]),
            None,
        ),
    ] {
        let mut item = AiItem::function_call_output("call_1", json!("unused"));
        item.content = MessageContent::Blocks(vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".into(),
            content_kind: Some(kind),
            content,
            is_error: Some(true),
            cache_control: None,
        }]);
        let (body, _) = CohereChatV2
            .encode_request(AiRequest::new("command-a", vec![item]))
            .unwrap();
        if let Some(expected) = expected {
            assert_eq!(body["messages"][0]["tool_call_id"], "call_1");
            let actual = body["messages"][0]["content"].as_str().unwrap();
            if kind == ToolResultContentKind::Json {
                assert_eq!(
                    serde_json::from_str::<Value>(actual).unwrap(),
                    serde_json::from_str::<Value>(&expected).unwrap()
                );
            } else {
                assert_eq!(actual, expected);
            }
        } else {
            assert_eq!(body["messages"], json!([]));
        }
    }
}

#[test]
fn encodes_cohere_tool_schema_without_openai_shape() {
    let mut request = AiRequest::new("command-a", vec![AiItem::output_text("hello")]);
    request.items[0].role = Role::User;
    request.tools = Some(vec![stravia_runtime_contract::protocol::ir::ToolSpec {
        name: "weather".into(),
        description: Some("Get weather".into()),
        parameters: json!({"type": "object"}),
        strict: None,
        cache_control: None,
        meta: None,
    }]);
    let (body, _) = CohereChatV2.encode_request(request).unwrap();
    assert_eq!(body["tools"][0]["function"]["name"], "weather");
    assert!(body.get("messages").is_some());
    assert!(body.get("choices").is_none());
}

#[test]
fn normalizes_cohere_null_tool_arguments() {
    let response = CohereChatV2
        .decode_response(json!({
            "generation_id": "gen_1",
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "weather", "arguments": "null"}
                }]
            },
            "finish_reason": "TOOL_CALL",
            "usage": {"tokens": {"input_tokens": 3, "output_tokens": 4}}
        }))
        .unwrap();

    assert_eq!(response.tool_calls().next().unwrap().arguments, "{}");
}

#[test]
fn omits_assistant_text_when_replaying_cohere_tool_calls() {
    let item = AiItem {
        role: Role::Assistant,
        content: MessageContent::Text("I will call weather.".to_owned().into()),
        tool_calls: Some(vec![ToolCall {
            id: "call_1".into(),
            name: "weather".into(),
            arguments: "{}".into(),
        }]),
        tool_call_id: None,
        meta: None,
    };

    let message = encode_message(&item)
        .unwrap()
        .expect("tool-call turn still encodes");
    assert!(message.get("content").is_none());
    assert_eq!(message["tool_calls"][0]["function"]["name"], "weather");
}

#[test]
fn replays_reasoning_as_thinking_parts_without_protected_payloads() {
    let block = |block| AiItem {
        role: Role::Assistant,
        content: MessageContent::Blocks(vec![block]),
        tool_calls: None,
        tool_call_id: None,
        meta: None,
    };
    let request = AiRequest::new(
        "command-a",
        vec![
            block(ContentBlock::Thinking {
                thinking: "let me think".into(),
                signature: Some("sig_protected".into()),
            }),
            block(ContentBlock::Reasoning {
                summary: vec!["summary".into()],
                content: vec![String::new(), "detail".into()],
                encrypted_content: Some("enc_protected".into()),
            }),
        ],
    );

    let (body, _) = CohereChatV2.encode_request(request).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(
        messages[0]["content"],
        json!([{"type": "thinking", "thinking": "let me think"}])
    );
    assert_eq!(
        messages[1]["content"],
        json!([
            {"type": "thinking", "thinking": "summary"},
            {"type": "thinking", "thinking": "detail"},
        ])
    );
    // 受保护载荷无原生载体，绝不能出现在线上 body 里。
    let body_text = serde_json::to_string(&body).unwrap();
    assert!(!body_text.contains("sig_protected"));
    assert!(!body_text.contains("enc_protected"));
}

#[test]
fn drops_protected_only_assistant_without_breaking_tool_pairing() {
    let protected_only = AiItem {
        role: Role::Assistant,
        content: MessageContent::Blocks(vec![
            ContentBlock::RedactedThinking {
                data: "redacted".into(),
            },
            ContentBlock::Reasoning {
                summary: Vec::new(),
                content: Vec::new(),
                encrypted_content: Some("enc".into()),
            },
        ]),
        tool_calls: None,
        tool_call_id: None,
        meta: None,
    };
    let with_call = AiItem {
        role: Role::Assistant,
        content: MessageContent::Blocks(vec![ContentBlock::RedactedThinking {
            data: "redacted".into(),
        }]),
        tool_calls: Some(vec![ToolCall {
            id: "call_1".into(),
            name: "weather".into(),
            arguments: "{}".into(),
        }]),
        tool_call_id: None,
        meta: None,
    };
    let result = AiItem::function_call_output("call_1", json!("sunny"));
    let mut request = AiRequest::new(
        "command-a",
        vec![AiItem::output_text("hi"), protected_only, with_call, result],
    );
    request.items[0].role = Role::User;

    let (body, _) = CohereChatV2.encode_request(request).unwrap();
    let messages = body["messages"].as_array().unwrap();
    // 空 assistant 条目被整条跳过；带 tool_call 的条目保留以维持配对。
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["tool_call_id"], "call_1");
    let body_text = serde_json::to_string(&body).unwrap();
    assert!(!body_text.contains("redacted"));
}

#[test]
fn omits_raw_choice_and_file_id_images_preserving_text() {
    let mut item = AiItem::output_text("keep");
    item.role = Role::User;
    item.content = MessageContent::Blocks(vec![
        ContentBlock::Text {
            text: "keep".to_owned().into(),
            cache_control: None,
        },
        ContentBlock::Image {
            source: MediaSource::FileId {
                file_id: "file_1".into(),
                detail: None,
            },
            detail: None,
            cache_control: None,
        },
    ]);
    let mut request = AiRequest::new("command-a", vec![item]);
    request.tool_choice = Some(ToolChoice::Raw(json!({"unsupported": true})));
    let (body, _) = CohereChatV2.encode_request(request).unwrap();
    assert!(body.get("tool_choice").is_none());
    assert_eq!(
        body["messages"][0]["content"],
        json!([{"type": "text", "text": "keep"}])
    );
}

#[test]
fn skips_role_specific_unsupported_blocks_preserving_text() {
    for role in [
        Role::System,
        Role::Developer,
        Role::User,
        Role::Assistant,
        Role::Tool,
    ] {
        let item = AiItem {
            role,
            content: MessageContent::Blocks(vec![
                ContentBlock::Text {
                    text: "keep".to_owned().into(),
                    cache_control: None,
                },
                ContentBlock::Image {
                    source: MediaSource::FileId {
                        file_id: "file_1".into(),
                        detail: None,
                    },
                    detail: None,
                    cache_control: None,
                },
            ]),
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            meta: None,
        };
        let message = encode_message(&item).unwrap().unwrap();
        let expected = if role == Role::User {
            json!([{"type": "text", "text": "keep"}])
        } else {
            json!("keep")
        };
        assert_eq!(message["content"], expected);
    }
}
