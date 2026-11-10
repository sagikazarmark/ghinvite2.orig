//! Scoped organization projection reads. These receipts describe projected
//! observations; absence or dispatch submission never proves a successful grant.
use super::{Error, Result, org_delivery::SnapshotRow};
use crate::{InvitationLinkId, RequestId, org_delivery::OrganizationDeliverySnapshot};
use async_trait::async_trait;

/// The caller must authorize the account/link/request before reading. Binding
/// all three prevents a request ID from escaping its authorized parent scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrgDeliveryQuery {
    pub account_id: u64,
    pub link_id: InvitationLinkId,
    pub request_id: RequestId,
}

#[async_trait]
pub trait OrgDeliveryReadStorage: Send + Sync + 'static {
    /// Every projected generation, ordered by organization ID then generation.
    /// Team receipts come from the same atomic parent snapshot.
    async fn list_organization_deliveries(
        &self,
        query: &OrgDeliveryQuery,
    ) -> Result<Vec<OrganizationDeliverySnapshot>>;
}

pub const LIST: &str = r#"SELECT d.id,o.receipt
FROM deliveries d
JOIN delivery_outcomes o ON o.delivery_id=d.id AND o.request_id=d.request_id
JOIN invitation_requests r ON r.id=d.request_id
JOIN invitation_links l ON l.id=r.invitation_link_id
WHERE d.kind='org' AND l.account_id=?1 AND l.id=?2 AND r.id=?3
ORDER BY d.target_id ASC,d.delivery_generation ASC"#;

pub fn decode(row: SnapshotRow, query: &OrgDeliveryQuery) -> Result<OrganizationDeliverySnapshot> {
    let snapshot = row.decode()?;
    let command = &snapshot.create.command;
    if command.account_id != query.account_id
        || command.link_id != Some(query.link_id)
        || command.request_id != query.request_id
    {
        return Err(Error::Corrupt("organization receipt scope mismatch".into()));
    }
    Ok(snapshot)
}
