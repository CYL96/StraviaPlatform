//! OpenAI Chat Completions ingress decoder — produces `AiRequest` directly.
//!
//! Known controls enter unified parameters; unsupported controls are dropped.

use anyhow::Result;
use serde_json::Value;

use stravia_runtime_contract::protocol::ids::OPENAI_COMPATIBLE_CHAT_COMPLETIONS_V1;
use stravia_runtime_contract::protocol::ir::AiItem;
use stravia_runtime_contract::protocol::ir::AiRequest;
use stravia_runtime_contract::protocol::ir::ContentBlock;
use stravia_runtime_contract::protocol::ir::GenerationConfig;
use stravia_runtime_contract::protocol::ir::MediaSource;
use stravia_runtime_contract::protocol::ir::MessageContent;
use stravia_runtime_contract::protocol::ir::ReasoningConfig;
use stravia_runtime_contract::protocol::ir::ResponseFormat;
use stravia_runtime_contract::protocol::ir::Role;
use stravia_runtime_contract::protocol::ir::StreamConfig;
use stravia_runtime_contract::protocol::ir::ToolCall;
use stravia_runtime_contract::protocol::ir::ToolChoice;
use stravia_runtime_contract::protocol::ir::ToolSpec;
use stravia_runtime_contract::protocol::ir::{
    AudioOutput, CacheRetention, ParameterReset, native_tool_from_chat_web_search_options,
};

use super::types::*;

pub struct OpenAIDecoder;

impl OpenAIDecoder {
    pub(crate) fn decode_request(&self, body: Value) -> Result<AiRequest> {
        let mut resets = Vec::new();
        for (field, reset) in [
            ("temperature", ParameterReset::Temperature),
            ("top_p", ParameterReset::TopP),
            ("presence_penalty", ParameterReset::PresencePenalty),
            ("frequency_penalty", ParameterReset::FrequencyPenalty),
            ("parallel_tool_calls", ParameterReset::ParallelToolCalls),
            ("store", ParameterReset::Store),
            ("prompt_cache_key", ParameterReset::CacheKey),
            ("prompt_cache_retention", ParameterReset::CacheRetention),
            ("verbosity", ParameterReset::Verbosity),
            ("service_tier", ParameterReset::ServiceTier),
            ("reasoning_effort", ParameterReset::ReasoningLevel),
            ("response_format", ParameterReset::ResponseFormat),
        ] {
            if body.get(field).is_some_and(Value::is_null) {
                resets.push(reset);
            }
        }
        if body
            .get("max_completion_tokens")
            .or_else(|| body.get("max_tokens"))
            .is_some_and(Value::is_null)
        {
            resets.push(ParameterReset::MaxTokens);
        }
        if body
            .get("safety_identifier")
            .or_else(|| body.get("user"))
            .is_some_and(Value::is_null)
        {
            resets.push(ParameterReset::EndUser);
        }
        if body.get("web_search_options").is_some_and(Value::is_null)
            || body
                .get("tools")
                .is_some_and(|value| value.is_null() || value.as_array().is_some_and(Vec::is_empty))
        {
            resets.push(ParameterReset::NativeTools);
        }
        if body
            .get("logprobs")
            .is_some_and(|v| v.is_null() || v.as_bool() == Some(false))
            || body.get("top_logprobs").is_some_and(Value::is_null)
        {
            resets.push(ParameterReset::Logprobs);
        }
        if body
            .pointer("/response_format/json_schema/strict")
            .is_some_and(Value::is_null)
        {
            resets.push(ParameterReset::ResponseFormatStrict);
        }
        let req: OpenAIRequest = serde_json::from_value(body)?;

        let messages = req
            .messages
            .into_iter()
            .map(decode_message)
            .collect::<Result<Vec<_>>>()?;

        let tools = req
            .tools
            .map(|tools_val| {
                tools_val
                    .into_iter()
                    .filter(|tool| tool.get("type").and_then(Value::as_str) == Some("function"))
                    .map(|mut tool| {
                        let func = tool
                            .get_mut("function")
                            .and_then(Value::as_object_mut)
                            .ok_or_else(|| {
                                anyhow::anyhow!("Chat tool requires a function object")
                            })?;
                        let name = func
                            .remove("name")
                            .and_then(|value| value.as_str().map(str::to_owned))
                            .ok_or_else(|| {
                                anyhow::anyhow!("Chat function tool requires a string name")
                            })?;
                        Ok(ToolSpec {
                            name,
                            description: func
                                .remove("description")
                                .and_then(|value| value.as_str().map(str::to_owned)),
                            parameters: func
                                .remove("parameters")
                                .unwrap_or(Value::Object(Default::default())),
                            strict: func.remove("strict").and_then(|value| value.as_bool()),
                            defer_loading: None,
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;

        let tool_choice = req.tool_choice.and_then(parse_tool_choice);

        // Prefer max_completion_tokens (o-models), fall back to max_tokens.
        let effective_max_tokens = req.max_completion_tokens.or(req.max_tokens);

        let reasoning = ReasoningConfig {
            level: req.reasoning_effort.as_deref().and_then(|value| {
                stravia_runtime_contract::thinking::ThinkingLevel::from_wire(value).ok()
            }),
            ..Default::default()
        };

        let include_usage = req
            .stream_options
            .as_ref()
            .map(|s| s.include_usage)
            .unwrap_or(false);

        let stop = req.stop.map(StopToken::into_vec);
        let response_format = req.response_format.map(parse_response_format);

        // ── Build AiRequest ───────────────────────────────────────────────────
        let mut ai_req = AiRequest::new(req.model, messages);
        ai_req.generation = GenerationConfig {
            temperature: req.temperature,
            max_tokens: effective_max_tokens,
            top_p: req.top_p,
            seed: req.seed,
            stop,
            frequency_penalty: req.frequency_penalty,
            presence_penalty: req.presence_penalty,
        };
        ai_req.stream = StreamConfig {
            enabled: req.stream,
            include_usage,
        };
        ai_req.tools = tools;
        ai_req.tool_choice = tool_choice;
        ai_req.parallel_tool_calls = req.parallel_tool_calls;
        ai_req.reasoning = reasoning;
        ai_req.response_format = response_format;
        ai_req.parameters.resets = resets;
        ai_req.parameters.audio = req.audio.map(|audio| AudioOutput {
            voice: audio.voice,
            format: audio.format,
        });
        ai_req.parameters.logit_bias = req.logit_bias;
        ai_req.parameters.logprobs = match req.logprobs {
            Some(false) => None,
            Some(true) => Some(req.top_logprobs.unwrap_or(0)),
            None => req.top_logprobs,
        };
        ai_req.parameters.modalities = req.modalities.map(|values| {
            values
                .into_iter()
                .filter_map(|value| serde_json::from_value(Value::String(value)).ok())
                .collect()
        });
        ai_req.parameters.candidate_count = req.n;
        ai_req.parameters.store = req.store;
        ai_req.parameters.metadata = req.metadata.and_then(|value| {
            value.as_object().map(|map| {
                map.iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_owned()))
                    })
                    .collect()
            })
        });
        ai_req.parameters.end_user_id = req.safety_identifier.or(req.user);
        ai_req.parameters.cache.key = req.prompt_cache_key;
        ai_req.parameters.cache.retention =
            req.prompt_cache_retention
                .and_then(|value| match value.as_str() {
                    "in_memory" => Some(CacheRetention::Default),
                    "24h" => Some(CacheRetention::Extended),
                    _ => None,
                });
        ai_req.parameters.verbosity = req
            .verbosity
            .and_then(|value| serde_json::from_value(Value::String(value)).ok());
        ai_req.parameters.native_tools = req
            .web_search_options
            .as_ref()
            .and_then(native_tool_from_chat_web_search_options)
            .into_iter()
            .collect();
        ai_req.parameters.service_tier = req
            .service_tier
            .and_then(|value| serde_json::from_value(Value::String(value)).ok());
        ai_req.meta.source_protocol = Some(OPENAI_COMPATIBLE_CHAT_COMPLETIONS_V1);
        ai_req.meta.vendor.ingress = Default::default();
        ai_req.normalize_parameters();

        Ok(ai_req)
    }
}

// ── Message decoding ──────────────────────────────────────────────────────────

fn decode_message(msg: OpenAIMessage) -> Result<AiItem> {
    let role = match msg.role.as_str() {
        "system" => Role::System,
        "developer" => Role::Developer,
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        other => anyhow::bail!("unknown role: {other}"),
    };

    let mut content = match msg.content {
        Some(OpenAIContent::Text(t)) => MessageContent::Text(t.into()),
        Some(OpenAIContent::Parts(parts)) => {
            let blocks = parts
                .into_iter()
                .map(|p| match p {
                    OpenAIContentPart::Text { text } => ContentBlock::Text {
                        text: text.into(),
                        cache_control: None,
                    },
                    OpenAIContentPart::ImageUrl { image_url } => {
                        let detail = image_url.detail;
                        let source = if image_url.url.starts_with("data:") {
                            crate::codec::parse_data_url_source(image_url.url)
                        } else {
                            MediaSource::Url(image_url.url)
                        };
                        ContentBlock::Image {
                            source,
                            detail,
                            cache_control: None,
                        }
                    }
                    OpenAIContentPart::InputAudio { input_audio } => ContentBlock::Audio {
                        source: MediaSource::Base64 {
                            media_type: format!("audio/{}", input_audio.format),
                            data: input_audio.data,
                        },
                    },
                })
                .collect();
            MessageContent::Blocks(blocks)
        }
        None => MessageContent::Text(String::new().into()),
    };

    let tool_calls = msg.tool_calls.map(|tcs| {
        tcs.into_iter()
            .map(|tc| ToolCall {
                id: (tc.id).into(),
                name: tc.function.name,
                arguments: tc.function.arguments,
            })
            .collect()
    });

    // Put any per-message extra fields + refusal into meta.
    let mut meta_obj = serde_json::Map::new();
    for (k, v) in msg.extra {
        if k != stravia_runtime_contract::protocol::ir::TOOL_RESULT_CONTENT_KIND_META {
            meta_obj.insert(k, v);
        }
    }
    if let Some(ref r) = msg.refusal {
        meta_obj.insert("refusal".to_string(), Value::String(r.clone()));
    }
    if role == Role::Assistant
        && let Some(reasoning) =
            super::stream::extract_reasoning_from_message(&Value::Object(meta_obj.clone()))
        && !reasoning.is_empty()
    {
        // 思考文本已进入 canonical block；保留副本会在历史恢复后再次转发旧 Preview。
        for field in ["reasoning_content", "reasoning"] {
            if meta_obj.get(field).is_some_and(Value::is_string) {
                meta_obj.remove(field);
            }
        }
        let mut blocks = vec![ContentBlock::Thinking {
            thinking: reasoning,
            signature: None,
        }];
        match content {
            MessageContent::Text(text) if !text.is_empty() => blocks.push(ContentBlock::Text {
                text,
                cache_control: None,
            }),
            MessageContent::Blocks(existing) => blocks.extend(existing),
            _ => {}
        }
        content = MessageContent::Blocks(blocks);
    }
    let meta = if meta_obj.is_empty() {
        None
    } else {
        Some(stravia_runtime_contract::protocol::ir::AiItemMetadata::boxed(Value::Object(meta_obj)))
    };

    Ok(AiItem {
        role,
        content,
        tool_calls,
        tool_call_id: (msg.tool_call_id).map(Into::into),
        meta,
    }
    .with_plain_tool_text_kind())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_tool_choice(v: Value) -> Option<ToolChoice> {
    match &v {
        Value::String(s) => match s.as_str() {
            "none" => Some(ToolChoice::None),
            "auto" => Some(ToolChoice::Auto),
            "required" => Some(ToolChoice::Required),
            _ => None,
        },
        Value::Object(obj) => {
            if obj.get("type").and_then(|t| t.as_str()) == Some("function")
                && let Some(name) = obj
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
            {
                return Some(ToolChoice::Named {
                    name: name.to_string(),
                });
            }
            None
        }
        _ => None,
    }
}

fn parse_response_format(f: ResponseFormatWire) -> ResponseFormat {
    match f {
        ResponseFormatWire::Text => ResponseFormat::Text,
        ResponseFormatWire::JsonObject => ResponseFormat::JsonObject,
        ResponseFormatWire::JsonSchema { json_schema } => ResponseFormat::JsonSchema {
            name: json_schema.name,
            description: json_schema.description,
            schema: json_schema.schema,
            strict: json_schema.strict,
        },
    }
}
