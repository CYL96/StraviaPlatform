use super::*;

fn request_with(extra: Value) -> Value {
    let mut request = serde_json::json!({
        "model": "model",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 1024
    });
    request
        .as_object_mut()
        .expect("request object")
        .extend(extra.as_object().expect("extra object").clone());
    request
}

#[test]
fn malformed_structured_output_and_controls_remain_decode_errors() {
    for extra in [
        serde_json::json!({"output_config": {"format": {"type": "json_schema", "schema": null}}}),
        serde_json::json!({"output_config": {"format": {"type": "json_schema", "schema": []}}}),
        serde_json::json!({"output_config": {"format": {"type": "json_schema"}}}),
        serde_json::json!({"output_config": {"format": {"type": "unknown", "schema": {}}}}),
        serde_json::json!({"tool_choice": {"type": "auto", "disable_parallel_tool_use": "false"}}),
        serde_json::json!({"stop_sequences": [1]}),
        serde_json::json!({"tools": [{"name": "read", "input_schema": {}, "strict": "true"}]}),
    ] {
        assert!(
            AnthropicDecoder
                .decode_request(request_with(extra.clone()))
                .is_err(),
            "malformed source controls must fail: {extra}"
        );
    }
}

#[test]
fn adaptive_thinking_uses_output_effort() {
    let request = AnthropicDecoder
        .decode_request(request_with(serde_json::json!({
            "thinking": {"type": "adaptive"},
            "output_config": {"effort": "medium"}
        })))
        .expect("adaptive thinking");

    assert!(request.reasoning.enabled);
    assert_eq!(request.reasoning.effort, Some(ReasoningEffort::Medium));
    assert_eq!(
        request.reasoning.level,
        Some(stravia_runtime_contract::thinking::ThinkingLevel::Medium)
    );
}

#[test]
fn output_effort_without_thinking_is_preserved() {
    let request = AnthropicDecoder
        .decode_request(request_with(serde_json::json!({
            "output_config": {"effort": "high"}
        })))
        .expect("output effort");

    assert!(request.reasoning.enabled);
    assert_eq!(request.reasoning.effort, Some(ReasoningEffort::High));
    assert_eq!(
        request.reasoning.level,
        Some(stravia_runtime_contract::thinking::ThinkingLevel::High)
    );
}

#[test]
fn explicit_effort_overrides_enabled_budget_and_disabled_thinking() {
    for thinking in [
        serde_json::json!({"type": "enabled", "budget_tokens": 1024, "display": "omitted"}),
        serde_json::json!({"type": "disabled", "display": "omitted"}),
    ] {
        let request = AnthropicDecoder
            .decode_request(request_with(serde_json::json!({
                "thinking": thinking,
                "output_config": {"effort": "max"}
            })))
            .unwrap();
        assert_eq!(
            request.reasoning.level,
            Some(stravia_runtime_contract::thinking::ThinkingLevel::Max)
        );
        assert!(request.reasoning.enabled);
        assert_eq!(request.reasoning.budget_tokens, None);
        assert_eq!(request.reasoning.display.as_deref(), Some("omitted"));
    }
}

#[test]
fn explicit_effort_does_not_accept_unknown_thinking_type() {
    assert!(
        AnthropicDecoder
            .decode_request(request_with(serde_json::json!({
                "thinking": {"type": "unknown"},
                "output_config": {"effort": "high"}
            })))
            .is_err()
    );
}

#[test]
fn same_protocol_round_trip_keeps_tool_strict_and_tool_result_is_error() {
    let body = serde_json::json!({
        "model": "model",
        "max_tokens": 1024,
        "tools": [
            {"name": "read", "description": "Read", "input_schema": {"type": "object"}, "strict": true},
            {"name": "grep", "description": "Grep", "input_schema": {"type": "object"}, "strict": false},
            {"name": "list", "input_schema": {"type": "object"}}
        ],
        "messages": [
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1", "name": "read", "input": {}}]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok", "is_error": false}
            ]}
        ]
    });

    let request = AnthropicDecoder.decode_request(body).expect("decode");
    let tools = request.tools.as_ref().expect("tools");
    assert_eq!(tools[0].strict, Some(true));
    assert_eq!(tools[1].strict, Some(false));
    assert_eq!(tools[2].strict, None);
    let (encoded, _) = crate::codec::anthropic::messages::encoder::AnthropicEncoder
        .encode_request(request)
        .expect("encode");

    assert_eq!(encoded["tools"][0]["strict"], true);
    assert_eq!(encoded["tools"][1]["strict"], false);
    assert!(encoded["tools"][2].get("strict").is_none());
    assert_eq!(encoded["messages"][2]["content"][0]["is_error"], false);
}
