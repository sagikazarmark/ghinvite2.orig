//! Only matched domains cross the ephemeral email-read boundary.
use super::{Identity, SignInError};
use ghinvite_core::{
    InvitationLinkId,
    email_domain::{AsciiDomain, DomainEvidence, DomainRule},
};

#[derive(Clone, Debug)]
pub struct DomainRestriction {
    pub link_id: InvitationLinkId,
    pub rules: Vec<DomainRule>,
}
#[async_trait::async_trait]
pub trait DomainContext: Send + Sync {
    /// Resolve current immutable rules with the pending capability and authenticated user ID.
    async fn restriction(&self, user_id: u64) -> Result<Option<DomainRestriction>, SignInError>;
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompletedSignIn {
    pub identity: Identity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub domain_evidence: Option<DomainEvidence>,
}

pub(super) fn evidence(
    restriction: DomainRestriction,
    emails: Vec<octologin::VerifiedEmail>,
    now: i64,
) -> Result<DomainEvidence, SignInError> {
    let checked_at =
        chrono::DateTime::from_timestamp(now, 0).ok_or(SignInError::DomainUnavailable)?;
    let mut matched_domains = std::collections::BTreeSet::new();
    // octologin returns verified addresses only: unverified ones never count.
    for email in emails {
        let Some((local, raw_domain)) = email.email.rsplit_once('@') else {
            continue;
        };
        if local.is_empty()
            || local.contains('@')
            || email
                .email
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            continue;
        }
        let Ok(domain) = AsciiDomain::parse(raw_domain) else {
            continue;
        };
        if !domain.is_noreply() && restriction.rules.iter().any(|rule| rule.matches(&domain)) {
            matched_domains.insert(domain);
        }
    }
    Ok(DomainEvidence {
        link_id: restriction.link_id,
        matched_domains: matched_domains.into_iter().collect(),
        checked_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn noreply_never_qualifies_even_when_rule_would_match_and_subdomain_excludes_apex() {
        let restriction = DomainRestriction {
            link_id: InvitationLinkId::new(),
            rules: ["*.github.com", "*.example.com"]
                .into_iter()
                .map(|s| DomainRule::parse(s).unwrap())
                .collect(),
        };
        let emails = vec![
            octologin::VerifiedEmail::new("private@users.noreply.github.com", false),
            octologin::VerifiedEmail::new("private@example.com", false),
        ];
        assert!(
            evidence(restriction, emails, 100)
                .unwrap()
                .matched_domains
                .is_empty()
        );
    }
}
