//! Link lifecycle decisions. Expiry and exhaustion remain immutable guardrails.
use crate::{
    InvitationLinkId, RequestId,
    invitation_link::{AccountAdmin, LinkSnapshot},
};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PauseReason {
    Manual,
    AbuseSuspected { window_start: DateTime<Utc> },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkPause {
    /// The link revision that established this pause, also its hold epoch.
    pub epoch: u64,
    pub at: DateTime<Utc>,
    pub by: Option<u64>,
    pub reason: PauseReason,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct LifecycleOperationId(RequestId);
impl TryFrom<String> for LifecycleOperationId {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        crate::request_lifecycle::parse_operation_id(&value).map(Self)
    }
}
impl From<LifecycleOperationId> for String {
    fn from(value: LifecycleOperationId) -> Self {
        value.0.to_string()
    }
}
/// A private control operation is scoped to its authoritative link revision.
/// It never reuses a requester/admin supplied operation ID, which could collide
/// with an earlier control receipt and suppress a required hold.
pub fn control_operation_id(link_id: InvitationLinkId, revision: u64) -> LifecycleOperationId {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("ghinvite/link-control/v1/{link_id}/{revision}"));
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    LifecycleOperationId(RequestId::from_ulid(ulid::Ulid::from_bytes(bytes)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleAction {
    Pause,
    Resume,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LifecycleCommand {
    pub link_id: InvitationLinkId,
    pub admin: AccountAdmin,
    pub operation_id: LifecycleOperationId,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleRefusal {
    Revoked,
    AlreadyPaused,
    NotPaused,
    ExpiredOrExhausted,
}
pub type LifecycleOutcome = Result<LinkSnapshot, LifecycleRefusal>;

/// The caller journals time and retains the exact input-bound result. Manual
/// pause changes no request/delivery; release of an abuse hold is a caller effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid link lifecycle state")]
pub struct InvalidLifecycleState;

pub fn decide(
    link: &LinkSnapshot,
    action: LifecycleAction,
    actor: u64,
    now: DateTime<Utc>,
) -> Result<LifecycleOutcome, InvalidLifecycleState> {
    if link.revision == 0
        || link.revision >= crate::storage::integer::MAX_EXACT
        || actor == 0
        || actor > crate::storage::integer::MAX_EXACT
        || now < link.created_at
    {
        return Err(InvalidLifecycleState);
    }
    if link.revoked_at.is_some() {
        return Ok(Err(LifecycleRefusal::Revoked));
    }
    let mut next = link.clone();
    match action {
        LifecycleAction::Pause => {
            if link.pause.is_some() {
                return Ok(Err(LifecycleRefusal::AlreadyPaused));
            }
            next.revision += 1;
            next.pause = Some(LinkPause {
                epoch: next.revision,
                at: now,
                by: Some(actor),
                reason: PauseReason::Manual,
            });
        }
        LifecycleAction::Resume => {
            if link.pause.is_none() {
                return Ok(Err(LifecycleRefusal::NotPaused));
            }
            if crate::Inactive::check(
                false,
                link.creation.expires_at,
                link.uses,
                link.creation.max_uses,
                now,
            )
            .is_some()
            {
                return Ok(Err(LifecycleRefusal::ExpiredOrExhausted));
            }
            next.revision += 1;
            next.pause = None;
        }
    }
    Ok(Ok(next))
}
