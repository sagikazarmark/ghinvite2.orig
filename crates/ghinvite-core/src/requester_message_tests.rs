use crate::request_lifecycle::{DecisionAction, TerminalDecision, normalize_requester_message};

#[test]
fn legacy_decision_and_action_bytes_do_not_gain_a_public_message() {
    let action = r#"{"kind":"decline","reason":"Private historical note"}"#;
    let parsed: DecisionAction = serde_json::from_str(action).unwrap();
    assert_eq!(serde_json::to_string(&parsed).unwrap(), action);
    let decision = r#"{"decision_id":"request.declined/old","decided_by":7,"effective_at":"2026-01-01T00:00:00Z","evaluated_at":"2026-01-01T00:00:00Z","decline_reason":"Private historical note"}"#;
    let parsed: TerminalDecision = serde_json::from_str(decision).unwrap();
    assert_eq!(parsed.requester_message, None);
    assert_eq!(serde_json::to_string(&parsed).unwrap(), decision);
}

#[test]
fn shared_message_is_bounded_normalized_and_never_derived_from_notes() {
    assert_eq!(normalize_requester_message(None).unwrap(), None);
    assert_eq!(
        normalize_requester_message(Some(" \r\n\t ".into())).unwrap(),
        None
    );
    assert_eq!(
        normalize_requester_message(Some("  Please\r\ntry again.  ".into())).unwrap(),
        Some("Please\ntry again.".into())
    );
    assert!(normalize_requester_message(Some("é".repeat(500))).is_ok());
    assert!(normalize_requester_message(Some("é".repeat(501))).is_err());
    for control in ['\0', '\r', '\u{1b}', '\u{85}'] {
        assert!(normalize_requester_message(Some(format!("message{control}text"))).is_err());
    }
}
