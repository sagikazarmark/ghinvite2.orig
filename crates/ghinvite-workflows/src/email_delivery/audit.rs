//! Semantic transition history is committed in the same record as its snapshot.
use super::*;
use ghinvite_core::storage::email_delivery::{EmailAuditFact, EmailAuditKind};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Semantic {
    Other,
    Blocked(EmailBlockedReason),
    Unknown,
}
fn semantic(outcome: &EmailCreateOutcome) -> Semantic {
    match outcome {
        EmailCreateOutcome::Blocked { reason } => Semantic::Blocked(*reason),
        EmailCreateOutcome::OutcomeUnknown => Semantic::Unknown,
        _ => Semantic::Other,
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Audit {
    cursor: Option<Semantic>,
    pub pending: Vec<EmailAuditFact>,
}
/// Legacy records establish a baseline only. Missing historical clocks are not
/// reconstructed from approval or the retry's wall time.
pub(super) fn initialize(v: &mut Retained) {
    if v.audit.cursor.is_none() {
        v.audit.cursor = Some(semantic(&v.snapshot.outcome));
    }
}
pub(super) fn transition(v: &mut Retained, at: DateTime<Utc>) -> Result<(), TerminalError> {
    let current = semantic(&v.snapshot.outcome);
    if v.audit.cursor.as_ref() == Some(&current) {
        return Ok(());
    }
    let kind = match current {
        Semantic::Other => None,
        Semantic::Blocked(reason) => Some(EmailAuditKind::Blocked { reason }),
        Semantic::Unknown => Some(EmailAuditKind::OutcomeUnknown),
    };
    if let Some(kind) = kind {
        if v.audit.pending.len() >= 64 {
            return Err(conflict());
        }
        v.audit.pending.push(EmailAuditFact {
            transition_revision: v.snapshot.revision,
            at,
            kind,
        });
    }
    v.audit.cursor = Some(current);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn retained() -> Retained {
        let at = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        serde_json::from_value(serde_json::json!({
            "snapshot":{"command":{"batch":{"batch_id":ghinvite_core::bulk::BulkBatchId::new(),"row":1},
            "account_id":100,"installation_id":2,"organization":{"org_id":100,"org_login":"acme","grant":"member"},
            "teams":[],"email_ref":"08".repeat(32),"approved_by":7,"approved_at":at},"revision":1,"outcome":{"kind":"planned"}},
            "ordinal":1,"reserve":null,"slot":null,"completion":null,"wake":0
        })).unwrap()
    }
    #[test]
    fn semantic_cursor_survives_atomic_replay_and_distinguishes_episodes() {
        let mut v = retained();
        initialize(&mut v);
        let at = v.snapshot.command.approved_at;
        v.snapshot.revision = 2;
        v.snapshot.outcome = EmailCreateOutcome::Blocked {
            reason: EmailBlockedReason::EmailRecoveryUnavailable,
        };
        transition(&mut v, at).unwrap();
        let mut replay: Retained =
            serde_json::from_value(serde_json::to_value(&v).unwrap()).unwrap();
        replay.snapshot.revision = 3;
        transition(&mut replay, at + chrono::Duration::seconds(1)).unwrap();
        assert_eq!(replay.audit.pending, v.audit.pending);
        replay.snapshot.revision = 4;
        replay.snapshot.outcome = EmailCreateOutcome::Blocked {
            reason: EmailBlockedReason::NoSeat,
        };
        transition(&mut replay, at + chrono::Duration::seconds(2)).unwrap();
        replay.snapshot.revision = 5;
        replay.snapshot.outcome = EmailCreateOutcome::Throttled {
            until: at + chrono::Duration::seconds(60),
            reason: None,
        };
        transition(&mut replay, at + chrono::Duration::seconds(3)).unwrap();
        assert_eq!(replay.audit.pending.len(), 2, "no invented throttled event");
        replay.snapshot.revision = 6;
        replay.snapshot.outcome = EmailCreateOutcome::Blocked {
            reason: EmailBlockedReason::NoSeat,
        };
        transition(&mut replay, at + chrono::Duration::seconds(4)).unwrap();
        assert_eq!(
            replay
                .audit
                .pending
                .iter()
                .map(|e| e.transition_revision)
                .collect::<Vec<_>>(),
            vec![2, 4, 6]
        );
        assert_eq!(replay.audit.pending[0].at, at);
    }
    #[test]
    fn legacy_transient_receipt_establishes_baseline_without_inventing_history() {
        let mut v = retained();
        v.snapshot.outcome = EmailCreateOutcome::OutcomeUnknown;
        initialize(&mut v);
        let at = v.snapshot.command.approved_at;
        transition(&mut v, at).unwrap();
        assert!(v.audit.pending.is_empty());
    }
}
