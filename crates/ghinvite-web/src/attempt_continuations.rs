//! Session-bound continuations of in-flight attempts, retained before the
//! request reaches the authority so an uncertain outcome can be recovered with
//! the exact submitted input. Payloads are sealed with the immutable deployment keyring;
//! each record expires with the browser session that retained it. What an
//! outcome means stays with each flow.
use crate::{AppState, WebError};
use anykms_core::{Ciphertext, ErrorKind};
use chrono::Utc;
use ghinvite_core::purpose;
use ghinvite_core::storage::ContinuationStorage;
use ghinvite_core::storage::attempt_continuations::StoredContinuation;
use kekring::{Aad, Code, Keyring};
use serde::{Serialize, de::DeserializeOwned};
use std::fmt::Display;
use std::sync::Arc;

const UNAVAILABLE: &str = "Attempt recovery temporarily unavailable.";

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
    /// Separate typed recovery records for one browser's bulk batch. Bulk
    /// payloads must never enter the ordinary Console attempt list.
    pub(crate) fn bulk(
        tower: &tower_sessions::Session,
        user: u64,
        account: u64,
        batch: ghinvite_core::bulk::BulkBatchId,
    ) -> crate::Result<Self> {
        Self::of(tower, "bulk", &[&user, &account, &batch])
    }

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

/// Continuation storage for one app, sealed with its deployment keyring.
pub(crate) struct AttemptContinuations<'a> {
    storage: &'a dyn ContinuationStorage,
    keyring: Arc<Keyring>,
}

impl<'a> AttemptContinuations<'a> {
    pub(crate) fn new(state: &'a AppState) -> Self {
        Self {
            storage: state.storage.as_ref(),
            keyring: state.config.keyring.clone(),
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
                &self.seal(scope, binding.unwrap_or(id), payload)?,
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

    fn seal<T: Serialize>(
        &self,
        scope: &Scope,
        binding: &str,
        payload: &T,
    ) -> crate::Result<String> {
        let context = Aad::try_from(&purpose::AttemptContinuation {
            scope: &scope.key,
            binding,
        })
        .map_err(crypto_error)?;
        let plaintext = serde_json::to_vec(payload).map_err(unavailable)?;
        self.keyring
            .envelope()
            .encrypt_sync(&plaintext, &context)
            .map(|ciphertext| ciphertext.to_base64())
            .map_err(crypto_error)
    }

    fn open<T: DeserializeOwned>(
        &self,
        scope: &Scope,
        stored: StoredContinuation,
    ) -> crate::Result<T> {
        if stored.scope != scope.key {
            return Err(WebError::Unauthenticated);
        }
        let context = Aad::try_from(&purpose::AttemptContinuation {
            scope: &stored.scope,
            binding: &stored.binding,
        })
        .map_err(crypto_error)?;
        let ciphertext = Ciphertext::from_base64(&stored.payload).map_err(crypto_error)?;
        let plaintext = self
            .keyring
            .envelope()
            .decrypt_sync(&ciphertext, &context)
            .map_err(crypto_error)?;
        serde_json::from_slice(&plaintext).map_err(unavailable)
    }
}

fn crypto_error(error: anykms_core::Error) -> WebError {
    // Rollout skew is a code on a DecryptFailed, so it is matched before the kind.
    if Code::of(&error) == Some(Code::VersionAboveHighWaterMark) {
        return WebError::KeyringRetry;
    }
    match error.kind() {
        ErrorKind::KeyVersionUnusable | ErrorKind::DecryptFailed => WebError::Unauthenticated,
        _ => {
            tracing::error!(alert = "p1_keyring_configuration", kind = ?error.kind(), "P1 continuation encryption unavailable");
            WebError::KeyringConfig
        }
    }
}

/// Actual D1 gate, exposed only in the fixture build. Uses the same mechanism
/// and persisted columns as console/invitation continuations.
#[cfg(feature = "runtime-tests")]
pub async fn verify_continuation_envelope(
    storage: &dyn ContinuationStorage,
    keyring: Arc<Keyring>,
) -> crate::Result<()> {
    let store = AttemptContinuations { storage, keyring };
    let scope = Scope {
        key: "runtime-envelope-continuation".into(),
        expires_at: Utc::now().timestamp() + 60,
    };
    for id in ["a", "b", "copied"] {
        store.release(&scope, id).await?;
    }
    if store
        .retain(
            &scope,
            "a",
            Some("logical-binding"),
            &"private payload".to_owned(),
        )
        .await?
        != Retention::Retained
    {
        return Err(unavailable("retain"));
    }
    if store.load::<String>(&scope, "a").await? != Some("private payload".into()) {
        return Err(unavailable("roundtrip"));
    }
    if store
        .retain(
            &scope,
            "b",
            Some("logical-binding"),
            &"other input".to_owned(),
        )
        .await?
        != Retention::Bound("private payload".into())
    {
        return Err(unavailable("binding recovery"));
    }
    let raw = storage
        .get_attempt_continuation(&scope.key, "a", Utc::now().timestamp())
        .await?
        .ok_or_else(|| unavailable("readback"))?;
    if raw.scope != scope.key
        || raw.binding != "logical-binding"
        || raw.payload.contains("private payload")
    {
        return Err(unavailable("plaintext/context readback"));
    }
    storage
        .retain_attempt_continuation(
            &scope.key,
            "copied",
            "other-binding",
            &raw.payload,
            scope.expires_at,
            Utc::now().timestamp(),
        )
        .await?;
    if !matches!(
        store.load::<String>(&scope, "copied").await,
        Err(WebError::Unauthenticated)
    ) {
        return Err(unavailable("binding substitution accepted"));
    }
    for id in ["a", "b", "copied"] {
        store.release(&scope, id).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use ghinvite_storage_sqlx::SqlxStorage;

    // A distant timestamp representable by both SQLite and D1 bindings.
    const LIVE: i64 = 4_102_444_800; // 2100-01-01 UTC

    fn scope(session: &str, user: u64, account: u64) -> Scope {
        Scope::derive("console", session, &[&user, &account], LIVE)
    }

    fn store<'a>(storage: &'a SqlxStorage, secret: &'a [u8; 32]) -> AttemptContinuations<'a> {
        AttemptContinuations {
            storage,
            keyring: crate::WebConfig::for_local_dev_with_secret(*secret).keyring,
        }
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

#[cfg(test)]
mod rotation_tests {
    use super::*;
    fn ring(current: u32, generations: &[(u32, u8)]) -> Arc<Keyring> {
        let mut builder = Keyring::builder("p1");
        for (v, k) in generations {
            builder = builder.generation(*v, [*k; 32]);
        }
        Arc::new(builder.current(current).build().unwrap())
    }
    #[tokio::test]
    async fn continuation_rotation_classifies_failures_without_losing_retained_input() {
        let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap();
        let scope = Scope {
            key: "console-user-subject".into(),
            expires_at: Utc::now().timestamp() + 300,
        };
        let old = AttemptContinuations {
            storage: &storage,
            keyring: ring(1, &[(1, 7)]),
        };
        old.retain(&scope, "original", Some("subject"), &"input".to_owned())
            .await
            .unwrap();
        let both = AttemptContinuations {
            storage: &storage,
            keyring: ring(2, &[(1, 7), (2, 8)]),
        };
        assert_eq!(
            both.load::<String>(&scope, "original").await.unwrap(),
            Some("input".into())
        );
        assert_eq!(
            both.retain(&scope, "retry", Some("subject"), &"changed".to_owned())
                .await
                .unwrap(),
            Retention::Bound("input".into())
        );
        both.retain(&scope, "new", None, &"new input".to_owned())
            .await
            .unwrap();
        assert!(matches!(
            old.load::<String>(&scope, "new").await,
            Err(WebError::KeyringRetry)
        ));
        let retired = AttemptContinuations {
            storage: &storage,
            keyring: ring(2, &[(2, 8)]),
        };
        assert!(matches!(
            retired.load::<String>(&scope, "original").await,
            Err(WebError::Unauthenticated)
        ));
        let changed = AttemptContinuations {
            storage: &storage,
            keyring: ring(2, &[(2, 9)]),
        };
        // Another root under a listed version is KeyIdMismatch: a DecryptFailed, not a
        // configuration error.
        assert!(matches!(
            changed.load::<String>(&scope, "new").await,
            Err(WebError::Unauthenticated)
        ));
        let stored = storage
            .get_attempt_continuation(&scope.key, "new", Utc::now().timestamp())
            .await
            .unwrap()
            .unwrap();
        let other_scope = Scope {
            key: "other-scope".into(),
            expires_at: scope.expires_at,
        };
        storage
            .retain_attempt_continuation(
                &other_scope.key,
                "new",
                "new",
                &stored.payload,
                scope.expires_at,
                Utc::now().timestamp(),
            )
            .await
            .unwrap();
        assert!(matches!(
            both.load::<String>(&other_scope, "new").await,
            Err(WebError::Unauthenticated)
        ));
        assert_eq!(
            both.load::<String>(&scope, "new").await.unwrap(),
            Some("new input".into())
        );
    }
}
