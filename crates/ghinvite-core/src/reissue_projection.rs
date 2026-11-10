//! Independent, revision-bound proof of an administrator reissue. No recursive
//! projection history: an older compatible projection may jump to this epoch.
use crate::{
    RequestState,
    delivery::CreateCommand,
    org_delivery::OrganizationCreateCommand,
    reissue::{BudgetAttribution, ReissueCommand, ReissueTarget},
    request_lifecycle::RequestSnapshot,
    storage::projection::AuditIntent,
};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReissuedDelivery {
    Repository {
        before: CreateCommand,
        after: CreateCommand,
    },
    Organization {
        before: OrganizationCreateCommand,
        after: OrganizationCreateCommand,
    },
}
impl ReissuedDelivery {
    pub fn generations(&self) -> (u64, u64) {
        match self {
            Self::Repository { before, after } => {
                (before.delivery_generation, after.delivery_generation)
            }
            Self::Organization { before, after } => {
                (before.delivery_generation, after.delivery_generation)
            }
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestReissueProjection {
    pub command: ReissueCommand,
    pub before: RequestSnapshot,
    pub after: RequestSnapshot,
    /// Immutable original decision, independently reconstructible if SQL lags.
    pub original_approval: RequestSnapshot,
    pub approval_events: Vec<AuditIntent>,
    pub delivery: ReissuedDelivery,
    pub at: DateTime<Utc>,
}
impl RequestReissueProjection {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.command.validate()?;
        let invalid = "invalid request reissue proof";
        if self.before.request_id != self.command.request_id
            || self.command.admin.account_id != self.before.account_id
            || !matches!(
                self.before.state,
                RequestState::Approved | RequestState::Lapsed
            )
            || self.original_approval.state != RequestState::Approved
            || self.original_approval.revision > self.before.revision
            || self.after.state != RequestState::Approved
            || self.before.revision.checked_add(1) != Some(self.after.revision)
            || self
                .original_approval
                .decision
                .as_ref()
                .is_none_or(|d| self.at < d.effective_at)
        {
            return Err(invalid);
        }
        let mut expected = self.original_approval.clone();
        expected.revision = self.before.revision;
        expected.state = self.before.state;
        if expected != self.before {
            return Err(invalid);
        }
        expected.state = RequestState::Approved;
        expected.revision = self.after.revision;
        if expected != self.after {
            return Err(invalid);
        }
        let (old, new) = self.delivery.generations();
        if old == 0 || old.checked_add(1) != Some(new) || new > u32::MAX.into() {
            return Err(invalid);
        }
        match &self.delivery {
            ReissuedDelivery::Repository { before, after } => {
                crate::bulk::validate_origin(before.link_id, before.batch)?;
                if self.command.target
                    != (ReissueTarget::Repository {
                        repo_id: before.repo_id,
                    })
                    || before.request_id != self.before.request_id
                    || before.link_id != self.before.link_id
                    || before.batch != self.before.batch
                    || before.account_id != self.before.account_id
                    || before.requester_id != self.before.requester_id
                    || self.before.decision.as_ref().is_none_or(|d| {
                        before.approval_id != d.decision_id || before.approved_at != d.effective_at
                    })
                    || (after.invitation_id.is_some()
                        && before.invitation_id == after.invitation_id)
                {
                    return Err(invalid);
                }
                let mut expected = before.clone();
                expected.invitation_id = after.invitation_id;
                expected.delivery_generation = new;
                expected.budget_attribution = BudgetAttribution::AdministratorReissue;
                if &expected != after {
                    return Err(invalid);
                }
            }
            ReissuedDelivery::Organization { before, after } => {
                before.validate().map_err(|_| invalid)?;
                after.validate().map_err(|_| invalid)?;
                if self.command.target
                    != (ReissueTarget::Organization {
                        org_id: before.organization.org_id,
                    })
                    || before.request_id != self.before.request_id
                    || before.link_id != self.before.link_id
                    || before.batch != self.before.batch
                    || before.account_id != self.before.account_id
                    || before.requester_id != self.before.requester_id
                    || self.before.decision.as_ref().is_none_or(|d| {
                        before.approval_id != d.decision_id || before.approved_at != d.effective_at
                    })
                    || (after.invitation_id.is_some()
                        && before.invitation_id == after.invitation_id)
                {
                    return Err(invalid);
                }
                let mut expected = before.clone();
                expected.invitation_id = after.invitation_id;
                expected.delivery_generation = new;
                expected.budget_attribution = BudgetAttribution::AdministratorReissue;
                if &expected != after {
                    return Err(invalid);
                }
            }
        }
        Ok(())
    }
}
