//! 协议无关的原生工具声明；供应商私有选项不进入 canonical 请求。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::request::ToolSpec;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeTool {
    WebSearch(WebSearchTool),
    CodeExecution(CodeExecutionTool),
    ImageGeneration(ImageGenerationTool),
    ToolSearch(ToolSearchTool),
    Custom(CustomTool),
    Namespace(ToolNamespace),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeToolKind {
    WebSearch,
    CodeExecution,
    ImageGeneration,
    ToolSearch,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebSearchTool {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_domains: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocked_domains: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_location: Option<UserLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_size: Option<SearchContextSize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserLocation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchContextSize {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodeExecutionTool {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageGenerationTool {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolSearchTool {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grammar: Option<ToolGrammar>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolGrammar {
    pub syntax: String,
    pub definition: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolNamespace {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<NamespaceMember>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NamespaceMember {
    Function(ToolSpec),
    Custom(CustomTool),
}

fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_owned)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn location(value: &Value) -> Option<UserLocation> {
    value.as_object()?;
    Some(UserLocation {
        country: string(value, "country"),
        region: string(value, "region"),
        city: string(value, "city"),
        timezone: string(value, "timezone"),
    })
}

fn context_size(value: &Value) -> Option<SearchContextSize> {
    match value.get("search_context_size")?.as_str()? {
        "low" => Some(SearchContextSize::Low),
        "medium" => Some(SearchContextSize::Medium),
        "high" => Some(SearchContextSize::High),
        _ => None,
    }
}

fn web_search(value: &Value) -> WebSearchTool {
    WebSearchTool {
        max_uses: value
            .get("max_uses")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        allowed_domains: strings(value.get("allowed_domains")),
        blocked_domains: strings(value.get("blocked_domains")),
        user_location: value.get("user_location").and_then(location),
        context_size: context_size(value),
    }
}

fn custom(value: &Value) -> Option<CustomTool> {
    let grammar = value
        .get("format")
        .filter(|format| format.get("type").and_then(Value::as_str) == Some("grammar"))
        .and_then(|format| {
            Some(ToolGrammar {
                syntax: string(format, "syntax")?,
                definition: string(format, "definition")?,
            })
        });
    Some(CustomTool {
        name: string(value, "name")?,
        description: string(value, "description"),
        defer_loading: value.get("defer_loading").and_then(Value::as_bool),
        grammar,
    })
}

/// 从 Responses 工具对象提取可统一的意图，未知工具不透传。
pub fn native_tool_from_responses(value: &Value) -> Option<NativeTool> {
    match value.get("type")?.as_str()? {
        "web_search"
        | "web_search_preview"
        | "web_search_2025_08_26"
        | "web_search_preview_2025_03_11" => {
            let mut tool = web_search(value);
            tool.max_uses = None;
            tool.blocked_domains.clear();
            tool.allowed_domains = strings(
                value
                    .get("filters")
                    .and_then(|filters| filters.get("allowed_domains")),
            );
            Some(NativeTool::WebSearch(tool))
        }
        "code_interpreter" => Some(NativeTool::CodeExecution(CodeExecutionTool {
            container: string(value, "container"),
        })),
        "image_generation" => Some(NativeTool::ImageGeneration(ImageGenerationTool {
            action: string(value, "action"),
            output_format: string(value, "output_format"),
            size: string(value, "size"),
            quality: string(value, "quality"),
        })),
        "tool_search" => Some(NativeTool::ToolSearch(ToolSearchTool {
            execution: string(value, "execution"),
            description: string(value, "description"),
            parameters: value
                .get("parameters")
                .filter(|value| !value.is_null())
                .cloned(),
        })),
        "custom" => custom(value).map(NativeTool::Custom),
        "namespace" => Some(NativeTool::Namespace(ToolNamespace {
            name: string(value, "name")?,
            description: string(value, "description"),
            tools: value
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|member| match member.get("type")?.as_str()? {
                    "function" => Some(NamespaceMember::Function(ToolSpec {
                        name: string(member, "name")?,
                        description: string(member, "description"),
                        parameters: member
                            .get("parameters")
                            .cloned()
                            .unwrap_or_else(|| json!({})),
                        strict: member.get("strict").and_then(Value::as_bool),
                        defer_loading: member.get("defer_loading").and_then(Value::as_bool),
                    })),
                    "custom" => custom(member).map(NamespaceMember::Custom),
                    _ => None,
                })
                .collect(),
        })),
        _ => None,
    }
}

fn object<T: Serialize>(value: &T) -> Map<String, Value> {
    // 本模块只序列化固定结构；它们没有可能失败的 map key 或自定义序列化器。
    match serde_json::to_value(value).expect("native tool structures serialize to JSON") {
        Value::Object(fields) => fields,
        _ => unreachable!("native tool structures are objects"),
    }
}

fn typed(mut fields: Map<String, Value>, kind: &str) -> Value {
    fields.insert("type".into(), Value::String(kind.into()));
    Value::Object(fields)
}

fn custom_json(tool: &CustomTool) -> Value {
    let mut fields = object(tool);
    fields.remove("grammar");
    if let Some(grammar) = &tool.grammar {
        fields.insert("format".into(), typed(object(grammar), "grammar"));
    }
    typed(fields, "custom")
}

fn search_options(tool: &WebSearchTool) -> Map<String, Value> {
    let mut fields = Map::new();
    if let Some(context_size) = tool.context_size {
        fields.insert(
            "search_context_size".into(),
            match context_size {
                SearchContextSize::Low => "low",
                SearchContextSize::Medium => "medium",
                SearchContextSize::High => "high",
            }
            .into(),
        );
    }
    fields
}

/// 输出只含 Responses 官方工具字段，不恢复已丢弃的供应商选项。
pub fn native_tool_to_responses(tool: &NativeTool) -> Option<Value> {
    Some(match tool {
        NativeTool::WebSearch(tool) => {
            let mut fields = search_options(tool);
            if !tool.allowed_domains.is_empty() {
                fields.insert(
                    "filters".into(),
                    json!({"allowed_domains": tool.allowed_domains}),
                );
            }
            if let Some(location) = &tool.user_location {
                fields.insert(
                    "user_location".into(),
                    typed(object(location), "approximate"),
                );
            }
            typed(fields, "web_search")
        }
        NativeTool::CodeExecution(tool) => json!({"type": "code_interpreter", "container":
            tool.container.as_ref().map_or_else(|| json!({"type": "auto"}), |id| json!(id))}),
        NativeTool::ImageGeneration(tool) => typed(object(tool), "image_generation"),
        NativeTool::ToolSearch(tool) => typed(object(tool), "tool_search"),
        NativeTool::Custom(tool) => custom_json(tool),
        NativeTool::Namespace(tool) => {
            let mut fields = Map::new();
            fields.insert("name".into(), tool.name.clone().into());
            if let Some(description) = &tool.description {
                fields.insert("description".into(), description.clone().into());
            }
            // 协议要求 tools 数组，即使 canonical 的空集合在快照中省略。
            fields.insert(
                "tools".into(),
                Value::Array(
                    tool.tools
                        .iter()
                        .map(|member| match member {
                            NamespaceMember::Function(tool) => typed(object(tool), "function"),
                            NamespaceMember::Custom(tool) => custom_json(tool),
                        })
                        .collect(),
                ),
            );
            typed(fields, "namespace")
        }
    })
}

pub fn native_tool_from_anthropic(value: &Value) -> Option<NativeTool> {
    let kind = value.get("type")?.as_str()?;
    if kind.starts_with("web_search_") {
        let mut tool = web_search(value);
        tool.context_size = None;
        Some(NativeTool::WebSearch(tool))
    } else if kind.starts_with("code_execution_") {
        // container 是请求顶层字段，由调用方合并，不能从工具内的私有字段导入。
        Some(NativeTool::CodeExecution(CodeExecutionTool::default()))
    } else {
        None
    }
}

pub fn native_tool_to_anthropic(tool: &NativeTool) -> Option<Value> {
    match tool {
        NativeTool::WebSearch(tool) => {
            let mut fields = object(tool);
            fields.remove("context_size");
            if let Some(location) = &tool.user_location {
                fields.insert(
                    "user_location".into(),
                    typed(object(location), "approximate"),
                );
            }
            fields.insert("name".into(), "web_search".into());
            Some(typed(fields, "web_search_20250305"))
        }
        NativeTool::CodeExecution(_) => {
            // 当前官方代码执行版本与仓库 Claude Code 原生投影保持一致。
            Some(json!({"type": "code_execution_20260120", "name": "code_execution"}))
        }
        _ => None,
    }
}

pub fn native_tool_from_gemini(value: &Value) -> Option<NativeTool> {
    if let Some(search) = value.get("googleSearch").filter(|value| value.is_object()) {
        Some(NativeTool::WebSearch(WebSearchTool {
            blocked_domains: strings(search.get("excludeDomains")),
            ..WebSearchTool::default()
        }))
    } else if value
        .get("googleSearchRetrieval")
        .is_some_and(Value::is_object)
    {
        Some(NativeTool::WebSearch(WebSearchTool::default()))
    } else if value.get("codeExecution").is_some_and(Value::is_object) {
        Some(NativeTool::CodeExecution(CodeExecutionTool::default()))
    } else {
        None
    }
}

pub fn native_tool_to_gemini(tool: &NativeTool) -> Option<Value> {
    match tool {
        NativeTool::WebSearch(tool) => Some(if tool.blocked_domains.is_empty() {
            json!({"googleSearch": {}})
        } else {
            json!({"googleSearch": {"excludeDomains": tool.blocked_domains}})
        }),
        NativeTool::CodeExecution(_) => Some(json!({"codeExecution": {}})),
        _ => None,
    }
}

pub fn native_tool_from_chat_web_search_options(value: &Value) -> Option<NativeTool> {
    value.as_object()?;
    Some(NativeTool::WebSearch(WebSearchTool {
        context_size: context_size(value),
        user_location: value
            .get("user_location")
            .and_then(|location| location.get("approximate"))
            .and_then(location),
        ..WebSearchTool::default()
    }))
}

pub fn native_tool_to_chat_web_search_options(tool: &NativeTool) -> Option<Value> {
    let NativeTool::WebSearch(tool) = tool else {
        return None;
    };
    let mut fields = search_options(tool);
    if let Some(location) = &tool.user_location {
        fields.insert(
            "user_location".into(),
            json!({"type": "approximate", "approximate": location}),
        );
    }
    Some(Value::Object(fields))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_roundtrips_all_supported_variants() {
        let values = [
            json!({"type":"web_search","filters":{"allowed_domains":["example.com"]},"search_context_size":"high","user_location":{"type":"approximate","city":"Paris","timezone":"Europe/Paris"}}),
            json!({"type":"code_interpreter","container":"cntr_123"}),
            json!({"type":"code_interpreter","container":{"type":"auto"}}),
            json!({"type":"image_generation","action":"edit","output_format":"png","size":"1024x1024","quality":"high"}),
            json!({"type":"tool_search","execution":"client","description":"find","parameters":{"type":"object"}}),
            json!({"type":"custom","name":"grammar","defer_loading":false,"format":{"type":"grammar","syntax":"lark","definition":"start: /.+/"}}),
            json!({"type":"namespace","name":"tools","description":"business","tools":[{"type":"function","name":"lookup","parameters":{"type":"object"},"strict":false,"defer_loading":true},{"type":"custom","name":"text"}]}),
        ];
        for value in values {
            let tool = native_tool_from_responses(&value).unwrap();
            assert_eq!(native_tool_to_responses(&tool), Some(value));
            assert_eq!(
                serde_json::from_value::<NativeTool>(serde_json::to_value(&tool).unwrap()).unwrap(),
                tool
            );
        }
    }

    #[test]
    fn anthropic_roundtrips_and_uses_current_code_execution_version() {
        let values = [
            json!({"type":"web_search_20250305","name":"web_search","max_uses":0,"allowed_domains":["example.com"],"blocked_domains":["blocked.test"],"user_location":{"type":"approximate","country":"FR","region":"IDF","city":"Paris","timezone":"Europe/Paris"}}),
            json!({"type":"code_execution_20260120","name":"code_execution"}),
        ];
        for value in values {
            let tool = native_tool_from_anthropic(&value).unwrap();
            assert_eq!(native_tool_to_anthropic(&tool), Some(value));
        }
        assert_eq!(
            native_tool_to_anthropic(
                &native_tool_from_anthropic(&json!({"type":"code_execution_20250522"})).unwrap()
            )
            .unwrap()["type"],
            "code_execution_20260120"
        );
    }

    #[test]
    fn gemini_roundtrips_and_normalizes_legacy_search() {
        for value in [
            json!({"googleSearch":{}}),
            json!({"googleSearch":{"excludeDomains":["blocked.test"]}}),
            json!({"codeExecution":{}}),
        ] {
            let tool = native_tool_from_gemini(&value).unwrap();
            assert_eq!(native_tool_to_gemini(&tool), Some(value));
        }
        let tool = native_tool_from_gemini(
            &json!({"googleSearchRetrieval":{"dynamicRetrievalConfig":{"mode":"MODE_DYNAMIC"}}}),
        )
        .unwrap();
        assert_eq!(
            native_tool_to_gemini(&tool),
            Some(json!({"googleSearch":{}}))
        );
    }

    #[test]
    fn chat_search_roundtrips_without_unknown_fields() {
        let value = json!({"search_context_size":"low","user_location":{"type":"approximate","approximate":{"city":"Tokyo","country":"JP"}}});
        let tool = native_tool_from_chat_web_search_options(&value).unwrap();
        assert_eq!(native_tool_to_chat_web_search_options(&tool), Some(value));
        let tool = native_tool_from_chat_web_search_options(&json!({"private":true,"user_location":{"type":"approximate","approximate":{"city":"Tokyo","private":true}}})).unwrap();
        assert_eq!(
            native_tool_to_chat_web_search_options(&tool),
            Some(json!({"user_location":{"type":"approximate","approximate":{"city":"Tokyo"}}}))
        );
        assert!(native_tool_from_chat_web_search_options(&Value::Null).is_none());
    }

    #[test]
    fn unsupported_tools_are_not_imported_or_projected() {
        for kind in [
            "file_search",
            "mcp",
            "computer",
            "computer_use_preview",
            "local_shell",
            "shell",
            "apply_patch",
            "bash",
            "text_editor",
            "memory",
            "web_fetch",
            "function",
        ] {
            assert!(native_tool_from_responses(&json!({"type":kind})).is_none());
            assert!(native_tool_from_anthropic(&json!({"type":kind})).is_none());
        }
        assert!(native_tool_from_gemini(&json!({"urlContext":{}})).is_none());
        for tool in [
            NativeTool::ImageGeneration(ImageGenerationTool::default()),
            NativeTool::ToolSearch(ToolSearchTool::default()),
            NativeTool::Custom(CustomTool {
                name: "text".into(),
                description: None,
                defer_loading: None,
                grammar: None,
            }),
            NativeTool::Namespace(ToolNamespace {
                name: "tools".into(),
                description: None,
                tools: vec![],
            }),
        ] {
            assert!(native_tool_to_responses(&tool).is_some());
            assert!(native_tool_to_anthropic(&tool).is_none());
            assert!(native_tool_to_gemini(&tool).is_none());
            assert!(native_tool_to_chat_web_search_options(&tool).is_none());
        }
        assert!(
            native_tool_to_chat_web_search_options(&NativeTool::CodeExecution(
                CodeExecutionTool::default()
            ))
            .is_none()
        );
    }

    #[test]
    fn defaults_omit_empty_fields_and_protocol_options_do_not_leak() {
        let tool: NativeTool = serde_json::from_value(json!({"type":"web_search"})).unwrap();
        assert_eq!(
            serde_json::to_value(&tool).unwrap(),
            json!({"type":"web_search"})
        );
        assert_eq!(
            native_tool_to_responses(&tool),
            Some(json!({"type":"web_search"}))
        );
        assert_eq!(
            native_tool_to_anthropic(&tool),
            Some(json!({"type":"web_search_20250305","name":"web_search"}))
        );
        for kind in [
            "web_search_preview",
            "web_search_preview_2025_03_11",
            "web_search_2025_08_26",
        ] {
            let tool = native_tool_from_responses(&json!({"type":kind,"private":true,"max_uses":10,"blocked_domains":["private"],"filters":{"allowed_domains":["example.com",1]}})).unwrap();
            assert_eq!(
                native_tool_to_responses(&tool),
                Some(json!({"type":"web_search","filters":{"allowed_domains":["example.com"]}}))
            );
        }
        let tool = native_tool_from_responses(&json!({"type":"namespace","name":"business","tools":[{"type":"web_search"},{"type":"namespace","name":"nested"},{"type":"custom","name":"text","private":true}]})).unwrap();
        assert_eq!(
            native_tool_to_responses(&tool),
            Some(
                json!({"type":"namespace","name":"business","tools":[{"type":"custom","name":"text"}]})
            )
        );
    }

    #[test]
    fn cross_protocol_projection_keeps_only_expressible_controls() {
        let tool = NativeTool::WebSearch(WebSearchTool {
            max_uses: Some(3),
            allowed_domains: vec!["allowed.test".into()],
            blocked_domains: vec!["blocked.test".into()],
            user_location: Some(UserLocation {
                city: Some("Paris".into()),
                ..UserLocation::default()
            }),
            context_size: Some(SearchContextSize::Medium),
        });
        assert_eq!(
            native_tool_to_responses(&tool),
            Some(json!({
                "type":"web_search","filters":{"allowed_domains":["allowed.test"]},
                "user_location":{"type":"approximate","city":"Paris"},"search_context_size":"medium"
            }))
        );
        assert_eq!(
            native_tool_to_anthropic(&tool),
            Some(json!({
                "type":"web_search_20250305","name":"web_search","max_uses":3,
                "allowed_domains":["allowed.test"],"blocked_domains":["blocked.test"],
                "user_location":{"type":"approximate","city":"Paris"}
            }))
        );
        assert_eq!(
            native_tool_to_gemini(&tool),
            Some(json!({"googleSearch":{"excludeDomains":["blocked.test"]}}))
        );
        assert_eq!(
            native_tool_to_chat_web_search_options(&tool),
            Some(json!({
                "search_context_size":"medium","user_location":{"type":"approximate","approximate":{"city":"Paris"}}
            }))
        );
        let code = NativeTool::CodeExecution(CodeExecutionTool {
            container: Some("cntr_123".into()),
        });
        assert_eq!(
            native_tool_to_anthropic(&code),
            Some(json!({"type":"code_execution_20260120","name":"code_execution"}))
        );
        assert_eq!(
            native_tool_to_gemini(&code),
            Some(json!({"codeExecution":{}}))
        );
    }

    #[test]
    fn malformed_optional_controls_do_not_replace_valid_tool_intent() {
        let tool = native_tool_from_responses(&json!({
            "type":"web_search","search_context_size":"unknown","user_location":null,
            "filters":{"allowed_domains":[false,"valid.test",null]}
        }))
        .unwrap();
        assert_eq!(
            native_tool_to_responses(&tool),
            Some(json!({
                "type":"web_search","filters":{"allowed_domains":["valid.test"]}
            }))
        );
        for value in [Value::Null, json!([]), json!({}), json!({"type":1})] {
            assert!(native_tool_from_responses(&value).is_none());
            assert!(native_tool_from_anthropic(&value).is_none());
            assert!(native_tool_from_gemini(&value).is_none());
        }
        assert!(native_tool_from_responses(&json!({"type":"custom"})).is_none());
        assert!(native_tool_from_responses(&json!({"type":"namespace"})).is_none());
        assert!(
            native_tool_from_gemini(&json!({"googleSearch":null,"codeExecution":false})).is_none()
        );
    }
}
