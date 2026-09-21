//! Audit Log appends, shared by SQLite and D1. There is no update or delete.
use crate::audit::{AuditEvent, EventType};

/// Installation projectors retain complete audit events before insertion. An
/// identical primary-key replay succeeds; differing content violates NOT NULL
/// and leaves the stored event intact. Shared SQLite/D1 statement semantics.
pub const INSTALLATION_REPLAY: &str = " ON CONFLICT(id) DO UPDATE SET account_id = CASE WHEN audit_events.account_id IS excluded.account_id AND audit_events.occurred_at IS excluded.occurred_at AND audit_events.event_type IS excluded.event_type AND audit_events.actor_kind IS excluded.actor_kind AND audit_events.actor_id IS excluded.actor_id AND audit_events.target_kind IS excluded.target_kind AND audit_events.target_id IS excluded.target_id AND audit_events.metadata IS excluded.metadata AND audit_events.request_id IS excluded.request_id THEN audit_events.account_id ELSE NULL END";

/// Bind ID, account ID, occurred time, event type, actor kind, nullable actor
/// ID, target kind, target ID, [`metadata`] and nullable request ID.
pub fn insert(event_type: EventType) -> String {
    let mut sql = String::from(
        "INSERT INTO audit_events (id, account_id, occurred_at, event_type, actor_kind, actor_id, target_kind, target_id, metadata, request_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    );
    // Metadata commands journal their event ID before writing. Ignore only
    // that primary-key replay, not other constraints or database failures.
    match event_type {
        EventType::InvitationLinkMetadataUpdated => sql.push_str(" ON CONFLICT(id) DO NOTHING"),
        EventType::InstallationCreated
        | EventType::InstallationReposChanged
        | EventType::InstallationUninstalled => sql.push_str(INSTALLATION_REPLAY),
        _ => {}
    }
    sql
}

/// Stored metadata text; JSON null is stored as SQL NULL.
pub fn metadata(event: &AuditEvent) -> Option<String> {
    (!event.metadata.is_null())
        .then(|| serde_json::to_string(&event.metadata).expect("audit metadata serializes"))
}
