//! 平台能力的远程应用协议抓包；仅在当前请求启用 Debug 时安装接收端。

use std::{future::Future, sync::Arc};

use bytes::Bytes;
use serde_json::Value;

/// 在实际远程收发位置观察到的一条记录，不表示 TCP、TLS 或 HTTP/2 帧。
pub struct WireEvent {
    pub direction: &'static str,
    pub transport: &'static str,
    pub protocol: &'static str,
    pub message_type: &'static str,
    pub url: String,
    pub status_code: Option<u16>,
    pub headers: Value,
    pub payload: Bytes,
}

/// 接收端拥有脱敏、容量控制与持久化；传输不得因诊断失败改变结果。
pub trait WireSink: Send + Sync {
    fn record(&self, event: WireEvent);
    fn gap(&self, reason: &'static str);
}

#[derive(Clone)]
pub struct Capture(Arc<dyn WireSink>);

impl Capture {
    pub fn new(sink: impl WireSink + 'static) -> Self {
        Self(Arc::new(sink))
    }

    pub fn record(&self, event: WireEvent) {
        self.0.record(event);
    }

    pub fn gap(&self, reason: &'static str) {
        self.0.gap(reason);
    }
}

tokio::task_local! {
    static CAPTURE: Option<Capture>;
}

/// 无捕获时不构造载荷；派生任务须显式传入其所属请求的接收端。
pub fn current() -> Option<Capture> {
    CAPTURE.try_with(Clone::clone).ok().flatten()
}

/// 请求作用域离开后自动恢复原接收端，不修改共享客户端或全局配置。
pub fn scope<F: Future>(capture: Option<Capture>, future: F) -> impl Future<Output = F::Output> {
    CAPTURE.scope(capture, future)
}
