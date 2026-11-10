//! Canonical target grants, shared by SQLite and D1. No global permission facade.
use crate::{InvitationLink, InvitationLinkId, LinkSecretState, SecretHash, targets::Targets};
use chrono::{DateTime, Utc};
use serde::Deserialize;
macro_rules! select {
    ($filter:literal) => {
        concat!(
            "SELECT l.id,l.source,json_extract(l.projection_content,'$.creation.batch_source') AS batch_source_json,l.installation_app,l.installation_id,l.organization_installation_id,l.account_id,l.created_by,l.created_at,
    l.expires_at,l.max_uses,l.access_lifetime_secs AS access_lifetime_seconds,l.email_domains_json,COALESCE(l.invitation_lifetime_secs,604800) AS invitation_lifetime_seconds,l.abuse_threshold AS abuse_threshold_per_hour,COALESCE(l.budget_share_pct,50) AS budget_share_pct,l.uses_count,l.approval_required,l.description,l.internal_note,
    l.revoked_at,l.revoked_by,l.secret_sha256,l.secret_generation,l.state,l.paused_at,l.paused_by,l.pause_epoch,l.pause_reason,
    json_extract(l.abuse_window_json,'$') AS abuse_window_start,
    r.kind AS target_kind,r.target_id,r.display_name,r.grant AS target_grant
    FROM invitation_links l LEFT JOIN invitation_link_targets r ON r.link_id=l.id ",
            $filter
        )
    };
}
pub const GET: &str = select!("WHERE l.id=?1 ORDER BY r.kind,r.target_id");
pub const FOR_ACCOUNT: &str =
    select!("WHERE l.account_id=?1 ORDER BY l.created_at DESC,l.id,r.kind,r.target_id");
pub const BELONGS_TO_ACCOUNT: &str =
    "SELECT 1 AS present FROM invitation_links WHERE id=?1 AND account_id=?2 LIMIT 1";
#[derive(Debug, Deserialize)]
pub struct LinkRepoRow {
    pub source: String,
    pub batch_source_json: Option<String>,
    pub id: InvitationLinkId,
    pub installation_app: crate::account::App,
    pub installation_id: u64,
    pub organization_installation_id: Option<u64>,
    pub account_id: u64,
    pub created_by: u64,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    pub email_domains_json: Option<String>,
    pub invitation_lifetime_seconds: u32,
    pub access_lifetime_seconds: Option<u32>,
    pub abuse_threshold_per_hour: Option<u32>,
    pub budget_share_pct: u8,
    pub uses_count: u32,
    pub approval_required: i64,
    pub description: String,
    pub internal_note: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<u64>,
    pub secret_sha256: SecretHash,
    pub secret_generation: u32,
    pub state: String,
    pub paused_at: Option<DateTime<Utc>>,
    pub paused_by: Option<u64>,
    pub pause_epoch: Option<u64>,
    pub pause_reason: Option<String>,
    pub abuse_window_start: Option<DateTime<Utc>>,
    pub target_kind: Option<String>,
    pub target_id: Option<u64>,
    pub display_name: Option<String>,
    pub target_grant: Option<String>,
}
pub fn fold(rows: Vec<LinkRepoRow>) -> super::Result<Vec<InvitationLink>> {
    let mut links: Vec<InvitationLink> = Vec::new();
    let mut target_rows = Vec::new();
    let mut primary_app = None;
    let finish = |link: &mut InvitationLink,
                  rows: &mut Vec<super::link_targets::Row>,
                  app: crate::account::App|
     -> super::Result<()> {
        // A missing child projection is explicitly empty and can be repaired by
        // replay; it never inherits grants from retained JSON or another target.
        link.targets = if rows.is_empty() {
            Targets::default()
        } else {
            super::link_targets::decode(std::mem::take(rows))?
        };
        if !link.targets.repos.is_empty() || link.targets.organization.is_some() {
            link.validate_installations()
                .map_err(|_| super::Error::Corrupt("invalid target installation binding".into()))?;
            if link.primary_app() != app {
                return Err(super::Error::Corrupt(
                    "invalid primary installation app".into(),
                ));
            }
        }
        Ok(())
    };
    for row in rows {
        if links.last().is_none_or(|link| link.id != row.id) {
            if let Some(link) = links.last_mut() {
                finish(
                    link,
                    &mut target_rows,
                    primary_app.expect("link app retained"),
                )?;
            }
            primary_app = Some(row.installation_app);
            if !matches!(row.approval_required, 0 | 1)
                || row.secret_generation == 0
                || !(3600..=604800).contains(&row.invitation_lifetime_seconds)
                || row.abuse_threshold_per_hour == Some(0)
                || !(1..=100).contains(&row.budget_share_pct)
            {
                return Err(super::Error::Corrupt("invalid invitation link".into()));
            }
            let pause = match (row.paused_at, row.pause_epoch, row.pause_reason.as_deref()) {
                (None, None, None)
                    if row.paused_by.is_none() && row.abuse_window_start.is_none() =>
                {
                    None
                }
                (Some(at), Some(epoch), Some(reason)) if epoch > 0 => {
                    use crate::link_lifecycle::{LinkPause, PauseReason};
                    let reason = match (reason, row.paused_by, row.abuse_window_start) {
                        ("manual", Some(by), None) if by > 0 => PauseReason::Manual,
                        ("abuse_suspected", None, Some(window_start)) if window_start <= at => {
                            PauseReason::AbuseSuspected { window_start }
                        }
                        _ => return Err(super::Error::Corrupt("invalid link pause".into())),
                    };
                    Some(LinkPause {
                        epoch,
                        at,
                        by: row.paused_by,
                        reason,
                    })
                }
                _ => return Err(super::Error::Corrupt("invalid link pause".into())),
            };
            let expected_state = if row.revoked_at.is_some() {
                "revoked"
            } else if pause.is_some() {
                "paused"
            } else {
                "active"
            };
            if row.state != expected_state {
                return Err(super::Error::Corrupt("invalid link state".into()));
            }
            let batch_source: Option<crate::bulk::BatchRef> = row
                .batch_source_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| super::Error::Corrupt("invalid link source".into()))?;
            if row.source
                != if batch_source.is_some() {
                    "batch"
                } else {
                    "console"
                }
                || batch_source.is_some_and(|source| source.validate().is_err())
            {
                return Err(super::Error::Corrupt("invalid link source".into()));
            }
            links.push(InvitationLink {
                batch_source,
                access_lifetime_seconds: row.access_lifetime_seconds,
                email_domains: decode_domain_rules(row.email_domains_json.as_deref())?,
                id: row.id,
                organization_installation_id: row.organization_installation_id,
                installation_id: row.installation_id,
                account_id: row.account_id,
                created_by: row.created_by,
                created_at: row.created_at,
                expires_at: row.expires_at,
                max_uses: row.max_uses,
                invitation_lifetime_seconds: row.invitation_lifetime_seconds,
                abuse_threshold_per_hour: row.abuse_threshold_per_hour,
                budget_share_pct: row.budget_share_pct,
                uses_count: row.uses_count,
                approval_required: row.approval_required != 0,
                description: row.description,
                internal_note: row.internal_note,
                revoked_at: row.revoked_at,
                revoked_by: row.revoked_by,
                pause,
                secret: LinkSecretState {
                    secret_generation: row.secret_generation,
                    sha256: row.secret_sha256,
                },
                targets: Targets::default(),
            });
        }
        match (
            row.target_kind,
            row.target_id,
            row.display_name,
            row.target_grant,
        ) {
            (Some(kind), Some(target_id), Some(display_name), Some(grant)) => {
                target_rows.push(super::link_targets::Row {
                    kind,
                    target_id,
                    display_name,
                    grant,
                })
            }
            (None, None, None, None) => {}
            _ => return Err(super::Error::Corrupt("incomplete target row".into())),
        }
    }
    if let Some(link) = links.last_mut() {
        finish(
            link,
            &mut target_rows,
            primary_app.expect("link app retained"),
        )?;
    }
    Ok(links)
}

fn decode_domain_rules(value: Option<&str>) -> super::Result<Vec<crate::email_domain::DomainRule>> {
    let rules: Vec<crate::email_domain::DomainRule> = value
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| super::Error::Corrupt("invalid email domain rules".into()))?
        .unwrap_or_default();
    if rules.len() > crate::email_domain::MAX_DOMAIN_RULES
        || rules.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(super::Error::Corrupt("invalid email domain rules".into()));
    }
    Ok(rules)
}
