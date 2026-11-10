//! Link control messages addressed to the retained request owner. These carry
//! immutable admission facts so control can arrive before initialization.
use crate::{
    InvitationLinkId, RequestId, delivery_control::LinkControl,
    link_lifecycle::LifecycleOperationId,
};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestControl {
    pub link_id: InvitationLinkId,
    pub request_id: RequestId,
    pub operation_id: LifecycleOperationId,
    pub revision: u64,
    pub hold_epoch: u64,
    pub at: DateTime<Utc>,
    pub control: LinkControl,
    pub admitted_at: DateTime<Utc>,
    pub invitation_lifetime_seconds: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlOutcome {
    Applied,
    IgnoredStale,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlReceipt {
    pub outcome: ControlOutcome,
    /// The current link control revision when this operation was first handled.
    pub revision: u64,
}
