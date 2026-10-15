use serde_json::json;
use stravia_runtime_contract::protocol::ir::AiRequest;
use stravia_runtime_contract::protocol::ir::request::{ToolChoice, ToolSpec};

fn declaration(name: &str, parameters: serde_json::Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: Some("Unavailable compatibility declaration; never call this tool.".into()),
        parameters,
        strict: None,
        defer_loading: None,
    }
}

/// 只补出站声明；本插件没有这些工具的执行能力。
pub(crate) fn prepare(request: &mut AiRequest) {
    request.stream.enabled = true;
    // 上游免费原生接口只允许 auto，即使客户端已经提供完整工具集也必须归一。
    request.tool_choice = Some(ToolChoice::Auto);

    let tools = request.tools.get_or_insert_with(Vec::new);
    let missing_bash = !tools.iter().any(|tool| tool.name == "bash");
    let missing_read = !tools.iter().any(|tool| tool.name == "read");
    let names = match (missing_bash, missing_read) {
        (true, true) => "bash`, `read",
        (true, false) => "bash",
        (false, true) => "read",
        (false, false) => return,
    };
    if missing_bash {
        // 空 schema 无法满足免费层门控，保留官方实际输入字段而不引入执行器。
        tools.push(declaration(
            "bash",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout": {
                        "type": "integer",
                        "exclusiveMinimum": 0,
                        "maximum": 9007199254740991u64
                    },
                    "workdir": {"type": "string"}
                },
                "required": ["command"]
            }),
        ));
    }
    if missing_read {
        tools.push(declaration(
            "read",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {
                    "filePath": {"type": "string"},
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 9007199254740991u64
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 9007199254740991u64
                    }
                },
                "required": ["filePath"]
            }),
        ));
    }

    const NOTICE: &str = "The following tools are compatibility declarations only, are unavailable in this environment, and must never be called: `";
    // 原有指令（包括空白）是客户端业务数据，不能 trim 或替换。
    let instructions = request.instructions.get_or_insert_with(String::new);
    let separator = if instructions.is_empty() { "" } else { "\n\n" };
    instructions.reserve(separator.len() + NOTICE.len() + names.len() + 2);
    instructions.push_str(separator);
    instructions.push_str(NOTICE);
    instructions.push_str(names);
    instructions.push_str("`.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use stravia_vendor_common::common;

    fn request() -> AiRequest {
        AiRequest::new("big-pickle", vec![])
    }

    fn client_tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: Some("Keep the client's business description.".into()),
            parameters: json!({
                "type": "object",
                "properties": {"customer": {"type": "string", "enum": ["Acme"]}},
                "required": ["customer"],
                "x-business": {"retention": 12}
            }),
            strict: Some(true),
            defer_loading: Some(true),
        }
    }

    #[test]
    fn client_definitions_survive_and_only_missing_names_are_forbidden() {
        for (provided, missing) in [("bash", "read"), ("read", "bash")] {
            let mut request = request();
            let original = vec![client_tool(provided), client_tool("business_tool")];
            request.tools = Some(original.clone());
            request.instructions = Some("  Preserve exactly.\n".into());
            prepare(&mut request);
            let tools = request.tools.as_ref().unwrap();
            assert_eq!(&tools[..original.len()], original.as_slice());
            assert_eq!(tools.len(), 3);
            assert_eq!(tools[2].name, missing);
            let instructions = request.instructions.as_deref().unwrap();
            assert!(instructions.starts_with("  Preserve exactly.\n\n\n"));
            assert!(instructions.contains(&format!("`{missing}`")));
            assert!(!instructions.contains(&format!("`{provided}`")));
        }
    }

    #[test]
    fn injection_preserves_empty_and_existing_instruction_prefixes() {
        for original in [None, Some(""), Some(" \t\n"), Some("System policy.")] {
            let mut request = request();
            request.instructions = original.map(str::to_owned);
            prepare(&mut request);
            let instructions = request.instructions.as_deref().unwrap();
            let prefix = original.unwrap_or_default();
            assert!(instructions.starts_with(prefix));
            if !prefix.is_empty() {
                assert!(instructions[prefix.len()..].starts_with("\n\n"));
            }
            assert!(instructions.contains("`bash`"));
            assert!(instructions.contains("`read`"));
            assert!(request.stream.enabled);
        }
    }

    #[test]
    fn complete_client_tools_leave_instructions_untouched() {
        for instructions in [None, Some(""), Some(" \t\n"), Some("Do business work.")] {
            let mut request = request();
            let original = vec![client_tool("bash"), client_tool("read")];
            request.tools = Some(original.clone());
            request.instructions = instructions.map(str::to_owned);
            prepare(&mut request);
            assert_eq!(request.tools.as_ref().unwrap(), &original);
            assert_eq!(request.instructions.as_deref(), instructions);
            assert!(request.stream.enabled);
        }
    }

    #[test]
    fn every_tool_choice_encodes_as_native_auto_with_or_without_injection() {
        let choices = [
            None,
            Some(ToolChoice::Auto),
            Some(ToolChoice::None),
            Some(ToolChoice::Required),
            Some(ToolChoice::Named {
                name: "bash".into(),
            }),
            Some(ToolChoice::Allowed {
                names: vec!["read".into()],
                required: true,
            }),
            Some(ToolChoice::Native(
                stravia_runtime_contract::protocol::ir::NativeToolKind::WebSearch,
            )),
        ];
        for choice in choices {
            for complete in [false, true] {
                let mut request = request();
                request.tool_choice = choice.clone();
                if complete {
                    request.tools = Some(vec![client_tool("bash"), client_tool("read")]);
                }
                prepare(&mut request);
                let encoded = common::encode_inference_request(
                    "openai-compatible/chat-completions/v1",
                    request,
                )
                .unwrap();
                assert_eq!(encoded.body["tool_choice"], "auto");
                assert_eq!(encoded.body["stream"], true);
            }
        }
    }

    #[test]
    fn injected_wire_schemas_accept_real_inputs_and_reject_invalid_inputs() {
        let mut request = request();
        prepare(&mut request);
        let encoded =
            common::encode_inference_request("openai-compatible/chat-completions/v1", request)
                .unwrap();
        let tools = encoded.body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        let schema = |name: &str| {
            &tools
                .iter()
                .find(|tool| tool["function"]["name"] == name)
                .unwrap()["function"]["parameters"]
        };
        let bash = jsonschema::validator_for(schema("bash")).expect("valid native bash schema");
        let read = jsonschema::validator_for(schema("read")).expect("valid native read schema");
        for validator in [&bash, &read] {
            assert!(!validator.is_valid(&json!({})));
            assert!(!validator.is_valid(&json!([])));
        }
        for input in [
            json!({"command": "pwd"}),
            json!({"command": "pwd", "timeout": 1, "workdir": "/workspace"}),
            json!({"command": "pwd", "timeout": 9007199254740991u64}),
        ] {
            assert!(bash.is_valid(&input), "{input}");
        }
        for input in [
            json!({"command": false}),
            json!({"command": "pwd", "timeout": 0}),
            json!({"command": "pwd", "timeout": -1}),
            json!({"command": "pwd", "timeout": 1.5}),
            json!({"command": "pwd", "timeout": 9007199254740992u64}),
            json!({"command": "pwd", "workdir": 42}),
        ] {
            assert!(!bash.is_valid(&input), "{input}");
        }
        assert!(read.is_valid(&json!({"filePath": "/workspace/input.txt"})));
        assert!(!read.is_valid(&json!({"filePath": 42})));
        for name in ["offset", "limit"] {
            for value in [json!(0), json!(1), json!(9007199254740991u64)] {
                let mut input = json!({"filePath": "/workspace/input.txt"});
                input[name] = value;
                assert!(read.is_valid(&input), "{input}");
            }
            for value in [
                json!(-1),
                json!(1.5),
                json!("1"),
                json!(9007199254740992u64),
            ] {
                let mut input = json!({"filePath": "/workspace/input.txt"});
                input[name] = value;
                assert!(!read.is_valid(&input), "{input}");
            }
        }
    }
}
