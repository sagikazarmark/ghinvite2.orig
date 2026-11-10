//! Hash-only capability lookup. Plaintext link secrets never enter storage.
use crate::{InvitationLinkId, SecretHash};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkSecret {
    pub secret_sha256: SecretHash,
    pub link_id: InvitationLinkId,
    pub secret_generation: u32,
    pub created_at: DateTime<Utc>,
    pub retired_at: Option<DateTime<Utc>>,
}
impl LinkSecret {
    pub fn validate(&self) -> super::Result<()> {
        if self.secret_generation == 0 || self.retired_at.is_some() {
            return Err(super::Error::ProjectionInvariant(
                "invalid link secret registration".into(),
            ));
        }
        super::integer::unsigned(u64::from(self.secret_generation))?;
        Ok(())
    }
}
pub const REGISTER: &str = "INSERT INTO link_secrets(secret_sha256,link_id,secret_generation,created_at) VALUES(?1,?2,?3,?4) ON CONFLICT(secret_sha256) DO NOTHING";
pub const GET: &str = "SELECT * FROM link_secrets WHERE secret_sha256=?1 AND retired_at IS NULL";
pub const EXACT: &str = "SELECT * FROM link_secrets WHERE secret_sha256=?1";
pub const RETIRE: &str = "UPDATE link_secrets SET retired_at=?3 WHERE link_id=?1 AND secret_generation<=?2 AND retired_at IS NULL";
pub fn registered(expected: &LinkSecret, actual: Option<LinkSecret>) -> super::Result<()> {
    if actual.as_ref().is_none_or(|actual| {
        actual.secret_sha256 != expected.secret_sha256
            || actual.link_id != expected.link_id
            || actual.secret_generation != expected.secret_generation
            || actual.created_at != expected.created_at
    }) {
        return Err(super::Error::ProjectionInvariant(
            "link secret binding conflict".into(),
        ));
    }
    Ok(())
}
