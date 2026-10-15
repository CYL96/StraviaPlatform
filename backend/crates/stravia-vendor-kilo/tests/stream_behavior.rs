use super::*;
use serde_json::json;
use stravia_runtime_contract::protocol::ir::AiErrorKind;

#[test]
fn pre_created_responses_billing_failure_preserves_cause() {
    let value = json!({"type":"response.failed","response":{"id":"resp-1","object":"response","status":"failed","error":{"type":"billing_error","message":"No credits"}}});
    let (_, error) = stream::event(format!("data: {value}\n\n").as_bytes()).unwrap();
    let error = error.expect("failure must bypass the decoder's response.created requirement");
    assert_eq!(error.kind, AiErrorKind::QuotaExceeded);
    assert_eq!(error.message, "No credits");
}
