use chrono::{DateTime, Utc};
use web::commands::{DecideInvitationRequest, InvitationRequestDecisionView};

fn at(iso: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(iso)
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn external_command_implementations_can_inspect_approve_decisions() {
    let decided_at = at("2026-05-20T11:45:00Z");
    let command = DecideInvitationRequest::approve(domain::RequestId::new(), 7, decided_at);

    assert_eq!(
        command.decision(),
        InvitationRequestDecisionView::Approve {
            decided_by: 7,
            decided_at,
        }
    );
}

#[test]
fn external_command_implementations_can_inspect_decline_decisions() {
    let decided_at = at("2026-05-20T12:15:00Z");
    let command = DecideInvitationRequest::decline(
        domain::RequestId::new(),
        8,
        decided_at,
        Some("not enough context".into()),
    );

    assert_eq!(
        command.decision(),
        InvitationRequestDecisionView::Decline {
            decided_by: 8,
            decided_at,
            reason: Some("not enough context"),
        }
    );
}
