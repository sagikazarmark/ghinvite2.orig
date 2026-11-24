//! Complete organization list observations. Failed-list presence is retained as
//! evidence; it means expiry only under the #86 lifetime rule in [`expired`].
use crate::org_delivery::OrganizationInvitationRole;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListedOrganizationInvitation {
    pub upstream_id: u64,
    pub login: Option<String>,
    pub role: OrganizationInvitationRole,
    pub created_at: DateTime<Utc>,
    pub team_count: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FailedOrganizationInvitation {
    pub invitation: ListedOrganizationInvitation,
    pub failed_at: Option<DateTime<Utc>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OrganizationObservation {
    pub account_id: u64,
    pub installation_id: u64,
    pub org_id: u64,
    pub org_login: String,
    pub observation_started_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
    pub pending: Vec<ListedOrganizationInvitation>,
    pub failed: Vec<FailedOrganizationInvitation>,
}
impl OrganizationObservation {
    pub fn validate(&self) -> Result<(), &'static str> {
        let numeric = |id| id > 0 && id <= crate::storage::integer::MAX_EXACT;
        if !numeric(self.account_id)
            || !numeric(self.installation_id)
            || self.org_id != self.account_id
            || self.observed_at < self.observation_started_at
        {
            return Err("invalid organization observation identity");
        }
        crate::targets::Targets {
            organization: Some(crate::targets::OrganizationTarget {
                org_id: self.org_id,
                org_login: self.org_login.clone(),
                grant: crate::targets::MemberGrant::Member,
            }),
            teams: vec![],
            repos: vec![],
        }
        .validate()
        .map_err(|_| "invalid organization observation target")?;
        for entries in [
            self.pending.iter().collect::<Vec<_>>(),
            self.failed.iter().map(|f| &f.invitation).collect(),
        ] {
            let mut seen = BTreeSet::new();
            for entry in entries {
                if !numeric(entry.upstream_id)
                    || !seen.insert(entry.upstream_id)
                    || entry.team_count > crate::storage::integer::MAX_EXACT
                    || entry.login.as_ref().is_some_and(|s| {
                        s.is_empty() || s.len() > 100 || s.chars().any(char::is_control)
                    })
                {
                    return Err("invalid organization invitation list");
                }
            }
        }
        Ok(())
    }
}

impl OrganizationObservation {
    /// GitHub moves an organization invitation that outlives its 7-day
    /// lifetime to the failed list. This complete observation shows `upstream_id`
    /// expired when it still lists the invitation, pending or failed, more than
    /// 7 days after creation. A failure GitHub stamped before that lifetime ended
    /// failed for another reason and stays unclassified; absence is never expiry.
    pub fn expired(&self, upstream_id: u64) -> bool {
        let lifetime = chrono::Duration::seconds(i64::from(
            crate::delivery::GITHUB_INVITATION_LIFETIME_SECONDS,
        ));
        let past = |created_at| {
            crate::delivery::past_github_lifetime(created_at, self.observation_started_at)
        };
        self.pending
            .iter()
            .any(|i| i.upstream_id == upstream_id && past(i.created_at))
            || self.failed.iter().any(|f| {
                f.invitation.upstream_id == upstream_id
                    && past(f.invitation.created_at)
                    && f.failed_at.is_none_or(|failed_at| {
                        f.invitation
                            .created_at
                            .checked_add_signed(lifetime)
                            .is_some_and(|expiry| failed_at >= expiry)
                    })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observation() -> OrganizationObservation {
        let now = Utc::now();
        OrganizationObservation {
            account_id: 100,
            installation_id: 2,
            org_id: 100,
            org_login: "acme".into(),
            observation_started_at: now,
            observed_at: now,
            pending: vec![ListedOrganizationInvitation {
                upstream_id: 99,
                login: Some("alice".into()),
                role: OrganizationInvitationRole::DirectMember,
                created_at: now - chrono::Duration::days(1),
                team_count: 2,
            }],
            failed: vec![],
        }
    }
    #[test]
    fn incomplete_or_conflicting_list_identity_cannot_be_accepted() {
        let good = observation();
        assert!(good.validate().is_ok());
        let mut duplicate = good.clone();
        duplicate.pending.push(duplicate.pending[0].clone());
        assert!(duplicate.validate().is_err());
        let mut wrong_account = good.clone();
        wrong_account.org_id = 200;
        assert!(wrong_account.validate().is_err());
        let mut impossible_time = good.clone();
        impossible_time.observed_at -= chrono::Duration::seconds(1);
        assert!(impossible_time.validate().is_err());
        let mut imprecise = good;
        imprecise.pending[0].upstream_id = crate::storage::integer::MAX_EXACT + 1;
        assert!(imprecise.validate().is_err());
    }
    #[test]
    fn listing_past_the_seven_day_lifetime_is_expiry_and_absence_never_is() {
        let mut value = observation();
        let created = value.pending[0].created_at;
        let lifetime = chrono::Duration::days(7);
        value.observation_started_at = created + lifetime;
        value.observed_at = value.observation_started_at;
        assert!(!value.expired(99), "exactly seven days is still pending");
        value.observation_started_at += chrono::Duration::seconds(1);
        value.observed_at = value.observation_started_at;
        assert!(value.expired(99), "still pending after its lifetime");
        assert!(!value.expired(98), "another invitation is not this one");
        let pending = value.pending.remove(0);
        assert!(!value.expired(99), "absence is never expiry");
        for (failed_at, expired) in [
            (None, true),
            (Some(created + lifetime), true),
            (Some(created + lifetime - chrono::Duration::hours(1)), false),
        ] {
            value.failed = vec![FailedOrganizationInvitation {
                invitation: pending.clone(),
                failed_at,
            }];
            assert_eq!(value.expired(99), expired, "{failed_at:?}");
        }
        value.observation_started_at = created + lifetime;
        assert!(
            !value.expired(99),
            "a failed entry inside its lifetime is not yet expiry"
        );
    }
    #[test]
    fn failed_list_preserves_ambiguity_without_inventing_an_expiry_reason() {
        let mut value = observation();
        value.failed.push(FailedOrganizationInvitation {
            invitation: value.pending[0].clone(),
            failed_at: None,
        });
        assert!(
            value.validate().is_ok(),
            "an invitation can move between sequential complete list reads; owner decides whether evidence is conclusive"
        );
        let wire = serde_json::to_value(&value).unwrap();
        assert!(wire["failed"][0]["failed_at"].is_null());
        assert!(wire["failed"][0].get("expired").is_none());
        value.failed.push(value.failed[0].clone());
        assert!(
            value.validate().is_err(),
            "duplicate identities inside one walk are invalid"
        );
    }
}
