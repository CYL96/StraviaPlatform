//! Anthropic Messages ingress decoder — produces `AiRequest` directly.
//!
//! Cache-control and exotic blocks (Document, InputAudio) are now carried
//! natively in `ContentBlock`. Raw wire values are also preserved in
//! `meta.vendor.ingress` so the old Anthropic encoder (pre-PR-3) can still
//! round-trip them.

use anyhow::Result;
use serde_json::Value;

use stravia_runtime_contract::protocol::ids::ANTHROPIC_MESSAGES_2023_06_01;
use stravia_runtime_contract::protocol::ir::AiItem;
use stravia_runtime_contract::protocol::ir::AiRequest;
use stravia_runtime_contract::protocol::ir::AnthropicExt;
use stravia_runtime_contract::protocol::ir::CacheTtl;
use stravia_runtime_contract::protocol::ir::ContentBlock;
use stravia_runtime_contract::protocol::ir::DocumentSource as IrDocumentSource;
use stravia_runtime_contract::protocol::ir::GenerationConfig;
use stravia_runtime_contract::protocol::ir::MediaSource;
use stravia_runtime_contract::protocol::ir::MessageContent;
use stravia_runtime_contract::protocol::ir::ProtocolExt;
use stravia_runtime_contract::protocol::ir::ReasoningConfig;
use stravia_runtime_contract::protocol::ir::ReasoningEffort;
use stravia_runtime_contract::protocol::ir::Role;
use stravia_runtime_contract::protocol::ir::StreamConfig;
use stravia_runtime_contract::protocol::ir::ToolCall;
use stravia_runtime_contract::protocol::ir::ToolChoice;
use stravia_runtime_contract::protocol::ir::ToolSpec;

use super::types::{
    AnthropicContent, AnthropicContentBlock, AnthropicImageSource, AnthropicMessage,
    AnthropicRequest, AnthropicSystem, CacheControl as WireCacheControl,
    DocumentSource as WireDocumentSource,
};

pub struct AnthropicDecoder;

impl AnthropicDecoder {
    pub(crate) fn decode_request(&self, body: Value) -> Result<AiRequest> {
        let req: AnthropicRequest = serde_json::from_value(body)?;

        // ── Cache-control / exotic-block detection (for raw preservation) ──────
        let needs_raw_msgs = req.messages.iter().any(|m| {
            if let AnthropicContent::Blocks(blocks) = &m.content {
                blocks.iter().any(|b| {
                    b.cache_control().is_some()
                        || b.is_exotic()
                        || matches!(
                            b,
                            AnthropicContentBlock::ToolResult {
                                is_error: Some(_),
                                ..
                            }
                        )
                })
            } else {
                false
            }
        });

        let needs_raw_system = match &req.system {
            Some(AnthropicSystem::Blocks(blocks)) => {
                blocks.iter().any(|b| b.cache_control.is_some())
            }
            _ => false,
        };

        // 原样快照保留 cache_control 和工具原生字段；strict 同时进入 IR，
        // 以便跨协议保留官方严格工具调用约束。
        let needs_raw_tools = req
            .tools
            .as_ref()
            .map(|ts| {
                ts.iter()
                    .any(|t| t.cache_control.is_some() || t.strict.is_some())
            })
            .unwrap_or(false);

        // Snapshot raw wire values before consuming req.
        // Inline "system" role messages are filtered out of the snapshot so that
        // the Anthropic encoder's raw-passthrough path (used when cache_control is
        // present) never forwards non-standard roles to strict downstream endpoints.
        let raw_messages: Option<Value> = if needs_raw_msgs {
            let wire_msgs: Vec<&AnthropicMessage> = req
                .messages
                .iter()
                .filter(|m| matches!(m.role.as_str(), "user" | "assistant"))
                .collect();
            serde_json::to_value(wire_msgs).ok()
        } else {
            None
        };
        let raw_system: Option<Value> = if needs_raw_system {
            req.system
                .as_ref()
                .and_then(|s| serde_json::to_value(s).ok())
        } else {
            None
        };
        let raw_tools: Option<Value> = if needs_raw_tools {
            req.tools
                .as_ref()
                .and_then(|t| serde_json::to_value(t).ok())
        } else {
            None
        };

        // ── System prompt ─────────────────────────────────────────────────────
        let instructions = req
            .system
            .as_ref()
            .map(|system| match system {
                AnthropicSystem::Text(t) => t.clone(),
                AnthropicSystem::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        if b.kind == "text" {
                            Some(b.text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            })
            .filter(|text| !text.is_empty());

        let mut messages: Vec<AiItem> = Vec::new();

        for msg in req.messages {
            messages.extend(decode_message(msg)?);
        }

        // ── Tools ─────────────────────────────────────────────────────────────
        let mut user_tools: Vec<ToolSpec> = Vec::new();
        let mut server_tools: Vec<Value> = Vec::new();

        if let Some(tools) = req.tools {
            for t in tools {
                if let Some(ref tool_type) = t.tool_type {
                    // Built-in / server tool — preserve as raw Value in AnthropicExt.
                    let mut raw = serde_json::Map::new();
                    raw.insert("type".to_string(), Value::String(tool_type.clone()));
                    if let Some(d) = &t.description {
                        raw.insert("description".to_string(), Value::String(d.clone()));
                    }
                    if let Some(s) = &t.input_schema {
                        raw.insert("input_schema".to_string(), s.clone());
                    }
                    server_tools.push(Value::Object(raw));
                    // Also keep sentinel ToolSpec so dispatcher knows about it.
                    user_tools.push(ToolSpec {
                        name: format!("__builtin__{}", tool_type),
                        description: t.description,
                        parameters: t.input_schema.unwrap_or(Value::Object(Default::default())),
                        strict: t.strict,
                        cache_control: t.cache_control.as_ref().map(map_cache_control),
                        meta: None,
                    });
                } else if let Some(schema) = t.input_schema {
                    user_tools.push(ToolSpec {
                        name: t.name,
                        description: t.description,
                        parameters: schema,
                        strict: t.strict,
                        cache_control: t.cache_control.as_ref().map(map_cache_control),
                        meta: None,
                    });
                }
            }
        }

        // ── tool_choice ───────────────────────────────────────────────────────
        let disable_parallel_tool_calls = req
            .tool_choice
            .as_ref()
            .and_then(|choice| choice.get("disable_parallel_tool_use"))
            .map(|value| {
                value.as_bool().ok_or_else(|| {
                    anyhow::anyhow!("Anthropic disable_parallel_tool_use must be a boolean")
                })
            })
            .transpose()?;
        let tool_choice = req.tool_choice.map(parse_tool_choice);
        let response_format = parse_output_format(req.output_config.as_ref())?;

        // ── Reasoning config ──────────────────────────────────────────────────
        let output_effort = parse_output_effort(req.output_config.as_ref())?;
        let reasoning = match req.thinking.as_ref() {
            Some(thinking) if output_effort.is_some() => {
                if !matches!(thinking.kind.as_str(), "enabled" | "disabled" | "adaptive") {
                    anyhow::bail!("unsupported Anthropic thinking type: {}", thinking.kind);
                }
                // 显式档位优先于开关和预算，不能把冲突预算带到 Target 选择阶段。
                reasoning_from_effort(
                    output_effort.as_deref().expect("explicit effort"),
                    thinking.display.clone(),
                )?
            }
            Some(thinking) if thinking.kind == "adaptive" => reasoning_from_effort(
                output_effort.as_deref().unwrap_or("high"),
                thinking.display.clone(),
            )?,
            Some(thinking) => {
                let enabled = match thinking.kind.as_str() {
                    "enabled" => true,
                    "disabled" => false,
                    value => anyhow::bail!("unsupported Anthropic thinking type: {value}"),
                };
                ReasoningConfig {
                    enabled,
                    budget_tokens: thinking.budget_tokens,
                    display: thinking.display.clone(),
                    level: Some(if enabled {
                        thinking
                            .budget_tokens
                            .map(stravia_runtime_contract::thinking::ThinkingLevel::from_budget)
                            .unwrap_or(stravia_runtime_contract::thinking::ThinkingLevel::Medium)
                    } else {
                        stravia_runtime_contract::thinking::ThinkingLevel::Off
                    }),
                    ..Default::default()
                }
            }
            None => output_effort
                .as_deref()
                .map(|effort| reasoning_from_effort(effort, None))
                .transpose()?
                .unwrap_or_default(),
        };

        // ── AnthropicExt ──────────────────────────────────────────────────────
        let ant_ext = AnthropicExt {
            output_config: req.output_config,
            top_k: req.top_k,
            container: req.container.as_ref().map(|c| Value::String(c.clone())),
            service_tier: req.service_tier.clone(),
            server_tools: if server_tools.is_empty() {
                None
            } else {
                Some(server_tools)
            },
            ..Default::default()
        };

        // ── Vendor ingress bag — backward compat for old Anthropic encoder ────
        let mut ingress = std::collections::HashMap::new();

        if let Some(v) = raw_messages {
            ingress.insert("__anthropic_raw_messages".into(), v);
        }
        if let Some(v) = raw_system {
            ingress.insert("__anthropic_raw_system".into(), v);
        }
        if let Some(v) = raw_tools {
            ingress.insert("__anthropic_raw_tools".into(), v);
        }
        if let Some(ref thinking) = req.thinking
            && let Ok(v) = serde_json::to_value(thinking)
        {
            ingress.insert("__anthropic_thinking".into(), v);
        }
        if let Some(ref cm) = req.context_management {
            ingress.insert("__anthropic_context_management".into(), cm.clone());
        }
        if let Some(ref c) = req.container {
            ingress.insert("__anthropic_container".into(), Value::String(c.clone()));
        }
        if let Some(ref st) = req.service_tier {
            ingress.insert("__anthropic_service_tier".into(), Value::String(st.clone()));
        }
        if let Some(ref meta) = req.metadata {
            ingress.insert("__anthropic_metadata".into(), meta.clone());
        }
        if let Some(ref stops) = req.stop_sequences
            && let Ok(v) = serde_json::to_value(stops)
        {
            ingress.insert("__anthropic_stop_sequences".into(), v);
        }
        if let Some(k) = req.top_k {
            ingress.insert("__anthropic_top_k".into(), Value::Number(k.into()));
        }

        // ── Build AiRequest ───────────────────────────────────────────────────
        let tools_opt = if user_tools.is_empty() {
            None
        } else {
            Some(user_tools)
        };

        let mut ai_req = AiRequest::new(req.model, messages);
        ai_req.instructions = instructions;
        ai_req.generation = GenerationConfig {
            temperature: req.temperature,
            max_tokens: Some(req.max_tokens),
            top_p: req.top_p,
            stop: req.stop_sequences,
            ..Default::default()
        };
        ai_req.stream = StreamConfig {
            enabled: req.stream,
            include_usage: false,
        };
        ai_req.tools = tools_opt;
        ai_req.tool_choice = tool_choice;
        ai_req.disable_parallel_tool_calls = disable_parallel_tool_calls;
        ai_req.parallel_tool_calls = disable_parallel_tool_calls.map(|disabled| !disabled);
        ai_req.response_format = response_format;
        ai_req.reasoning = reasoning;
        ai_req.ext = Some(ProtocolExt::Anthropic(ant_ext));
        ai_req.meta.source_protocol = Some(ANTHROPIC_MESSAGES_2023_06_01);
        ai_req.meta.vendor.ingress = ingress;

        Ok(ai_req)
    }
}

// ── Message decoding helpers ──────────────────────────────────────────────────

fn decode_message(msg: AnthropicMessage) -> Result<Vec<AiItem>> {
    let role = match msg.role.as_str() {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        // Claude Code >=2.1.154 uses the official mid-conversation system messages feature,
        // embedding {"role":"system"} inside the messages array for skill/hook context.
        // See: https://platform.claude.com/docs/en/build-with-claude/mid-conversation-system-messages
        "system" => Role::System,
        other => anyhow::bail!("unknown Anthropic role: {other}"),
    };

    if role == Role::User
        && let AnthropicContent::Blocks(blocks) = msg.content
    {
        return decode_user_blocks(blocks);
    }

    let (content, tool_calls, tool_call_id) = match msg.content {
        AnthropicContent::Text(t) => (MessageContent::Text(t.into()), None, None),
        AnthropicContent::Blocks(blocks) => {
            let mut content_blocks: Vec<ContentBlock> = Vec::new();
            let mut tcs: Vec<ToolCall> = Vec::new();
            let mut tc_id: Option<String> = None;
            let mut thinking_texts: Vec<String> = Vec::new();

            for block in blocks {
                match block {
                    AnthropicContentBlock::Text {
                        text,
                        cache_control,
                        ..
                    } => {
                        content_blocks.push(ContentBlock::Text {
                            text: text.into(),
                            cache_control: cache_control.as_ref().map(map_cache_control),
                        });
                    }
                    AnthropicContentBlock::Thinking {
                        thinking,
                        signature,
                        ..
                    } => {
                        if role == Role::Assistant && !thinking.is_empty() {
                            thinking_texts.push(thinking.clone());
                        }
                        content_blocks.push(ContentBlock::Thinking {
                            thinking,
                            signature,
                        });
                    }
                    AnthropicContentBlock::RedactedThinking { data } => {
                        content_blocks.push(ContentBlock::RedactedThinking { data });
                    }
                    AnthropicContentBlock::Image {
                        source,
                        cache_control,
                        ..
                    } => {
                        content_blocks.push(ContentBlock::Image {
                            source: map_image_source(source)?,
                            detail: None,
                            cache_control: cache_control.as_ref().map(map_cache_control),
                        });
                    }
                    AnthropicContentBlock::ToolUse {
                        id,
                        name,
                        input,
                        cache_control,
                        ..
                    } => {
                        tcs.push(ToolCall {
                            id: (id.clone()).into(),
                            name: name.clone(),
                            arguments: input.to_string(),
                        });
                        content_blocks.push(ContentBlock::ToolUse {
                            id: id.into(),
                            name,
                            input,
                            cache_control: cache_control.as_ref().map(map_cache_control),
                        });
                    }
                    AnthropicContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        cache_control,
                        ..
                    } => {
                        tc_id = Some(tool_use_id.clone());
                        let content = content.unwrap_or(Value::Null);
                        let content_kind = if content.is_string() || content.is_array() {
                            stravia_runtime_contract::protocol::ir::ToolResultContentKind::ContentBlocks
                        } else {
                            stravia_runtime_contract::protocol::ir::ToolResultContentKind::Json
                        };
                        content_blocks.push(ContentBlock::ToolResult {
                            tool_use_id: tool_use_id.into(),
                            content,
                            content_kind: Some(content_kind),
                            is_error: None,
                            cache_control: cache_control.as_ref().map(map_cache_control),
                        });
                    }
                    AnthropicContentBlock::Document {
                        source,
                        title,
                        context,
                        cache_control,
                        ..
                    } => {
                        content_blocks.push(map_document_block(
                            source,
                            title,
                            context,
                            cache_control,
                        )?);
                    }
                    AnthropicContentBlock::InputAudio { source, .. } => {
                        content_blocks.push(ContentBlock::Audio {
                            source: MediaSource::Base64 {
                                media_type: source.media_type,
                                data: source.data,
                            },
                        });
                    }
                }
            }

            let tool_calls_opt = if tcs.is_empty() { None } else { Some(tcs) };

            let meta = build_reasoning_meta(&thinking_texts);

            if content_blocks.len() == 1
                && let ContentBlock::Text { text, .. } = &content_blocks[0]
            {
                return Ok(vec![AiItem {
                    role,
                    content: MessageContent::Text(text.clone()),
                    tool_calls: tool_calls_opt,
                    tool_call_id: (tc_id).map(Into::into),
                    meta,
                }]);
            }

            return Ok(vec![AiItem {
                role,
                content: MessageContent::Blocks(content_blocks),
                tool_calls: tool_calls_opt,
                tool_call_id: (tc_id).map(Into::into),
                meta,
            }]);
        }
    };

    Ok(vec![AiItem {
        role,
        content,
        tool_calls,
        tool_call_id,
        meta: None,
    }])
}

/// Build `meta = {"reasoning_content": ...}` so the OpenAI-compat encoder can
/// re-emit thinking text as a top-level `reasoning_content` field on assistant
/// messages (required by providers like Xiaomi Mimo / DeepSeek thinking mode).
fn build_reasoning_meta(
    thinking_texts: &[String],
) -> Option<Box<stravia_runtime_contract::protocol::ir::AiItemMetadata>> {
    if thinking_texts.is_empty() {
        return None;
    }
    let joined = thinking_texts.join("\n\n");
    if joined.is_empty() {
        return None;
    }
    Some(
        stravia_runtime_contract::protocol::ir::AiItemMetadata::boxed(
            serde_json::json!({ "reasoning_content": joined }),
        ),
    )
}

fn decode_user_blocks(blocks: Vec<AnthropicContentBlock>) -> Result<Vec<AiItem>> {
    let mut messages: Vec<AiItem> = Vec::new();
    let mut user_blocks: Vec<ContentBlock> = Vec::new();

    for block in blocks {
        match block {
            AnthropicContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                cache_control,
                ..
            } => {
                let content = content.unwrap_or_else(|| Value::String(String::new()));
                let content_kind = if content.is_array() {
                    "content_blocks"
                } else {
                    "json"
                };
                let meta = Some(
                    stravia_runtime_contract::protocol::ir::AiItemMetadata::boxed(
                        serde_json::json!({
                            (stravia_runtime_contract::protocol::ir::TOOL_RESULT_CONTENT_KIND_META): content_kind
                        }),
                    ),
                );
                let content = match content {
                    Value::String(text) if is_error.is_none() && cache_control.is_none() => {
                        MessageContent::Text(text.into())
                    }
                    content => MessageContent::Blocks(vec![ContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone().into(),
                        content,
                        content_kind: Some(if content_kind == "content_blocks" {
                            stravia_runtime_contract::protocol::ir::ToolResultContentKind::ContentBlocks
                        } else {
                            stravia_runtime_contract::protocol::ir::ToolResultContentKind::Json
                        }),
                        is_error,
                        cache_control: cache_control.as_ref().map(map_cache_control),
                    }]),
                };
                messages.push(AiItem {
                    role: Role::Tool,
                    content,
                    tool_calls: None,
                    tool_call_id: Some((tool_use_id).into()),
                    meta,
                });
            }
            AnthropicContentBlock::Text {
                text,
                cache_control,
                ..
            } => {
                user_blocks.push(ContentBlock::Text {
                    text: text.into(),
                    cache_control: cache_control.as_ref().map(map_cache_control),
                });
            }
            AnthropicContentBlock::Thinking {
                thinking,
                signature,
                ..
            } => {
                user_blocks.push(ContentBlock::Thinking {
                    thinking,
                    signature,
                });
            }
            AnthropicContentBlock::RedactedThinking { data } => {
                user_blocks.push(ContentBlock::RedactedThinking { data });
            }
            AnthropicContentBlock::Image {
                source,
                cache_control,
                ..
            } => {
                user_blocks.push(ContentBlock::Image {
                    source: map_image_source(source)?,
                    detail: None,
                    cache_control: cache_control.as_ref().map(map_cache_control),
                });
            }
            AnthropicContentBlock::ToolUse {
                id,
                name,
                input,
                cache_control,
                ..
            } => {
                user_blocks.push(ContentBlock::ToolUse {
                    id: id.into(),
                    name,
                    input,
                    cache_control: cache_control.as_ref().map(map_cache_control),
                });
            }
            AnthropicContentBlock::Document {
                source,
                title,
                context,
                cache_control,
                ..
            } => {
                user_blocks.push(map_document_block(source, title, context, cache_control)?);
            }
            AnthropicContentBlock::InputAudio { source, .. } => {
                user_blocks.push(ContentBlock::Audio {
                    source: MediaSource::Base64 {
                        media_type: source.media_type,
                        data: source.data,
                    },
                });
            }
        }
    }

    if !user_blocks.is_empty() {
        let content = if user_blocks.len() == 1
            && let ContentBlock::Text { text, .. } = &user_blocks[0]
        {
            MessageContent::Text(text.clone())
        } else {
            MessageContent::Blocks(user_blocks)
        };
        messages.insert(
            0,
            AiItem {
                role: Role::User,
                content,
                tool_calls: None,
                tool_call_id: None,
                meta: None,
            },
        );
    }

    Ok(messages)
}

// ── Type-mapping helpers ──────────────────────────────────────────────────────

fn map_cache_control(
    _cc: &WireCacheControl,
) -> stravia_runtime_contract::protocol::ir::CacheControl {
    // Anthropic only has "ephemeral" TTL.
    stravia_runtime_contract::protocol::ir::CacheControl {
        ttl: CacheTtl::Ephemeral5m,
        breakpoint_priority: 0,
    }
}

fn map_image_source(src: AnthropicImageSource) -> Result<MediaSource> {
    Ok(match src.source_type.as_str() {
        "base64" => {
            let media_type = src
                .media_type
                .ok_or_else(|| anyhow::anyhow!("Anthropic base64 image requires media_type"))?;
            anyhow::ensure!(
                matches!(
                    media_type.as_str(),
                    "image/jpeg" | "image/png" | "image/gif" | "image/webp"
                ),
                "Unsupported Anthropic image MIME type: {media_type}"
            );
            MediaSource::Base64 {
                media_type,
                data: src
                    .data
                    .ok_or_else(|| anyhow::anyhow!("Anthropic base64 image requires data"))?,
            }
        }
        "url" => MediaSource::Url(
            src.url
                .ok_or_else(|| anyhow::anyhow!("Anthropic URL image requires url"))?,
        ),
        "file" => MediaSource::FileId {
            file_id: src
                .file_id
                .ok_or_else(|| anyhow::anyhow!("Anthropic file image requires file_id"))?,
            detail: None,
        },
        kind => anyhow::bail!("Unsupported Anthropic image source type: {kind}"),
    })
}

fn map_document_block(
    source: WireDocumentSource,
    title: Option<String>,
    context: Option<String>,
    cache_control: Option<WireCacheControl>,
) -> Result<ContentBlock> {
    if source.kind == "file" {
        return Ok(ContentBlock::File {
            source: MediaSource::FileId {
                file_id: source
                    .file_id
                    .ok_or_else(|| anyhow::anyhow!("Anthropic file document requires file_id"))?,
                detail: None,
            },
            media_type: None,
        });
    }
    Ok(ContentBlock::Document {
        source: map_doc_source(source)?,
        title,
        context,
        cache_control: cache_control.as_ref().map(map_cache_control),
    })
}

fn map_doc_source(src: WireDocumentSource) -> Result<IrDocumentSource> {
    Ok(match src.kind.as_str() {
        "base64" => {
            anyhow::ensure!(
                src.media_type.as_deref() == Some("application/pdf"),
                "Anthropic base64 document requires application/pdf MIME type"
            );
            IrDocumentSource::Base64Pdf {
                data: src
                    .data
                    .ok_or_else(|| anyhow::anyhow!("Anthropic base64 document requires data"))?,
            }
        }
        "text" => {
            anyhow::ensure!(
                src.media_type.as_deref() == Some("text/plain"),
                "Anthropic text document requires text/plain MIME type"
            );
            IrDocumentSource::PlainText {
                data: src
                    .data
                    .ok_or_else(|| anyhow::anyhow!("Anthropic text document requires data"))?,
            }
        }
        "url" => IrDocumentSource::Url(
            src.url
                .ok_or_else(|| anyhow::anyhow!("Anthropic URL document requires url"))?,
        ),
        "content" => {
            let content = match src.content {
                Some(Value::String(text)) => vec![ContentBlock::Text {
                    text: text.into(),
                    cache_control: None,
                }],
                Some(Value::Array(values)) => {
                    let mut content = Vec::with_capacity(values.len());
                    for value in values {
                        if !matches!(
                            value.get("type").and_then(Value::as_str),
                            Some("text" | "image")
                        ) {
                            continue;
                        }
                        match serde_json::from_value::<AnthropicContentBlock>(value)? {
                            AnthropicContentBlock::Text {
                                text,
                                cache_control,
                            } => {
                                content.push(ContentBlock::Text {
                                    text: text.into(),
                                    cache_control: cache_control.as_ref().map(map_cache_control),
                                });
                            }
                            AnthropicContentBlock::Image {
                                source,
                                cache_control,
                            } => {
                                content.push(ContentBlock::Image {
                                    source: map_image_source(source)?,
                                    detail: None,
                                    cache_control: cache_control.as_ref().map(map_cache_control),
                                });
                            }
                            _ => unreachable!("document content was filtered to text and image"),
                        }
                    }
                    content
                }
                _ => anyhow::bail!("Anthropic document content must be a string or block array"),
            };
            IrDocumentSource::Blocks { content }
        }
        kind => anyhow::bail!("Unsupported Anthropic document source type: {kind}"),
    })
}

fn parse_tool_choice(v: Value) -> ToolChoice {
    match v.get("type").and_then(|t| t.as_str()) {
        Some("auto") => ToolChoice::Auto,
        Some("any") => ToolChoice::Required,
        Some("none") => ToolChoice::None,
        Some("tool") => {
            if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                ToolChoice::Named {
                    name: name.to_string(),
                }
            } else {
                ToolChoice::Raw(v)
            }
        }
        _ => ToolChoice::Raw(v),
    }
}

fn parse_output_format(
    output_config: Option<&Value>,
) -> Result<Option<stravia_runtime_contract::protocol::ir::ResponseFormat>> {
    let Some(format) = output_config
        .and_then(|config| config.get("format"))
        .filter(|format| !format.is_null())
    else {
        return Ok(None);
    };
    if format.get("type").and_then(Value::as_str) != Some("json_schema") {
        anyhow::bail!("unsupported Anthropic output_config.format type");
    }
    let schema = format
        .get("schema")
        .filter(|schema| schema.is_object())
        .ok_or_else(|| {
            anyhow::anyhow!("Anthropic output_config.format.schema must be an object")
        })?;
    Ok(Some(
        stravia_runtime_contract::protocol::ir::ResponseFormat::JsonSchema {
            name: "response".into(),
            schema: schema.clone(),
            strict: Some(true),
        },
    ))
}

fn parse_output_effort(output_config: Option<&Value>) -> Result<Option<String>> {
    let Some(output_config) = output_config else {
        return Ok(None);
    };
    let output_config = output_config
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Anthropic output_config must be an object"))?;
    output_config
        .get("effort")
        .filter(|effort| !effort.is_null())
        .map(|effort| {
            effort
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("Anthropic output_config.effort must be a string"))
        })
        .transpose()
}

fn reasoning_from_effort(effort: &str, display: Option<String>) -> Result<ReasoningConfig> {
    let level = stravia_runtime_contract::thinking::ThinkingLevel::from_wire(effort)?;
    let effort = ReasoningEffort::from_openai_str(
        if level == stravia_runtime_contract::thinking::ThinkingLevel::Off {
            "none"
        } else {
            level.as_str()
        },
    )?;
    Ok(ReasoningConfig {
        enabled: level != stravia_runtime_contract::thinking::ThinkingLevel::Off,
        effort: Some(effort),
        display,
        level: Some(level),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests;
