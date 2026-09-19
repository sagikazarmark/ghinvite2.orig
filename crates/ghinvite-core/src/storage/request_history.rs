//! Bounded, newest-first request history shared by SQLx and D1.
use crate::{InvitationRequest, RequestId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const PAGE_SIZE: usize = 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Boundary {
    pub admitted_at: DateTime<Utc>,
    pub id: RequestId,
}

impl From<&InvitationRequest> for Boundary {
    fn from(request: &InvitationRequest) -> Self {
        Self {
            admitted_at: request.created_at,
            id: request.id,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Page {
    pub requests: Vec<InvitationRequest>,
    pub older: Option<Boundary>,
}

// Match migration 0011. Preserve nanoseconds and equivalent Z/+00:00 encodings.
const TIME_KEY: &str = "(substr(created_at,1,19) || '.' || substr(CASE WHEN substr(created_at,20,1) = '.' THEN replace(replace(substr(created_at,21),'+00:00',''),'Z','') ELSE '' END || '000000000',1,9))";

pub fn boundary_time(boundary: Boundary) -> String {
    boundary
        .admitted_at
        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
        .trim_end_matches('Z')
        .to_owned()
}

/// Bind account, link, nullable time key, nullable request ID. Fetch one extra
/// row to determine whether another page exists without a count or history scan.
pub fn query(before: Option<Boundary>) -> String {
    let bound = if before.is_some() {
        format!("{TIME_KEY} <= ?3 AND ({TIME_KEY}, id) < (?3, ?4)")
    } else {
        "?3 IS NULL AND ?4 IS NULL".into()
    };
    format!(
        "SELECT id, invitation_link_id, requester_id, justification, state, decided_by, decided_at, decline_reason, created_at, decision_deadline FROM invitation_requests WHERE invitation_link_id = ?2 AND EXISTS (SELECT 1 FROM invitation_links WHERE id = ?2 AND account_id = ?1) AND {bound} ORDER BY {TIME_KEY} DESC, id DESC LIMIT {}",
        PAGE_SIZE + 1
    )
}

pub fn page(mut requests: Vec<InvitationRequest>) -> Page {
    let more = requests.len() > PAGE_SIZE;
    requests.truncate(PAGE_SIZE);
    let older = if more {
        requests.last().map(Boundary::from)
    } else {
        None
    };
    Page { requests, older }
}
