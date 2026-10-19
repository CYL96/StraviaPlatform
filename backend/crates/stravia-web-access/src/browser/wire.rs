//! CDP 应用消息按页面归属捕获，不依赖共享事件泵的 task-local。
use super::cdp::Cdp;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use stravia_runtime_contract::wire_debug::{Capture, WireEvent};

#[derive(Default)]
pub(super) struct Wire {
    owners: parking_lot::Mutex<HashMap<String, Capture>>,
    requests: parking_lot::Mutex<HashMap<(String, String), Response>>,
}

struct Response {
    url: String,
    status: Option<u16>,
}

impl Response {
    fn from_value(value: &Value) -> Self {
        Self {
            url: value["url"].as_str().unwrap_or_default().to_owned(),
            status: value["status"]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok()),
        }
    }

    fn event(self, body: Vec<u8>) -> WireEvent {
        WireEvent {
            direction: "upstream_response",
            transport: "http",
            protocol: "web-access/chromium",
            message_type: "body_chunk",
            url: self.url,
            status_code: self.status,
            headers: Value::Null,
            payload: body.into(),
        }
    }
}

pub(super) struct Body {
    capture: Capture,
    response: Option<Response>,
    session: String,
    id: String,
}

impl Drop for Body {
    fn drop(&mut self) {
        if self.response.is_some() {
            self.capture.gap("chromium_response_body_capture_cancelled");
        }
    }
}

impl Body {
    pub(super) async fn read(mut self, cdp: Cdp) {
        let result = cdp
            .call(
                Some(&self.session),
                "Network.getResponseBody",
                json!({"requestId": self.id}),
            )
            .await;
        let body = match result {
            Ok(value) => match value["body"].as_str() {
                Some(body) if value["base64Encoded"].as_bool() == Some(true) => {
                    match base64::engine::general_purpose::STANDARD.decode(body) {
                        Ok(body) => Some(body),
                        Err(_) => {
                            self.capture.gap("chromium_response_base64_invalid");
                            None
                        }
                    }
                }
                Some(body) => Some(body.as_bytes().to_vec()),
                None => {
                    self.capture.gap("chromium_response_body_unavailable");
                    None
                }
            },
            Err(_) => {
                self.capture.gap("chromium_response_body_unavailable");
                None
            }
        };
        if let (Some(response), Some(body)) = (self.response.take(), body) {
            self.capture.record(response.event(body));
        }
    }
}

impl Wire {
    pub(super) fn finish(&self, event: &Value) -> Option<Body> {
        let session = event["sessionId"].as_str()?;
        let capture = self.owners.lock().get(session).cloned()?;
        let id = event["params"]["requestId"].as_str()?;
        let response = self
            .requests
            .lock()
            .remove(&(session.to_owned(), id.to_owned()));
        if response.is_none() {
            capture.gap("chromium_response_metadata_unavailable");
        }
        Some(Body {
            capture,
            response,
            session: session.to_owned(),
            id: id.to_owned(),
        })
    }
    pub(super) fn own(&self, session: &str, capture: Capture) {
        // CDP 只提供解码后的实体正文，不能还原传输分块或压缩字节，
        // 也不能可靠地把 shared/service worker 归属到唯一页面。
        capture.gap("chromium_cdp_decoded_bodies_and_unattributed_shared_workers");
        capture.gap("chromium_stream_chunks_websocket_webtransport_not_observed");
        self.owners.lock().insert(session.to_owned(), capture);
    }

    pub(super) fn inherit(&self, parent: &str, child: &str) {
        let mut owners = self.owners.lock();
        if let Some(capture) = owners.get(parent).cloned() {
            owners.insert(child.to_owned(), capture);
        }
    }

    pub(super) fn detach(&self, session: &str) {
        let capture = self.owners.lock().remove(session);
        self.requests.lock().retain(|(owner, _), _| {
            if owner != session {
                return true;
            }
            if let Some(capture) = &capture {
                capture.gap("chromium_target_closed_before_response_body");
            }
            false
        });
    }

    pub(super) fn event(&self, event: &Value) {
        let Some(session) = event["sessionId"].as_str() else {
            return;
        };
        let Some(capture) = self.owners.lock().get(session).cloned() else {
            return;
        };
        let params = &event["params"];
        let Some(id) = params["requestId"].as_str() else {
            return;
        };
        let key = (session.to_owned(), id.to_owned());
        match event["method"].as_str().unwrap_or_default() {
            "Network.requestWillBeSent" => {
                let request = &params["request"];
                let mut body = Vec::new();
                if let Some(entries) = request["postDataEntries"].as_array() {
                    for entry in entries {
                        match entry["bytes"].as_str().and_then(|value| {
                            base64::engine::general_purpose::STANDARD.decode(value).ok()
                        }) {
                            Some(bytes) => body.extend_from_slice(&bytes),
                            None => capture.gap("chromium_request_body_entry_unavailable"),
                        }
                    }
                } else if request["hasPostData"].as_bool() == Some(true) {
                    capture.gap("chromium_request_body_bytes_unavailable");
                }
                capture.record(WireEvent {
                    direction: "upstream_request",
                    transport: "http",
                    protocol: "web-access/chromium",
                    message_type: "request",
                    url: request["url"].as_str().unwrap_or_default().to_owned(),
                    status_code: None,
                    headers: request["headers"].clone(),
                    payload: body.into(),
                });
                if let Some(response) = params.get("redirectResponse") {
                    let mut headers = response_event(response, Vec::new());
                    headers.message_type = "response_headers";
                    capture.record(headers);
                    capture.gap("chromium_redirect_response_body_unavailable");
                }
                self.requests
                    .lock()
                    .insert(key, Response::from_value(request));
            }
            "Network.responseReceived" => {
                let mut headers = response_event(&params["response"], Vec::new());
                headers.message_type = "response_headers";
                capture.record(headers);
                self.requests
                    .lock()
                    .insert(key, Response::from_value(&params["response"]));
            }
            "Network.requestWillBeSentExtraInfo" | "Network.responseReceivedExtraInfo" => {
                let request = event["method"] == "Network.requestWillBeSentExtraInfo";
                let url = self
                    .requests
                    .lock()
                    .get(&key)
                    .map(|value| value.url.clone())
                    .unwrap_or_default();
                capture.record(WireEvent {
                    direction: if request {
                        "upstream_request"
                    } else {
                        "upstream_response"
                    },
                    transport: "http",
                    protocol: "web-access/chromium",
                    message_type: if request {
                        "request"
                    } else {
                        "response_headers"
                    },
                    url,
                    status_code: params["statusCode"]
                        .as_u64()
                        .and_then(|value| u16::try_from(value).ok()),
                    headers: params["headers"].clone(),
                    payload: Vec::new().into(),
                });
            }
            "Network.loadingFailed" => {
                self.requests.lock().remove(&key);
                capture.gap("chromium_network_failed_or_download_body_unavailable");
            }
            _ => {}
        }
    }
}

fn response_event(response: &Value, body: Vec<u8>) -> WireEvent {
    WireEvent {
        direction: "upstream_response",
        transport: "http",
        protocol: "web-access/chromium",
        message_type: "body_chunk",
        url: response["url"].as_str().unwrap_or_default().to_owned(),
        status_code: response["status"]
            .as_u64()
            .and_then(|value| u16::try_from(value).ok()),
        headers: response["headers"].clone(),
        payload: body.into(),
    }
}
