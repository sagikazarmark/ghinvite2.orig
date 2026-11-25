//! Server-side sessions, in memory: the browser holds only a random ID in a
//! cookie.
//!
//! Two kinds, under two cookies:
//!
//! - a **pending sign-in** (cookie [`LOGIN_COOKIE`], 10 minutes): the state
//!   and PKCE verifier between `/login` and `/callback`. Taken out, once, by
//!   the callback.
//! - a **signed-in account** (cookie [`SESSION_COOKIE`], 8 hours): who signed
//!   in and what they may see. Never a token.
//!
//! A successful sign-in always gets a new ID (no session fixation), and the
//! session it replaces is removed. A restart signs everyone out; a store
//! shared by several processes would replace the map.

use octologin::{PendingAuthorization, UserId};
use octotoken::InstallationId;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The cookie that names a pending sign-in.
pub const LOGIN_COOKIE: &str = "login";

/// The cookie that names a signed-in session.
pub const SESSION_COOKIE: &str = "session";

/// How long a pending sign-in waits for its callback. GitHub's codes live
/// 10 minutes too.
pub const LOGIN_TTL: Duration = Duration::from_secs(10 * 60);

/// How long a signed-in session lasts, from sign-in (not from last use).
pub const SESSION_TTL: Duration = Duration::from_secs(8 * 60 * 60);

/// The most sessions, pending or signed in, held at once: `/login` needs no
/// credentials, so without a bound anyone could fill the memory.
pub const MAX_SESSIONS: usize = 10_000;

/// Who signed in, and what they may see, as GitHub said at sign-in.
#[derive(Clone, Debug)]
pub struct Account {
    /// The stable identity.
    pub user: UserId,
    /// The login at sign-in.
    pub login: String,
    /// GitHub's avatar URL.
    pub avatar_url: Option<String>,
    /// The App's installations the user can access, each with the
    /// repositories they can access in it.
    pub access: Vec<InstallationAccess>,
    /// When `access` was read.
    pub checked: Instant,
}

/// One installation the user can access (`GET /user/installations`) and
/// the IDs of the repositories in it they can access
/// (`GET /user/installations/{id}/repositories`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallationAccess {
    /// The installation.
    pub id: InstallationId,
    /// Its account's login.
    pub account: String,
    /// Repository IDs.
    pub repositories: BTreeSet<u64>,
}

/// Why a session could not be created.
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum SessionError {
    /// [`MAX_SESSIONS`] unexpired sessions exist.
    #[error("example:sessions_full")]
    Full,
    /// The operating system's random number generator failed.
    #[error("example:randomness")]
    Randomness,
}

enum Kind {
    Pending(PendingAuthorization),
    SignedIn(Arc<Account>),
}

struct Entry {
    expires: Instant,
    kind: Kind,
}

/// The session store.
#[derive(Default)]
pub struct Sessions {
    entries: Mutex<HashMap<String, Entry>>,
}

impl Sessions {
    /// Keep a pending sign-in; returns its ID for [`LOGIN_COOKIE`].
    pub fn start(&self, pending: PendingAuthorization) -> Result<String, SessionError> {
        self.insert(Kind::Pending(pending), LOGIN_TTL)
    }

    /// Take the pending sign-in `id` out, if it exists and has not expired.
    /// One callback per sign-in: a second one finds nothing.
    pub fn take_pending(&self, id: &str) -> Option<PendingAuthorization> {
        let mut entries = self.lock();
        match entries.get(id) {
            Some(Entry {
                kind: Kind::Pending(_),
                ..
            }) => {}
            _ => return None,
        }
        match entries.remove(id) {
            Some(Entry {
                expires,
                kind: Kind::Pending(pending),
            }) if expires > Instant::now() => Some(pending),
            _ => None,
        }
    }

    /// Keep a signed-in account under a new ID, for [`SESSION_COOKIE`].
    pub fn sign_in(&self, account: Account) -> Result<String, SessionError> {
        self.insert(Kind::SignedIn(Arc::new(account)), SESSION_TTL)
    }

    /// The account of session `id`, if it is signed in and unexpired.
    pub fn account(&self, id: &str) -> Option<Arc<Account>> {
        match self.lock().get(id) {
            Some(Entry {
                expires,
                kind: Kind::SignedIn(account),
            }) if *expires > Instant::now() => Some(Arc::clone(account)),
            _ => None,
        }
    }

    /// Forget session `id`, of either kind.
    pub fn remove(&self, id: &str) {
        self.lock().remove(id);
    }

    fn insert(&self, kind: Kind, ttl: Duration) -> Result<String, SessionError> {
        let id = random_id()?;
        let now = Instant::now();
        let mut entries = self.lock();
        if entries.len() >= MAX_SESSIONS {
            entries.retain(|_, entry| entry.expires > now);
            if entries.len() >= MAX_SESSIONS {
                return Err(SessionError::Full);
            }
        }
        entries.insert(
            id.clone(),
            Entry {
                expires: now + ttl,
                kind,
            },
        );
        Ok(id)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        // A panic while holding the lock leaves the map consistent (every
        // operation is a single insert, remove or retain).
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// 32 random bytes, base64url: unguessable, and safe in a cookie.
fn random_id() -> Result<String, SessionError> {
    use base64::Engine as _;
    let mut bytes = [0u8; 32];
    getrandom_02::getrandom(&mut bytes).map_err(|_| SessionError::Randomness)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(state: &str) -> PendingAuthorization {
        PendingAuthorization {
            state: state.to_owned(),
            pkce_verifier: String::from("v").into(),
        }
    }

    fn account() -> Account {
        Account {
            user: UserId::new(1),
            login: "octocat".into(),
            avatar_url: None,
            access: Vec::new(),
            checked: Instant::now(),
        }
    }

    #[test]
    fn a_pending_sign_in_is_taken_once() {
        let sessions = Sessions::default();
        let id = sessions.start(pending("s")).unwrap();
        assert_eq!(sessions.take_pending(&id).unwrap().state, "s");
        assert!(sessions.take_pending(&id).is_none());
    }

    #[test]
    fn kinds_do_not_mix() {
        let sessions = Sessions::default();
        let pending = sessions.start(pending("s")).unwrap();
        let signed_in = sessions.sign_in(account()).unwrap();
        assert!(sessions.account(&pending).is_none());
        assert!(sessions.take_pending(&signed_in).is_none());
        assert_eq!(sessions.account(&signed_in).unwrap().login, "octocat");
    }

    #[test]
    fn ids_are_fresh_and_removable() {
        let sessions = Sessions::default();
        let first = sessions.sign_in(account()).unwrap();
        let second = sessions.sign_in(account()).unwrap();
        assert_ne!(first, second);
        assert_eq!(first.len(), 43);
        sessions.remove(&first);
        assert!(sessions.account(&first).is_none());
        assert!(sessions.account(&second).is_some());
    }
}
