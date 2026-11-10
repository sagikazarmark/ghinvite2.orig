//! Email-addressed organization invitations.
//!
//! One POST; never resolves an address to a user and never reads membership.
//! The caller owns budget, fence and recovery policy. No address escapes in
//! the receipt, errors or `Debug`. This module does not send mail.
//!
//! The validated address type lives with its storage (ghinvite's email
//! vault); [`create`] takes the address text and re-applies the same mailbox
//! rules before sending.
use crate::{endpoints::Endpoint, error::ApiError, rest::Rest};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// GitHub's identity and creation time for a created invitation; nothing else
/// crosses the API boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// GitHub's numeric organization invitation identifier.
    pub id: u64,
    /// GitHub's creation time, positive timing evidence.
    pub created_at: DateTime<Utc>,
}

/// A supported single mailbox: ASCII dot-atom local part and a DNS/IDNA
/// domain. Returns the address with the domain normalized and the local
/// part's case preserved, or `None`.
fn normalize(input: &str) -> Option<Zeroizing<String>> {
    let (local, domain) = input.split_once('@')?;
    let atom = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~".contains(&b);
    if local.is_empty()
        || local.len() > 64
        || !local
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(atom))
    {
        return None;
    }
    let url::Host::Domain(domain) = url::Host::parse(domain).ok()? else {
        return None;
    };
    if domain.is_empty()
        || domain.len() > 253
        || !domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
    {
        return None;
    }
    let normalized = Zeroizing::new(format!("{local}@{domain}"));
    (normalized.len() <= 254).then_some(normalized)
}

/// Invite `email` to `org` as a direct member of `team_ids`. Exactly one HTTP
/// attempt: callers must fence this non-idempotent POST.
///
/// An address outside the supported mailbox syntax is
/// `Decode { what: "email_invitation_address" }`, and zero or duplicate team
/// IDs `Decode { what: "email_invitation_teams" }`, both before any request.
/// Anything but a 201 with a nonzero ID is
/// `Decode { what: "email_invitation_receipt" }`.
pub async fn create(
    rest: &Rest<'_>,
    org: &str,
    email: &str,
    team_ids: &[u64],
) -> Result<Receipt, ApiError> {
    let email = normalize(email).ok_or(ApiError::Decode {
        what: "email_invitation_address",
    })?;
    if team_ids.contains(&0)
        || team_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != team_ids.len()
    {
        return Err(ApiError::Decode {
            what: "email_invitation_teams",
        });
    }
    let endpoint = Endpoint::CreateOrgInvitation;
    endpoint.authorize(rest)?;
    #[derive(Serialize)]
    struct Input<'a> {
        email: &'a str,
        role: &'static str,
        team_ids: &'a [u64],
    }
    let response = rest
        .call_with_version(
            endpoint.meta().method,
            endpoint.meta().path.seg(org),
            &[],
            Some(&Input {
                email: email.as_str(),
                role: "direct_member",
                team_ids,
            }),
            endpoint.meta().api_version,
        )
        .await?;
    let receipt: Receipt = response.body;
    if response.status != 201 || receipt.id == 0 {
        return Err(ApiError::Decode {
            what: "email_invitation_receipt",
        });
    }
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_rules_match_the_vault_address() {
        assert_eq!(
            normalize("Alice.Smith+tag@EXAMPLE.test").unwrap().as_str(),
            "Alice.Smith+tag@example.test"
        );
        assert_eq!(
            normalize("a@bücher.example").unwrap().as_str(),
            "a@xn--bcher-kva.example"
        );
        for invalid in [
            "",
            "a",
            "no-at",
            "@example.test",
            "a@",
            "a@@b.com",
            ".a@b.com",
            "a..b@b.com",
            "a@-b.com",
            "a@b_.com",
            "a@b.com\r\nBcc:x@y.com",
            "a@127.0.0.1",
            "a@[127.0.0.1]",
            "a@b.com.",
            "a b@c.com",
            "ü@x.com",
            "\"quoted\"@example.test",
            "a@bad-.example",
            &format!("{}@x.com", "a".repeat(65)),
            &format!("a@{}.test", "b".repeat(64)),
        ] {
            assert!(normalize(invalid).is_none(), "{invalid}");
        }
    }
}
