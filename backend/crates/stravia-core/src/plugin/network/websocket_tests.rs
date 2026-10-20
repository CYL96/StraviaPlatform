use super::*;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
};
use tungstenite_deflate::Message as EngineMessage;

type ServerSocket = tokio_tungstenite_deflate::WebSocketStream<TcpStream>;

async fn server(extension: Option<&'static str>) -> (Url, tokio::task::JoinHandle<ServerSocket>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("ws://{}/socket", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
            assert!(request.len() < 16 * 1024);
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("GET /socket HTTP/1.1\r\n"));
        let key = request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("sec-websocket-key")
                    .then(|| value.trim())
            })
            .unwrap();
        let accept = derive_accept_key(key.as_bytes());
        let extensions = extension
            .map(|value| format!("Sec-WebSocket-Extensions: {value}\r\n"))
            .unwrap_or_default();
        stream.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nConnection: keep-alive, Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n{extensions}\r\n").as_bytes()).await.unwrap();
        // Invalid negotiation tests never use the returned engine.
        let compression = extension.and_then(|value| validate_extension(value).ok());
        tokio_tungstenite_deflate::WebSocketStream::from_raw_socket(
            stream,
            Role::Server,
            Some(options(compression)),
        )
        .await
    });
    (url, task)
}

fn preferred() -> WsCompression {
    WsCompression::PermessageDeflate(CompressionNegotiation::Preferred)
}
fn required() -> WsCompression {
    WsCompression::PermessageDeflate(CompressionNegotiation::Required)
}
fn client() -> Client {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .http1_only()
        .build()
        .unwrap()
}

async fn connect(url: &Url, compression: &WsCompression) -> Result<WebSocket, HostFailure> {
    let handshake = Handshake::new(url, &HeaderMap::new(), &[], Some(compression)).unwrap();
    let response = handshake.send(&client()).await.unwrap();
    handshake.finish(response).await
}

#[tokio::test]
async fn legacy_handshake_does_not_offer_compression_and_exchanges_application_bytes() {
    let (url, server) = server(None).await;
    let mut headers = HeaderMap::new();
    headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
    headers.insert(
        header::SEC_WEBSOCKET_EXTENSIONS,
        HeaderValue::from_static(OFFER),
    );
    let handshake = Handshake::new(&url, &headers, &[], None).unwrap();
    assert!(
        !handshake
            .headers()
            .contains_key(header::SEC_WEBSOCKET_EXTENSIONS)
    );
    assert_eq!(handshake.headers()[header::ACCEPT_ENCODING], "gzip");
    let response = handshake.send(&client()).await.unwrap();
    let mut client = handshake.finish(response).await.unwrap();
    let mut peer = server.await.unwrap();
    client.send(Message::Text("legacy".into())).await.unwrap();
    assert_eq!(
        peer.next().await.unwrap().unwrap(),
        EngineMessage::Text("legacy".into())
    );
    peer.send(EngineMessage::Text("reply".into()))
        .await
        .unwrap();
    assert!(
        matches!(client.next().await.unwrap().unwrap(), Message::Text(text) if text == "reply")
    );
}

#[tokio::test]
async fn negotiated_compression_round_trips_both_directions_and_retains_dictionary_state() {
    for extension in [
        "permessage-deflate",
        "permessage-deflate; client_no_context_takeover; server_no_context_takeover",
        "permessage-deflate; client_max_window_bits=12; server_max_window_bits=12",
    ] {
        let (url, server) = server(Some(extension)).await;
        let mut client = connect(&url, &required()).await.unwrap();
        let mut peer = server.await.unwrap();
        assert!(client.compressed());
        for _ in 0..3 {
            let payload = "压缩-message-".repeat(4096);
            client.send(Message::Text(payload.clone())).await.unwrap();
            assert_eq!(
                peer.next()
                    .await
                    .unwrap()
                    .unwrap()
                    .into_text()
                    .unwrap()
                    .as_str(),
                payload
            );
            let bytes = Bytes::from(vec![0xab; 128 * 1024]);
            peer.send(EngineMessage::Binary(bytes.clone()))
                .await
                .unwrap();
            assert!(
                matches!(client.next().await.unwrap().unwrap(), Message::Binary(actual) if actual == bytes)
            );
        }
    }
}

#[tokio::test]
async fn preferred_absent_extension_keeps_original_connection_and_required_refuses_it() {
    let (url, server) = server(None).await;
    let mut client = connect(&url, &preferred()).await.unwrap();
    let mut peer = server.await.unwrap();
    assert!(!client.compressed());
    client
        .send(Message::Text("same connection".into()))
        .await
        .unwrap();
    assert_eq!(
        peer.next()
            .await
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap()
            .as_str(),
        "same connection"
    );

    let (url, server) = self::server(None).await;
    assert!(connect(&url, &required()).await.is_err());
    let mut peer = server.await.unwrap();
    // No business data is ever sent on a failed required handshake.
    assert!(peer.next().await.is_none_or(|result| result.is_err()));
}

#[tokio::test]
async fn malformed_unoffered_and_duplicate_extensions_fail_before_business_send() {
    for extension in [
        "permessage-deflate; unknown=1",
        "permessage-deflate; server_no_context_takeover=1",
        "permessage-deflate; server_no_context_takeover; server_no_context_takeover",
        "permessage-deflate; server_max_window_bits",
        "permessage-deflate; client_max_window_bits",
        "permessage-deflate; client_max_window_bits=7",
        "permessage-deflate; client_max_window_bits=16",
        "permessage-deflate; client_max_window_bits=012",
        "permessage-deflate; server_max_window_bits=\"09\"",
        "permessage-deflate; client_max_window_bits_xyz=15",
        "permessage-deflate; client_max_window_bits=15=garbage",
        "permessage-deflate, permessage-deflate",
        "some-other-extension",
    ] {
        let (url, server) = server(Some(extension)).await;
        assert!(connect(&url, &preferred()).await.is_err(), "{extension}");
        drop(server.await.unwrap());
    }
    let (url, server) = server(Some("permessage-deflate")).await;
    assert!(connect(&url, &WsCompression::Disabled).await.is_err());
    drop(server.await.unwrap());
}

#[test]
fn quoted_window_bits_and_context_flags_map_to_actual_engine_configuration() {
    let config = validate_extension("permessage-deflate; client_max_window_bits=\"12\"; server_max_window_bits=10; client_no_context_takeover").unwrap();
    assert_eq!(config.client_max_window_bits().get(), 12);
    assert_eq!(config.server_max_window_bits().get(), 10);
    assert!(config.client_no_context_takeover);
    assert!(!config.server_no_context_takeover);
}

#[test]
fn response_integrity_rejects_nonce_protocol_version_and_duplicate_accept() {
    let handshake = Handshake::new(
        &Url::parse("ws://localhost/socket").unwrap(),
        &HeaderMap::new(),
        &["expected".into()],
        Some(&preferred()),
    )
    .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    headers.insert(
        header::SEC_WEBSOCKET_ACCEPT,
        HeaderValue::from_str(&handshake.accept).unwrap(),
    );
    assert!(
        handshake
            .validate(
                reqwest::StatusCode::SWITCHING_PROTOCOLS,
                reqwest::Version::HTTP_11,
                &headers
            )
            .is_ok()
    );
    assert!(
        handshake
            .validate(reqwest::StatusCode::OK, reqwest::Version::HTTP_11, &headers)
            .is_err()
    );
    assert!(
        handshake
            .validate(
                reqwest::StatusCode::SWITCHING_PROTOCOLS,
                reqwest::Version::HTTP_2,
                &headers
            )
            .is_err()
    );
    headers.insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_static("unoffered"),
    );
    assert!(
        handshake
            .validate(
                reqwest::StatusCode::SWITCHING_PROTOCOLS,
                reqwest::Version::HTTP_11,
                &headers
            )
            .is_err()
    );
    headers.remove(header::SEC_WEBSOCKET_PROTOCOL);
    headers.append(
        header::SEC_WEBSOCKET_ACCEPT,
        HeaderValue::from_static("wrong"),
    );
    assert!(
        handshake
            .validate(
                reqwest::StatusCode::SWITCHING_PROTOCOLS,
                reqwest::Version::HTTP_11,
                &headers
            )
            .is_err()
    );
}

#[tokio::test]
async fn fragmented_message_allows_interleaved_uncompressed_ping() {
    use tungstenite_deflate::protocol::frame::{
        Frame,
        coding::{Data, OpCode},
    };
    let (url, server) = server(Some("permessage-deflate")).await;
    let mut client = connect(&url, &preferred()).await.unwrap();
    let mut peer = server.await.unwrap();
    peer.send(EngineMessage::Frame(Frame::message(
        b"hel".to_vec(),
        OpCode::Data(Data::Text),
        false,
    )))
    .await
    .unwrap();
    peer.send(EngineMessage::Ping(Bytes::from_static(b"probe")))
        .await
        .unwrap();
    peer.send(EngineMessage::Frame(Frame::message(
        b"lo".to_vec(),
        OpCode::Data(Data::Continue),
        true,
    )))
    .await
    .unwrap();
    assert!(
        matches!(client.next().await.unwrap().unwrap(), Message::Ping(bytes) if bytes == b"probe"[..])
    );
    assert!(
        matches!(client.next().await.unwrap().unwrap(), Message::Text(text) if text == "hello")
    );
    client.socket.flush().await.unwrap();
    assert!(
        matches!(peer.next().await.unwrap().unwrap(), EngineMessage::Pong(bytes) if bytes == b"probe"[..])
    );
}

#[tokio::test]
async fn rfc7692_compressed_fragments_allow_interleaved_control_frames() {
    let (url, server) = server(Some("permessage-deflate")).await;
    let mut client = connect(&url, &required()).await.unwrap();
    let mut peer = server.await.unwrap();
    // RFC 7692 section 7.2.3.1's "Hello" example, split at 3 compressed
    // octets, with an uncompressed Ping between the two data fragments.
    peer.get_mut()
        .write_all(&[
            0x41, 0x03, 0xf2, 0x48, 0xcd, 0x89, 0x01, b'?', 0x80, 0x04, 0xc9, 0xc9, 0x07, 0x00,
        ])
        .await
        .unwrap();
    assert!(
        matches!(client.next().await.unwrap().unwrap(), Message::Ping(bytes) if bytes == b"?"[..])
    );
    assert!(
        matches!(client.next().await.unwrap().unwrap(), Message::Text(text) if text == "Hello")
    );
    client.socket.flush().await.unwrap();
    assert!(
        matches!(peer.next().await.unwrap().unwrap(), EngineMessage::Pong(bytes) if bytes == b"?"[..])
    );
}

#[tokio::test]
async fn malformed_frames_fail_strictly_and_end_the_socket() {
    for bytes in [
        &[0xc9, 0][..], // Control frames cannot carry RSV1, even with compression negotiated.
        &[0x81, 1, 0xff][..], // Text must be UTF-8.
        &[0x88, 1, 0][..], // A close status cannot contain only one byte.
        &[0xc2, 3, 0xff, 0xff, 0xff][..], // Corrupt deflate input.
    ] {
        let (url, server) = server(Some("permessage-deflate")).await;
        let mut client = connect(&url, &preferred()).await.unwrap();
        let mut peer = server.await.unwrap();
        peer.get_mut().write_all(bytes).await.unwrap();
        assert!(client.next().await.unwrap().is_err(), "{bytes:?}");
        assert!(client.next().await.is_none());
    }
}

#[tokio::test]
async fn compressed_frame_without_negotiation_is_rejected() {
    let (url, server) = server(None).await;
    let mut client = connect(&url, &WsCompression::Disabled).await.unwrap();
    let mut peer = server.await.unwrap();
    peer.get_mut()
        .write_all(&[0xc1, 0x07, 0xf2, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00])
        .await
        .unwrap();
    assert!(client.next().await.unwrap().is_err());
}

#[tokio::test]
async fn large_messages_remain_supported_and_inflation_enforces_its_budget() {
    let (url, server) = server(Some("permessage-deflate")).await;
    let mut client = connect(&url, &required()).await.unwrap();
    let mut peer = server.await.unwrap();
    let payload = Bytes::from(vec![b'x'; 17 * 1024 * 1024]);
    peer.send(EngineMessage::Binary(payload.clone()))
        .await
        .unwrap();
    assert!(
        matches!(client.next().await.unwrap().unwrap(), Message::Binary(actual) if actual == payload)
    );

    // Exercise the same production configuration with a reduced test budget;
    // the malicious expansion must fail in the engine, not in a post-read check.
    let (left, right) = tokio::io::duplex(64 * 1024);
    let mut config = options(Some(DeflateConfig::new()));
    config.max_message_size = Some(1024);
    let mut receiver = tokio_tungstenite_deflate::WebSocketStream::from_raw_socket(
        left,
        Role::Client,
        Some(config),
    )
    .await;
    let mut sender = tokio_tungstenite_deflate::WebSocketStream::from_raw_socket(
        right,
        Role::Server,
        Some(options(Some(DeflateConfig::new()))),
    )
    .await;
    sender
        .send(EngineMessage::Binary(Bytes::from(vec![0; 128 * 1024])))
        .await
        .unwrap();
    assert!(receiver.next().await.unwrap().is_err());
}
