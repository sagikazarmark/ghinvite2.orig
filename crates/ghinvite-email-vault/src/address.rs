//! A transient email address: validated, normalized and zeroized on drop.
//!
//! Moved from gh-toolkit's `email_invitation` (GM-07). The vault encrypts
//! `{"email": Address::as_str(), …}`, so the normal form must not change
//! (PD-9; pinned by `golden_address_canonical_form`).
use zeroize::Zeroizing;

/// A supported single mailbox: ASCII dot-atom local part and a DNS/IDNA domain.
/// Quoted local parts, comments, address lists and SMTPUTF8 local parts are not
/// accepted. Local-part case is preserved; only the domain is normalized.
/// Deliberately has no Clone, Display or Serialize implementation.
pub struct Address(Zeroizing<String>);
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid email address")]
/// Address validation failed; the diagnostic contains no part of the address.
pub struct InvalidAddress;

impl InvalidAddress {
    /// Stable error code suitable for metrics and journals; no diagnostic values.
    pub const fn code(&self) -> &'static str {
        "gh:email:invalid_address"
    }
}

impl Address {
    /// Validate the supported mailbox syntax, preserve the local part and canonicalize the domain without retaining the original input.
    pub fn parse(input: &str) -> Result<Self, InvalidAddress> {
        let (local, domain) = input.split_once('@').ok_or(InvalidAddress)?;
        let atom = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~".contains(&b);
        if local.is_empty()
            || local.len() > 64
            || !local
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(atom))
        {
            return Err(InvalidAddress);
        }
        let url::Host::Domain(domain) = url::Host::parse(domain).map_err(|_| InvalidAddress)?
        else {
            return Err(InvalidAddress);
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
            return Err(InvalidAddress);
        }
        let normalized = Zeroizing::new(format!("{local}@{domain}"));
        if normalized.len() > 254 {
            return Err(InvalidAddress);
        }
        Ok(Self(normalized))
    }
    /// For the transient vault/HTTP boundary only; never log this value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Address([REDACTED])")
    }
}
