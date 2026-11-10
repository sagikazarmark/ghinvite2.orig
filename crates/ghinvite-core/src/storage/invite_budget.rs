//! Core-owned monotonic budget snapshot projection and shared adapter contract.
use super::{Error, Result, integer};
use crate::{account::AccountInstallationRef, budget::BudgetSnapshot};
use async_trait::async_trait;
use serde::Deserialize;
#[async_trait]
pub trait InviteBudgetStorage: Send + Sync + 'static {
    /// Display-only delayed delivery observations, scoped to an app/account and
    /// optionally one already-authorized link. At most 100 links are returned.
    async fn share_limited_links(
        &self,
        owner: AccountInstallationRef,
        link_id: Option<crate::InvitationLinkId>,
    ) -> Result<Vec<ShareLimitedLink>>;
    async fn project_invite_budget(&self, snapshot: &BudgetSnapshot) -> Result<()>;
    async fn get_invite_budget(
        &self,
        owner: AccountInstallationRef,
    ) -> Result<Option<BudgetSnapshot>>;
    async fn list_invite_budget_buckets(
        &self,
        owner: AccountInstallationRef,
    ) -> Result<Vec<crate::budget::BucketSnapshot>>;
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ShareLimitedLink {
    pub link_id: crate::InvitationLinkId,
    pub description: String,
}

pub const SHARE_LIMITED_LINKS: &str = "SELECT DISTINCT l.id AS link_id,l.description FROM deliveries d JOIN invitation_requests r ON r.id=d.request_id JOIN invitation_links l ON l.id=r.invitation_link_id WHERE d.kind=?1 AND l.account_id=?2 AND (?3='' OR l.id=?3) AND d.create_outcome='throttled' AND d.outcome_reason='link_share' AND d.tracking_stopped_reason IS NULL AND (d.settlement IS NULL OR d.settlement='sent') ORDER BY l.id DESC LIMIT 100";
pub fn encode(snapshot: &BudgetSnapshot) -> Result<String> {
    integer::validate(snapshot)?;
    if snapshot.account_id == 0
        || snapshot.revision == 0
        || snapshot
            .buckets
            .iter()
            .any(|b| !b.bucket.valid_for(snapshot.app) || b.cap == 0)
        || snapshot
            .buckets
            .windows(2)
            .any(|pair| pair[0].bucket >= pair[1].bucket)
    {
        return Err(Error::ProjectionInvariant("invalid budget snapshot".into()));
    }
    serde_json::to_string(snapshot)
        .map_err(|_| Error::ProjectionInvariant("invalid budget snapshot".into()))
}
pub fn classify(message: String) -> Error {
    super::projection::sql::classify(message)
}
#[derive(Deserialize)]
pub struct Row {
    pub snapshot_json: String,
}
impl Row {
    pub fn decode(self) -> Result<BudgetSnapshot> {
        let s: BudgetSnapshot = serde_json::from_str(&self.snapshot_json)
            .map_err(|_| Error::Corrupt("budget snapshot".into()))?;
        encode(&s)?;
        Ok(s)
    }
}
pub const GET: &str =
    "SELECT snapshot_json FROM invite_budget_projection WHERE app=?1 AND account_id=?2";
/// A single snapshot transaction rejects differing same-revision content;
/// delayed older snapshots cannot resurrect removed rows or erase newer ones.
pub const APPLY: &[&str] = &[
    "INSERT INTO projection_assertions(invariant) SELECT 0 WHERE EXISTS(SELECT 1 FROM invite_budget_projection WHERE app=json_extract(?1,'$.app') AND account_id=json_extract(?1,'$.account_id') AND revision=json_extract(?1,'$.revision') AND snapshot_json!=?1)",
    "DELETE FROM invite_budget_snapshot WHERE app=json_extract(?1,'$.app') AND account_id=json_extract(?1,'$.account_id') AND NOT EXISTS(SELECT 1 FROM invite_budget_projection WHERE app=json_extract(?1,'$.app') AND account_id=json_extract(?1,'$.account_id') AND revision>=json_extract(?1,'$.revision'))",
    "INSERT INTO invite_budget_snapshot(app,account_id,bucket,used_24h,cap,closed_until,next_slot_at,projection_revision,observed_at) SELECT json_extract(?1,'$.app'),json_extract(?1,'$.account_id'),CASE json_extract(b.value,'$.bucket.kind') WHEN 'org' THEN 'org' WHEN 'content' THEN 'content' ELSE 'repo:'||CAST(json_extract(b.value,'$.bucket.repo_id') AS INTEGER) END,json_extract(b.value,'$.used_24h'),json_extract(b.value,'$.cap'),json_extract(b.value,'$.closed_until'),json_extract(b.value,'$.next_slot_at'),json_extract(?1,'$.revision'),json_extract(?1,'$.observed_at') FROM json_each(?1,'$.buckets') b WHERE NOT EXISTS(SELECT 1 FROM invite_budget_projection WHERE app=json_extract(?1,'$.app') AND account_id=json_extract(?1,'$.account_id') AND revision>=json_extract(?1,'$.revision'))",
    "INSERT INTO invite_budget_projection(app,account_id,revision,snapshot_json) VALUES(json_extract(?1,'$.app'),json_extract(?1,'$.account_id'),json_extract(?1,'$.revision'),?1) ON CONFLICT(app,account_id) DO UPDATE SET revision=excluded.revision,snapshot_json=excluded.snapshot_json WHERE invite_budget_projection.revision<excluded.revision",
    "DELETE FROM projection_assertions WHERE ?1 IS NOT NULL",
];

pub const LIST: &str = "SELECT bucket,used_24h,cap,closed_until,next_slot_at FROM invite_budget_snapshot WHERE app=?1 AND account_id=?2 ORDER BY CASE WHEN bucket='org' THEN 0 WHEN substr(bucket,1,5)='repo:' THEN 1 ELSE 2 END,CAST(substr(bucket,6) AS INTEGER)";
#[derive(Deserialize)]
pub struct BucketRow {
    pub bucket: String,
    pub used_24h: u32,
    pub cap: u32,
    pub closed_until: Option<chrono::DateTime<chrono::Utc>>,
    pub next_slot_at: Option<chrono::DateTime<chrono::Utc>>,
}
impl BucketRow {
    pub fn decode(self) -> Result<crate::budget::BucketSnapshot> {
        let bucket = match self.bucket.as_str() {
            "org" => crate::budget::BucketId::Org,
            "content" => crate::budget::BucketId::Content,
            other => {
                let repo_id = other
                    .strip_prefix("repo:")
                    .and_then(|v| v.parse::<u64>().ok())
                    .filter(|v| *v > 0 && *v <= integer::MAX_EXACT)
                    .ok_or_else(|| Error::Corrupt("budget bucket identity".into()))?;
                crate::budget::BucketId::Repo { repo_id }
            }
        };
        Ok(crate::budget::BucketSnapshot {
            bucket,
            used_24h: self.used_24h,
            cap: self.cap,
            closed_until: self.closed_until,
            next_slot_at: self.next_slot_at,
        })
    }
}
#[cfg(feature = "test-suite")]
pub mod conformance;
