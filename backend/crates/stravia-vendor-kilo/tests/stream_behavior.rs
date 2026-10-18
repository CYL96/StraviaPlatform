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

#[test]
fn framed_events_preserve_split_utf8_crlf_and_unfinished_tail() {
    let wire = "data: {\"text\":\"世界\"}\r\n\r\ndata: [DONE]\n\ndata: tail";
    let split = wire.find('世').unwrap() + 1;
    let mut frames = stream::Frames::default();
    assert!(frames.push(&wire.as_bytes()[..split]).unwrap().is_empty());
    {
        let events = frames.push(&wire.as_bytes()[split..]).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], "data: {\"text\":\"世界\"}\r\n\r\n".as_bytes());
        let (done, failure) = stream::event(events[1]).unwrap();
        assert_eq!(&*done, b"data: [DONE]\n\n");
        assert!(failure.is_none());
    }
    assert_eq!(frames.finish(), b"data: tail");
}

#[test]
fn multiple_data_lines_are_combined_without_changing_json() {
    let (wire, failure) =
        stream::event(b"data: {\"type\":\"chunk\",\r\ndata: \"text\":\"ok\"}\r\n\r\n").unwrap();
    assert!(failure.is_none());
    let text = std::str::from_utf8(&wire).unwrap();
    assert!(text.contains(r#""text":"ok""#));
    assert!(text.contains(r#""type":"chunk""#));
}
