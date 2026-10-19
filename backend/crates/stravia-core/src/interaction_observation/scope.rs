use stravia_runtime_contract::wire_debug::{self, Capture, WireEvent, WireSink};

use super::{RunEvent, RunObserver, WirePayload};

tokio::task_local! {
    static OBSERVER: Option<RunObserver>;
}

struct RemoteWireSink(RunObserver);

impl WireSink for RemoteWireSink {
    fn record(&self, event: WireEvent) {
        self.0.record_debug(|| RunEvent::Wire {
            direction: event.direction.into(),
            transport: event.transport.into(),
            protocol: event.protocol.into(),
            message_type: event.message_type.into(),
            model_turn_id: None,
            attempt_id: None,
            status_code: event.status_code,
            url: Some(event.url),
            headers: event.headers,
            payload: WirePayload::Bytes(event.payload),
        });
    }

    fn gap(&self, reason: &'static str) {
        if let Some(trace) = &self.0.inner.trace {
            trace.mark_partial(reason, false);
        }
    }
}

/// 仅传播诊断归属；离开平台工具执行作用域后不保留环境状态。
pub(crate) fn scope<F: Future>(
    observer: Option<RunObserver>,
    future: F,
) -> impl Future<Output = F::Output> {
    let capture = observer
        .as_ref()
        .filter(|observer| observer.inner.debug_enabled)
        .and_then(|observer| {
            let inherited = OBSERVER
                .try_with(|current| {
                    current.as_ref().is_some_and(|current| {
                        std::sync::Arc::ptr_eq(&current.inner, &observer.inner)
                    })
                })
                .unwrap_or(false);
            if inherited {
                wire_debug::current()
            } else {
                Some(Capture::new(RemoteWireSink(observer.clone())))
            }
        });
    wire_debug::scope(capture, OBSERVER.scope(observer, future))
}

pub(crate) fn current() -> Option<RunObserver> {
    OBSERVER.try_with(Clone::clone).ok().flatten()
}
