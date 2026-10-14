use super::*;

#[test]
fn projects_nested_tool_results_to_native_carriers() {
    use stravia_runtime_contract::protocol::ir::ToolResultContentKind;
    let mixed = json!([
        {"type": "text", "text": "keep"},
        {"type": "input_text", "text": " more"},
        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aW1n"}},
        {"type": "unknown", "text": "drop"},
        {"type": "image", "source": {"type": "url", "url": "https://example.com/image.png"}}
    ]);
    let result = |kind, content| ContentBlock::ToolResult {
        tool_use_id: "call_1".into(),
        content_kind: Some(kind),
        content,
        is_error: Some(true),
        cache_control: None,
    };
    for role in [Role::User, Role::Tool] {
        let item = AiItem {
            role,
            content: MessageContent::Blocks(vec![result(
                ToolResultContentKind::ContentBlocks,
                mixed.clone(),
            )]),
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            meta: None,
        };
        let (body, _) = BedrockConverseV1
            .encode_request(AiRequest::new("model", vec![item]))
            .unwrap();
        let output = &body["messages"][0]["content"][0]["toolResult"];
        assert_eq!(output["toolUseId"], "call_1");
        assert_eq!(output["status"], "error");
        assert_eq!(
            output["content"],
            json!([
                {"text": "keep"}, {"text": " more"},
                {"image": {"format": "png", "source": {"bytes": "aW1n"}}}
            ])
        );
        let item = AiItem {
            role,
            content: MessageContent::Blocks(vec![result(
                ToolResultContentKind::Json,
                mixed.clone(),
            )]),
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            meta: None,
        };
        let (body, _) = BedrockConverseV1
            .encode_request(AiRequest::new("model", vec![item]))
            .unwrap();
        assert_eq!(
            body["messages"][0]["content"][0]["toolResult"]["content"],
            json!([{"json": mixed}])
        );
    }
}

#[test]
fn replays_native_bedrock_tool_result_blocks_without_json_wrapping() {
    let content = json!([
        {"text": "keep"}, {"json": [{"type": "image", "business": true}]},
        {"image": {"format": "png", "source": {"bytes": "aW1n"}}}
    ]);
    let request = BedrockConverseV1.decode_request(json!({"modelId": "model", "messages": [
        {"role": "user", "content": [{"toolResult": {"toolUseId": "call_1", "content": content, "status": "error"}}]}
    ]})).unwrap();
    let (body, _) = BedrockConverseV1.encode_request(request).unwrap();
    assert_eq!(
        body["messages"][0]["content"][0]["toolResult"]["content"],
        content
    );
    assert_eq!(
        body["messages"][0]["content"][0]["toolResult"]["status"],
        "error"
    );
}

#[test]
fn omits_unrepresentable_nested_bedrock_results() {
    use stravia_runtime_contract::protocol::ir::ToolResultContentKind;
    for role in [Role::User, Role::Tool] {
        let item = AiItem {
            role,
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content_kind: Some(ToolResultContentKind::ContentBlocks),
                content: json!([{"type": "unknown", "text": "drop"}]),
                is_error: Some(true),
                cache_control: None,
            }]),
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            meta: None,
        };
        let (body, _) = BedrockConverseV1
            .encode_request(AiRequest::new("model", vec![item]))
            .unwrap();
        assert_eq!(body["messages"], json!([]));
    }
}

#[test]
fn preserves_native_document_bytes_text_names_and_context() {
    let mut item = user_item("unused");
    item.content = MessageContent::Blocks(vec![
        ContentBlock::Text {
            text: "read attachments".to_owned().into(),
            cache_control: None,
        },
        ContentBlock::Document {
            source: DocumentSource::Base64Pdf {
                data: "JVBERi0xLjQK".into(),
            },
            title: Some("Quarterly report [2026] (final)-1".into()),
            context: Some("Use the supplied totals".into()),
            cache_control: None,
        },
        ContentBlock::Document {
            source: DocumentSource::PlainText {
                data: "Plain document\nSecond line".into(),
            },
            title: Some("invalid.pdf".into()),
            context: Some("text context".into()),
            cache_control: None,
        },
        ContentBlock::Document {
            source: DocumentSource::Url("https://example.com/private.pdf".into()),
            title: None,
            context: None,
            cache_control: None,
        },
        ContentBlock::Audio {
            source: MediaSource::Url("https://example.com/audio.wav".into()),
        },
    ]);
    let (body, _) = BedrockConverseV1
        .encode_request(AiRequest::new("model", vec![item]))
        .unwrap();
    assert_eq!(
        body["messages"],
        json!([{"role": "user", "content": [
            {"text": "read attachments"},
            {"document": {"format": "pdf", "name": "Quarterly report [2026] (final)-1",
                "source": {"bytes": "JVBERi0xLjQK"}, "context": "Use the supplied totals"}},
            {"document": {"format": "txt", "name": "document",
                "source": {"text": "Plain document\nSecond line"}, "context": "text context"}}
        ]}])
    );
}

#[test]
fn encodes_known_inline_file_document_formats() {
    for (mime, format) in [
        ("application/pdf", "pdf"),
        ("text/csv", "csv"),
        ("application/msword", "doc"),
        (
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "docx",
        ),
        ("application/vnd.ms-excel", "xls"),
        (
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "xlsx",
        ),
        ("text/html", "html"),
        ("text/plain", "txt"),
        ("text/markdown", "md"),
    ] {
        for override_mime in [false, true] {
            let mut item = user_item("unused");
            item.content = MessageContent::Blocks(vec![
                ContentBlock::Text {
                    text: "read".to_owned().into(),
                    cache_control: None,
                },
                ContentBlock::File {
                    source: MediaSource::Base64 {
                        media_type: if override_mime {
                            "application/octet-stream"
                        } else {
                            mime
                        }
                        .into(),
                        data: "AAEC/w==".into(),
                    },
                    media_type: override_mime.then(|| mime.to_owned()),
                },
                ContentBlock::File {
                    source: MediaSource::Base64 {
                        media_type: "application/zip".into(),
                        data: "eA==".into(),
                    },
                    media_type: None,
                },
            ]);
            let (body, _) = BedrockConverseV1
                .encode_request(AiRequest::new("model", vec![item]))
                .unwrap();
            assert_eq!(
                body["messages"][0]["content"],
                json!([
                    {"text": "read"},
                    {"document": {"format": format, "name": "document", "source": {"bytes": "AAEC/w=="}}}
                ])
            );
        }
    }
}

#[test]
fn keeps_only_official_document_names_without_rewriting() {
    for (title, expected) in [
        (None, "document"),
        (Some(""), "document"),
        (Some("two  spaces"), "document"),
        (Some("report.pdf"), "document"),
        (Some("report_name"), "document"),
        (Some("A [B] (C)-1"), "A [B] (C)-1"),
    ] {
        let mut item = user_item("unused");
        item.content = MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "read".to_owned().into(),
                cache_control: None,
            },
            ContentBlock::Document {
                source: DocumentSource::PlainText {
                    data: "body".into(),
                },
                title: title.map(str::to_owned),
                context: None,
                cache_control: None,
            },
        ]);
        let (body, _) = BedrockConverseV1
            .encode_request(AiRequest::new("model", vec![item]))
            .unwrap();
        let document = &body["messages"][0]["content"][1]["document"];
        assert_eq!(document["name"], expected);
        assert!(document.get("context").is_none());
    }
    assert!(bedrock_document_name_valid(&"a".repeat(200)));
    assert!(!bedrock_document_name_valid(&"a".repeat(201)));
}

#[test]
fn adds_only_required_empty_text_carrier_for_document_only_users() {
    for source in [
        DocumentSource::Base64Pdf {
            data: "JVBERg==".into(),
        },
        DocumentSource::PlainText {
            data: "actual body".into(),
        },
    ] {
        let mut item = user_item("unused");
        item.content = MessageContent::Blocks(vec![ContentBlock::Document {
            source,
            title: Some("attachment".into()),
            context: Some("context stays metadata".into()),
            cache_control: None,
        }]);
        let (body, _) = BedrockConverseV1
            .encode_request(AiRequest::new("model", vec![item.clone()]))
            .unwrap();
        assert_eq!(body["messages"][0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][0]["content"][1], json!({"text": ""}));
        item.role = Role::Assistant;
        let (body, _) = BedrockConverseV1
            .encode_request(AiRequest::new("model", vec![item]))
            .unwrap();
        assert_eq!(body["messages"], json!([]));
    }
}

#[test]
fn encodes_converse_tool_config_not_chat_completions() {
    let mut request = AiRequest::new(
        "anthropic.claude",
        vec![AiItem {
            role: Role::User,
            content: MessageContent::Text("Hello".to_owned().into()),
            tool_calls: None,
            tool_call_id: None,
            meta: None,
        }],
    );
    request.tools = Some(vec![ToolSpec {
        name: "weather".into(),
        description: None,
        parameters: json!({"type":"object"}),
        strict: None,
        cache_control: None,
        meta: None,
    }]);
    let (body, _) = BedrockConverseV1.encode_request(request).unwrap();
    assert_eq!(
        body["toolConfig"]["tools"][0]["toolSpec"]["name"],
        "weather"
    );
    assert!(body.get("messages").is_some());
    assert!(body.get("choices").is_none());
}

fn user_item(text: &str) -> AiItem {
    AiItem {
        role: Role::User,
        content: MessageContent::Text(text.to_owned().into()),
        tool_calls: None,
        tool_call_id: None,
        meta: None,
    }
}

fn assistant_block(block: ContentBlock) -> AiItem {
    AiItem {
        role: Role::Assistant,
        content: MessageContent::Blocks(vec![block]),
        tool_calls: None,
        tool_call_id: None,
        meta: None,
    }
}

#[test]
fn replays_signed_thinking_and_redacted_natively() {
    let request = AiRequest::new(
        "anthropic.claude",
        vec![
            user_item("hi"),
            assistant_block(ContentBlock::Thinking {
                thinking: "let me think".into(),
                signature: Some("sig_protected".into()),
            }),
            assistant_block(ContentBlock::RedactedThinking {
                data: "cmVkYWN0ZWQ=".into(),
            }),
            assistant_block(ContentBlock::Reasoning {
                summary: vec!["summary".into()],
                content: vec!["detail".into()],
                encrypted_content: Some("enc_protected".into()),
            }),
        ],
    );

    let (body, _) = BedrockConverseV1.encode_request(request).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(
        messages[1]["content"][0],
        json!({"reasoningContent": {"reasoningText": {
            "text": "let me think",
            "signature": "sig_protected",
        }}})
    );
    assert_eq!(
        messages[2]["content"][0],
        json!({"reasoningContent": {"redactedContent": "cmVkYWN0ZWQ="}})
    );
    // encrypted_content 借 reasoningText.signature 原生承载。
    assert_eq!(
        messages[3]["content"][0],
        json!({"reasoningContent": {"reasoningText": {
            "text": "summary\ndetail",
            "signature": "enc_protected",
        }}})
    );
}

#[test]
fn downgrades_unsigned_reasoning_to_text_blocks() {
    // Claude on Bedrock 拒绝无签名 reasoningContent，明文只能走 text 块。
    let request = AiRequest::new(
        "anthropic.claude",
        vec![
            assistant_block(ContentBlock::Thinking {
                thinking: "plain thought".into(),
                signature: None,
            }),
            assistant_block(ContentBlock::Reasoning {
                summary: vec!["s1".into()],
                content: vec![String::new(), "c1".into()],
                encrypted_content: None,
            }),
        ],
    );

    let (body, _) = BedrockConverseV1.encode_request(request).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["content"], json!([{"text": "plain thought"}]));
    // 每段非空文本一个 text 块，保持原位置顺序。
    assert_eq!(
        messages[1]["content"],
        json!([{"text": "s1"}, {"text": "c1"}])
    );
}

#[test]
fn skips_empty_assistant_without_breaking_tool_pairing() {
    let with_call = AiItem {
        role: Role::Assistant,
        content: MessageContent::Blocks(vec![ContentBlock::RedactedThinking {
            data: "cmVkYWN0ZWQ=".into(),
        }]),
        tool_calls: Some(vec![ToolCall {
            id: "call_1".into(),
            name: "weather".into(),
            arguments: "{}".into(),
        }]),
        tool_call_id: None,
        meta: None,
    };
    let request = AiRequest::new(
        "anthropic.claude",
        vec![
            user_item("a"),
            // 无签名且无明文：编码为空，整条跳过。
            assistant_block(ContentBlock::Thinking {
                thinking: String::new(),
                signature: None,
            }),
            // redacted 有原生载体，且 tool call 必须保留。
            with_call,
            AiItem::function_call_output("call_1", json!("sunny")),
            user_item("b"),
        ],
    );

    let (body, _) = BedrockConverseV1.encode_request(request).unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(
        messages[1]["content"],
        json!([
            {"reasoningContent": {"redactedContent": "cmVkYWN0ZWQ="}},
            {"toolUse": {"toolUseId": "call_1", "name": "weather", "input": {}}},
        ])
    );
    assert_eq!(
        messages[2]["content"][0]["toolResult"]["toolUseId"],
        "call_1"
    );
    assert_eq!(messages[3]["content"], json!([{"text": "b"}]));
}

#[test]
fn omits_unsupported_controls_roles_and_noninline_images() {
    let mut system = user_item("system-only");
    system.role = Role::System;
    let mut developer = user_item("developer-only");
    developer.role = Role::Developer;
    let mut user = user_item("keep");
    user.content = MessageContent::Blocks(vec![
        ContentBlock::Text {
            text: "keep".to_owned().into(),
            cache_control: None,
        },
        ContentBlock::Image {
            source: MediaSource::Url("https://example.com/image.png".into()),
            detail: None,
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
    let mut request = AiRequest::new("model", vec![system, developer, user]);
    request.generation.frequency_penalty = Some(0.5);
    request.generation.presence_penalty = Some(0.5);
    request.generation.seed = Some(1);
    request.reasoning.enabled = true;
    request.response_format =
        Some(stravia_runtime_contract::protocol::ir::ResponseFormat::JsonObject);
    request.tools = Some(vec![ToolSpec {
        name: "weather".into(),
        description: None,
        parameters: json!({"type": "object"}),
        strict: None,
        cache_control: None,
        meta: None,
    }]);
    request.tool_choice = Some(ToolChoice::Raw(json!({"unsupported": true})));
    let (body, _) = BedrockConverseV1.encode_request(request).unwrap();
    assert_eq!(
        body["messages"],
        json!([{"role": "user", "content": [{"text": "keep"}]}])
    );
    assert_eq!(
        body["system"],
        json!([{"text": "system-only"}, {"text": "developer-only"}])
    );
    assert!(body.get("inferenceConfig").is_none());
    assert!(body["toolConfig"].get("toolChoice").is_none());
}

#[test]
fn preserves_text_while_dropping_role_specific_unsupported_blocks() {
    let blocks = vec![
        ContentBlock::Text {
            text: "keep".to_owned().into(),
            cache_control: None,
        },
        ContentBlock::Audio {
            source: MediaSource::Url("https://example.com/audio.wav".into()),
        },
    ];
    let mut item = user_item("unused");
    item.content = MessageContent::Blocks(blocks);
    assert_eq!(
        encode_user_content(&item.content).unwrap(),
        vec![json!({"text": "keep"})]
    );
    item.role = Role::Assistant;
    assert_eq!(
        encode_assistant_content(&item).unwrap(),
        vec![json!({"text": "keep"})]
    );
    item.role = Role::Tool;
    item.tool_call_id = Some("call_1".into());
    assert_eq!(
        encode_tool_result(&item).unwrap()[0]["toolResult"]["content"],
        json!([{"text": "keep"}])
    );
}

#[test]
fn drops_malformed_and_nonobject_calls_with_their_results() {
    for arguments in ["not-json", "[]", "null", "42", "\"text\""] {
        let mut assistant = assistant_block(ContentBlock::Thinking {
            thinking: String::new(),
            signature: None,
        });
        assistant.tool_calls = Some(vec![ToolCall {
            id: "bad".into(),
            name: "weather".into(),
            arguments: arguments.into(),
        }]);
        let request = AiRequest::new(
            "model",
            vec![
                assistant.clone(),
                AiItem::function_call_output("bad", json!("result")),
                user_item("keep"),
            ],
        );
        let body = stravia_protocol_codec::transform::ProtocolTransform::encode_request_with(
            &BedrockConverseV1,
            request,
        )
        .unwrap()
        .body;
        assert_eq!(
            body["messages"],
            json!([{"role": "user", "content": [{"text": "keep"}]}])
        );
        let mut response = AiResponse::new("response", "model");
        assistant.content = MessageContent::Blocks(Vec::new());
        response.items.push(assistant);
        assert_eq!(
            BedrockConverseV1.encode_response(&response)["output"]["message"]["content"],
            json!([])
        );
    }
}
