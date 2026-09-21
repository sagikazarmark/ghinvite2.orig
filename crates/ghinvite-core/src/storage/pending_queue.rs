//! Bounded account-local decision queue, shared by SQLite and D1.
use crate::{InvitationLinkId, Permission, RequestId};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;

pub const PENDING_PAGE_SIZE: usize = 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingBoundary {
    pub created_at: DateTime<Utc>,
    pub request_id: RequestId,
}

impl PendingBoundary {
    pub fn seek_key(self) -> String {
        let time = self
            .created_at
            .to_rfc3339_opts(SecondsFormat::Nanos, true)
            .trim_end_matches('Z')
            .to_string();
        format!("{time}/{}", self.request_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PendingRow {
    pub request_id: RequestId,
    pub link_slug: String,
    pub link_description: Option<String>,
    pub link_id: Option<InvitationLinkId>,
    pub requester_login: String,
    pub justification: Option<String>,
    pub created_at: DateTime<Utc>,
    pub decision_deadline: Option<DateTime<Utc>>,
    pub permission: Option<Permission>,
    pub repos: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub approval_required: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingPage {
    pub rows: Vec<PendingRow>,
    pub next: Option<PendingBoundary>,
}

impl PendingPage {
    pub fn from_json(rows: Vec<String>) -> super::Result<Self> {
        let mut rows = rows
            .into_iter()
            .map(|row| {
                serde_json::from_str::<PendingRow>(&row)
                    .map_err(|e| super::Error::Corrupt(e.to_string()))
            })
            .collect::<super::Result<Vec<_>>>()?;
        let more = rows.len() > PENDING_PAGE_SIZE;
        rows.truncate(PENDING_PAGE_SIZE);
        let next = rows.last().filter(|_| more).map(|row| PendingBoundary {
            created_at: row.created_at,
            request_id: row.request_id,
        });
        Ok(Self { rows, next })
    }
}

// Keep this expression identical to `idx_pending_queue_seek` in migrations. One composite text key
// seeks past even a large equal-timestamp prefix (a tuple over an expression
// index only seeks the timestamp on SQLite). Both components are fixed width.
const SEEK_KEY: &str = "(substr(r.created_at,1,19) || '.' || substr(CASE WHEN substr(r.created_at,20,1) = '.' THEN replace(replace(substr(r.created_at,21),'+00:00',''),'Z','') ELSE '' END || '000000000',1,9) || '/' || r.id)";

/// Pending-request count for one account, with the same scoping as [`query`]:
/// pending state, seeked by the derived `queue_account_id`, and authorized by
/// the current owning link's account (requests whose link is missing are not
/// counted). One statement over `idx_pending_queue_seek`; no rows are loaded.
pub const COUNT_QUERY: &str = "SELECT COUNT(*) AS pending FROM invitation_requests r
      JOIN invitation_links owner ON owner.id = r.invitation_link_id AND owner.account_id = ?1
      WHERE r.queue_account_id = ?1 AND r.state = 'pending'";

/// One statement, at most 26 requests. Related links/repository scopes are
/// materialized once per distinct page link, not once per request. No total count
/// or all-account hydration. Missing users retain the historical ID fallback;
/// links without a verifiable owner are excluded.
pub fn query(after: bool) -> String {
    let seek = if after {
        format!("AND {SEEK_KEY} > ?2")
    } else {
        "AND ?2 IS NULL".into()
    };
    let limit = PENDING_PAGE_SIZE + 1;
    format!(
        r#"WITH page AS MATERIALIZED (
      SELECT r.id, r.invitation_link_id, r.requester_id, r.justification,
        r.created_at, r.decision_deadline, {SEEK_KEY} AS seek_key FROM invitation_requests r
      JOIN invitation_links owner ON owner.id = r.invitation_link_id AND owner.account_id = ?1
      WHERE r.queue_account_id = ?1 AND r.state = 'pending' {seek}
      ORDER BY {SEEK_KEY} LIMIT {limit}
    ), links AS MATERIALIZED (
      SELECT l.id, l.slug, l.description, l.permission, l.expires_at,
        l.approval_required, COALESCE((SELECT json_group_array(repo_full_name) FROM
        (SELECT repo_full_name FROM invitation_link_repos WHERE invitation_link_id = l.id ORDER BY repo_id)), '[]') AS repos
      FROM invitation_links l WHERE l.id IN (SELECT invitation_link_id FROM page)
    )
    SELECT json_object(
      'request_id', p.id, 'link_id', l.id, 'link_slug', COALESCE(l.slug, '(deleted link)'),
      'link_description', l.description, 'requester_login', COALESCE(u.login, 'user-' || p.requester_id),
      'justification', p.justification, 'created_at', p.created_at, 'decision_deadline', p.decision_deadline,
      'permission', l.permission, 'repos', json(COALESCE(l.repos, '[]')), 'expires_at', l.expires_at,
      'approval_required', json(CASE l.approval_required WHEN 1 THEN 'true' WHEN 0 THEN 'false' ELSE 'null' END)
    ) AS row_json FROM page p
    LEFT JOIN links l ON l.id = p.invitation_link_id
    LEFT JOIN users u ON u.user_id = p.requester_id
    ORDER BY p.seek_key"#
    )
}
