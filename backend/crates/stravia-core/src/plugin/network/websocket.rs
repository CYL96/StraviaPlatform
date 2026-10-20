//! reqwest 负责路由、TLS 与 HTTP；tungstenite 负责分帧与 RFC 7692 编解码。

use std::{collections::BTreeSet, io};

use bytes::Bytes;
use futures::{SinkExt as _, StreamExt as _};
use reqwest::{
    Client, Response, Url,
    header::{self, HeaderMap, HeaderValue},
};
use stravia_vendor_runtime::HostFailure;
use stravia_vendor_sdk::{CompressionNegotiation, WsCompression};
use tungstenite_deflate::{
    extensions::compression::deflate::DeflateConfig,
    handshake::{client::generate_key, derive_accept_key},
    protocol::{Role, WebSocketConfig},
};

/// 在解压期间检查预算，不能先分配完整展开内容再检查。
/// 单消息上限为 256 MiB，保留超过 16 MiB 的推理签名支持。
const MAX_MESSAGE_BYTES: usize = 256 * 1024 * 1024;
const OFFER: &str = "permessage-deflate; client_max_window_bits";

/// 在池查找之前检查显式入口的头所有权，避免复用绕过入口策略。
pub(super) fn validate_headers(
    headers: &HeaderMap,
    compression: Option<&WsCompression>,
) -> Result<(), HostFailure> {
    if compression.is_some() {
        for name in [
            header::ACCEPT_ENCODING,
            header::CONTENT_ENCODING,
            header::CONTENT_LENGTH,
            header::SEC_WEBSOCKET_EXTENSIONS,
            header::SEC_WEBSOCKET_KEY,
            header::SEC_WEBSOCKET_VERSION,
            header::SEC_WEBSOCKET_PROTOCOL,
            header::UPGRADE,
            header::CONNECTION,
        ] {
            if headers.contains_key(name) {
                return Err(super::invalid(
                    "guest cannot provide host-owned WebSocket handshake headers",
                ));
            }
        }
    }
    Ok(())
}

pub(super) struct Handshake {
    url: Url,
    headers: HeaderMap,
    protocols: Vec<String>,
    compression: WsCompression,
    accept: String,
}

impl Handshake {
    pub(super) fn new(
        url: &Url,
        headers: &HeaderMap,
        protocols: &[String],
        compression: Option<&WsCompression>,
    ) -> Result<Self, HostFailure> {
        let mut url = url.clone();
        let scheme = match url.scheme() {
            "ws" | "http" => "http",
            "wss" | "https" => "https",
            _ => return Err(super::invalid("invalid WebSocket URL scheme")),
        };
        url.set_scheme(scheme)
            .map_err(|_| super::invalid("invalid WebSocket URL scheme"))?;
        let mut seen = BTreeSet::new();
        for protocol in protocols {
            if protocol.is_empty() || !protocol.bytes().all(is_token) || !seen.insert(protocol) {
                return Err(super::invalid("invalid or duplicate WebSocket subprotocol"));
            }
        }
        let key = generate_key();
        let accept = derive_accept_key(key.as_bytes());
        let mut headers = headers.clone();
        if !headers.contains_key(header::HOST) {
            headers.insert(
                header::HOST,
                HeaderValue::from_str(&url[url::Position::BeforeHost..url::Position::AfterPort])
                    .map_err(|_| super::invalid("invalid WebSocket host"))?,
            );
        }
        if !headers.contains_key(header::ACCEPT) {
            headers.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
        }
        headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert(
            header::SEC_WEBSOCKET_VERSION,
            HeaderValue::from_static("13"),
        );
        headers.insert(
            header::SEC_WEBSOCKET_KEY,
            HeaderValue::from_str(&key).expect("base64 nonce"),
        );
        if compression.is_some() {
            headers.insert(
                header::ACCEPT_ENCODING,
                HeaderValue::from_static("identity"),
            );
        }
        // 旧入口从不协商扩展，即便 guest 手写了扩展头。
        headers.remove(header::SEC_WEBSOCKET_EXTENSIONS);
        if !protocols.is_empty() {
            headers.insert(
                header::SEC_WEBSOCKET_PROTOCOL,
                HeaderValue::from_str(&protocols.join(", "))
                    .map_err(|_| super::invalid("invalid WebSocket subprotocol"))?,
            );
        }
        let compression = compression.copied().unwrap_or(WsCompression::Disabled);
        if !matches!(compression, WsCompression::Disabled) {
            headers.insert(
                header::SEC_WEBSOCKET_EXTENSIONS,
                HeaderValue::from_static(OFFER),
            );
        }
        Ok(Self {
            url,
            headers,
            protocols: protocols.to_vec(),
            compression,
            accept,
        })
    }

    pub(super) fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub(super) async fn send(&self, client: &Client) -> Result<Response, reqwest::Error> {
        client
            .get(self.url.clone())
            .version(reqwest::Version::HTTP_11)
            .headers(self.headers.clone())
            .send()
            .await
    }

    pub(super) async fn finish(self, response: Response) -> Result<WebSocket, HostFailure> {
        let extension = self
            .validate(response.status(), response.version(), response.headers())
            .map_err(super::websocket_transport_failure)?;
        let compressed = extension.is_some();
        let io = response
            .upgrade()
            .await
            .map_err(super::websocket_transport_failure)?;
        let socket = tokio_tungstenite_deflate::WebSocketStream::from_raw_socket(
            io,
            Role::Client,
            Some(options(extension)),
        )
        .await;
        Ok(WebSocket {
            socket,
            compressed,
            ended: false,
        })
    }

    fn validate(
        &self,
        status: reqwest::StatusCode,
        version: reqwest::Version,
        headers: &HeaderMap,
    ) -> io::Result<Option<DeflateConfig>> {
        if status != reqwest::StatusCode::SWITCHING_PROTOCOLS
            || version != reqwest::Version::HTTP_11
        {
            return Err(protocol_error(
                "WebSocket upgrade requires HTTP/1.1 status 101",
            ));
        }
        if !contains_token(headers, header::CONNECTION, "upgrade")?
            || !single_header(headers, header::UPGRADE)?
                .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
        {
            return Err(protocol_error(
                "invalid WebSocket Connection/Upgrade headers",
            ));
        }
        if single_header(headers, header::SEC_WEBSOCKET_ACCEPT)? != Some(self.accept.as_str()) {
            return Err(protocol_error("invalid WebSocket accept nonce"));
        }
        if let Some(version) = single_header(headers, header::SEC_WEBSOCKET_VERSION)?
            && version != "13"
        {
            return Err(protocol_error("unsupported WebSocket version"));
        }
        if let Some(selected) = single_header(headers, header::SEC_WEBSOCKET_PROTOCOL)?
            && !self.protocols.iter().any(|offered| offered == selected)
        {
            return Err(protocol_error(
                "server selected an unoffered WebSocket subprotocol",
            ));
        }
        let extension = single_header(headers, header::SEC_WEBSOCKET_EXTENSIONS)?;
        match (&self.compression, extension) {
            (WsCompression::Disabled, Some(_)) => Err(protocol_error(
                "server selected an unoffered WebSocket extension",
            )),
            (WsCompression::PermessageDeflate(CompressionNegotiation::Required), None) => Err(
                protocol_error("server did not accept required permessage-deflate"),
            ),
            (_, None) => Ok(None),
            (_, Some(value)) => validate_extension(value).map(Some),
        }
    }
}

fn is_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn single_header(headers: &HeaderMap, name: header::HeaderName) -> io::Result<Option<&str>> {
    let mut values = headers.get_all(name).iter();
    let value = values
        .next()
        .map(|value| value.to_str().map(str::trim))
        .transpose()
        .map_err(protocol_error)?;
    if values.next().is_some() {
        return Err(protocol_error("duplicate WebSocket handshake header"));
    }
    Ok(value)
}

fn contains_token(
    headers: &HeaderMap,
    name: header::HeaderName,
    expected: &str,
) -> io::Result<bool> {
    let mut found = false;
    for value in headers.get_all(name) {
        for token in value
            .to_str()
            .map_err(protocol_error)?
            .split(',')
            .map(str::trim)
        {
            if token.is_empty() || !token.bytes().all(is_token) {
                return Err(protocol_error("invalid HTTP upgrade token"));
            }
            found |= token.eq_ignore_ascii_case(expected);
        }
    }
    Ok(found)
}

/// 从真实协商参数构造引擎上下文，不重复握手，也不把 offer 当作协商成功。
fn validate_extension(value: &str) -> io::Result<DeflateConfig> {
    let mut parts = value.split(';');
    if parts.next().map(str::trim) != Some("permessage-deflate") {
        return Err(protocol_error("unsupported WebSocket extension"));
    }
    let mut seen = BTreeSet::new();
    let mut config = DeflateConfig::new();
    for part in parts {
        let (name, value) = match part.trim().split_once('=') {
            Some((name, value)) => (name.trim(), Some(value.trim())),
            None => (part.trim(), None),
        };
        if !seen.insert(name) {
            return Err(protocol_error("duplicate permessage-deflate parameter"));
        }
        match name {
            "server_no_context_takeover" | "client_no_context_takeover" if value.is_none() => {
                if name == "server_no_context_takeover" {
                    config.server_no_context_takeover = true;
                } else {
                    config.client_no_context_takeover = true;
                }
            }
            "server_max_window_bits" | "client_max_window_bits" => {
                let value = value
                    .ok_or_else(|| protocol_error("missing permessage-deflate window bits"))?;
                let quoted = value.starts_with('"') && value.ends_with('"') && value.len() >= 2;
                let value = if quoted {
                    &value[1..value.len() - 1]
                } else {
                    value
                };
                let mut digits = value.bytes();
                let mut bits = 0_u8;
                let mut count = 0;
                while let Some(digit) = digits.next() {
                    let digit = if quoted && digit == b'\\' {
                        digits.next().ok_or_else(|| {
                            protocol_error("invalid quoted permessage-deflate window bits")
                        })?
                    } else {
                        digit
                    };
                    if !digit.is_ascii_digit() || count == 2 || (count == 0 && digit == b'0') {
                        return Err(protocol_error("invalid permessage-deflate window bits"));
                    }
                    bits = bits * 10 + digit - b'0';
                    count += 1;
                }
                // 服务端 8 位窗口的合法输出可由较大的解码窗口安全读取；
                // zlib 不能表达客户端 8 位压缩窗口，不能假装支持该协商结果。
                if !(8..=15).contains(&bits) || (name == "client_max_window_bits" && bits == 8) {
                    return Err(protocol_error("unsupported permessage-deflate window bits"));
                }
                config = config
                    .set_max_window_bits(
                        if name == "server_max_window_bits" {
                            Role::Server
                        } else {
                            Role::Client
                        },
                        bits.max(9),
                    )
                    .map_err(protocol_error)?;
            }
            _ => {
                return Err(protocol_error(
                    "invalid or unknown permessage-deflate parameter",
                ));
            }
        }
    }
    Ok(config)
}

fn options(compression: Option<DeflateConfig>) -> WebSocketConfig {
    let mut config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES))
        .max_write_buffer_size(MAX_MESSAGE_BYTES + 128 * 1024);
    config.extensions.permessage_deflate = compression;
    config
}

#[derive(Debug)]
pub(super) enum Message {
    Text(String),
    Binary(Bytes),
    Ping(Bytes),
    Pong(Bytes),
    Close { code: u16, reason: String },
}

pub(super) struct WebSocket {
    socket: tokio_tungstenite_deflate::WebSocketStream<reqwest::Upgraded>,
    compressed: bool,
    ended: bool,
}

impl WebSocket {
    pub(super) fn compressed(&self) -> bool {
        self.compressed
    }

    pub(super) async fn send(&mut self, message: Message) -> io::Result<()> {
        use tungstenite_deflate::{
            Message as EngineMessage,
            protocol::frame::{CloseFrame, coding::CloseCode},
        };
        let frame = match message {
            Message::Text(text) => EngineMessage::Text(text.into()),
            Message::Binary(bytes) => EngineMessage::Binary(bytes),
            Message::Ping(bytes) if bytes.len() <= 125 => EngineMessage::Ping(bytes),
            Message::Pong(bytes) if bytes.len() <= 125 => EngineMessage::Pong(bytes),
            Message::Close { code, reason } => {
                if !CloseCode::from(code).is_allowed() || reason.len() > 123 {
                    return Err(protocol_error("invalid outgoing WebSocket close frame"));
                }
                EngineMessage::Close(Some(CloseFrame {
                    code: CloseCode::from(code),
                    reason: reason.into(),
                }))
            }
            _ => {
                return Err(protocol_error(
                    "WebSocket control payload exceeds 125 bytes",
                ));
            }
        };
        self.socket.send(frame).await.map_err(io::Error::other)
    }

    pub(super) async fn next(&mut self) -> Option<io::Result<Message>> {
        if self.ended {
            return None;
        }
        let result = match self.socket.next().await? {
            Ok(frame) => message(frame),
            Err(error) => Err(io::Error::other(error)),
        };
        if result.is_err() || matches!(result, Ok(Message::Close { .. })) {
            self.ended = true;
        }
        Some(result)
    }

    pub(super) async fn close(&mut self, code: u16, reason: Option<String>) -> io::Result<()> {
        self.send(Message::Close {
            code,
            reason: reason.unwrap_or_default(),
        })
        .await
    }
}

fn message(frame: tungstenite_deflate::Message) -> io::Result<Message> {
    use tungstenite_deflate::Message as EngineMessage;
    match frame {
        EngineMessage::Text(text) => Ok(Message::Text(text.to_string())),
        EngineMessage::Binary(bytes) => Ok(Message::Binary(bytes)),
        EngineMessage::Ping(bytes) => Ok(Message::Ping(bytes)),
        EngineMessage::Pong(bytes) => Ok(Message::Pong(bytes)),
        EngineMessage::Close(close) => Ok(Message::Close {
            code: close
                .as_ref()
                .map(|close| close.code.into())
                .unwrap_or(1005),
            reason: close
                .map(|close| close.reason.to_string())
                .unwrap_or_default(),
        }),
        _ => Err(protocol_error(
            "invalid WebSocket control or continuation frame",
        )),
    }
}

fn protocol_error(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
#[path = "websocket_tests.rs"]
mod tests;
