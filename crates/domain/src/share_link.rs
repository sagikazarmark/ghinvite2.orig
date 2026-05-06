use crate::ids::ShareLinkId;
use crate::permission::Permission;
use crate::slug::Slug;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareLink {
    pub id: ShareLinkId,
    pub slug: Slug,
    pub installation_id: u64,
    pub account_id: u64,
    pub created_by: u64, // user_id
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    pub uses_count: u32,
    pub permission: Permission,
    pub approval_required: bool,
    pub internal_note: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<u64>,
    pub repos: Vec<ShareLinkRepo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ShareLinkRepo {
    pub repo_id: u64,
    pub repo_full_name: String,
}

impl ShareLink {
    /// `is_active` is the per-spec derived state: not revoked AND not expired AND not exhausted.
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        if self.revoked_at.is_some() {
            return false;
        }
        if let Some(expires) = self.expires_at
            && now >= expires
        {
            return false;
        }
        if let Some(max) = self.max_uses
            && self.uses_count >= max
        {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn at(y: i32, mo: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, 0, 0, 0).unwrap()
    }

    fn base_link() -> ShareLink {
        let mut rng = ChaCha8Rng::seed_from_u64(20_260_504);
        ShareLink {
            id: ShareLinkId::new(),
            slug: Slug::generate(&mut rng),
            installation_id: 1,
            account_id: 2,
            created_by: 3,
            created_at: at(2026, 1, 1),
            expires_at: None,
            max_uses: None,
            uses_count: 0,
            permission: Permission::Pull,
            approval_required: false,
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos: vec![],
        }
    }

    #[test]
    fn fresh_link_is_active() {
        let l = base_link();
        assert!(l.is_active(at(2026, 1, 2)));
    }

    #[test]
    fn revoked_link_is_not_active() {
        let mut l = base_link();
        l.revoked_at = Some(at(2026, 1, 2));
        l.revoked_by = Some(99);
        assert!(!l.is_active(at(2026, 1, 3)));
    }

    #[test]
    fn expired_link_is_not_active() {
        let mut l = base_link();
        l.expires_at = Some(at(2026, 1, 5));
        assert!(l.is_active(at(2026, 1, 4)));
        assert!(!l.is_active(at(2026, 1, 5))); // boundary: at expires_at, no longer active
        assert!(!l.is_active(at(2026, 1, 6)));
    }

    #[test]
    fn exhausted_link_is_not_active() {
        let mut l = base_link();
        l.max_uses = Some(2);
        l.uses_count = 1;
        assert!(l.is_active(at(2026, 2, 1)));
        l.uses_count = 2;
        assert!(!l.is_active(at(2026, 2, 1)));
        l.uses_count = 3;
        assert!(!l.is_active(at(2026, 2, 1)));
    }

    #[test]
    fn unlimited_uses_never_exhaust() {
        let mut l = base_link();
        l.max_uses = None;
        l.uses_count = u32::MAX;
        assert!(l.is_active(at(2026, 2, 1)));
    }
}
