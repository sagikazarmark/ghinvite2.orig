//! Session-bound continuations of in-flight attempts, retained before the
//! request reaches the authority so an uncertain outcome can be recovered with
//! the exact submitted input. Payloads are sealed under the session secret;
//! each record expires with the browser session that retained it. What an
//! outcome means stays with each flow.
use crate::{AppState, WebError};
use base64::Engine;
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use chrono::Utc;
use ghinvite_core::storage::ContinuationStorage;
use ghinvite_core::storage::attempt_continuations::StoredContinuation;
use rand::RngCore;
use serde::{Serialize, de::DeserializeOwned};
use std::fmt::Display;

const UNAVAILABLE: &str = "Attempt recovery temporarily unavailable.";
const NONCE: usize = 24;

fn unavailable(_: impl Display) -> WebError {
    WebError::Session(UNAVAILABLE.into())
}

/// One browser session's continuations for one flow and subject. Nothing
/// retained in one scope is visible from another.
pub(crate) struct Scope {
    key: String,
    expires_at: i64,
}

impl Scope {
    /// A console admin's mutations of one account.
    pub(crate) fn console(
        tower: &tower_sessions::Session,
        user: u64,
        account: u64,
    ) -> crate::Result<Self> {
        Self::of(tower, "console", &[&user, &account])
    }

    /// A requester's invitation requests through one invitation link.
    pub(crate) fn invitation(
        tower: &tower_sessions::Session,
        user: u64,
        code: &str,
    ) -> crate::Result<Self> {
        let link: ghinvite_core::InvitationLinkId = code.parse().map_err(|_| WebError::NotFound)?;
        Self::of(tower, "invitation", &[&user, &link])
    }

    fn of(
        tower: &tower_sessions::Session,
        flow: &str,
        parts: &[&dyn Display],
    ) -> crate::Result<Self> {
        let session = tower.id().ok_or_else(|| unavailable("missing session"))?;
        Ok(Self::derive(
            flow,
            &session.to_string(),
            parts,
            tower.expiry_date().unix_timestamp(),
        ))
    }

    fn derive(flow: &str, session: &str, parts: &[&dyn Display], expires_at: i64) -> Self {
        use sha2::{Digest, Sha256};
        let mut input = format!("ghinvite/attempt-continuation/v1/{flow}/{session}");
        for part in parts {
            input.push_str(&format!("/{part}"));
        }
        Self {
            key: hex::encode(Sha256::digest(input)),
            expires_at,
        }
    }
}

/// What retaining a payload under an identity found.
#[derive(Debug, PartialEq)]
pub(crate) enum Retention<T> {
    /// This payload is the identity's continuation, retained now or before.
    Retained,
    /// The identity is already bound to this different payload.
    Conflict(T),
    /// The binding is held by another identity's continuation.
    Bound(T),
}

/// Continuation storage for one app, sealed with its session secret.
pub(crate) struct AttemptContinuations<'a> {
    storage: &'a dyn ContinuationStorage,
    secret: &'a [u8; 32],
}

impl<'a> AttemptContinuations<'a> {
    pub(crate) fn new(state: &'a AppState) -> Self {
        Self {
            storage: state.storage.as_ref(),
            secret: &state.config.session_secret,
        }
    }

    /// Retain `payload` as the continuation for `id`. The first writer wins
    /// atomically, both by identity and by `binding`, a logical subject that
    /// may be shared by several identities (the identity itself when absent).
    pub(crate) async fn retain<T: Serialize + DeserializeOwned>(
        &self,
        scope: &Scope,
        id: &str,
        binding: Option<&str>,
        payload: &T,
    ) -> crate::Result<Retention<T>> {
        let stored = self
            .storage
            .retain_attempt_continuation(
                &scope.key,
                id,
                binding.unwrap_or(id),
                &self.seal(scope, id, payload)?,
                scope.expires_at,
                Utc::now().timestamp(),
            )
            .await
            .map_err(unavailable)?;
        let bound_elsewhere = stored.id != id;
        let original: T = self.open(scope, stored)?;
        let submitted = serde_json::to_value(payload).map_err(unavailable)?;
        Ok(if bound_elsewhere {
            Retention::Bound(original)
        } else if serde_json::to_value(&original).map_err(unavailable)? != submitted {
            Retention::Conflict(original)
        } else {
            Retention::Retained
        })
    }

    pub(crate) async fn load<T: DeserializeOwned>(
        &self,
        scope: &Scope,
        id: &str,
    ) -> crate::Result<Option<T>> {
        self.storage
            .get_attempt_continuation(&scope.key, id, Utc::now().timestamp())
            .await
            .map_err(unavailable)?
            .map(|stored| self.open(scope, stored))
            .transpose()
    }

    /// Every live continuation in the scope, oldest retained first.
    pub(crate) async fn list<T: DeserializeOwned>(&self, scope: &Scope) -> crate::Result<Vec<T>> {
        self.storage
            .list_attempt_continuations(&scope.key, Utc::now().timestamp())
            .await
            .map_err(unavailable)?
            .into_iter()
            .map(|stored| self.open(scope, stored))
            .collect()
    }

    /// The most recently retained live continuation in the scope.
    pub(crate) async fn latest<T: DeserializeOwned>(
        &self,
        scope: &Scope,
    ) -> crate::Result<Option<T>> {
        Ok(self.list(scope).await?.pop())
    }

    /// Forget a continuation, so its identity and binding can carry other
    /// input. Releasing a missing continuation succeeds.
    pub(crate) async fn release(&self, scope: &Scope, id: &str) -> crate::Result<()> {
        self.storage
            .release_attempt_continuation(&scope.key, id)
            .await
            .map_err(unavailable)
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(self.secret.into())
    }

    fn seal<T: Serialize>(&self, scope: &Scope, id: &str, payload: &T) -> crate::Result<String> {
        let mut nonce = [0; NONCE];
        rand::rngs::OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(unavailable)?;
        let plaintext = serde_json::to_vec(payload).map_err(unavailable)?;
        let encrypted = self
            .cipher()
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &plaintext,
                    aad: aad(scope, id).as_bytes(),
                },
            )
            .map_err(unavailable)?;
        let mut bytes = nonce.to_vec();
        bytes.extend(encrypted);
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    fn open<T: DeserializeOwned>(
        &self,
        scope: &Scope,
        stored: StoredContinuation,
    ) -> crate::Result<T> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(stored.payload)
            .map_err(unavailable)?;
        if bytes.len() < NONCE {
            return Err(unavailable("invalid continuation"));
        }
        let plaintext = self
            .cipher()
            .decrypt(
                &XNonce::try_from(&bytes[..NONCE]).map_err(unavailable)?,
                Payload {
                    msg: &bytes[NONCE..],
                    aad: aad(scope, &stored.id).as_bytes(),
                },
            )
            .map_err(unavailable)?;
        serde_json::from_slice(&plaintext).map_err(unavailable)
    }
}

/// Binds a sealed payload to its scope and identity, so it cannot be replayed
/// under another.
fn aad(scope: &Scope, id: &str) -> String {
    format!("ghinvite/attempt-continuation/v1/{}/{id}", scope.key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghinvite_storage_sqlx::SqlxStorage;

    const LIVE: i64 = i64::MAX;

    fn scope(session: &str, user: u64, account: u64) -> Scope {
        Scope::derive("console", session, &[&user, &account], LIVE)
    }

    fn store<'a>(storage: &'a SqlxStorage, secret: &'a [u8; 32]) -> AttemptContinuations<'a> {
        AttemptContinuations { storage, secret }
    }

    async fn retain(
        store: &AttemptContinuations<'_>,
        scope: &Scope,
        id: &str,
        payload: &str,
    ) -> Retention<String> {
        store
            .retain(scope, id, None, &payload.to_owned())
            .await
            .unwrap()
    }

    async fn load(store: &AttemptContinuations<'_>, scope: &Scope, id: &str) -> Option<String> {
        store.load(scope, id).await.unwrap()
    }

    #[tokio::test]
    async fn scopes_are_isolated_by_flow_session_user_and_subject() {
        let storage = SqlxStorage::in_memory().await.unwrap();
        let store = store(&storage, &[7; 32]);
        let scopes = [
            scope("s1", 1, 10),
            scope("s2", 1, 10),
            scope("s1", 2, 10),
            scope("s1", 1, 11),
            Scope::derive("invitation", "s1", &[&1, &10], LIVE),
        ];
        for (i, scope) in scopes.iter().enumerate() {
            let payload = format!("p{i}");
            assert_eq!(
                retain(&store, scope, "a", &payload).await,
                Retention::Retained
            );
        }
        for (i, scope) in scopes.iter().enumerate() {
            assert_eq!(load(&store, scope, "a").await, Some(format!("p{i}")));
            assert_eq!(store.list::<String>(scope).await.unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn payloads_are_sealed_to_their_secret_scope_and_identity() {
        let storage = SqlxStorage::in_memory().await.unwrap();
        let store = store(&storage, &[7; 32]);
        let scope = scope("s1", 1, 10);
        retain(&store, &scope, "a", "justification").await;
        assert_eq!(
            load(&store, &scope, "a").await.as_deref(),
            Some("justification")
        );
        let sealed = storage
            .get_attempt_continuation(&scope.key, "a", 0)
            .await
            .unwrap()
            .unwrap()
            .payload;
        assert!(!sealed.contains("justification"));

        let other_secret = [8; 32];
        assert!(
            self::store(&storage, &other_secret)
                .load::<String>(&scope, "a")
                .await
                .is_err()
        );

        // The same ciphertext stored under another identity does not open.
        storage
            .retain_attempt_continuation(&scope.key, "b", "b", &sealed, LIVE, 0)
            .await
            .unwrap();
        assert!(store.load::<String>(&scope, "b").await.is_err());

        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(&sealed)
            .unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        let tampered = base64::engine::general_purpose::STANDARD.encode(bytes);
        storage
            .retain_attempt_continuation(&scope.key, "c", "c", &tampered, LIVE, 0)
            .await
            .unwrap();
        assert!(store.load::<String>(&scope, "c").await.is_err());
    }

    #[tokio::test]
    async fn continuations_expire_with_their_session() {
        let storage = SqlxStorage::in_memory().await.unwrap();
        let store = store(&storage, &[7; 32]);
        let expired = Scope::derive("console", "s1", &[&1, &10], Utc::now().timestamp() - 1);
        assert!(
            store
                .retain(&expired, "a", None, &"p".to_owned())
                .await
                .is_err()
        );
        // A record retained while its session was live cannot be read once it ends.
        storage
            .retain_attempt_continuation(&expired.key, "b", "b", "sealed", 1, 0)
            .await
            .unwrap();
        assert_eq!(load(&store, &expired, "b").await, None);
        assert!(store.list::<String>(&expired).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_first_payload_binds_its_identity_and_binding() {
        let storage = SqlxStorage::in_memory().await.unwrap();
        let store = store(&storage, &[7; 32]);
        let scope = scope("s1", 1, 10);
        assert_eq!(retain(&store, &scope, "a", "p1").await, Retention::Retained);
        assert_eq!(retain(&store, &scope, "a", "p1").await, Retention::Retained);
        assert_eq!(
            retain(&store, &scope, "a", "p2").await,
            Retention::Conflict("p1".into())
        );
        let bound = |id: &'static str, payload: &'static str| {
            let store = &store;
            let scope = &scope;
            async move {
                store
                    .retain(scope, id, Some("request"), &payload.to_owned())
                    .await
                    .unwrap()
            }
        };
        assert_eq!(bound("b", "p3").await, Retention::Retained);
        assert_eq!(bound("c", "p4").await, Retention::Bound("p3".into()));
        assert_eq!(load(&store, &scope, "c").await, None);
    }

    #[tokio::test]
    async fn release_frees_identity_and_binding() {
        let storage = SqlxStorage::in_memory().await.unwrap();
        let store = store(&storage, &[7; 32]);
        let scope = scope("s1", 1, 10);
        store
            .retain(&scope, "a", Some("request"), &"p1".to_owned())
            .await
            .unwrap();
        store.release(&scope, "a").await.unwrap();
        store.release(&scope, "a").await.unwrap();
        assert_eq!(load(&store, &scope, "a").await, None);
        assert_eq!(
            store
                .retain(&scope, "b", Some("request"), &"p2".to_owned())
                .await
                .unwrap(),
            Retention::Retained
        );
        assert_eq!(retain(&store, &scope, "a", "p3").await, Retention::Retained);
    }

    #[tokio::test]
    async fn lists_oldest_first_and_latest_is_the_newest() {
        let storage = SqlxStorage::in_memory().await.unwrap();
        let store = store(&storage, &[7; 32]);
        let scope = scope("s1", 1, 10);
        assert_eq!(store.latest::<String>(&scope).await.unwrap(), None);
        for (id, payload) in [("z", "first"), ("a", "second"), ("m", "third")] {
            retain(&store, &scope, id, payload).await;
        }
        // Replaying an older identity does not make it the latest.
        retain(&store, &scope, "z", "first").await;
        assert_eq!(
            store.list::<String>(&scope).await.unwrap(),
            ["first", "second", "third"]
        );
        assert_eq!(
            store.latest::<String>(&scope).await.unwrap().as_deref(),
            Some("third")
        );
        store.release(&scope, "m").await.unwrap();
        assert_eq!(
            store.latest::<String>(&scope).await.unwrap().as_deref(),
            Some("second")
        );
    }
}
