//! Exact reviewed-row cancellation. This stops future writes; it never means
//! that already granted access or an uncertain upstream effect was removed.
use crate::{
    RequestId, admission::AdmissionOperationId, invitation_link::AccountAdmin,
    request_lifecycle::InitializeRequest,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelRequest {
    pub operation_id: AdmissionOperationId,
    pub admin: AccountAdmin,
    pub input: InitializeRequest,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelRequestReceipt {
    pub request_id: RequestId,
    /// Every exact target owner has acknowledged the stop barrier. Counts below
    /// distinguish no-write aborts from effects which cannot be undone here.
    pub stopped: bool,
    pub aborted: u16,
    pub already_written: u16,
    pub already_completed: u16,
    pub uncertain: u16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CancelUnsentResult {
    Aborted,
    AlreadyWritten,
    AlreadyCompleted,
    MayHaveWritten,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositoryCancelUnsent {
    pub operation_id: AdmissionOperationId,
    pub admin: AccountAdmin,
    pub command: crate::delivery::CreateCommand,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrganizationCancelUnsent {
    pub operation_id: AdmissionOperationId,
    pub admin: AccountAdmin,
    pub command: crate::org_delivery::OrganizationCreateCommand,
}
