//! Validated console visibility and shared predicates for delegated readers.
//! These scopes are internal assertions from the authority service, not wire input.
use super::{AuditPosition, Error, Result, audit_read, integer, invitation_links, pending_queue};
use crate::audit::EventType;
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleScope {
    account_id: u64,
    repos: Option<BTreeSet<u64>>,
}
impl ConsoleScope {
    pub fn account(account_id: u64) -> Result<Self> {
        valid_id(account_id)?;
        Ok(Self {
            account_id,
            repos: None,
        })
    }
    pub fn repo_admin(account_id: u64, repos: impl IntoIterator<Item = u64>) -> Result<Self> {
        valid_id(account_id)?;
        let mut ids = BTreeSet::new();
        for id in repos {
            valid_id(id)?;
            ids.insert(id);
            if ids.len() > 5000 {
                return Err(invalid());
            }
        }
        Ok(Self {
            account_id,
            repos: Some(ids),
        })
    }
    pub fn account_id(&self) -> u64 {
        self.account_id
    }
    pub fn is_account(&self) -> bool {
        self.repos.is_none()
    }
    /// A single JSON binding avoids SQLite/D1 variable-count limits.
    pub fn repo_ids_json(&self) -> Option<String> {
        self.repos
            .as_ref()
            .map(|ids| serde_json::to_string(ids).expect("numeric set"))
    }
}
fn invalid() -> Error {
    Error::ProjectionInvariant("invalid console scope".into())
}
fn valid_id(id: u64) -> Result<()> {
    if id == 0 || id > integer::MAX_EXACT {
        Err(invalid())
    } else {
        Ok(())
    }
}
// All fragments are selected by code, never derived from a request or identifier.
fn allowed_links(binding: &str) -> String {
    format!(
        "SELECT cs.id FROM invitation_links cs WHERE cs.account_id=?1 \
        AND EXISTS(SELECT 1 FROM invitation_link_targets t WHERE t.link_id=cs.id AND t.kind='repo') \
        AND NOT EXISTS(SELECT 1 FROM invitation_link_targets t WHERE t.link_id=cs.id \
            AND (t.kind!='repo' OR t.target_id NOT IN (SELECT value FROM json_each({binding}))))"
    )
}
fn allowed_batches(binding: &str) -> String {
    format!(
        "SELECT cb.id FROM bulk_batches cb WHERE cb.account_id=?1 \
        AND json_extract(cb.manifest,'$.targets.organization') IS NULL \
        AND json_array_length(cb.manifest,'$.targets.teams')=0 \
        AND json_array_length(cb.manifest,'$.targets.repos')>0 \
        AND NOT EXISTS(SELECT 1 FROM json_each(cb.manifest,'$.targets.repos') t \
            WHERE json_extract(t.value,'$.repo_id') NOT IN (SELECT value FROM json_each({binding})))"
    )
}
fn scoped(sql: &str, anchor: &str, replacement: &str) -> String {
    // A refactor of an owner query must never silently drop the authorization filter.
    assert_eq!(
        sql.matches(anchor).count(),
        1,
        "console query scope anchor changed"
    );
    sql.replacen(anchor, replacement, 1)
}
pub fn links_query() -> String {
    scoped(
        invitation_links::FOR_ACCOUNT,
        "WHERE l.account_id=?1",
        &format!(
            "WHERE l.account_id=?1 AND l.id IN ({})",
            allowed_links("?2")
        ),
    )
}
pub fn count_query() -> String {
    format!(
        "{} AND r.invitation_link_id IN ({})",
        pending_queue::COUNT_QUERY,
        allowed_links("?2")
    )
}
pub fn pending_query(after: bool) -> String {
    scoped(
        &pending_queue::query(after),
        "r.state = 'pending'",
        &format!(
            "r.state = 'pending' AND r.invitation_link_id IN ({})",
            allowed_links("?3")
        ),
    )
}
pub fn audit_query(event: Option<EventType>, position: AuditPosition, probe: bool) -> String {
    let links = allowed_links("?5");
    let batches = allowed_batches("?5");
    let requests = format!(
        "SELECT ar.id FROM invitation_requests ar WHERE \
        ar.invitation_link_id IN ({links}) OR ar.batch_id IN ({batches})"
    );
    let visible = format!(
        "((audit_events.target_kind='invitation_link' AND audit_events.target_id IN ({links})) \
        OR (audit_events.target_kind='bulk_batch' AND audit_events.target_id IN ({batches})) \
        OR (audit_events.target_kind='invitation_request' AND audit_events.target_id IN ({requests})) \
        OR (audit_events.target_kind='access_grant' AND EXISTS(SELECT 1 FROM access_grants ag WHERE ag.id=audit_events.target_id AND ag.request_id IN ({requests}))) \
        OR (audit_events.target_kind='repository_delivery' AND EXISTS(SELECT 1 FROM deliveries ad WHERE ad.kind='repo' AND ad.id=audit_events.target_id AND ad.request_id IN ({requests}))) \
        OR (audit_events.target_kind='github_invitation' AND EXISTS(SELECT 1 FROM deliveries ad LEFT JOIN delivery_outcomes ado ON ado.delivery_id=ad.id AND ado.request_id=ad.request_id WHERE (ad.id=audit_events.target_id OR json_extract(ado.receipt,'$.create.command.invitation_id')=audit_events.target_id) AND ad.request_id IN ({requests}))))"
    );
    scoped(
        &audit_read::query(event, position, probe),
        "WHERE account_id = ?1 AND",
        &format!("WHERE account_id = ?1 AND {visible} AND"),
    )
}
/// Names are display hints from projected link targets, never authority evidence.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct RepoTarget {
    pub id: u64,
    pub full_name: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoTargetPage {
    pub rows: Vec<RepoTarget>,
    pub next: Option<u64>,
}
pub const REPO_TARGETS: &str = "WITH targets AS (SELECT t.target_id AS id,t.display_name AS full_name FROM invitation_link_targets t JOIN invitation_links l ON l.id=t.link_id WHERE l.account_id=?1 AND t.kind='repo' UNION ALL SELECT json_extract(t.value,'$.repo_id'),json_extract(t.value,'$.repo_full_name') FROM bulk_batches b JOIN json_each(b.manifest,'$.targets.repos') t WHERE b.account_id=?1) SELECT id,MIN(full_name) AS full_name FROM targets WHERE id>?2 GROUP BY id ORDER BY id LIMIT ?3";
pub fn target_query_args(
    account_id: u64,
    after: Option<u64>,
    limit: u16,
) -> Result<(u64, u64, u64)> {
    valid_id(account_id)?;
    if let Some(id) = after {
        valid_id(id)?;
    }
    if !(1..=501).contains(&limit) {
        return Err(invalid());
    }
    Ok((account_id, after.unwrap_or(0), u64::from(limit) + 1))
}
pub fn target_page(mut rows: Vec<RepoTarget>, limit: u16) -> Result<RepoTargetPage> {
    for row in &rows {
        valid_id(row.id)?;
    }
    let more = rows.len() > usize::from(limit);
    rows.truncate(usize::from(limit));
    let next = rows.last().filter(|_| more).map(|r| r.id);
    Ok(RepoTargetPage { rows, next })
}
