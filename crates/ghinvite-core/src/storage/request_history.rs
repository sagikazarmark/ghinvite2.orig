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
    /// Optional profile enrichment fetched with the bounded page, keyed by the
    /// immutable requester ID. Missing profiles do not hide historical requests.
    pub requester_logins: std::collections::HashMap<u64, String>,
    pub older: Option<Boundary>,
}

// Match migration 0011. A single fixed-width time/ID key seeks past even a
// large equal-timestamp prefix; SQLite tuple comparisons only seek the time.
const SEEK_KEY: &str = "(substr(created_at,1,19) || '.' || substr(CASE WHEN substr(created_at,20,1) = '.' THEN replace(replace(substr(created_at,21),'+00:00',''),'Z','') ELSE '' END || '000000000',1,9) || '/' || id)";

pub fn boundary_key(boundary: Boundary) -> String {
    let time = boundary
        .admitted_at
        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
        .trim_end_matches('Z')
        .to_owned();
    format!("{time}/{}", boundary.id)
}

/// Bind account, link, nullable seek key. Fetch one extra
/// row to determine whether another page exists without a count or history scan.
pub fn query(before: Option<Boundary>) -> String {
    let bound = if before.is_some() {
        format!("{SEEK_KEY} < ?3")
    } else {
        "?3 IS NULL".into()
    };
    format!(
        "SELECT id, invitation_link_id, requester_id, justification, state, decided_by, decided_at, decline_reason, created_at, decision_deadline, (SELECT login FROM users WHERE user_id = invitation_requests.requester_id) AS requester_login FROM invitation_requests WHERE invitation_link_id = ?2 AND EXISTS (SELECT 1 FROM invitation_links WHERE id = ?2 AND account_id = ?1) AND {bound} ORDER BY {SEEK_KEY} DESC LIMIT {}",
        PAGE_SIZE + 1
    )
}

pub fn page(mut rows: Vec<(InvitationRequest, Option<String>)>) -> Page {
    let more = rows.len() > PAGE_SIZE;
    rows.truncate(PAGE_SIZE);
    let requester_logins = rows
        .iter()
        .filter_map(|(request, login)| {
            login
                .as_ref()
                .map(|login| (request.requester_id, login.clone()))
        })
        .collect();
    let requests: Vec<_> = rows.into_iter().map(|(request, _)| request).collect();
    let older = if more {
        requests.last().map(Boundary::from)
    } else {
        None
    };
    Page {
        requests,
        older,
        requester_logins,
    }
}
