use std::collections::BTreeMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use base64::Engine as _;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

use super::Gateway;
use crate::admin::provider_allowance::{
    AllowanceCondition, AllowanceKind, ExhaustionForecastStatus, ProviderAllowanceStatus,
};
use crate::auth::types::AuthBindingStatus;
use crate::auth::{
    AuthCompletionInput, AuthCompletionValue, AuthSessionCandidate, AuthSessionStatusData,
    OAuthCallbackMode, OAuthSessionStartOptions,
};
use crate::config::GatewayConfig;
use crate::db::models::{CreateProvider, ProviderCredentialInput, ProviderSourceInput};

const DEEPSEEK_HOST: &str = "api.deepseek.com";
const DEVIN_HOST: &str = "server.codeium.com";
const DEVIN_BASE_URL: &str = "https://server.codeium.com";
const DEVIN_TOKEN_PATH: &str =
    "/exa.seat_management_pb.SeatManagementService/ExchangeDevinCLIPKCECode";
const DEVIN_ALLOWANCE_PATH: &str = "/exa.seat_management_pb.SeatManagementService/GetUserStatus";
const DEVIN_MODELS_PATH: &str = "/exa.api_server_pb.ApiServerService/GetCliModelConfigs";

#[derive(Clone, Debug)]
struct ObservedRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

#[derive(Clone)]
struct LocalResponse {
    status: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    release: Option<Arc<Notify>>,
}

impl LocalResponse {
    fn json(body: Value) -> Self {
        Self {
            status: "200 OK",
            content_type: "application/json",
            body: serde_json::to_vec(&body).expect("fixture JSON"),
            release: None,
        }
    }

    fn protobuf(body: Vec<u8>) -> Self {
        Self {
            status: "200 OK",
            content_type: "application/proto",
            body,
            release: None,
        }
    }
}

async fn write_local_response(
    socket: &mut (impl AsyncWrite + Unpin),
    response: LocalResponse,
) -> io::Result<()> {
    if let Some(release) = response.release {
        release.notified().await;
    }
    let head = format!(
        "HTTP/1.1 {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(&response.body).await
}

// 使用真实时间限制测试等待，并持续yield，避免暂停的Tokio时钟自动推进。
async fn with_real_time_limit<F: Future>(future: F) -> anyhow::Result<F::Output> {
    let until = std::time::Instant::now() + Duration::from_secs(10);
    tokio::pin!(future);
    loop {
        anyhow::ensure!(
            std::time::Instant::now() < until,
            "loopback operation exceeded ten seconds of real time"
        );
        tokio::select! {
            biased;
            result = &mut future => return Ok(result),
            _ = tokio::task::yield_now() => {}
        }
    }
}

struct LoopbackOnlyResolver {
    address: SocketAddr,
}

impl reqwest::dns::Resolve for LoopbackOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        if matches!(name.as_str(), DEEPSEEK_HOST | DEVIN_HOST) {
            let addresses: reqwest::dns::Addrs = Box::new(std::iter::once(self.address));
            Box::pin(std::future::ready(Ok(addresses)))
        } else {
            let error = io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("test DNS blocked undeclared host {}", name.as_str()),
            );
            Box::pin(std::future::ready(Err(error.into())))
        }
    }
}

struct LocalTlsServer {
    address: SocketAddr,
    certificate_der: Vec<u8>,
    requests: mpsc::UnboundedReceiver<ObservedRequest>,
    task: tokio::task::JoinHandle<anyhow::Result<Vec<ObservedRequest>>>,
}

async fn local_tls_server(
    request_count: usize,
    response: impl Fn(&ObservedRequest) -> LocalResponse + Send + Sync + 'static,
) -> anyhow::Result<LocalTlsServer> {
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec![DEEPSEEK_HOST.to_string(), DEVIN_HOST.to_string()])?;
    let certificate_der = cert.der().to_vec();
    let private_key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(signing_key.serialize_der()));
    let mut tls_config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(vec![cert.der().clone()], private_key)?;
    tls_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(tls_config));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let response = Arc::new(response);
    let (request_tx, requests) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut observed = Vec::with_capacity(request_count);
        for _ in 0..request_count {
            let (socket, _) = listener.accept().await?;
            let mut socket = acceptor.accept(socket).await?;
            let request = read_http_request(&mut socket).await?;
            request_tx
                .send(request.clone())
                .map_err(|_| anyhow::anyhow!("request observer dropped"))?;
            let reply = response(&request);
            observed.push(request);
            // 取消或超时可能在fixture释放响应前关闭客户端连接。
            if let Err(error) = write_local_response(&mut socket, reply).await {
                tracing::debug!(
                    ?error,
                    "local TLS fixture client closed before reply completed"
                );
            }
        }
        Ok(observed)
    });
    Ok(LocalTlsServer {
        address,
        certificate_der,
        requests,
        task,
    })
}

async fn read_http_request(
    socket: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> anyhow::Result<ObservedRequest> {
    let mut bytes = Vec::new();
    let mut scratch = [0_u8; 4096];
    let (header_end, content_length) = loop {
        let read = socket.read(&mut scratch).await?;
        anyhow::ensure!(
            read != 0,
            "local TLS upstream closed before request headers"
        );
        bytes.extend_from_slice(&scratch[..read]);
        if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let header_text = std::str::from_utf8(&bytes[..header_end])?;
            let content_length = header_text
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            break (header_end + 4, content_length);
        }
        anyhow::ensure!(
            bytes.len() <= 1024 * 1024,
            "local TLS request headers too large"
        );
    };
    while bytes.len() < header_end + content_length {
        let read = socket.read(&mut scratch).await?;
        anyhow::ensure!(read != 0, "local TLS upstream closed before request body");
        bytes.extend_from_slice(&scratch[..read]);
    }

    let header_text = std::str::from_utf8(&bytes[..header_end - 4])?;
    let mut lines = header_text.lines();
    let mut request_line = lines.next().unwrap_or_default().split_ascii_whitespace();
    let method = request_line.next().unwrap_or_default().to_string();
    let path = request_line.next().unwrap_or_default().to_string();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    Ok(ObservedRequest {
        method,
        path,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

async fn gateway_for(
    server: &LocalTlsServer,
    vendors: &[&str],
) -> anyhow::Result<(tempfile::TempDir, Gateway)> {
    let directory = tempfile::tempdir()?;
    let mut gateway = Gateway::from_storage(
        GatewayConfig {
            data_dir: directory.path().to_path_buf(),
            ..Default::default()
        },
        Arc::new(crate::storage::MemoryStorage::new(
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )),
    )
    .await?;
    for vendor in vendors {
        crate::plugin::test_support::install_distributed_vendor(&gateway, vendor).await?;
    }
    let root = reqwest::tls::Certificate::from_der(&server.certificate_der)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .https_only(true)
        .http1_only()
        .tls_certs_only([root])
        .dns_resolver(LoopbackOnlyResolver {
            address: server.address,
        })
        .build()?;
    gateway.vendor_http_client = client.clone();
    gateway.vendor_websocket_client = client;
    Ok((directory, gateway))
}

async fn create_vendor_provider(
    gateway: &Gateway,
    name: &str,
    vendor: &str,
    channel: &str,
    protocol: &str,
    base_url: &str,
    credential: ProviderCredentialInput,
) -> anyhow::Result<crate::db::models::Provider> {
    gateway
        .admin()
        .create_provider(CreateProvider {
            name: Some(name.into()),
            source: ProviderSourceInput::Custom {
                vendor: vendor.into(),
                channel: channel.into(),
                protocol: Some(protocol.into()),
                base_url: base_url.into(),
                models_source: None,
                static_models: None,
            },
            credential,
            vendor_options: Default::default(),
            use_proxy: false,
        })
        .await
}

fn proto_varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return out;
        }
    }
}

fn proto_u64(field: u32, value: u64) -> Vec<u8> {
    let mut out = proto_varint(u64::from(field) << 3);
    out.extend(proto_varint(value));
    out
}

fn proto_len(field: u32, value: &[u8]) -> Vec<u8> {
    let mut out = proto_varint((u64::from(field) << 3) | 2);
    out.extend(proto_varint(value.len() as u64));
    out.extend_from_slice(value);
    out
}

fn proto_string(field: u32, value: &str) -> Vec<u8> {
    proto_len(field, value.as_bytes())
}

fn devin_allowance_fixture() -> Vec<u8> {
    let mut plan = Vec::new();
    plan.extend(proto_u64(1, 17));
    plan.extend(proto_string(2, "Devin Max Contract"));
    plan.extend(proto_u64(12, 100));
    plan.extend(proto_u64(13, 10));

    let mut status = Vec::new();
    status.extend(proto_len(1, &plan));
    // Proto3 omits the exhausted daily window's zero remaining percentage.
    status.extend(proto_u64(15, 33));
    status.extend(proto_u64(16, 12_500_000));
    status.extend(proto_u64(17, 2_000_000_000));
    status.extend(proto_u64(18, 2_000_100_000));

    let mut user = Vec::new();
    user.extend(proto_len(13, &status));
    user.extend(proto_u64(28, 25));
    user.extend(proto_u64(29, 4));
    proto_len(1, &user)
}

fn query_parameter(url: &str, key: &str) -> anyhow::Result<String> {
    reqwest::Url::parse(url)?
        .query_pairs()
        .find_map(|(name, value)| (name == key).then(|| value.into_owned()))
        .ok_or_else(|| anyhow::anyhow!("missing `{key}` in authorization URL"))
}

async fn start_devin_session(
    gateway: &Gateway,
) -> anyhow::Result<crate::auth::AuthSessionInitData> {
    gateway
        .admin()
        .init_oauth_session(
            AuthSessionCandidate {
                vendor_id: "devin".into(),
                channel: "devin".into(),
                provider_id: None,
                base_url: DEVIN_BASE_URL.into(),
                protocol: Some("devin-connect".into()),
                options: Default::default(),
                credentials: Default::default(),
                use_proxy: false,
            },
            OAuthSessionStartOptions {
                callback_mode: OAuthCallbackMode::Auto,
                redirect_uri: "http://127.0.0.1:18765/oauth/callback".into(),
                listener_port: Some(18765),
                fallback_reason: None,
            },
        )
        .await
}

fn devin_oauth_provider(name: &str) -> CreateProvider {
    CreateProvider {
        name: Some(name.into()),
        source: ProviderSourceInput::Custom {
            vendor: "devin".into(),
            channel: "devin".into(),
            protocol: Some("devin-connect".into()),
            base_url: DEVIN_BASE_URL.into(),
            models_source: None,
            static_models: None,
        },
        credential: ProviderCredentialInput::None,
        vendor_options: Default::default(),
        use_proxy: false,
    }
}

#[tokio::test]
async fn deepseek_allowance_uses_real_tls_get_and_preserves_multi_currency_unknowns()
-> anyhow::Result<()> {
    let mut server = local_tls_server(1, |_| {
        LocalResponse::json(json!({
            "is_available": true,
            "balance_infos": [
                {"currency": "CNY", "total_balance": "12.50"},
                {"currency": "USD", "total_balance": "3.25"},
                {"total_balance": "4.75"}
            ]
        }))
    })
    .await?;
    let (_directory, gateway) = gateway_for(&server, &[]).await?;
    let provider = create_vendor_provider(
        &gateway,
        "DeepSeek TLS allowance",
        "deepseek",
        "default",
        "openai-compatible",
        "https://api.deepseek.com",
        ProviderCredentialInput::ApiKey {
            value: "deepseek-contract-secret".into(),
        },
    )
    .await?;

    let snapshot = gateway
        .admin()
        .refresh_provider_allowance(&provider.id)
        .await?
        .expect("DeepSeek supports allowance");
    assert_eq!(snapshot.status, ProviderAllowanceStatus::Fresh);
    assert_eq!(snapshot.allowances.len(), 3);
    for (key, value, currency) in [
        ("credits_balance_cny", 12.5, Some("CNY")),
        ("credits_balance_usd", 3.25, Some("USD")),
        ("credits_balance", 4.75, None),
    ] {
        let allowance = snapshot
            .allowances
            .iter()
            .find(|allowance| allowance.key == key)
            .unwrap_or_else(|| panic!("missing allowance {key}"));
        assert_eq!(allowance.kind, AllowanceKind::Balance);
        let remaining = allowance.remaining.as_ref().expect("remaining balance");
        assert_eq!(remaining.value, value);
        assert_eq!(remaining.currency.as_deref(), currency);
        assert_eq!(allowance.condition, None);
        assert_eq!(allowance.forecast.status, ExhaustionForecastStatus::Unknown);
    }

    let request = server.requests.recv().await.expect("DeepSeek request");
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/user/balance");
    assert_eq!(
        request.headers.get("host").map(String::as_str),
        Some(DEEPSEEK_HOST)
    );
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer deepseek-contract-secret")
    );
    assert!(request.body.is_empty());
    server.task.await??;
    Ok(())
}

#[tokio::test]
async fn guard_save_after_suspension_submission_gets_successor_evidence() -> anyhow::Result<()> {
    let release_successor = Arc::new(Notify::new());
    let server_release = release_successor.clone();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_calls = calls.clone();
    let mut server = local_tls_server(2, move |_| {
        let successor = server_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) != 0;
        // Devin retains exhausted daily/weekly windows, unlike DeepSeek's
        // zero-balance currency filtering. Omitted percentages are proto3 zero.
        let mut plan = proto_u64(1, 17);
        plan.extend(proto_string(2, "Guard publication contract"));
        plan.extend(proto_u64(12, 100));
        plan.extend(proto_u64(13, 10));
        let mut status = proto_len(1, &plan);
        if successor {
            status.extend(proto_u64(14, 10));
        }
        status.extend(proto_u64(17, 2_000_000_000));
        status.extend(proto_u64(18, 2_000_100_000));
        let user = proto_len(13, &status);
        let mut response = LocalResponse::protobuf(proto_len(1, &user));
        if successor {
            response.release = Some(server_release.clone());
        }
        response
    })
    .await?;
    // Exercise the production SQLite provider repository while retaining an
    // independently gateable memory sample store for the publication boundary.
    let directory = tempfile::tempdir()?;
    let mut gateway = Gateway::new(GatewayConfig {
        data_dir: directory.path().to_path_buf(),
        ..Default::default()
    })
    .await?;
    // Keep the sample tail independently gateable without locking provider DB writes.
    gateway.allowance_samples = crate::admin::provider_allowance::AllowanceSampleStore::memory();
    crate::plugin::test_support::install_distributed_vendor(&gateway, "devin").await?;
    let root = reqwest::tls::Certificate::from_der(&server.certificate_der)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .https_only(true)
        .http1_only()
        .tls_certs_only([root])
        .dns_resolver(LoopbackOnlyResolver {
            address: server.address,
        })
        .build()?;
    gateway.vendor_http_client = client.clone();
    gateway.vendor_websocket_client = client;
    let authorization = gateway
        .admin()
        .init_oauth_session(
            AuthSessionCandidate {
                vendor_id: "devin".into(),
                channel: "devin".into(),
                provider_id: None,
                base_url: DEVIN_BASE_URL.into(),
                protocol: Some("devin-connect".into()),
                options: Default::default(),
                credentials: Default::default(),
                use_proxy: false,
            },
            OAuthSessionStartOptions {
                callback_mode: OAuthCallbackMode::Manual,
                redirect_uri: "chisel-show-auth-token".into(),
                listener_port: None,
                fallback_reason: None,
            },
        )
        .await?;
    gateway
        .admin()
        .complete_oauth_session(
            &authorization.session_id,
            AuthCompletionInput {
                input: AuthCompletionValue::Manual {
                    value: "session_token=devin-contract-token".into(),
                },
            },
        )
        .await?;
    let provider = gateway
        .admin()
        .create_provider_with_oauth_session(
            &authorization.session_id,
            devin_oauth_provider("Guard publication barrier"),
        )
        .await?;
    let crate::admin::provider_allowance::AllowanceSampleStore::Memory(rows) =
        &gateway.allowance_samples
    else {
        panic!("memory gateway must use the real memory sample store")
    };
    // This is the real sample-store write lock: hold A after its suspension
    // submission but before forecasts/cache publication, without blocking guards.
    let publication_gate = rows.write().await;
    let first = gateway
        .admin()
        .replace_provider_allowance_guards(&provider.id, vec!["daily".into()])
        .await
        .context("guard successor regression: saving A guard keys")?
        .unwrap();
    assert!(first.target.refreshing);
    let request = tokio::time::timeout(Duration::from_secs(10), server.requests.recv())
        .await
        .context("guard successor regression: waiting for first upstream request")?
        .expect("first allowance request");
    assert_eq!(request.path, DEVIN_ALLOWANCE_PATH);
    // Observe committed consumer-visible evidence, not inflight internals.
    // The lifecycle also consumes state.changed(), so its Notify cannot serve
    // as an exclusive test gate. This bounded yield loop waits only for proof
    // that A submitted; the held real sample lock keeps its publication pending.
    let submitted = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(suspension) = gateway
                .admin()
                .get_provider_allowance_suspension(&provider.id)
                .await?
            {
                break Ok::<_, anyhow::Error>(suspension);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("guard successor regression: waiting for A persisted suspension")??;
    assert_eq!(submitted.triggered_keys, vec!["daily"]);
    let keys = vec!["daily".into(), "weekly".into()];
    let saved = tokio::time::timeout(
        Duration::from_secs(5),
        gateway
            .admin()
            .replace_provider_allowance_guards(&provider.id, keys),
    )
    .await
    .context("guard successor regression: waiting for B guard save")??
    .unwrap();
    assert_eq!(saved.keys, vec!["daily", "weekly"]);
    assert!(saved.target.refreshing);
    // C must merge with B while B waits for A's complete publication.
    let merged = tokio::time::timeout(
        Duration::from_secs(5),
        gateway
            .admin()
            .replace_provider_allowance_guards(&provider.id, saved.keys.clone()),
    )
    .await
    .context("guard successor regression: waiting for C guard save")??
    .unwrap();
    assert_eq!(merged.keys, saved.keys);
    assert!(server.requests.try_recv().is_err());
    let gw = gateway.clone();
    let id = provider.id.clone();
    let mut pending = tokio::spawn(async move { gw.admin().get_provider_allowance(&id).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut pending)
            .await
            .is_err()
    );
    drop(publication_gate);
    let request = tokio::time::timeout(Duration::from_secs(10), server.requests.recv())
        .await
        .context("guard successor regression: waiting for successor upstream request")?
        .expect("successor allowance request");
    assert_eq!(request.path, DEVIN_ALLOWANCE_PATH);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    // A's cleanup must not remove B: a later GET still joins held B, not A's cache.
    let gw = gateway.clone();
    let id = provider.id.clone();
    let mut later = tokio::spawn(async move { gw.admin().get_provider_allowance(&id).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut later)
            .await
            .is_err()
    );
    release_successor.notify_one();
    let snapshot = tokio::time::timeout(Duration::from_secs(10), pending)
        .await
        .context("guard successor regression: waiting for B snapshot")???
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), later)
            .await
            .context("guard successor regression: waiting for later GET snapshot")???
            .unwrap(),
        snapshot
    );
    assert_eq!(snapshot.status, ProviderAllowanceStatus::Fresh);
    let daily = snapshot
        .allowances
        .iter()
        .find(|item| item.key == "daily")
        .unwrap();
    assert_eq!(daily.used_percent, Some(90.0));
    assert!(
        snapshot
            .allowances
            .iter()
            .filter(|item| matches!(item.key.as_str(), "daily" | "weekly"))
            .all(|item| item.guarded)
    );
    assert_eq!(
        snapshot.suspension.as_ref().unwrap().triggered_keys,
        vec!["weekly"]
    );
    assert_eq!(
        gateway
            .admin()
            .get_provider_allowance_suspension(&provider.id)
            .await?
            .unwrap()
            .triggered_keys,
        vec!["weekly"]
    );
    assert_eq!(
        gateway
            .admin()
            .get_provider_allowance(&provider.id)
            .await?
            .unwrap(),
        snapshot
    );
    assert_eq!(server.task.await??.len(), 2);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn gateway_background_discovery_and_allowance_keep_distinct_phases_and_stop_on_shutdown()
-> anyhow::Result<()> {
    let first_sample = Arc::new(Notify::new());
    let first_release = Arc::clone(&first_sample);
    // 后台任务在构造时捕获HTTP客户端；使用字面量loopback地址而非替换前台客户端。
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let (request_tx, mut requests) = mpsc::unbounded_channel();
    let server_task = tokio::spawn(async move {
        let mut first_allowance = true;
        for _ in 0..5 {
            let (mut socket, _) = listener.accept().await?;
            let request = read_http_request(&mut socket).await?;
            if request.path != "/oauth/exchange" {
                request_tx
                    .send(request.clone())
                    .map_err(|_| anyhow::anyhow!("request observer dropped"))?;
            }
            let response = match request.path.as_str() {
                "/oauth/exchange" => LocalResponse::json(json!({
                    "access_token": "schedule-token",
                    "refresh_token": "schedule-refresh-token",
                    "expires_at_unix_ms": 4_102_444_800_000_i64
                })),
                "/allowance" => {
                    if first_allowance {
                        first_allowance = false;
                        let mut response =
                            LocalResponse::json(json!({"error": "temporarily unavailable"}));
                        response.status = "503 Service Unavailable";
                        response.release = Some(Arc::clone(&first_release));
                        response
                    } else {
                        LocalResponse::json(json!({"remaining": "7.25"}))
                    }
                }
                "/models" => LocalResponse::json(json!({
                    "models": [{"id": "scheduled-model", "display_name": "Scheduled Model", "family": "management", "selector": "management-selector"}],
                    "next_cursor": null
                })),
                path => panic!("unexpected scheduled upstream request: {path}"),
            };
            write_local_response(&mut socket, response).await?;
        }
        Ok::<(), anyhow::Error>(())
    });
    tokio::time::pause();
    let clock_guard = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    let directory = tempfile::tempdir()?;
    let gateway = Gateway::from_storage(
        GatewayConfig {
            data_dir: directory.path().to_path_buf(),
            ..Default::default()
        },
        Arc::new(crate::storage::MemoryStorage::new(
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )),
    )
    .await?;
    let artifact = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("target/vendor-test-fixtures/management-v2.wasm");
    let preview = gateway
        .admin()
        .preview_vendor_plugin(std::fs::read(&artifact)?)
        .await?;
    gateway
        .admin()
        .confirm_vendor_plugin(crate::plugin::ConfirmPluginUpdate {
            preview_id: preview.id,
            allow_data_discard: false,
        })
        .await?;
    let authorization = gateway
        .admin()
        .init_oauth_session(
            AuthSessionCandidate {
                vendor_id: "fixture.management".into(),
                channel: "default".into(),
                provider_id: None,
                base_url: base_url.clone(),
                protocol: Some("fixture-management".into()),
                options: BTreeMap::from([("workspace".into(), json!("schedule-workspace"))]),
                credentials: BTreeMap::new(),
                use_proxy: false,
            },
            OAuthSessionStartOptions {
                callback_mode: OAuthCallbackMode::Manual,
                redirect_uri: "http://127.0.0.1:18765/callback".into(),
                listener_port: None,
                fallback_reason: None,
            },
        )
        .await?;
    let state = query_parameter(
        authorization
            .auth_url
            .as_deref()
            .context("fixture authorization URL")?,
        "state",
    )?;
    gateway
        .admin()
        .complete_oauth_session(
            &authorization.session_id,
            AuthCompletionInput {
                input: AuthCompletionValue::CallbackUrl {
                    value: format!(
                        "{}?code=fixture-code&state={state}",
                        authorization.redirect_uri
                    ),
                },
            },
        )
        .await?;
    let provider = gateway
        .admin()
        .create_provider_with_oauth_session(
            &authorization.session_id,
            CreateProvider {
                name: Some("Scheduled management maintenance".into()),
                source: ProviderSourceInput::Custom {
                    vendor: "fixture.management".into(),
                    channel: "default".into(),
                    protocol: Some("fixture-management".into()),
                    base_url,
                    models_source: None,
                    static_models: None,
                },
                credential: ProviderCredentialInput::None,
                vendor_options: BTreeMap::from([("workspace".into(), json!("schedule-workspace"))])
                    .into_iter()
                    .collect(),
                use_proxy: false,
            },
        )
        .await?;
    // 构造前固定调度时钟，guard只阻止自动跳时，不改变产品std::Instant预算。
    // 先推进首个毫秒，唤醒timer wheel中启动即到期的维护任务。
    tokio::time::advance(Duration::from_millis(1)).await;
    let result = async {
        // 无请求窗口仅为有界佐证；主要以60分钟模型往返、库存发布后的请求顺序验证错峰。
        let settle = || async {
            let until = std::time::Instant::now() + Duration::from_millis(100);
            while std::time::Instant::now() < until {
                tokio::task::yield_now().await;
            }
        };
        async fn next_request(
            requests: &mut mpsc::UnboundedReceiver<ObservedRequest>,
        ) -> anyhow::Result<ObservedRequest> {
            with_real_time_limit(requests.recv())
                .await?
                .context("scheduled loopback request observer closed")
        }
        settle().await;
        tokio::time::advance(Duration::from_secs(29 * 60)).await;
        settle().await;
        assert!(
            requests.try_recv().is_err(),
            "no sampling before 30 minutes"
        );
        tokio::time::advance(Duration::from_secs(60)).await;
        settle().await;
        assert!(
            requests.try_recv().is_err(),
            "allowance sampling must wait until the 35-minute phase"
        );
        tokio::time::advance(Duration::from_secs(5 * 60)).await;
        assert_eq!(
            next_request(&mut requests)
                .await
                .context("35-minute allowance phase must issue its first request")?
                .path,
            "/allowance"
        );
        let admin = gateway.admin();
        let pending = admin.get_provider_allowance(&provider.id);
        tokio::pin!(pending);
        // 先经公开API加入被fixture阻塞的采样，再释放503，避免错误后另起刷新。
        tokio::select! {
            biased;
            _ = &mut pending => panic!("withheld first sample must still be pending"),
            _ = std::future::ready(()) => {}
        }
        first_sample.notify_one();
        let failed = with_real_time_limit(&mut pending)
            .await
            .context("failed scheduled sample was not published")??
            .expect("failed allowance sample");
        assert_eq!(failed.status, ProviderAllowanceStatus::Error);
        assert!(failed.error.is_some());
        assert!(failed.allowances.is_empty());
        tokio::time::advance(Duration::from_secs(25 * 60)).await;
        assert_eq!(
            next_request(&mut requests)
                .await
                .context("60-minute model phase must discover inventory")?
                .path,
            "/models"
        );
        with_real_time_limit(async {
            loop {
                let inventory = gateway.admin().list_provider_models(&provider.id).await?;
                if inventory
                    .models
                    .iter()
                    .any(|model| model.id == "scheduled-model")
                {
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("scheduled discovery result was not published")??;
        settle().await;
        assert!(
            requests.try_recv().is_err(),
            "60-minute model discovery must not trigger allowance sampling"
        );
        tokio::time::advance(Duration::from_secs(5 * 60)).await;
        assert_eq!(
            next_request(&mut requests)
                .await
                .context("65-minute allowance phase must issue its second request")?
                .path,
            "/allowance"
        );
        let admin = gateway.admin();
        let snapshot = with_real_time_limit(admin.get_provider_allowance(&provider.id))
            .await
            .context("scheduled allowance result was not published")??
            .expect("scheduled allowance result");
        assert_eq!(snapshot.status, ProviderAllowanceStatus::Fresh);
        let remaining = snapshot.allowances[0]
            .remaining
            .as_ref()
            .expect("sampled balance");
        assert_eq!(remaining.value, 7.25);
        assert_eq!(remaining.unit, "requests");
        assert_eq!(remaining.currency, None);
        settle().await;
        gateway.shutdown().await;
        tokio::time::advance(Duration::from_secs(2 * 60 * 60)).await;
        settle().await;
        assert!(
            requests.try_recv().is_err(),
            "shutdown must cancel future maintenance"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    gateway.shutdown().await;
    server_task.abort();
    clock_guard.abort();
    tokio::time::resume();
    result
}

#[tokio::test]
async fn paginated_discovery_exhausts_one_minute_without_replacing_inventory() -> anyhow::Result<()>
{
    let first_page = Arc::new(Notify::new());
    let second_page = Arc::new(Notify::new());
    let first_release = Arc::clone(&first_page);
    let second_release = Arc::clone(&second_page);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server_calls = Arc::clone(&calls);
    let mut server = local_tls_server(3, move |request| {
        let call = server_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(request.method, "GET");
        match call {
            0 => {
                assert_eq!(request.path, "/v1/models");
                LocalResponse::json(json!({"data": [{"id": "confirmed-model"}]}))
            }
            1 => {
                assert_eq!(request.path, "/v1/models");
                let mut response = LocalResponse::json(json!({
                    "data": [{"id": "unconfirmed-first-page"}],
                    "next_cursor": "second-page"
                }));
                response.release = Some(Arc::clone(&first_release));
                response
            }
            2 => {
                assert_eq!(request.path, "/v1/models?after=second-page");
                let mut response = LocalResponse::json(json!({
                    "data": [{"id": "unconfirmed-second-page"}]
                }));
                response.release = Some(Arc::clone(&second_release));
                response
            }
            _ => unreachable!("unexpected discovery request"),
        }
    })
    .await?;
    let (_directory, gateway) = gateway_for(&server, &[]).await?;
    let provider = create_vendor_provider(
        &gateway,
        "Custom shared pagination budget",
        "custom",
        "default",
        "openai-compatible",
        "https://api.deepseek.com",
        ProviderCredentialInput::ApiKey {
            value: "deepseek-pagination-secret".into(),
        },
    )
    .await?;
    let confirmed = gateway.admin().sync_provider_models(&provider.id).await?;
    assert_eq!(
        confirmed.outcome,
        crate::provider_models::ModelChangeOutcome::Success
    );
    let before = gateway.admin().list_provider_models(&provider.id).await?;
    assert_eq!(before.models.len(), 1);
    assert_eq!(before.models[0].id, "confirmed-model");
    server
        .requests
        .recv()
        .await
        .expect("confirmed discovery request");

    let sync_gateway = gateway.clone();
    let provider_id = provider.id.clone();
    let started_at = std::time::Instant::now();
    let mut sync = tokio::spawn(async move {
        sync_gateway
            .admin()
            .sync_provider_models(&provider_id)
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), server.requests.recv())
        .await?
        .expect("first discovery page request");
    // 首页消耗真实31秒；第二页只能使用同一次60秒预算的剩余部分，不能重计时。
    tokio::time::sleep(Duration::from_secs(31)).await;
    first_page.notify_one();
    tokio::time::timeout(Duration::from_secs(10), server.requests.recv())
        .await?
        .expect("second discovery page request");
    let completed = tokio::time::timeout(Duration::from_secs(35), &mut sync).await;
    let elapsed = started_at.elapsed();
    // 无论同步完成或超时，都释放本地fixture，避免响应任务挂起。
    second_page.notify_one();
    server.task.await??;
    let error = completed
        .context("paginated discovery must fail within its single 60-second budget")??
        .expect_err("an unfinished final page must be an explicit discovery failure");
    let failure = error
        .downcast_ref::<crate::provider_models::ProviderModelSyncFailure>()
        .context("admin API must expose a persisted sync failure summary")?;
    assert_eq!(
        failure.summary.outcome,
        crate::provider_models::ModelChangeOutcome::Failure
    );
    assert!(
        elapsed >= Duration::from_secs(60),
        "discovery must retain its full one-minute budget"
    );
    let state = gateway
        .admin()
        .provider_model_sync_state(&provider.id)
        .await?;
    assert_eq!(state.last_check.as_ref(), Some(&failure.summary));
    let after = gateway.admin().list_provider_models(&provider.id).await?;
    assert_eq!(
        serde_json::to_value(&after.models)?,
        serde_json::to_value(&before.models)?
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    gateway.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn devin_slow_model_discovery_publishes_the_confirmed_inventory() -> anyhow::Result<()> {
    let release = Arc::new(Notify::new());
    let server_release = Arc::clone(&release);
    let mut config = proto_string(1, "Delayed catalog model");
    config.extend(proto_string(22, "contract-delayed-model"));
    config.extend(proto_len(23, &proto_string(23, "contract-delayed-model")));
    let body = proto_len(1, &config);
    let mut server = local_tls_server(1, move |_| {
        let mut response = LocalResponse::protobuf(body.clone());
        response.release = Some(Arc::clone(&server_release));
        response
    })
    .await?;
    let (_directory, gateway) = gateway_for(&server, &["devin"]).await?;
    let authorization = gateway
        .admin()
        .init_oauth_session(
            AuthSessionCandidate {
                vendor_id: "devin".into(),
                channel: "devin".into(),
                provider_id: None,
                base_url: DEVIN_BASE_URL.into(),
                protocol: Some("devin-connect".into()),
                options: Default::default(),
                credentials: Default::default(),
                use_proxy: false,
            },
            OAuthSessionStartOptions {
                callback_mode: OAuthCallbackMode::Manual,
                redirect_uri: "chisel-show-auth-token".into(),
                listener_port: None,
                fallback_reason: None,
            },
        )
        .await?;
    gateway
        .admin()
        .complete_oauth_session(
            &authorization.session_id,
            AuthCompletionInput {
                input: AuthCompletionValue::Manual {
                    value: "session_token=devin-contract-token".into(),
                },
            },
        )
        .await?;
    let provider = gateway
        .admin()
        .create_provider_with_oauth_session(
            &authorization.session_id,
            devin_oauth_provider("Devin slow discovery"),
        )
        .await?;
    gateway
        .start_provider_model_sync(tokio::time::Instant::now())
        .await?;
    let request = tokio::time::timeout(Duration::from_secs(10), server.requests.recv())
        .await?
        .expect("Devin discovery request");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, DEVIN_MODELS_PATH);
    let sync_gateway = gateway.clone();
    let provider_id = provider.id.clone();
    let sync = tokio::spawn(async move {
        sync_gateway
            .admin()
            .sync_provider_models(&provider_id)
            .await
    });
    // 真实上游延迟跨过旧的 30 秒发现上限；不缩短产品 deadline 来模拟。
    tokio::time::sleep(Duration::from_secs(31)).await;
    release.notify_one();
    let summary = tokio::time::timeout(Duration::from_secs(10), sync).await???;
    assert_eq!(
        summary.outcome,
        crate::provider_models::ModelChangeOutcome::Success
    );
    assert_eq!(
        summary.source,
        crate::provider_models::ModelChangeSource::AutomaticSync,
        "manual checks must join the scheduled discovery instead of issuing another request"
    );
    let state = gateway
        .admin()
        .provider_model_sync_state(&provider.id)
        .await?;
    assert_eq!(state.last_check.as_ref(), Some(&summary));
    let inventory = gateway.admin().list_provider_models(&provider.id).await?;
    assert!(
        inventory
            .models
            .iter()
            .any(|model| model.id == "contract-delayed-model"),
        "the delayed upstream model must be saved, not replaced by static fallback"
    );
    server.task.await??;
    Ok(())
}

#[tokio::test]
async fn devin_allowance_uses_real_tls_protobuf_request_and_decodes_credit_windows()
-> anyhow::Result<()> {
    let mut server =
        local_tls_server(1, |_| LocalResponse::protobuf(devin_allowance_fixture())).await?;
    let (_directory, gateway) = gateway_for(&server, &["devin"]).await?;
    let authorization = gateway
        .admin()
        .init_oauth_session(
            AuthSessionCandidate {
                vendor_id: "devin".into(),
                channel: "devin".into(),
                provider_id: None,
                base_url: DEVIN_BASE_URL.into(),
                protocol: Some("devin-connect".into()),
                options: Default::default(),
                credentials: Default::default(),
                use_proxy: false,
            },
            OAuthSessionStartOptions {
                callback_mode: OAuthCallbackMode::Manual,
                redirect_uri: "chisel-show-auth-token".into(),
                listener_port: None,
                fallback_reason: None,
            },
        )
        .await?;
    gateway
        .admin()
        .complete_oauth_session(
            &authorization.session_id,
            AuthCompletionInput {
                input: AuthCompletionValue::Manual {
                    value: "session_token=devin-contract-token".into(),
                },
            },
        )
        .await?;
    let provider = gateway
        .admin()
        .create_provider_with_oauth_session(
            &authorization.session_id,
            devin_oauth_provider("Devin TLS allowance"),
        )
        .await?;

    let snapshot = gateway
        .admin()
        .refresh_provider_allowance(&provider.id)
        .await?
        .expect("Devin supports allowance");
    assert_eq!(snapshot.status, ProviderAllowanceStatus::Fresh);
    assert_eq!(snapshot.plan_label.as_deref(), Some("Devin Max Contract"));
    let daily = snapshot
        .allowances
        .iter()
        .find(|allowance| allowance.key == "daily")
        .expect("exhausted daily window");
    assert_eq!(daily.used_percent, Some(100.0));
    assert_eq!(daily.condition, Some(AllowanceCondition::Exhausted));
    assert_eq!(daily.reset_at, Some(2_000_000_000_000));
    let weekly = snapshot
        .allowances
        .iter()
        .find(|allowance| allowance.key == "weekly")
        .expect("weekly window");
    assert_eq!(weekly.used_percent, Some(67.0));
    assert_eq!(weekly.reset_at, Some(2_000_100_000_000));
    let prompt = snapshot
        .allowances
        .iter()
        .find(|allowance| allowance.key == "prompt_credits")
        .expect("prompt credit allowance");
    assert_eq!(prompt.used.as_ref().map(|amount| amount.value), Some(25.0));
    assert_eq!(
        prompt.limit.as_ref().map(|amount| amount.value),
        Some(100.0)
    );
    assert_eq!(
        prompt.remaining.as_ref().map(|amount| amount.value),
        Some(75.0)
    );
    assert_eq!(prompt.used_percent, Some(25.0));
    let flow = snapshot
        .allowances
        .iter()
        .find(|allowance| allowance.key == "flow_credits")
        .expect("flow credit allowance");
    assert_eq!(
        flow.remaining.as_ref().map(|amount| amount.value),
        Some(6.0)
    );
    let balance = snapshot
        .allowances
        .iter()
        .find(|allowance| allowance.key == "balance_usd")
        .expect("USD balance");
    assert_eq!(
        balance.remaining.as_ref().map(|amount| amount.value),
        Some(12.5)
    );

    let request = server.requests.recv().await.expect("Devin request");
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, DEVIN_ALLOWANCE_PATH);
    assert_eq!(
        request.headers.get("host").map(String::as_str),
        Some(DEVIN_HOST)
    );
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Basic devin-contract-token-devin-contract-token")
    );
    assert_eq!(
        request.headers.get("content-type").map(String::as_str),
        Some("application/proto")
    );
    assert_eq!(
        request
            .headers
            .get("connect-protocol-version")
            .map(String::as_str),
        Some("1")
    );
    assert!(
        request
            .body
            .windows("devin-contract-token".len())
            .any(|window| window == b"devin-contract-token")
    );
    server.task.await??;
    Ok(())
}

#[tokio::test]
async fn devin_pkce_exchanges_are_session_isolated_and_bind_their_own_credentials()
-> anyhow::Result<()> {
    let mut server = local_tls_server(2, |request| {
        let body: Value = serde_json::from_slice(&request.body).expect("OAuth JSON body");
        let code = body["code"].as_str().expect("authorization code");
        LocalResponse::json(json!({
            "sessionToken": format!("token-for-{code}"),
            "refreshToken": format!("refresh-for-{code}"),
            "expiresIn": 3600,
            "scope": "openid offline_access",
            "apiServerUrl": DEVIN_BASE_URL
        }))
    })
    .await?;
    let (_directory, gateway) = gateway_for(&server, &["devin"]).await?;
    let first = start_devin_session(&gateway).await?;
    let second = start_devin_session(&gateway).await?;
    let first_url = first.auth_url.as_deref().expect("first authorization URL");
    let second_url = second
        .auth_url
        .as_deref()
        .expect("second authorization URL");
    let first_state = query_parameter(first_url, "state")?;
    let second_state = query_parameter(second_url, "state")?;
    assert_ne!(first_state, second_state);
    assert_ne!(
        query_parameter(first_url, "code_challenge")?,
        query_parameter(second_url, "code_challenge")?
    );

    let crossed = gateway
        .admin()
        .complete_oauth_session(
            &first.session_id,
            AuthCompletionInput {
                input: AuthCompletionValue::CallbackUrl {
                    value: format!(
                        "http://127.0.0.1:18765/oauth/callback?code=crossed&state={second_state}"
                    ),
                },
            },
        )
        .await
        .expect_err("a different session state must not exchange");
    assert!(crossed.to_string().contains("STATE_MISMATCH"));
    assert!(server.requests.try_recv().is_err());

    for (session, state, code) in [
        (&first, first_state.as_str(), "first-code"),
        (&second, second_state.as_str(), "second-code"),
    ] {
        let status = gateway
            .admin()
            .complete_oauth_session(
                &session.session_id,
                AuthCompletionInput {
                    input: AuthCompletionValue::CallbackUrl {
                        value: format!(
                            "http://127.0.0.1:18765/oauth/callback?code={code}&state={state}"
                        ),
                    },
                },
            )
            .await?;
        assert!(matches!(status, AuthSessionStatusData::Ready { .. }));
    }

    let first_request = server.requests.recv().await.expect("first exchange");
    let second_request = server.requests.recv().await.expect("second exchange");
    for (request, auth_url, expected_code) in [
        (&first_request, first_url, "first-code"),
        (&second_request, second_url, "second-code"),
    ] {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, DEVIN_TOKEN_PATH);
        assert_eq!(
            request.headers.get("host").map(String::as_str),
            Some(DEVIN_HOST)
        );
        assert_eq!(
            request
                .headers
                .get("connect-protocol-version")
                .map(String::as_str),
            Some("1")
        );
        let body: Value = serde_json::from_slice(&request.body)?;
        assert_eq!(body["code"], expected_code);
        let verifier = body["codeVerifier"].as_str().expect("PKCE verifier");
        let expected_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(
            query_parameter(auth_url, "code_challenge")?,
            expected_challenge
        );
    }

    let first_provider = gateway
        .admin()
        .create_provider_with_oauth_session(
            &first.session_id,
            devin_oauth_provider("first isolated Devin"),
        )
        .await?;
    let second_provider = gateway
        .admin()
        .create_provider_with_oauth_session(
            &second.session_id,
            devin_oauth_provider("second isolated Devin"),
        )
        .await?;
    let first_credential = gateway
        .storage
        .oauth_credentials()
        .get(&first_provider.id)
        .await?
        .expect("first OAuth credential");
    let second_credential = gateway
        .storage
        .oauth_credentials()
        .get(&second_provider.id)
        .await?
        .expect("second OAuth credential");
    assert_eq!(first_credential.access_token, "token-for-first-code");
    assert_eq!(second_credential.access_token, "token-for-second-code");
    assert_ne!(
        first_credential.access_token,
        second_credential.access_token
    );
    for provider in [&first_provider, &second_provider] {
        let status = gateway
            .admin()
            .get_provider_oauth_status(&provider.id)
            .await?;
        assert_eq!(status.status, AuthBindingStatus::Connected.as_str());
        assert_eq!(status.resource_url.as_deref(), Some(DEVIN_BASE_URL));
    }
    server.task.await??;
    Ok(())
}

#[tokio::test]
async fn cancelling_a_devin_exchange_prevents_a_late_tls_callback_from_restoring_it()
-> anyhow::Result<()> {
    let release = Arc::new(Notify::new());
    let response_release = Arc::clone(&release);
    let mut server = local_tls_server(1, move |_| LocalResponse {
        status: "200 OK",
        content_type: "application/json",
        body: serde_json::to_vec(&json!({
            "sessionToken": "late-session-token",
            "apiServerUrl": DEVIN_BASE_URL
        }))
        .expect("fixture JSON"),
        release: Some(Arc::clone(&response_release)),
    })
    .await?;
    let (_directory, gateway) = gateway_for(&server, &["devin"]).await?;
    let session = start_devin_session(&gateway).await?;
    let auth_url = session.auth_url.as_deref().expect("authorization URL");
    let state = query_parameter(auth_url, "state")?;
    let session_id = session.session_id.clone();
    let completing_gateway = gateway.clone();
    let completion = tokio::spawn(async move {
        completing_gateway
            .admin()
            .complete_oauth_session(
                &session_id,
                AuthCompletionInput {
                    input: AuthCompletionValue::CallbackUrl {
                        value: format!(
                            "http://127.0.0.1:18765/oauth/callback?code=late-code&state={state}"
                        ),
                    },
                },
            )
            .await
    });

    let request = server.requests.recv().await.expect("in-flight exchange");
    assert_eq!(request.path, DEVIN_TOKEN_PATH);
    gateway
        .admin()
        .cancel_oauth_session(&session.session_id)
        .await?;
    release.notify_one();
    let completion_error = completion
        .await?
        .expect_err("a cancelled session must reject its late exchange result");
    assert!(
        completion_error.to_string().contains("not found")
            || completion_error.to_string().contains("cancel")
    );
    assert!(
        !gateway
            .auth_sessions
            .read()
            .await
            .contains_key(&session.session_id)
    );
    assert!(
        gateway
            .admin()
            .get_oauth_session_status(&session.session_id)
            .await
            .is_err()
    );
    server.task.await??;
    Ok(())
}
