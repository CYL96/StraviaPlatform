use super::*;

#[test]
fn decodes_both_official_schema_carriers_and_preserves_native_roundtrip() {
    use crate::codec::google::gemini::encoder::GoogleEncoder;
    for (carrier, schema) in [
        (
            "responseSchema",
            serde_json::json!({
                "type": "OBJECT", "properties": {"answer": {"type": "STRING", "nullable": true}, "items": {"type": "ARRAY", "minItems": "1", "items": {"type": "INTEGER"}}}
            }),
        ),
        (
            "responseJsonSchema",
            serde_json::json!({
                "type": "object", "additionalProperties": false,
                "$defs": {"entry": {"type": "string"}},
                "properties": {"answer": {"$ref": "#/$defs/entry"}}
            }),
        ),
    ] {
        let mut config = serde_json::json!({"responseMimeType": "application/json"});
        config[carrier] = schema.clone();
        let request = GoogleDecoder
            .decode_request(serde_json::json!({"contents": [], "generationConfig": config}))
            .expect("native schema");
        let Some(ProtocolExt::Google(ext)) = &request.ext else {
            panic!("Google extension")
        };
        let normalized = ext.response_json_schema.as_ref().expect("canonical schema");
        if carrier == "responseSchema" {
            assert_eq!(normalized["type"], "object");
            assert_eq!(
                normalized["properties"]["answer"]["type"],
                serde_json::json!(["string", "null"])
            );
            assert!(normalized["properties"]["answer"].get("nullable").is_none());
            assert_eq!(normalized["properties"]["items"]["minItems"], 1);
            assert_eq!(
                normalized["properties"]["items"]["items"]["type"],
                "integer"
            );
        } else {
            assert_eq!(normalized, &schema);
        }
        let (body, _) = GoogleEncoder
            .encode_request(request)
            .expect("native roundtrip");
        assert_eq!(body["generationConfig"][carrier], schema);
    }
}

#[test]
fn decodes_and_roundtrips_native_function_calling_modes() {
    use crate::codec::google::gemini::encoder::GoogleEncoder;
    for config in [
        serde_json::json!({"mode": "AUTO"}),
        serde_json::json!({"mode": "NONE"}),
        serde_json::json!({"mode": "ANY"}),
        serde_json::json!({"mode": "ANY", "allowedFunctionNames": ["lookup"]}),
        serde_json::json!({"mode": "ANY", "allowedFunctionNames": ["lookup", "search"]}),
    ] {
        let request = GoogleDecoder.decode_request(serde_json::json!({"contents": [], "toolConfig": {"functionCallingConfig": config}})).expect("native tool choice");
        match config["mode"].as_str().unwrap() {
            "AUTO" => assert!(matches!(request.tool_choice, Some(ToolChoice::Auto))),
            "NONE" => assert!(matches!(request.tool_choice, Some(ToolChoice::None))),
            "ANY"
                if config["allowedFunctionNames"]
                    .as_array()
                    .is_some_and(|names| names.len() == 1) =>
            {
                assert!(
                    matches!(&request.tool_choice, Some(ToolChoice::Named { name }) if name == "lookup")
                )
            }
            _ => assert!(matches!(request.tool_choice, Some(ToolChoice::Required))),
        }
        let (body, _) = GoogleEncoder
            .encode_request(request)
            .expect("native tool choice roundtrip");
        assert_eq!(body["toolConfig"]["functionCallingConfig"], config);
    }
    let request = GoogleDecoder.decode_request(serde_json::json!({"contents": [], "toolConfig": {"functionCallingConfig": {"mode": "UNKNOWN"}}})).expect("unknown mode is not rejected");
    assert!(request.tool_choice.is_none());
}

#[test]
fn decodes_thought_signature_and_function_ids() {
    let request = GoogleDecoder
        .decode_request(serde_json::json!({
            "contents": [
                {
                    "role": "model",
                    "parts": [
                        {
                            "text": "checked the repository",
                            "thought": true,
                            "thoughtSignature": "opaque-reasoning"
                        },
                        {
                            "functionCall": {
                                "id": "call_read",
                                "name": "read",
                                "args": {"path": "Cargo.toml"}
                            }
                        }
                    ]
                },
                {
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "id": "call_read",
                            "name": "read",
                            "response": {"result": "workspace"}
                        }
                    }]
                }
            ]
        }))
        .expect("Gemini request");

    let MessageContent::Blocks(assistant_blocks) = &request.items[0].content else {
        panic!("assistant blocks");
    };
    assert!(matches!(
        &assistant_blocks[0],
        ContentBlock::Thinking {
            thinking,
            signature: Some(signature)
        } if thinking == "checked the repository" && signature == "opaque-reasoning"
    ));
    assert!(matches!(
        &assistant_blocks[1],
        ContentBlock::ToolUse { id, name, .. }
            if id == "call_read" && name == "read"
    ));

    let MessageContent::Blocks(tool_blocks) = &request.items[1].content else {
        panic!("tool blocks");
    };
    assert!(matches!(
        &tool_blocks[0],
        ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call_read"
    ));
    assert_eq!(request.items[1].tool_call_id.as_deref(), Some("call_read"));
}

#[test]
fn missing_function_ids_are_compact_deterministic_and_skip_supplied_ids() {
    let body = serde_json::json!({
        "contents": [
            {
                "role": "model",
                "parts": [
                    {"functionCall": {"name": "first", "args": {}}},
                    {"functionCall": {"id": "tc_1", "name": "external", "args": {}}}
                ]
            },
            {
                "role": "model",
                "parts": [
                    {"functionCall": {"name": "second", "args": {}}}
                ]
            }
        ]
    });

    let first = GoogleDecoder
        .decode_request(body.clone())
        .expect("Gemini request");
    let repeated = GoogleDecoder.decode_request(body).expect("Gemini request");
    let first_ids: Vec<&str> = first
        .items
        .iter()
        .flat_map(|item| item.tool_calls.iter().flatten())
        .map(|call| call.id.as_str())
        .collect();
    let repeated_ids: Vec<&str> = repeated
        .items
        .iter()
        .flat_map(|item| item.tool_calls.iter().flatten())
        .map(|call| call.id.as_str())
        .collect();

    assert_eq!(first_ids, ["tc_2", "tc_1", "tc_3"]);
    assert_eq!(repeated_ids, first_ids);
}

#[test]
fn include_thoughts_preserves_display_without_selecting_intensity() {
    for (include, display) in [(true, "summarized"), (false, "omitted")] {
        let request = GoogleDecoder
            .decode_request(serde_json::json!({
                "contents": [{"role": "user", "parts": [{"text": "reason"}]}],
                "generationConfig": {
                    "thinkingConfig": {"includeThoughts": include}
                }
            }))
            .expect("Gemini request");

        assert_eq!(request.reasoning.display.as_deref(), Some(display));
        assert_eq!(request.reasoning.level, None);
        assert!(!request.reasoning.enabled);
    }
}

#[test]
fn thinking_level_decodes_case_insensitively() {
    let request = GoogleDecoder
        .decode_request(serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": "reason"}]}],
            "generationConfig": {
                "thinkingConfig": {"thinkingLevel": "HIGH"}
            }
        }))
        .expect("Gemini request");

    assert_eq!(
        request.reasoning.level,
        Some(stravia_runtime_contract::thinking::ThinkingLevel::High)
    );
}

#[test]
fn unknown_thinking_level_is_rejected() {
    let error = GoogleDecoder
        .decode_request(serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": "reason"}]}],
            "generationConfig": {
                "thinkingConfig": {"thinkingLevel": "turbo"}
            }
        }))
        .expect_err("unknown thinkingLevel must fail");

    assert!(
        error
            .to_string()
            .contains("unsupported Gemini thinkingLevel")
    );
}
