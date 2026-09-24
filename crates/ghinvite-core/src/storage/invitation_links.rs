//! Invitation links with their repository scope, shared by SQLite and D1. One
//! LEFT JOIN row per repository (or one repo-less row), folded by [`fold`].
use crate::{InvitationLink, InvitationLinkId, InvitationLinkRepo, Permission};
use chrono::{DateTime, Utc};
use serde::Deserialize;

macro_rules! select {
    ($filter:literal) => {
        concat!(
            "SELECT l.id, l.installation_id, l.account_id, l.created_by, l.created_at,
            l.expires_at, l.max_uses, l.uses_count, l.permission, l.approval_required,
            l.description, l.internal_note, l.revoked_at, l.revoked_by, r.repo_id, r.repo_full_name
            FROM invitation_links l LEFT JOIN invitation_link_repos r ON r.invitation_link_id = l.id ",
            $filter
        )
    };
}

pub const GET: &str = select!("WHERE l.id = ?1 ORDER BY r.repo_id");
/// Newest first; each link's rows are adjacent, as [`fold`] requires.
pub const FOR_ACCOUNT: &str =
    select!("WHERE l.account_id = ?1 ORDER BY l.created_at DESC, l.id, r.repo_id");
/// Bind link ID and account ID; returns at most one row.
pub const BELONGS_TO_ACCOUNT: &str =
    "SELECT 1 AS present FROM invitation_links WHERE id = ?1 AND account_id = ?2 LIMIT 1";

#[derive(Debug, Deserialize)]
pub struct LinkRepoRow {
    pub id: InvitationLinkId,
    pub installation_id: u64,
    pub account_id: u64,
    pub created_by: u64,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    pub uses_count: u32,
    pub permission: Permission,
    pub approval_required: i64,
    pub description: String,
    pub internal_note: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<u64>,
    pub repo_id: Option<u64>,
    pub repo_full_name: Option<String>,
}

/// Collapse join rows into links in row order. Rows of one link must be
/// adjacent; repositories keep their row order.
pub fn fold(rows: Vec<LinkRepoRow>) -> super::Result<Vec<InvitationLink>> {
    let mut links: Vec<InvitationLink> = Vec::new();
    for row in rows {
        let repo = match (row.repo_id, row.repo_full_name.clone()) {
            (Some(repo_id), Some(repo_full_name)) => Some(InvitationLinkRepo {
                repo_id,
                repo_full_name,
            }),
            _ => None,
        };
        if links.last().is_none_or(|link| link.id != row.id) {
            links.push(InvitationLink {
                id: row.id,
                installation_id: row.installation_id,
                account_id: row.account_id,
                created_by: row.created_by,
                created_at: row.created_at,
                expires_at: row.expires_at,
                max_uses: row.max_uses,
                uses_count: row.uses_count,
                permission: row.permission,
                approval_required: row.approval_required != 0,
                description: row.description,
                internal_note: row.internal_note,
                revoked_at: row.revoked_at,
                revoked_by: row.revoked_by,
                repos: Vec::new(),
            });
        }
        let link = links.last_mut().expect("pushed above");
        link.repos.extend(repo);
    }
    Ok(links)
}
