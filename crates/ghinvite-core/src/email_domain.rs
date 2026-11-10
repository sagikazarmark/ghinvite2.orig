//! Immutable email-domain rules and the minimal evidence retained after sign-in.
use crate::InvitationLinkId;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Bounds immutable link policy size independently of form input encoding.
pub const MAX_DOMAIN_RULES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid email domain")]
pub struct InvalidDomain;

/// UTS #46 non-transitional ASCII, with no terminal root dot.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct AsciiDomain(String);

impl AsciiDomain {
    pub fn parse(value: &str) -> Result<Self, InvalidDomain> {
        // The strict IDNA path rejects empty labels, invalid hyphens, non-DNS
        // ASCII and excessive label/domain lengths. It also maps Unicode dots.
        let ascii = idna::uts46::Uts46::new()
            .to_ascii(
                value.as_bytes(),
                idna::uts46::AsciiDenyList::STD3,
                idna::uts46::Hyphens::Check,
                idna::uts46::DnsLength::VerifyAllowRootDot,
            )
            .map_err(|_| InvalidDomain)?;
        let ascii = ascii.strip_suffix('.').unwrap_or(&ascii);
        if ascii.is_empty() {
            return Err(InvalidDomain);
        }
        Ok(Self(ascii.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_noreply(&self) -> bool {
        self.0 == "users.noreply.github.com"
    }
}

impl TryFrom<String> for AsciiDomain {
    type Error = InvalidDomain;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}
impl From<AsciiDomain> for String {
    fn from(value: AsciiDomain) -> Self {
        value.0
    }
}
impl fmt::Display for AsciiDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DomainRule {
    Exact(AsciiDomain),
    Subdomains(AsciiDomain),
}

impl DomainRule {
    pub fn parse(value: &str) -> Result<Self, InvalidDomain> {
        match value.trim().strip_prefix("*.") {
            Some(domain) => AsciiDomain::parse(domain).map(Self::Subdomains),
            None => AsciiDomain::parse(value.trim()).map(Self::Exact),
        }
    }

    pub fn matches(&self, domain: &AsciiDomain) -> bool {
        match self {
            Self::Exact(base) => domain == base,
            Self::Subdomains(base) => domain
                .as_str()
                .strip_suffix(base.as_str())
                .is_some_and(|prefix| prefix.len() > 1 && prefix.ends_with('.')),
        }
    }
}
impl fmt::Display for DomainRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(domain) => domain.fmt(f),
            Self::Subdomains(domain) => write!(f, "*.{domain}"),
        }
    }
}

/// Trusted sign-in adapter assertion. Never contains addresses or unmatched domains.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DomainEvidence {
    pub link_id: InvitationLinkId,
    pub matched_domains: Vec<AsciiDomain>,
    pub checked_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DomainEvidenceError {
    #[error("email domain evidence is stale")]
    Stale,
    #[error("email domain does not match")]
    Mismatch,
}

impl DomainEvidence {
    /// Run against the admission's journaled clock, including future-date rejection.
    pub fn check(
        &self,
        rules: &[DomainRule],
        link_id: InvitationLinkId,
        now: DateTime<Utc>,
    ) -> Result<(), DomainEvidenceError> {
        if self.link_id != link_id
            || self.checked_at > now
            || now - self.checked_at > chrono::Duration::minutes(10)
        {
            return Err(DomainEvidenceError::Stale);
        }
        if self.matched_domains.is_empty()
            || self
                .matched_domains
                .iter()
                .any(|domain| domain.is_noreply() || !rules.iter().any(|rule| rule.matches(domain)))
        {
            return Err(DomainEvidenceError::Mismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_and_subdomain_boundaries() {
        let exact = DomainRule::parse("EXAMPLE.COM.").unwrap();
        assert!(exact.matches(&AsciiDomain::parse("example.com").unwrap()));
        let subdomains = DomainRule::parse("*.example.com").unwrap();
        for (domain, matched) in [
            ("eng.example.com", true),
            ("a.eng.example.com", true),
            ("example.com", false),
            ("badexample.com", false),
            ("example.com.evil.test", false),
        ] {
            assert_eq!(
                subdomains.matches(&AsciiDomain::parse(domain).unwrap()),
                matched
            );
        }
        assert!(
            DomainRule::parse("bücher.de")
                .unwrap()
                .matches(&AsciiDomain::parse("xn--bcher-kva.de").unwrap())
        );
        assert_eq!(
            AsciiDomain::parse("EXAMPLE。COM。").unwrap().as_str(),
            "example.com"
        );
        assert_eq!(
            AsciiDomain::parse("faß.de").unwrap().as_str(),
            "xn--fa-hia.de"
        );
        for invalid in [
            "",
            ".",
            "a..com",
            "*.example.com",
            "a@example.com",
            "a_b.com",
            "-a.com",
            "a-.com",
            " example.com",
            "example.com..",
            "https://example.com",
        ] {
            assert!(AsciiDomain::parse(invalid).is_err(), "{invalid}");
        }
        assert!(AsciiDomain::parse(&format!("{}.com", "x".repeat(64))).is_err());
        assert!(serde_json::from_str::<AsciiDomain>("\"a@b.com\"").is_err());
        assert_eq!(
            serde_json::to_string(&exact).unwrap(),
            r#"{"exact":"example.com"}"#
        );
    }

    #[test]
    fn evidence_is_link_bound_fresh_and_contains_only_matched_non_noreply_domains() {
        let link_id = InvitationLinkId::new();
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let rules = [DomainRule::parse("*.example.com").unwrap()];
        let mut evidence = DomainEvidence {
            link_id,
            matched_domains: vec![AsciiDomain::parse("eng.example.com").unwrap()],
            checked_at: now - chrono::Duration::minutes(10),
        };
        assert_eq!(evidence.check(&rules, link_id, now), Ok(()));
        assert_eq!(
            evidence.check(&rules, InvitationLinkId::new(), now),
            Err(DomainEvidenceError::Stale)
        );
        evidence.checked_at -= chrono::Duration::milliseconds(1);
        assert_eq!(
            evidence.check(&rules, link_id, now),
            Err(DomainEvidenceError::Stale)
        );
        evidence.checked_at = now + chrono::Duration::milliseconds(1);
        assert_eq!(
            evidence.check(&rules, link_id, now),
            Err(DomainEvidenceError::Stale)
        );
        evidence.checked_at = now;
        evidence
            .matched_domains
            .push(AsciiDomain::parse("private.other.test").unwrap());
        assert_eq!(
            evidence.check(&rules, link_id, now),
            Err(DomainEvidenceError::Mismatch)
        );
        evidence.matched_domains.clear();
        assert_eq!(
            evidence.check(&rules, link_id, now),
            Err(DomainEvidenceError::Mismatch)
        );
        evidence
            .matched_domains
            .push(AsciiDomain::parse("USERS.NOREPLY.GITHUB.COM.").unwrap());
        assert_eq!(
            evidence.check(&[DomainRule::parse("*.github.com").unwrap()], link_id, now),
            Err(DomainEvidenceError::Mismatch)
        );
    }
}
