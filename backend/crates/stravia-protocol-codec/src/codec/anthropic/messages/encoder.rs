use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use http::header::{HeaderMap, HeaderValue};
use serde_json::Value;

use stravia_runtime_contract::protocol::ir::AiRequest;
use stravia_runtime_contract::protocol::ir::request::AiItem;
use stravia_runtime_contract::protocol::ir::request::ContentBlock;
use stravia_runtime_contract::protocol::ir::request::MediaSource;
use stravia_runtime_contract::protocol::ir::request::MessageContent;
use stravia_runtime_contract::protocol::ir::request::Role;
use stravia_runtime_contract::protocol::ir::request::ToolChoice;
use stravia_runtime_contract::protocol::ir::request::ToolResultContentKind;

pub struct AnthropicEncoder;

impl AnthropicEncoder {
    pub(crate) fn encode_request(&self, mut req: AiRequest) -> Result<(Value, HeaderMap)> {
        let ingress = &mut req.meta.vendor.ingress;
        let supplied_tool_ids = if ingress.contains_key("__anthropic_raw_messages") {
            HashSet::new()
        } else {
            supplied_anthropic_tool_ids(&req.items)
        };

        // ── System ────────────────────────────────────────────────────────────
        // Prefer __anthropic_raw_system (preserves cache_control) if present.
        let system_val: Option<Value> = if let Some(v) = ingress.remove("__anthropic_raw_system") {
            Some(v)
        } else {
            let mut system_text = req.instructions.take().unwrap_or_default();
            for msg in &mut req.items {
                if matches!(msg.role, Role::System | Role::Developer) {
                    if !system_text.is_empty() {
                        system_text.push('\n');
                    }
                    append_system_content(
                        &mut system_text,
                        std::mem::replace(&mut msg.content, MessageContent::Blocks(Vec::new())),
                    );
                }
            }
            if system_text.is_empty() {
                None
            } else {
                Some(Value::String(system_text))
            }
        };

        // ── Messages ──────────────────────────────────────────────────────────
        // Prefer __anthropic_raw_messages (preserves cache_control / exotic
        // blocks) if present; otherwise reconstruct from Message.
        let messages_val: Value = if let Some(v) = ingress.remove("__anthropic_raw_messages") {
            project_native_anthropic_media(v)
        } else {
            let mut generated_tool_id_seq = 0;
            let mut raw_messages = Vec::new();
            for msg in req.items {
                if matches!(msg.role, Role::System | Role::Developer) {
                    continue;
                }
                raw_messages.push(encode_message(
                    msg,
                    &mut generated_tool_id_seq,
                    &supplied_tool_ids,
                )?);
            }
            Value::Array(normalize_anthropic_messages(raw_messages))
        };

        let max_tokens = req.generation.max_tokens.unwrap_or(4096);

        let mut body = object([
            ("model", Value::String(req.model)),
            ("messages", messages_val),
            ("max_tokens", max_tokens.into()),
            ("stream", Value::Bool(req.stream.enabled)),
        ]);

        let obj = body.as_object_mut().unwrap();

        if let Some(sv) = system_val {
            obj.insert("system".into(), sv);
        }
        if let Some(t) = req.generation.temperature {
            obj.insert("temperature".into(), t.into());
        }
        if let Some(p) = req.generation.top_p {
            obj.insert("top_p".into(), p.into());
        }

        // ── Tools ─────────────────────────────────────────────────────────────
        // Prefer raw tools (preserves cache_control) if present.
        if let Some(raw_tools) = ingress.remove("__anthropic_raw_tools") {
            obj.insert("tools".into(), raw_tools);
        } else if let Some(tools) = req.tools {
            let tools_val: Vec<Value> = tools
                .into_iter()
                .map(|t| {
                    if let Some(builtin_type) = t.name.strip_prefix("__builtin__") {
                        let mut entry = serde_json::json!({
                            "type": builtin_type,
                            "name": builtin_type,
                        });
                        if let Some(desc) = t.description {
                            entry
                                .as_object_mut()
                                .unwrap()
                                .insert("description".into(), Value::String(desc));
                        }
                        entry
                    } else {
                        let mut parameters = t.parameters;
                        let strict = t
                            .strict
                            .filter(|strict| !*strict || !schema_has_open_object(&parameters));
                        if strict == Some(true) {
                            omit_unsupported_schema_constraints(&mut parameters);
                        }
                        let mut entry = object([
                            ("name", Value::String(t.name)),
                            (
                                "description",
                                t.description.map(Value::String).unwrap_or(Value::Null),
                            ),
                            ("input_schema", parameters),
                        ]);
                        if let Some(strict) = strict {
                            entry
                                .as_object_mut()
                                .unwrap()
                                .insert("strict".into(), strict.into());
                        }
                        entry
                    }
                })
                .collect();
            obj.insert("tools".into(), Value::Array(tools_val));
        }

        // ── Tool choice ───────────────────────────────────────────────────────
        if let Some(tc) = req.tool_choice {
            let raw = tool_choice_to_value_raw(tc);
            if let Some(mapped) = map_tool_choice_for_anthropic(&raw) {
                obj.insert("tool_choice".into(), mapped);
            }
        }
        if let Some(disabled) = req
            .disable_parallel_tool_calls
            .or_else(|| req.parallel_tool_calls.map(|parallel| !parallel))
        {
            let choice = obj
                .entry("tool_choice")
                .or_insert_with(|| serde_json::json!({"type": "auto"}));
            if choice["type"] != "none" {
                choice
                    .as_object_mut()
                    .unwrap()
                    .insert("disable_parallel_tool_use".into(), disabled.into());
            }
        }
        if let Some(stop) = req.generation.stop {
            obj.insert("stop_sequences".into(), serde_json::to_value(stop)?);
        }

        let mut output_config = serde_json::Map::new();
        if let Some(stravia_runtime_contract::protocol::ir::ResponseFormat::JsonSchema {
            mut schema,
            ..
        }) = req.response_format
            && schema.is_object()
            && !schema_has_open_object(&schema)
        {
            omit_unsupported_schema_constraints(&mut schema);
            output_config.insert(
                "format".into(),
                serde_json::json!({"type": "json_schema", "schema": schema}),
            );
        }
        // 原生扩展优先于通用 format；effort 只合并到对应键，不能覆盖 format。
        let native_output_config = match req.ext {
            Some(stravia_runtime_contract::protocol::ir::ProtocolExt::Anthropic(ext)) => {
                ext.output_config
            }
            _ => None,
        }
        .or_else(|| ingress.remove("__anthropic_output_config"));
        if let Some(native) = native_output_config {
            let Value::Object(native) = native else {
                anyhow::bail!("Anthropic output_config must be an object");
            };
            output_config.extend(native);
        }

        if let Some(control) = req.reasoning.target_control.as_ref() {
            let thinking = match control {
                stravia_runtime_contract::thinking::TargetThinkingControl::Budget { value } => {
                    Some(serde_json::json!({"type": "enabled", "budget_tokens": value}))
                }
                stravia_runtime_contract::thinking::TargetThinkingControl::Enabled => {
                    Some(serde_json::json!({"type": "enabled"}))
                }
                stravia_runtime_contract::thinking::TargetThinkingControl::Disabled => {
                    Some(serde_json::json!({"type": "disabled"}))
                }
                stravia_runtime_contract::thinking::TargetThinkingControl::Effort { value } => {
                    output_config
                        .entry("effort")
                        .or_insert_with(|| Value::String(value.clone()));
                    Some(serde_json::json!({"type": "adaptive"}))
                }
                stravia_runtime_contract::thinking::TargetThinkingControl::Hidden => None,
            };
            if let Some(thinking) = thinking {
                obj.insert("thinking".into(), thinking);
            }
        }

        if !output_config.is_empty() {
            obj.insert("output_config".into(), Value::Object(output_config));
        }

        // ── Extra fields ──────────────────────────────────────────────────────
        if let Some(v) = ingress.remove("__anthropic_context_management") {
            obj.insert("context_management".into(), v);
        }
        for key in &[
            "__anthropic_container",
            "__anthropic_service_tier",
            "__anthropic_metadata",
            "__anthropic_stop_sequences",
            "__anthropic_top_k",
        ] {
            if let Some(v) = ingress.remove(*key) {
                let field_name = key.trim_start_matches("__anthropic_");
                obj.insert(field_name.into(), v);
            }
        }

        validate_anthropic_payload(&body)?;

        let mut headers = HeaderMap::new();
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));

        Ok((body, headers))
    }

    pub(crate) fn egress_path(&self, _model: &str, _stream: bool) -> String {
        "/v1/messages".to_string()
    }
}

// ── tool_choice helpers ───────────────────────────────────────────────────────

fn object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
    )
}

fn text_block(text: String) -> Value {
    object([
        ("type", Value::String("text".into())),
        ("text", Value::String(text)),
    ])
}

fn append_system_content(target: &mut String, content: MessageContent) {
    let mut append = |text| {
        let text = Arc::unwrap_or_clone(text);
        if target.is_empty() {
            *target = text;
        } else {
            target.push_str(&text);
        }
    };
    match content {
        MessageContent::Text(text) => append(text),
        MessageContent::Blocks(blocks) => {
            for block in blocks {
                if let ContentBlock::Text { text, .. } = block {
                    append(text);
                }
            }
        }
    }
}

// 只删目标不能表达的约束，不收紧对象、不改写枚举值或 schema 的数据字段。
fn omit_unsupported_schema_constraints(schema: &mut Value) {
    let mut recursive_refs = HashSet::new();
    collect_recursive_schema_refs(schema, schema, &mut recursive_refs);
    omit_schema_constraints(schema, &recursive_refs);
}

fn schema_has_open_object(schema: &Value) -> bool {
    let object_type = schema.get("type").is_some_and(|kind| {
        kind == "object"
            || kind
                .as_array()
                .is_some_and(|types| types.iter().any(|kind| kind == "object"))
    });
    if (object_type
        || schema.get("properties").is_some()
        || schema.get("additionalProperties").is_some())
        && schema.get("additionalProperties") != Some(&Value::Bool(false))
    {
        return true;
    }
    let mut open = false;
    schema_children(schema, |child| {
        if !open {
            open = schema_has_open_object(child);
        }
    });
    open
}

fn schema_children<'a>(schema: &'a Value, mut visit: impl FnMut(&'a Value)) {
    for key in ["properties", "$defs", "$def", "definitions"] {
        if let Some(children) = schema.get(key).and_then(Value::as_object) {
            for child in children.values() {
                visit(child);
            }
        }
    }
    if let Some(items) = schema.get("items") {
        visit(items);
    }
    for key in ["anyOf", "allOf", "oneOf", "prefixItems"] {
        if let Some(children) = schema.get(key).and_then(Value::as_array) {
            for child in children {
                visit(child);
            }
        }
    }
}

fn schema_has_reference_cycle<'a>(
    root: &'a Value,
    schema: &'a Value,
    active: &mut HashSet<&'a str>,
) -> bool {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && let Some(pointer) = reference.strip_prefix('#')
        && let Some(target) = root.pointer(pointer)
    {
        if !active.insert(reference) {
            return true;
        }
        let recursive = schema_has_reference_cycle(root, target, active);
        active.remove(reference);
        if recursive {
            return true;
        }
    }
    let mut recursive = false;
    schema_children(schema, |child| {
        if !recursive {
            recursive = schema_has_reference_cycle(root, child, active);
        }
    });
    recursive
}

fn collect_recursive_schema_refs(
    root: &Value,
    schema: &Value,
    recursive_refs: &mut HashSet<String>,
) {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
        && reference.starts_with('#')
        && schema_has_reference_cycle(root, schema, &mut HashSet::new())
    {
        recursive_refs.insert(reference.to_owned());
    }
    schema_children(schema, |child| {
        collect_recursive_schema_refs(root, child, recursive_refs)
    });
}

fn pattern_has_unsupported_tokens(pattern: &str) -> bool {
    let bytes = pattern.as_bytes();
    let mut index = 0;
    let mut in_class = false;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            if let Some(next) = bytes.get(index + 1)
                && !in_class
                && (matches!(*next, b'b' | b'B' | b'1'..=b'9'))
            {
                return true;
            }
            index += 2;
            continue;
        }
        if bytes[index] == b'[' {
            in_class = true;
        }
        if bytes[index] == b']' {
            in_class = false;
        }
        if !in_class && bytes[index] == b'(' {
            let suffix = &bytes[index..];
            if suffix.starts_with(b"(?=")
                || suffix.starts_with(b"(?!")
                || suffix.starts_with(b"(?<=")
                || suffix.starts_with(b"(?<!")
            {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn omit_schema_constraints(schema: &mut Value, recursive_refs: &HashSet<String>) {
    let Some(schema) = schema.as_object_mut() else {
        return;
    };
    for key in [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "minLength",
        "maxLength",
        "maxItems",
        "uniqueItems",
        "contains",
        "minContains",
        "maxContains",
    ] {
        schema.remove(key);
    }
    if schema
        .get("minItems")
        .is_some_and(|value| !matches!(value.as_u64(), Some(0 | 1)))
    {
        schema.remove("minItems");
    }
    if schema
        .get("additionalProperties")
        .is_some_and(|value| value != &Value::Bool(false))
    {
        schema.remove("additionalProperties");
    }
    if schema
        .get("$ref")
        .and_then(Value::as_str)
        .is_some_and(|reference| !reference.starts_with('#') || recursive_refs.contains(reference))
    {
        schema.remove("$ref");
    }
    if schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|values| {
            values
                .iter()
                .any(|value| value.is_object() || value.is_array())
        })
    {
        schema.remove("enum");
    }
    if schema
        .get("format")
        .and_then(Value::as_str)
        .is_some_and(|format| {
            !matches!(
                format,
                "date-time"
                    | "time"
                    | "date"
                    | "duration"
                    | "email"
                    | "hostname"
                    | "uri"
                    | "ipv4"
                    | "ipv6"
                    | "uuid"
            )
        })
    {
        schema.remove("format");
    }
    if schema
        .get("pattern")
        .and_then(Value::as_str)
        .is_some_and(pattern_has_unsupported_tokens)
    {
        schema.remove("pattern");
    }
    for key in ["properties", "$defs", "$def", "definitions"] {
        if let Some(children) = schema.get_mut(key).and_then(Value::as_object_mut) {
            for child in children.values_mut() {
                omit_schema_constraints(child, recursive_refs);
            }
        }
    }
    if let Some(items) = schema.get_mut("items") {
        omit_schema_constraints(items, recursive_refs);
    }
    for key in ["anyOf", "allOf", "oneOf", "prefixItems"] {
        if let Some(children) = schema.get_mut(key).and_then(Value::as_array_mut) {
            if key == "allOf" {
                for child in children.iter_mut() {
                    if let Some(child) = child.as_object_mut() {
                        child.remove("$ref");
                    }
                }
            }
            for child in children {
                omit_schema_constraints(child, recursive_refs);
            }
        }
    }
}

fn tool_choice_to_value_raw(tc: ToolChoice) -> Value {
    match tc {
        ToolChoice::Auto => Value::String("auto".into()),
        ToolChoice::None => Value::String("none".into()),
        ToolChoice::Required => Value::String("required".into()),
        ToolChoice::Named { name } => object([
            ("type", Value::String("tool".into())),
            ("name", Value::String(name)),
        ]),
        ToolChoice::Raw(v) => v,
    }
}

fn map_tool_choice_for_anthropic(raw: &Value) -> Option<Value> {
    if let Some(s) = raw.as_str() {
        return match s {
            "auto" => Some(serde_json::json!({ "type": "auto" })),
            "required" => Some(serde_json::json!({ "type": "any" })),
            "none" => Some(serde_json::json!({ "type": "none" })),
            _ => None,
        };
    }

    let obj = raw.as_object()?;
    let kind = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let disable_parallel = obj
        .get("disable_parallel_tool_use")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut result = match kind {
        "auto" => serde_json::json!({ "type": "auto" }),
        "required" | "any" => serde_json::json!({ "type": "any" }),
        "none" => serde_json::json!({ "type": "none" }),
        "tool" => {
            let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() {
                return None;
            }
            serde_json::json!({ "type": "tool", "name": name })
        }
        "function" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    obj.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                })
                .unwrap_or("");
            if name.is_empty() {
                return None;
            }
            serde_json::json!({ "type": "tool", "name": name })
        }
        _ => return None,
    };

    if disable_parallel && kind != "none" {
        result
            .as_object_mut()
            .unwrap()
            .insert("disable_parallel_tool_use".into(), Value::Bool(true));
    }

    Some(result)
}

// ── Payload validation ────────────────────────────────────────────────────────

const ALLOWED_BLOCK_TYPES: &[&str] = &[
    "text",
    "image",
    "thinking",
    "redacted_thinking",
    "tool_use",
    "tool_result",
    "document",
    "server_tool_use",
    "web_search_tool_result",
    "web_fetch_tool_result",
    "code_execution_tool_result",
    "bash_code_execution_tool_result",
    "text_editor_code_execution_tool_result",
    "tool_search_tool_result",
];

fn validate_anthropic_payload(body: &Value) -> Result<()> {
    let obj = body
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("anthropic payload must be object"))?;
    let _model = obj
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("anthropic payload missing model"))?;
    let _max_tokens = obj
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("anthropic payload missing max_tokens"))?;
    let messages = obj
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("anthropic payload missing messages"))?;
    for (idx, msg) in messages.iter().enumerate() {
        let role = msg
            .get("role")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("anthropic payload message[{idx}] missing role"))?;
        if role != "user" && role != "assistant" {
            anyhow::bail!("anthropic payload message[{idx}] invalid role: {role}");
        }

        if let Some(content) = msg.get("content") {
            match content {
                Value::String(_) => {}
                Value::Array(blocks) => {
                    for (bidx, block) in blocks.iter().enumerate() {
                        let btype =
                            block.get("type").and_then(|v| v.as_str()).ok_or_else(|| {
                                anyhow::anyhow!(
                                    "anthropic payload message[{idx}] block[{bidx}] missing type"
                                )
                            })?;
                        if !ALLOWED_BLOCK_TYPES.contains(&btype) {
                            anyhow::bail!(
                                "anthropic payload message[{idx}] unsupported block type: {btype}"
                            );
                        }
                        match btype {
                            "tool_use" => {
                                let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                if id.is_empty() || name.is_empty() {
                                    anyhow::bail!(
                                        "anthropic payload message[{idx}] tool_use block[{bidx}] missing id/name"
                                    );
                                }
                            }
                            "tool_result" => {
                                let tool_use_id = block
                                    .get("tool_use_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if tool_use_id.is_empty() {
                                    anyhow::bail!(
                                        "anthropic payload message[{idx}] tool_result block[{bidx}] missing tool_use_id"
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {
                    anyhow::bail!(
                        "anthropic payload message[{idx}] content must be string or array"
                    );
                }
            }
        } else {
            anyhow::bail!("anthropic payload message[{idx}] missing content");
        }
    }

    if let Some(tool_choice) = obj.get("tool_choice") {
        let tc = tool_choice
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("anthropic tool_choice must be object"))?;
        let t = tc.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if t != "auto" && t != "any" && t != "tool" && t != "none" {
            anyhow::bail!("anthropic tool_choice invalid type: {t}");
        }
        if t == "tool"
            && tc
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .is_empty()
        {
            anyhow::bail!("anthropic tool_choice=tool missing name");
        }
    }

    Ok(())
}

// ── Message encoding helpers ──────────────────────────────────────────────────

fn encode_message(
    mut msg: AiItem,
    generated_tool_id_seq: &mut usize,
    supplied_tool_ids: &HashSet<String>,
) -> Result<Value> {
    if let MessageContent::Blocks(blocks) = &mut msg.content {
        prepare_anthropic_media(blocks)?;
    }
    let role = match msg.role {
        Role::User | Role::Tool => "user",
        Role::Assistant => "assistant",
        Role::System | Role::Developer => unreachable!("instruction roles handled separately"),
    };

    if msg.role == Role::Tool {
        let (tool_content, hinted_tool_use_id) =
            anthropic_tool_result_payload(msg.content, generated_tool_id_seq, supplied_tool_ids);
        let tool_use_id = msg
            .tool_call_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .or_else(|| {
                hinted_tool_use_id
                    .as_deref()
                    .filter(|id| !id.trim().is_empty())
            })
            .map(str::to_owned)
            .unwrap_or_else(|| {
                next_synthetic_anthropic_tool_id(generated_tool_id_seq, supplied_tool_ids)
            });
        return Ok(object([
            ("role", Value::String(role.into())),
            (
                "content",
                Value::Array(vec![object([
                    ("type", Value::String("tool_result".into())),
                    ("tool_use_id", Value::String(tool_use_id)),
                    ("content", tool_content),
                ])]),
            ),
        ]));
    }

    let content = match msg.content {
        MessageContent::Text(t) => {
            let mut take_meta_text = |key| {
                let meta = msg.meta.as_mut()?;
                meta.object_extensions()?;
                match meta.remove_extension(key).ok().flatten()? {
                    Value::String(text) if !text.trim().is_empty() => Some(text),
                    _ => None,
                }
            };
            let reasoning = take_meta_text("reasoning_content");
            let reasoning_signature = take_meta_text("reasoning_signature");

            if reasoning.is_some() || msg.tool_calls.is_some() {
                let mut blocks: Vec<Value> = vec![];
                if let Some(text) = reasoning {
                    // Anthropic 拒绝无签名的 thinking 块：只有携带签名的思考才能
                    // 原生回放，无签名明文只能降级为普通文本，否则上游直接 400。
                    match reasoning_signature {
                        Some(signature) => blocks.push(object([
                            ("type", Value::String("thinking".into())),
                            ("thinking", Value::String(text)),
                            ("signature", Value::String(signature)),
                        ])),
                        None => blocks.push(text_block(text)),
                    }
                }
                if !t.is_empty() {
                    blocks.push(text_block(Arc::unwrap_or_clone(t)));
                }
                if let Some(tcs) = msg.tool_calls.take() {
                    for tc in tcs {
                        let input: Value = serde_json::from_str(&tc.arguments).map_err(
                            |error| {
                                anyhow::anyhow!(
                                    "anthropic tool_use input cannot represent arguments for tool call {}: {error}",
                                    tc.id
                                )
                            },
                        )?;
                        let id = normalized_anthropic_tool_id(
                            &tc.id,
                            generated_tool_id_seq,
                            supplied_tool_ids,
                        );
                        blocks.push(object([
                            ("type", Value::String("tool_use".into())),
                            ("id", Value::String(id)),
                            ("name", Value::String(tc.name)),
                            ("input", input),
                        ]));
                    }
                }
                Value::Array(blocks)
            } else {
                Value::String(Arc::unwrap_or_clone(t))
            }
        }
        MessageContent::Blocks(blocks) => {
            let represented_ids: HashSet<_> = blocks
                .iter()
                .filter_map(|block| {
                    if let ContentBlock::ToolUse { id, .. } = block {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect();
            // 一个推理块可能展开为多个 text 块，也可能整块省略，这里用 flat_map 展开。
            let mut arr: Vec<Value> = blocks
                .into_iter()
                .flat_map(|block| {
                    encode_content_block_for_anthropic_with_ids(
                        block,
                        generated_tool_id_seq,
                        supplied_tool_ids,
                    )
                })
                .collect();
            // Blocks 形态同样可能携带 msg.tool_calls（如 chat 解码出的
            // thinking+tool_calls 条目）；未以 ToolUse 块表达的调用必须补发，
            // 否则回放时整条调用被静默丢掉。
            if let Some(tcs) = msg.tool_calls.take() {
                for tc in tcs {
                    if represented_ids.contains(&tc.id) {
                        continue;
                    }
                    let input: Value = serde_json::from_str(&tc.arguments).map_err(|error| {
                        anyhow::anyhow!(
                            "anthropic tool_use input cannot represent arguments for tool call {}: {error}",
                            tc.id
                        )
                    })?;
                    let id = normalized_anthropic_tool_id(
                        &tc.id,
                        generated_tool_id_seq,
                        supplied_tool_ids,
                    );
                    arr.push(object([
                        ("type", Value::String("tool_use".into())),
                        ("id", Value::String(id)),
                        ("name", Value::String(tc.name)),
                        ("input", input),
                    ]));
                }
            }
            Value::Array(arr)
        }
    };

    Ok(object([
        ("role", Value::String(role.into())),
        ("content", content),
    ]))
}

fn project_native_anthropic_media(mut messages: Value) -> Value {
    if let Some(messages) = messages.as_array_mut() {
        messages.retain_mut(|message| {
            let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
                return true;
            };
            blocks.retain_mut(|block| {
                match block.get("type").and_then(Value::as_str) {
                    Some("input_audio") => return false,
                    Some("tool_result") => {
                        if let Some(content) =
                            block.get_mut("content").and_then(Value::as_array_mut)
                        {
                            content.retain_mut(|nested| {
                                if !matches!(
                                    nested.get("type").and_then(Value::as_str),
                                    Some(
                                        "text"
                                            | "image"
                                            | "document"
                                            | "search_result"
                                            | "tool_reference"
                                            | "browser_state"
                                    )
                                ) {
                                    return false;
                                }
                                if nested.get("type").and_then(Value::as_str) == Some("document")
                                    && nested.pointer("/source/type").and_then(Value::as_str)
                                        == Some("content")
                                    && let Some(parts) = nested
                                        .pointer_mut("/source/content")
                                        .and_then(Value::as_array_mut)
                                {
                                    parts.retain(|part| {
                                        matches!(
                                            part.get("type").and_then(Value::as_str),
                                            Some("text" | "image")
                                        )
                                    });
                                    return !parts.is_empty();
                                }
                                true
                            });
                        }
                    }
                    Some("document")
                        if block.pointer("/source/type").and_then(Value::as_str)
                            == Some("content") =>
                    {
                        if let Some(content) = block
                            .pointer_mut("/source/content")
                            .and_then(Value::as_array_mut)
                        {
                            content.retain(|nested| {
                                matches!(
                                    nested.get("type").and_then(Value::as_str),
                                    Some("text" | "image")
                                )
                            });
                            return !content.is_empty();
                        }
                    }
                    _ => {}
                }
                true
            });
            !blocks.is_empty()
        });
    }
    messages
}

fn prepare_anthropic_media(blocks: &mut [ContentBlock]) -> Result<()> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use stravia_runtime_contract::protocol::ir::DocumentSource;

    for block in blocks {
        match block {
            ContentBlock::File {
                source:
                    MediaSource::Base64 {
                        media_type: source_type,
                        data,
                    },
                media_type,
            } if source_type.as_str() == "text/plain"
                && media_type
                    .as_deref()
                    .is_none_or(|mime| mime == "text/plain") =>
            {
                let bytes = STANDARD.decode(data.as_bytes()).map_err(|error| {
                    anyhow::anyhow!("Invalid Anthropic text file base64: {error}")
                })?;
                let text = String::from_utf8(bytes).map_err(|error| {
                    anyhow::anyhow!("Anthropic text file must contain UTF-8: {error}")
                })?;
                *block = ContentBlock::Document {
                    source: DocumentSource::PlainText { data: text },
                    title: None,
                    context: None,
                    cache_control: None,
                };
            }
            ContentBlock::Document {
                source: DocumentSource::Blocks { content },
                ..
            } => {
                prepare_anthropic_media(content)?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
fn encode_content_block_for_anthropic(b: ContentBlock) -> Vec<Value> {
    let mut generated_tool_id_seq = 0;
    encode_content_block_for_anthropic_with_ids(b, &mut generated_tool_id_seq, &HashSet::new())
}

/// 一个 IR 块可能展开为多个 wire 块（无签名推理降级为逐段 text），
/// 也可能整块省略（空的无签名思考），因此返回 Vec。
fn encode_content_block_for_anthropic_with_ids(
    b: ContentBlock,
    generated_tool_id_seq: &mut usize,
    supplied_tool_ids: &HashSet<String>,
) -> Vec<Value> {
    match b {
        // Anthropic 拒绝无签名的 thinking 块：只有携带签名的思考才能原生回放；
        // 无签名明文降级为普通 text 块，否则上游直接 400。
        ContentBlock::Thinking {
            thinking,
            signature,
        } => match signature.filter(|sig| !sig.trim().is_empty()) {
            Some(signature) => vec![object([
                ("type", Value::String("thinking".into())),
                ("thinking", Value::String(thinking)),
                ("signature", Value::String(signature)),
            ])],
            None if thinking.is_empty() => Vec::new(),
            None => vec![text_block(thinking)],
        },
        // Responses ciphertext is not an Anthropic thinking signature.
        ContentBlock::Reasoning {
            summary, content, ..
        } => summary
            .into_iter()
            .chain(content)
            .filter(|text| !text.is_empty())
            .map(text_block)
            .collect(),
        ContentBlock::Image {
            source: MediaSource::Base64 { ref media_type, .. },
            ..
        } if !matches!(
            media_type.as_str(),
            "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        ) =>
        {
            Vec::new()
        }
        ContentBlock::Audio { .. } | ContentBlock::Video { .. } => Vec::new(),
        ContentBlock::File { source, media_type } => {
            let source = match source {
                MediaSource::FileId { file_id, .. } => object([
                    ("type", Value::String("file".into())),
                    ("file_id", Value::String(file_id)),
                ]),
                MediaSource::Base64 {
                    media_type: source_type,
                    data,
                } if source_type == "application/pdf"
                    && media_type
                        .as_deref()
                        .is_none_or(|mime| mime == "application/pdf") =>
                {
                    object([
                        ("type", Value::String("base64".into())),
                        ("media_type", Value::String("application/pdf".into())),
                        ("data", Value::String(data)),
                    ])
                }
                MediaSource::Url(url)
                    if media_type
                        .as_deref()
                        .is_none_or(|mime| mime == "application/pdf") =>
                {
                    object([
                        ("type", Value::String("url".into())),
                        ("url", Value::String(url)),
                    ])
                }
                _ => return Vec::new(),
            };
            vec![object([
                ("type", Value::String("document".into())),
                ("source", source),
            ])]
        }
        ContentBlock::Document {
            source,
            title,
            context,
            cache_control,
        } => {
            use stravia_runtime_contract::protocol::ir::DocumentSource;
            let source = match source {
                DocumentSource::Base64Pdf { data } => object([
                    ("type", Value::String("base64".into())),
                    ("media_type", Value::String("application/pdf".into())),
                    ("data", Value::String(data)),
                ]),
                DocumentSource::PlainText { data } => object([
                    ("type", Value::String("text".into())),
                    ("media_type", Value::String("text/plain".into())),
                    ("data", Value::String(data)),
                ]),
                DocumentSource::Url(url) => object([
                    ("type", Value::String("url".into())),
                    ("url", Value::String(url)),
                ]),
                DocumentSource::Blocks { content } => {
                    let content: Vec<_> = content
                        .into_iter()
                        .filter(|block| {
                            matches!(
                                block,
                                ContentBlock::Text { .. } | ContentBlock::Image { .. }
                            )
                        })
                        .flat_map(|block| {
                            encode_content_block_for_anthropic_with_ids(
                                block,
                                generated_tool_id_seq,
                                supplied_tool_ids,
                            )
                        })
                        .collect();
                    if content.is_empty() {
                        return Vec::new();
                    }
                    object([
                        ("type", Value::String("content".into())),
                        ("content", Value::Array(content)),
                    ])
                }
            };
            let mut block = object([
                ("type", Value::String("document".into())),
                ("source", source),
            ]);
            if let Some(title) = title {
                block["title"] = Value::String(title);
            }
            if let Some(context) = context {
                block["context"] = Value::String(context);
            }
            if let Some(cache_control) = cache_control {
                block["cache_control"] = serde_json::to_value(cache_control).unwrap_or(Value::Null);
            }
            vec![block]
        }
        other => vec![encode_single_anthropic_content_block(
            other,
            generated_tool_id_seq,
            supplied_tool_ids,
        )],
    }
}

fn encode_single_anthropic_content_block(
    b: ContentBlock,
    generated_tool_id_seq: &mut usize,
    supplied_tool_ids: &HashSet<String>,
) -> Value {
    match b {
        ContentBlock::Text {
            text,
            cache_control,
        } => {
            let mut block = text_block(Arc::unwrap_or_clone(text));
            if let Some(cc) = cache_control {
                block["cache_control"] = serde_json::to_value(cc).unwrap_or(Value::Null);
            }
            block
        }
        ContentBlock::Image {
            source,
            cache_control,
            ..
        } => {
            let src = match source {
                MediaSource::Base64 { media_type, data } => object([
                    ("type", Value::String("base64".into())),
                    ("media_type", Value::String(media_type)),
                    ("data", Value::String(data)),
                ]),
                MediaSource::Url(url) => object([
                    ("type", Value::String("url".into())),
                    ("url", Value::String(url)),
                ]),
                MediaSource::FileId { file_id, .. } => object([
                    ("type", Value::String("file".into())),
                    ("file_id", Value::String(file_id)),
                ]),
            };
            let mut block = object([("type", Value::String("image".into())), ("source", src)]);
            if let Some(cc) = cache_control {
                block["cache_control"] = serde_json::to_value(cc).unwrap_or(Value::Null);
            }
            block
        }
        // Thinking / Reasoning 由 `encode_content_block_for_anthropic_with_ids`
        // 统一处理（可能展开为多个块），不会走到这里。
        ContentBlock::RedactedThinking { data } => object([
            ("type", Value::String("redacted_thinking".into())),
            ("data", Value::String(data)),
        ]),
        ContentBlock::ToolUse {
            id,
            name,
            input,
            cache_control,
        } => {
            let id = normalized_anthropic_tool_id(&id, generated_tool_id_seq, supplied_tool_ids);
            let mut block = object([
                ("type", Value::String("tool_use".into())),
                ("id", Value::String(id)),
                ("name", Value::String(name)),
                ("input", input),
            ]);
            if let Some(cc) = cache_control {
                block["cache_control"] = serde_json::to_value(cc).unwrap_or(Value::Null);
            }
            block
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            content_kind,
            is_error,
            cache_control,
            ..
        } => {
            let tool_use_id = normalized_anthropic_tool_id(
                &tool_use_id,
                generated_tool_id_seq,
                supplied_tool_ids,
            );
            let mut block = object([
                ("type", Value::String("tool_result".into())),
                ("tool_use_id", Value::String(tool_use_id)),
                (
                    "content",
                    anthropic_tool_result_content(content, content_kind),
                ),
            ]);
            if let Some(err) = is_error {
                block["is_error"] = Value::Bool(err);
            }
            if let Some(cc) = cache_control {
                block["cache_control"] = serde_json::to_value(cc).unwrap_or(Value::Null);
            }
            block
        }
        ContentBlock::ServerToolUse {
            id,
            name,
            input,
            server_type,
            cache_control,
        } => {
            let mut block = object([
                (
                    "type",
                    Value::String(server_type.unwrap_or_else(|| "server_tool_use".into())),
                ),
                ("id", Value::String(id.to_string())),
                ("name", Value::String(name)),
                ("input", input),
            ]);
            if let Some(cache_control) = cache_control {
                block["cache_control"] = serde_json::to_value(cache_control).unwrap_or(Value::Null);
            }
            block
        }
        ContentBlock::ServerToolResult {
            tool_use_id,
            content,
            server_type,
            cache_control,
            ..
        } => {
            let mut block = object([
                (
                    "type",
                    Value::String(server_type.unwrap_or_else(|| "server_tool_result".into())),
                ),
                ("tool_use_id", Value::String(tool_use_id.to_string())),
                ("content", content),
            ]);
            if let Some(cache_control) = cache_control {
                block["cache_control"] = serde_json::to_value(cache_control).unwrap_or(Value::Null);
            }
            block
        }
        ContentBlock::Unknown { raw } => raw,
        other => crate::codec::content_block_wire_value(&other),
    }
}

fn anthropic_tool_result_content(content: Value, kind: Option<ToolResultContentKind>) -> Value {
    // 原生 content 只接受文本或内容块；业务 JSON 数组不能冒充原生块数组。
    if kind == Some(ToolResultContentKind::Json) || !(content.is_string() || content.is_array()) {
        Value::String(content.to_string())
    } else if let Value::Array(blocks) = content {
        Value::Array(
            blocks
                .into_iter()
                .filter(|block| {
                    matches!(
                        block.get("type").and_then(Value::as_str),
                        Some(
                            "text"
                                | "image"
                                | "document"
                                | "search_result"
                                | "tool_reference"
                                | "browser_state"
                        )
                    )
                })
                .collect(),
        )
    } else {
        content
    }
}

fn anthropic_tool_result_payload(
    content: MessageContent,
    generated_tool_id_seq: &mut usize,
    supplied_tool_ids: &HashSet<String>,
) -> (Value, Option<String>) {
    match content {
        MessageContent::Text(t) => (Value::String(Arc::unwrap_or_clone(t)), None),
        MessageContent::Blocks(blocks) => {
            let result_index = blocks.iter().position(|block| {
                matches!(
                    block,
                    ContentBlock::ToolResult { .. } | ContentBlock::ServerToolResult { .. }
                )
            });
            let mut blocks = blocks;
            if let Some(index) = result_index {
                match blocks.swap_remove(index) {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        content_kind,
                        ..
                    } => {
                        return (
                            anthropic_tool_result_content(content, content_kind),
                            Some(tool_use_id.to_string()),
                        );
                    }
                    ContentBlock::ServerToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => return (content, Some(tool_use_id.to_string())),
                    _ => unreachable!("selected a tool result"),
                }
            }
            (
                Value::Array(
                    blocks
                        .into_iter()
                        .flat_map(|block| {
                            encode_content_block_for_anthropic_with_ids(
                                block,
                                generated_tool_id_seq,
                                supplied_tool_ids,
                            )
                        })
                        .filter(|block| {
                            matches!(
                                block.get("type").and_then(Value::as_str),
                                Some(
                                    "text"
                                        | "image"
                                        | "document"
                                        | "search_result"
                                        | "tool_reference"
                                        | "browser_state"
                                )
                            )
                        })
                        .collect(),
                ),
                None,
            )
        }
    }
}

fn supplied_anthropic_tool_ids(messages: &[AiItem]) -> HashSet<String> {
    let mut ids = HashSet::new();
    for msg in messages {
        if let Some(tool_calls) = &msg.tool_calls {
            ids.extend(
                tool_calls
                    .iter()
                    .map(|call| call.id.trim())
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned),
            );
        }
        if let Some(id) = msg
            .tool_call_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
        {
            ids.insert(id.to_owned());
        }
        if let MessageContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                let id = match block {
                    ContentBlock::ToolUse { id, .. } | ContentBlock::ServerToolUse { id, .. } => {
                        Some(id.as_str())
                    }
                    ContentBlock::ToolResult { tool_use_id, .. }
                    | ContentBlock::ServerToolResult { tool_use_id, .. } => {
                        Some(tool_use_id.as_str())
                    }
                    _ => None,
                };
                if let Some(id) = id.filter(|id| !id.trim().is_empty()) {
                    ids.insert(id.to_owned());
                }
            }
        }
    }
    ids
}

fn next_synthetic_anthropic_tool_id(
    sequence: &mut usize,
    supplied_ids: &HashSet<String>,
) -> String {
    loop {
        *sequence += 1;
        let id = format!("tc_{}", *sequence);
        if !supplied_ids.contains(&id) {
            return id;
        }
    }
}

fn normalized_anthropic_tool_id(
    raw: &str,
    generated_tool_id_seq: &mut usize,
    supplied_tool_ids: &HashSet<String>,
) -> String {
    if raw.trim().is_empty() {
        next_synthetic_anthropic_tool_id(generated_tool_id_seq, supplied_tool_ids)
    } else {
        raw.to_owned()
    }
}

fn normalize_anthropic_messages(messages: Vec<Value>) -> Vec<Value> {
    let mut normalized: Vec<Value> = Vec::new();
    for mut msg in messages {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };
        let Some(Value::String(role)) = obj.remove("role") else {
            continue;
        };
        let blocks = content_to_blocks(obj.remove("content").unwrap_or(Value::Null));
        if blocks.is_empty() {
            continue;
        }

        if let Some(last) = normalized.last_mut() {
            let same_role = last.get("role").and_then(|v| v.as_str()) == Some(role.as_str());
            if same_role {
                if let Some(merged) = last.get_mut("content").and_then(Value::as_array_mut) {
                    merged.extend(blocks);
                }
                continue;
            }
        }

        normalized.push(object([
            ("role", Value::String(role)),
            ("content", Value::Array(blocks)),
        ]));
    }

    // DeepSeek's Anthropic-compatible endpoint requires assistant tool_use
    // blocks to trail the assistant turn when the next user turn contains
    // matching tool_result blocks. Codex Responses input may place assistant
    // commentary after function_call items, which otherwise normalizes into
    // `[tool_use, tool_use, text]`.
    for msg in &mut normalized {
        if msg.get("role").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        let Some(arr) = msg.get_mut("content").and_then(|v| v.as_array_mut()) else {
            continue;
        };
        if !arr
            .iter()
            .any(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
        {
            continue;
        }
        let mut thinking: Vec<Value> = Vec::new();
        let mut others: Vec<Value> = Vec::new();
        let mut tool_uses: Vec<Value> = Vec::new();
        for b in arr.drain(..) {
            match b.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                "thinking" => thinking.push(b),
                "tool_use" => tool_uses.push(b),
                _ => others.push(b),
            }
        }
        let mut reordered = thinking;
        reordered.extend(others);
        reordered.extend(tool_uses);
        if let Some(obj) = msg.as_object_mut() {
            obj.insert("content".into(), Value::Array(reordered));
        }
    }

    normalized
}

fn content_to_blocks(content: Value) -> Vec<Value> {
    match content {
        Value::String(s) => {
            if s.trim().is_empty() {
                Vec::new()
            } else {
                vec![text_block(s)]
            }
        }
        Value::Array(arr) => arr
            .into_iter()
            .filter(|v| {
                let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
                if t == "text" {
                    !v.get("text")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim()
                        .is_empty()
                } else {
                    true
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests;
