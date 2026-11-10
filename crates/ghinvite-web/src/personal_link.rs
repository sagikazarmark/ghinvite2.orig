//! Web-only show-once capability holder for a reviewed email row.
use crate::link_secret::LinkSecret;
use ghinvite_core::{
    InvitationLinkId,
    bulk::{BatchRef, PersonalLink},
};
use zeroize::Zeroizing;

/// No serialization or cloning: only `plan` may enter a durable command.
pub struct PreparedPersonalLink {
    plan: PersonalLink,
    secret: LinkSecret,
}
impl std::fmt::Debug for PreparedPersonalLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedPersonalLink")
            .field("plan", &self.plan)
            .finish_non_exhaustive()
    }
}
impl PreparedPersonalLink {
    pub fn generate(source: BatchRef) -> Result<Self, &'static str> {
        source.validate()?;
        let secret =
            LinkSecret::generate().map_err(|_| "personal invitation generation unavailable")?;
        let plan = PersonalLink {
            source,
            link_id: InvitationLinkId::new(),
            secret_sha256: secret.hash(),
        };
        Ok(Self { plan, secret })
    }
    pub fn plan(&self) -> &PersonalLink {
        &self.plan
    }
    /// Call only after the exact batch submission and link creation have been
    /// acknowledged. Render in a private no-store response, never a redirect.
    pub fn show_once(self) -> Zeroizing<String> {
        Zeroizing::new(format!("/i#{}", self.secret.show_once().as_str()))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fresh_id_and_secret_are_web_owned_and_durable_plan_is_hash_only() {
        let source = BatchRef {
            batch_id: ghinvite_core::bulk::BulkBatchId::new(),
            row: 1,
        };
        let first = PreparedPersonalLink::generate(source).unwrap();
        let second = PreparedPersonalLink::generate(source).unwrap();
        assert_ne!(first.plan.link_id, second.plan.link_id);
        assert_ne!(first.plan.secret_sha256, second.plan.secret_sha256);
        let wire = serde_json::to_string(first.plan()).unwrap();
        let debug = format!("{first:?}");
        let hash = first.plan.secret_sha256;
        let url = first.show_once();
        let secret = url.strip_prefix("/i#").unwrap();
        assert_eq!(LinkSecret::parse(secret).unwrap().hash(), hash);
        assert!(!wire.contains(secret));
        assert!(!debug.contains(secret));
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Recovery {
    version: u8,
    created_at: i64,
    expires_at: i64,
    plan: PersonalLink,
    secret: String,
}
impl Drop for Recovery {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.secret.zeroize();
    }
}
fn context(
    scope: &crate::email_rows::EmailRowsScope,
    plan: &PersonalLink,
) -> Result<anykms_keyring::EncryptionContext, crate::email_rows::PreparationError> {
    use crate::email_rows::PreparationError;
    if plan.source.batch_id != scope.batch_id || plan.source.validate().is_err() {
        return Err(PreparationError::Unavailable);
    }
    anykms_keyring::EncryptionContext::try_from(&anykms_keyring::purposes::AttemptContinuation {
        scope: &format!(
            "bulk-personal-link/v1/{}/{}/{}/{}/{}",
            scope.batch_id, scope.account_id, scope.installation_id, plan.source.row, plan.link_id
        ),
        binding: &scope.session,
    })
    .map_err(|_| PreparationError::Unavailable)
}
fn unavailable(
    keyring: &anykms_keyring::Keyring,
    error: anykms_core::Error,
) -> crate::email_rows::PreparationError {
    if error.kind() == anykms_core::ErrorKind::Configuration {
        tracing::error!(alert="p1_keyring_configuration",kind=?error.kind(),fingerprint=?keyring.fingerprint(),"personal invitation recovery unavailable");
    } else {
        tracing::warn!(kind=?error.kind(),fingerprint=?keyring.fingerprint(),"personal invitation recovery unavailable");
    }
    crate::email_rows::PreparationError::Unavailable
}
impl PreparedPersonalLink {
    /// Protect the original capability before a submission can be acknowledged
    /// ambiguously. Only this ciphertext belongs in the web continuation.
    pub fn seal(
        self,
        keyring: &anykms_keyring::Keyring,
        scope: &crate::email_rows::EmailRowsScope,
        now: i64,
    ) -> Result<String, crate::email_rows::PreparationError> {
        use crate::email_rows::PreparationError;
        use anykms_keyring::ContextEnvelopeSync;
        use base64::Engine as _;
        if now <= 0 || scope.session_expires <= now {
            return Err(PreparationError::Unavailable);
        }
        let context = context(scope, &self.plan)?;
        let payload = Recovery {
            version: 1,
            created_at: now,
            expires_at: scope
                .session_expires
                .min(now.checked_add(3600).ok_or(PreparationError::Unavailable)?),
            plan: self.plan,
            secret: self.secret.show_once().to_string(),
        };
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&payload).map_err(|_| PreparationError::Unavailable)?,
        );
        let encrypted = keyring
            .context_envelope(anykms_recipes::envelope::Padding::NONE)
            .and_then(|e| e.encrypt_sync(&plaintext, &context))
            .map_err(|e| unavailable(keyring, e))?;
        Ok(base64::engine::general_purpose::STANDARD.encode(encrypted.as_bytes()))
    }
    pub fn restore(
        ciphertext: &str,
        keyring: &anykms_keyring::Keyring,
        scope: &crate::email_rows::EmailRowsScope,
        expected: &PersonalLink,
        now: i64,
    ) -> Result<Self, crate::email_rows::PreparationError> {
        use crate::email_rows::PreparationError;
        use anykms_keyring::ContextEnvelopeSync;
        use base64::Engine as _;
        if ciphertext.len() > 8192 || now <= 0 || scope.session_expires <= now {
            return Err(PreparationError::Unavailable);
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(ciphertext)
            .map_err(|_| PreparationError::Unavailable)?;
        let context = context(scope, expected)?;
        let plaintext = keyring
            .context_envelope(anykms_recipes::envelope::Padding::NONE)
            .and_then(|e| e.decrypt_sync(&anykms_core::Ciphertext::from(bytes), &context))
            .map_err(|e| unavailable(keyring, e))?;
        let plaintext = Zeroizing::new(plaintext);
        let payload: Recovery =
            serde_json::from_slice(&plaintext).map_err(|_| PreparationError::Unavailable)?;
        if payload.version != 1
            || payload.plan != *expected
            || payload.created_at > now
            || payload.expires_at <= now
            || payload.expires_at > scope.session_expires
            || payload
                .expires_at
                .checked_sub(payload.created_at)
                .is_none_or(|span| !(1..=3600).contains(&span))
        {
            return Err(PreparationError::Unavailable);
        }
        let secret =
            LinkSecret::parse(&payload.secret).map_err(|_| PreparationError::Unavailable)?;
        if !secret.hash().constant_time_eq(&expected.secret_sha256) {
            return Err(PreparationError::Unavailable);
        }
        Ok(Self {
            plan: expected.clone(),
            secret,
        })
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use crate::email_rows::{EmailRowsScope, PreparedEmailRows};
    #[test]
    fn interrupted_submission_recovers_original_secret_only_in_exact_web_context() {
        let source = BatchRef {
            batch_id: ghinvite_core::bulk::BulkBatchId::new(),
            row: 1,
        };
        let prepared = PreparedPersonalLink::generate(source).unwrap();
        let plan = prepared.plan().clone();
        let scope = EmailRowsScope {
            session: "trusted-session".into(),
            batch_id: source.batch_id,
            account_id: 10,
            installation_id: 20,
            session_expires: 1800010000,
        };
        let ring = anykms_keyring::Keyring::builder("p1")
            .generation(1, [7; 32])
            .unwrap()
            .current(1)
            .unwrap()
            .build()
            .unwrap();
        let encrypted = prepared.seal(&ring, &scope, 1800000000).unwrap();
        let first = PreparedPersonalLink::restore(&encrypted, &ring, &scope, &plan, 1800000001)
            .unwrap()
            .show_once();
        let second = PreparedPersonalLink::restore(&encrypted, &ring, &scope, &plan, 1800003599)
            .unwrap()
            .show_once();
        assert_eq!(first, second);
        assert!(!encrypted.contains(first.strip_prefix("/i#").unwrap()));
        assert!(
            PreparedPersonalLink::restore(&encrypted, &ring, &scope, &plan, 1800003600).is_err()
        );
        assert!(
            PreparedPersonalLink::restore(&encrypted, &ring, &scope, &plan, 1799999999).is_err()
        );
        assert!(PreparedEmailRows::restore(&encrypted, &ring, &scope, 1800000001).is_err());
        for component in 0..7 {
            let mut other_scope = EmailRowsScope {
                session: scope.session.clone(),
                batch_id: scope.batch_id,
                account_id: scope.account_id,
                installation_id: scope.installation_id,
                session_expires: scope.session_expires,
            };
            let mut other_plan = plan.clone();
            match component {
                0 => other_scope.session = "other-session".into(),
                1 => other_scope.account_id += 1,
                2 => other_scope.installation_id += 1,
                3 => {
                    other_scope.batch_id = ghinvite_core::bulk::BulkBatchId::new();
                    other_plan.source.batch_id = other_scope.batch_id;
                }
                4 => other_plan.source.row += 1,
                5 => other_plan.link_id = InvitationLinkId::new(),
                _ => other_plan.secret_sha256 = ghinvite_core::SecretHash::from_bytes([9; 32]),
            }
            assert!(
                PreparedPersonalLink::restore(
                    &encrypted,
                    &ring,
                    &other_scope,
                    &other_plan,
                    1800000001
                )
                .is_err(),
                "component {component}"
            );
        }
    }
}
