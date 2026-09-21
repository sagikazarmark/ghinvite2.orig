//! Shared SQLite/D1 seek query. Only closed enums select SQL fragments; all
//! values are bound. Keep the ordering expression identical to the audit order indexes.
use crate::AuditEventId;
use crate::audit::{AuditEvent, EventType};
use chrono::{DateTime, SecondsFormat, Utc};

pub const AUDIT_PAGE_SIZE: usize = 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuditBoundary {
    pub occurred_at: DateTime<Utc>,
    pub id: AuditEventId,
}

impl From<&AuditEvent> for AuditBoundary {
    fn from(event: &AuditEvent) -> Self {
        Self {
            occurred_at: event.occurred_at,
            id: event.id,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuditPosition {
    #[default]
    Latest,
    Before(AuditBoundary),
    After(AuditBoundary),
}

impl AuditPosition {
    pub fn boundary(self) -> Option<AuditBoundary> {
        match self {
            Self::Latest => None,
            Self::Before(b) | Self::After(b) => Some(b),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AuditPage {
    pub events: Vec<AuditEvent>,
    pub has_older: bool,
    pub has_newer: bool,
}

// Both writers use DateTime<Utc>: RFC3339 with +00:00 and AutoSi fractions.
// Also accepts the equivalent Z representation. Do not use SQLite's date
// functions: they round fractions to milliseconds. No stored-data rewrite is
// needed: this expression right-pads fractions without changing their value.
const TIME_KEY: &str = "(substr(occurred_at,1,19) || '.' || substr(CASE WHEN substr(occurred_at,20,1) = '.' THEN replace(replace(substr(occurred_at,21),'+00:00',''),'Z','') ELSE '' END || '000000000',1,9))";

pub fn boundary_time(boundary: AuditBoundary) -> String {
    boundary
        .occurred_at
        .to_rfc3339_opts(SecondsFormat::Nanos, true)
        .trim_end_matches('Z')
        .to_string()
}

/// Four bindings: account ID, nullable exact event, nullable time key and ID.
/// Probes select only a constant, with LIMIT 1; page reads have LIMIT 25.
pub fn query(event: Option<EventType>, position: AuditPosition, probe: bool) -> String {
    let filter = if event.is_some() {
        "event_type = ?2"
    } else {
        "?2 IS NULL"
    };
    let boundary = match position {
        AuditPosition::Latest => "?3 IS NULL AND ?4 IS NULL".to_string(),
        AuditPosition::Before(_) => format!("{TIME_KEY} <= ?3 AND ({TIME_KEY}, id) < (?3, ?4)"),
        AuditPosition::After(_) => format!("{TIME_KEY} >= ?3 AND ({TIME_KEY}, id) > (?3, ?4)"),
    };
    // The redundant scalar bound makes SQLite seek the expression index; the
    // tuple alone can degrade to scanning every event for the account/filter.
    let order = if matches!(position, AuditPosition::After(_)) {
        "ASC"
    } else {
        "DESC"
    };
    let columns = if probe {
        "1 AS present"
    } else {
        "id, account_id, occurred_at, event_type, actor_kind, actor_id, target_kind, target_id, metadata, request_id"
    };
    let limit = if probe { 1 } else { AUDIT_PAGE_SIZE };
    format!(
        "SELECT {columns} FROM audit_events WHERE account_id = ?1 AND {filter} AND {boundary} ORDER BY {TIME_KEY} {order}, id {order} LIMIT {limit}"
    )
}
