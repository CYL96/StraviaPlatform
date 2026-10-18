use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use stravia_protocol_codec::transform::ProtocolTransform;
use stravia_runtime_contract::protocol::{
    ids::GOOGLE_GEMINI_GENERATE_CONTENT_V1BETA,
    ir::{AiErrorKind, Usage},
};
use stravia_vendor_common::common;
use stravia_vendor_sdk::{
    ErrorKind, GuestHost, MediaArtifact, MediaImageRequest, MediaImageResponse, PluginError,
    ProviderSnapshot, read_http_body,
};

use crate::{client, wire};

const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ENCODED_IMAGE_BYTES: usize = MAX_IMAGE_BYTES.div_ceil(3) * 4;
const MAX_RESPONSE_BYTES: usize = MAX_ENCODED_IMAGE_BYTES + 2 * 1024 * 1024;
const IMAGE_INSTRUCTION: &str = "You are an AI image generator. Generate images based on user descriptions. Focus on creating high-quality, visually appealing images that match the user's request.";

pub(crate) fn generate(
    host: &GuestHost,
    provider: &ProviderSnapshot,
    request: MediaImageRequest,
) -> Result<MediaImageResponse, PluginError> {
    let model = provider
        .model_metadata
        .as_ref()
        .and_then(|metadata| metadata.selector.as_deref())
        .filter(|model| !model.trim().is_empty())
        .or(provider
            .model
            .as_deref()
            .filter(|model| !model.trim().is_empty()))
        .ok_or_else(|| invalid("Antigravity requires an upstream image model"))?;
    let body = build_request(&request)?;
    let project = client::project(host, provider)?;
    let envelope = json!({
        "project":project, "model":model, "request":body,
        "requestType":"agent", "requestId":format!("agent-{}", uuid::Uuid::new_v4()),
        "userAgent":"antigravity"
    });
    let body = serde_json::to_vec(&envelope)
        .map_err(|_| invalid("Antigravity image request could not be encoded"))?;
    let mut headers = client::headers(provider)?;
    headers.push(("accept".into(), "text/event-stream".into()));
    host.emit_started()?;
    let response = host.http_start(stravia_vendor_sdk::wit::types::HttpRequest {
        method: "POST".into(),
        url: format!(
            "{}?alt=sse",
            client::endpoint(provider, "streamGenerateContent")
        ),
        headers,
        body,
    })?;
    let status = response.status()?;
    let headers = response.headers()?;
    if !(200..300).contains(&status) {
        let body = read_http_body(&response, client::MAX_JSON_BODY)?;
        return Err(common::upstream_error(status, &headers, &body));
    }
    let mut framing = wire::EventFraming::with_limit(MAX_RESPONSE_BYTES);
    let mut result = ImageStream::default();
    let mut received = 0usize;
    while let Some(chunk) = response.read_body()? {
        received = received
            .checked_add(chunk.len())
            .filter(|size| *size <= MAX_RESPONSE_BYTES)
            .ok_or_else(|| exhausted("Antigravity image response exceeds limit"))?;
        framing.push(&chunk, |payload| result.event(payload, &headers))?;
    }
    framing.finish(|payload| result.event(payload, &headers))?;
    result.finish()
}

fn build_request(request: &MediaImageRequest) -> Result<Value, PluginError> {
    if request.prompt.trim().is_empty() {
        return Err(invalid("Antigravity image prompt must not be empty"));
    }
    let mut parts = Vec::with_capacity(request.references.len() + 1);
    for reference in &request.references {
        if reference.bytes.is_empty()
            || reference.bytes.len() > MAX_IMAGE_BYTES
            || !image_mime(&reference.media_type)
        {
            return Err(invalid(
                "Antigravity references must contain non-empty image bytes of at most 32 MiB",
            ));
        }
        parts.push(json!({"inlineData":{"mimeType":reference.media_type, "data":STANDARD.encode(&reference.bytes)}}));
    }
    parts.push(json!({"text":request.prompt}));
    let mut config = json!({"responseModalities":["IMAGE"], "candidateCount":1});
    if request.aspect_ratio.is_some() || request.resolution.is_some() {
        let mut image = json!({});
        if let Some(ratio) = request.aspect_ratio {
            image["aspectRatio"] = json!(ratio);
        }
        if let Some(size) = request.resolution {
            image["imageSize"] = json!(size);
        }
        config["imageConfig"] = image;
    }
    Ok(json!({
        "contents":[{"role":"user", "parts":parts}],
        "systemInstruction":{"parts":[{"text":IMAGE_INSTRUCTION}]},
        "generationConfig":config
    }))
}

#[derive(Default)]
struct ImageStream {
    image: Option<MediaArtifact>,
    terminal: bool,
    usage: Option<Usage>,
}

impl ImageStream {
    fn event(&mut self, payload: &[u8], headers: &[(String, String)]) -> Result<(), PluginError> {
        if payload == b"[DONE]" {
            return Ok(());
        }
        let mut value: Value = serde_json::from_slice(payload)
            .map_err(|_| upstream("Antigravity image stream contained invalid JSON"))?;
        if let Some(error) = wire::response_error(&value, headers) {
            return Err(error);
        }
        let mut inner = value.get_mut("response").map(Value::take).unwrap_or(value);
        if let Some(candidates) = inner.get("candidates").and_then(Value::as_array) {
            if candidates.len() > 1 {
                return Err(upstream("Antigravity returned multiple image candidates"));
            }
            for candidate in candidates {
                if candidate
                    .get("index")
                    .is_some_and(|index| index.as_u64() != Some(0))
                {
                    return Err(upstream(
                        "Antigravity returned an unexpected image candidate",
                    ));
                }
                if self.terminal {
                    return Err(upstream(
                        "Antigravity image data followed its finish reason",
                    ));
                }
                for part in candidate
                    .pointer("/content/parts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(inline) = part.get("inlineData") {
                        if self.image.is_some() {
                            return Err(upstream("Antigravity returned multiple images"));
                        }
                        self.image = Some(decode_image(inline)?);
                    }
                }
                if let Some(reason) = candidate
                    .get("finishReason")
                    .and_then(Value::as_str)
                    .filter(|reason| !reason.is_empty())
                {
                    if reason != "STOP" {
                        return Err(upstream(
                            "Antigravity image generation did not finish successfully",
                        ));
                    }
                    self.terminal = true;
                }
            }
        }
        if inner.get("usageMetadata").is_some() {
            if self.terminal {
                wire::normalize_final_usage(&mut inner);
            }
            // 复用 Gemini codec 的已确认用量语义，包括思考与缓存计数。
            let usage_response = json!({"usageMetadata":inner.get("usageMetadata")});
            let protocol = GOOGLE_GEMINI_GENERATE_CONTENT_V1BETA;
            self.usage = Some(
                ProtocolTransform::global()
                    .bind(protocol, protocol)
                    .and_then(|pair| pair.decode_response(usage_response))
                    .map_err(common::map_response_transform_error)?
                    .usage,
            );
        }
        Ok(())
    }

    fn finish(self) -> Result<MediaImageResponse, PluginError> {
        if !self.terminal {
            return Err(common::model_error(
                AiErrorKind::UnexpectedEof,
                "Antigravity image stream ended before a finish reason",
            ));
        }
        let image = self
            .image
            .ok_or_else(|| upstream("Antigravity image generation returned no image"))?;
        Ok(MediaImageResponse {
            artifacts: vec![image],
            revised_prompt: None,
            usage: self.usage,
        })
    }
}

fn decode_image(inline: &Value) -> Result<MediaArtifact, PluginError> {
    let mime = inline
        .get("mimeType")
        .and_then(Value::as_str)
        .ok_or_else(|| upstream("Antigravity image MIME is missing"))?;
    if !image_mime(mime) {
        return Err(upstream("Antigravity returned invalid image MIME"));
    }
    let encoded = inline
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| upstream("Antigravity image bytes are missing"))?;
    if encoded.is_empty() || encoded.len() % 4 != 0 || encoded.len() > MAX_ENCODED_IMAGE_BYTES {
        return Err(upstream("Antigravity image has an invalid encoded size"));
    }
    let padding = encoded
        .as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'=')
        .count();
    if padding > 2 || encoded.len() / 4 * 3 - padding > MAX_IMAGE_BYTES {
        return Err(exhausted("Antigravity image exceeds the 32 MiB limit"));
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| upstream("Antigravity image contains invalid base64"))?;
    if bytes.is_empty() {
        return Err(upstream("Antigravity returned empty image bytes"));
    }
    Ok(MediaArtifact {
        media_type: mime.into(),
        bytes,
        upstream_ref: None,
        metadata: Default::default(),
    })
}

// 像素解码和实际格式校验由 Core 统一执行，插件不维护第二套格式识别表。
fn image_mime(mime: &str) -> bool {
    mime.get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image/"))
        && mime.len() > 6
        && mime
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'+' | b'.'))
}

fn invalid(message: &str) -> PluginError {
    common::plugin_error(ErrorKind::Invalid, message)
}
fn upstream(message: &str) -> PluginError {
    common::model_error(AiErrorKind::ServerError, message)
}
fn exhausted(message: &str) -> PluginError {
    common::plugin_error(ErrorKind::ResourceExhausted, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stravia_vendor_sdk::MediaReference;

    fn png() -> Vec<u8> {
        STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=").unwrap()
    }

    #[test]
    fn stream_requires_one_valid_image_and_successful_finish() {
        let event = json!({"response":{"candidates":[{"content":{"parts":[{"inlineData":{"mimeType":"image/png", "data":STANDARD.encode(png())}}]}}]}});
        let mut stream = ImageStream::default();
        stream
            .event(&serde_json::to_vec(&event).unwrap(), &[])
            .unwrap();
        assert!(stream.finish().is_err());
        let mut stream = ImageStream::default();
        stream
            .event(&serde_json::to_vec(&event).unwrap(), &[])
            .unwrap();
        assert!(
            stream
                .event(&serde_json::to_vec(&event).unwrap(), &[])
                .is_err()
        );
        let mut stream = ImageStream::default();
        stream
            .event(&serde_json::to_vec(&event).unwrap(), &[])
            .unwrap();
        stream
            .event(
                br#"{"response":{"candidates":[{"finishReason":"STOP"}]}}"#,
                &[],
            )
            .unwrap();
        assert_eq!(stream.finish().unwrap().artifacts[0].bytes, png());
    }

    #[test]
    fn invalid_images_and_empty_success_are_rejected() {
        for inline in [
            json!({"mimeType":"text/plain","data":"dGV4dA=="}),
            json!({"mimeType":"image/png","data":"!!!!"}),
            json!({"mimeType":"image/png","data":""}),
        ] {
            assert!(decode_image(&inline).is_err());
        }
        let mut stream = ImageStream::default();
        stream
            .event(
                br#"{"response":{"candidates":[{"finishReason":"STOP"}]}}"#,
                &[],
            )
            .unwrap();
        assert!(stream.finish().is_err());
    }

    #[test]
    fn empty_prompts_and_invalid_references_are_rejected() {
        let mut request = MediaImageRequest {
            prompt: " ".into(),
            references: vec![],
            aspect_ratio: None,
            resolution: None,
        };
        assert!(matches!(
            build_request(&request).unwrap_err().kind,
            ErrorKind::Invalid
        ));
        request.prompt = "paint".into();
        for reference in [
            MediaReference {
                media_type: "image/png".into(),
                bytes: Vec::new(),
            },
            MediaReference {
                media_type: "text/plain".into(),
                bytes: png(),
            },
            MediaReference {
                media_type: "image/".into(),
                bytes: png(),
            },
            MediaReference {
                media_type: "image/png".into(),
                bytes: vec![0; MAX_IMAGE_BYTES + 1],
            },
        ] {
            request.references = vec![reference];
            assert!(matches!(
                build_request(&request).unwrap_err().kind,
                ErrorKind::Invalid
            ));
        }
    }

    #[test]
    fn stream_rejects_candidate_mixing_failed_finish_and_upstream_error() {
        for event in [
            json!({"response":{"candidates":[{}, {}]}}),
            json!({"response":{"candidates":[{"index":1}]}}),
            json!({"response":{"candidates":[{"finishReason":"MAX_TOKENS"}]}}),
            json!({"error":{"code":429,"message":"rate limited"}}),
            json!({"response":{"error":{"code":503,"message":"unavailable"}}}),
        ] {
            assert!(
                ImageStream::default()
                    .event(&serde_json::to_vec(&event).unwrap(), &[])
                    .is_err()
            );
        }
    }
}
