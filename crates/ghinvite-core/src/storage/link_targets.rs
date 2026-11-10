//! Canonical target reader preserves each target's grant.
use crate::targets::*;
use serde::Deserialize;
pub const GET: &str = "SELECT kind,target_id,display_name,grant FROM invitation_link_targets WHERE link_id=?1 ORDER BY kind,target_id";
#[derive(Debug, Deserialize)]
pub struct Row {
    pub kind: String,
    pub target_id: u64,
    pub display_name: String,
    pub grant: String,
}
pub fn decode(rows: Vec<Row>) -> super::Result<Targets> {
    if rows.is_empty() {
        return Err(super::Error::NotFound);
    }
    let mut targets = Targets::default();
    let invalid = || super::Error::Corrupt("invalid target set".into());
    for r in rows {
        match r.kind.as_str() {
            "repo" => {
                let grant = match r.grant.as_str() {
                    "read" => RepoRole::Read,
                    "triage" => RepoRole::Triage,
                    "write" => RepoRole::Write,
                    "maintain" => RepoRole::Maintain,
                    "admin" => RepoRole::Admin,
                    _ => return Err(invalid()),
                };
                targets.repos.push(RepoTarget {
                    repo_id: r.target_id,
                    repo_full_name: r.display_name,
                    grant,
                });
            }
            "org" if r.grant == "member" && targets.organization.is_none() => {
                targets.organization = Some(OrganizationTarget {
                    org_id: r.target_id,
                    org_login: r.display_name,
                    grant: MemberGrant::Member,
                })
            }
            "team" if r.grant == "member" => targets.teams.push(TeamTarget {
                team_id: r.target_id,
                team_slug: r.display_name,
                grant: MemberGrant::Member,
            }),
            _ => return Err(invalid()),
        }
    }
    targets.validate().map_err(|_| invalid())?;
    Ok(targets)
}
