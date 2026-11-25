//! User-side credentials: OAuth authorization codes, GitHub App user access
//! tokens (`ghu_…`) with their refresh tokens (`ghr_…`), and classic
//! personal access tokens (`ghp_…`) minted through the admin API.
//!
//! Only the SHA-256 of each secret is kept (its redacted ID, as for
//! installation tokens), so `GET /__octoemu/state` can list every record
//! without a secret in it.

use crate::tokens::{Rejected, token_id};
use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;
use sha2::Digest as _;
use std::collections::HashMap;

/// A user access token's lifetime when the App's tokens expire
/// (documented: `expires_in` 28800).
pub(crate) const ACCESS_LIFETIME: TimeDelta = TimeDelta::seconds(28_800);

/// A refresh token's lifetime (documented: `refresh_token_expires_in`
/// 15897600, six months).
pub(crate) const REFRESH_LIFETIME: TimeDelta = TimeDelta::seconds(15_897_600);

/// An authorization code's lifetime (documented: 10 minutes).
pub(crate) const CODE_LIFETIME: TimeDelta = TimeDelta::minutes(10);

/// Live user access tokens per user and App; minting another revokes the
/// oldest (documented for OAuth apps, behaviour `user-token-cap`).
pub(crate) const TOKENS_PER_USER_APP: usize = 10;

/// New user access tokens per user and App per hour before authorizing
/// needs the browser again, unless the seed says otherwise.
pub(crate) const DEFAULT_TOKENS_PER_HOUR: u32 = 10;

/// What a user credential stands for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Grant {
    /// A GitHub App's user access token (`ghu_…`).
    UserToken { app_id: u64 },
    /// A classic personal access token (`ghp_…`), with its scopes.
    PersonalAccessToken { scopes: Vec<String> },
}

/// A minted user credential, without its secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct UserTokenRecord {
    /// The first 16 bytes of the token's SHA-256, in hex.
    pub(crate) id: String,
    #[serde(flatten)]
    pub(crate) grant: Grant,
    pub(crate) user_id: u64,
    pub(crate) login: String,
    pub(crate) issued_at: DateTime<Utc>,
    /// `None`: never expires.
    pub(crate) expires_at: Option<DateTime<Utc>>,
    pub(crate) revoked: bool,
    /// The token's last eight characters (`token_last_eight`).
    #[serde(skip)]
    pub(crate) last_eight: String,
    #[serde(skip)]
    seq: u64,
}

impl UserTokenRecord {
    /// The App of a user access token.
    pub(crate) fn app_id(&self) -> Option<u64> {
        match self.grant {
            Grant::UserToken { app_id } => Some(app_id),
            Grant::PersonalAccessToken { .. } => None,
        }
    }

    /// A numeric ID for the token (`id` in the token-check API).
    pub(crate) fn number(&self) -> u64 {
        self.seq
    }

    fn live(&self, now: DateTime<Utc>) -> bool {
        !self.revoked && self.expires_at.is_none_or(|at| now < at)
    }
}

/// A refresh token, without its secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct RefreshRecord {
    pub(crate) id: String,
    pub(crate) app_id: u64,
    pub(crate) user_id: u64,
    pub(crate) login: String,
    /// The access token it was issued with.
    pub(crate) access_token_id: String,
    pub(crate) issued_at: DateTime<Utc>,
    pub(crate) expires_at: DateTime<Utc>,
    /// Used (single use) or revoked with its access token.
    pub(crate) revoked: bool,
}

/// An authorization code, bound to its App, user and redirect URI.
#[derive(Clone, Debug)]
pub(crate) struct Code {
    pub(crate) app_id: u64,
    pub(crate) user_id: u64,
    pub(crate) login: String,
    pub(crate) redirect_uri: String,
    /// The S256 `code_challenge`, if the request sent one.
    pub(crate) challenge: Option<String>,
    expires_at: DateTime<Utc>,
    used: bool,
}

/// A freshly minted user access token.
#[derive(Debug)]
pub(crate) struct Minted {
    pub(crate) token: String,
    pub(crate) refresh_token: Option<String>,
    pub(crate) record: UserTokenRecord,
}

/// Every user credential of one emulator instance.
#[derive(Debug, Default)]
pub(crate) struct UserTokens {
    access: HashMap<String, UserTokenRecord>,
    refresh: HashMap<String, RefreshRecord>,
    codes: HashMap<String, Code>,
    /// (App ID, lowercased login, when) of every new user access token, for
    /// the hourly limit.
    minted: Vec<(u64, String, DateTime<Utc>)>,
    seq: u64,
}

const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// `prefix` and `length` random base62 characters (GitHub's token shape).
fn random_token(prefix: &str, length: usize) -> String {
    let mut bytes = vec![0u8; length];
    if getrandom_02::getrandom(&mut bytes).is_err() {
        // Uniqueness only, as in `tokens::Tokens::default`.
        for (i, chunk) in bytes.chunks_mut(32).enumerate() {
            let digest = sha2::Sha256::digest(format!(
                "{:?}{:?}{i}",
                std::time::SystemTime::now(),
                std::time::Instant::now()
            ));
            chunk.copy_from_slice(&digest[..chunk.len()]);
        }
    }
    let mut token = String::with_capacity(prefix.len() + length);
    token.push_str(prefix);
    for byte in bytes {
        token.push(BASE62[usize::from(byte) % BASE62.len()] as char);
    }
    token
}

fn whole_seconds(now: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp(now.timestamp(), 0).unwrap_or(now)
}

impl UserTokens {
    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// A new authorization code (20 characters, like GitHub's), valid for
    /// [`CODE_LIFETIME`].
    pub(crate) fn issue_code(
        &mut self,
        app_id: u64,
        user_id: u64,
        login: &str,
        redirect_uri: &str,
        challenge: Option<String>,
        now: DateTime<Utc>,
    ) -> String {
        let code = random_token("", 20);
        self.codes.insert(
            token_id(&code),
            Code {
                app_id,
                user_id,
                login: login.to_owned(),
                redirect_uri: redirect_uri.to_owned(),
                challenge,
                expires_at: now + CODE_LIFETIME,
                used: false,
            },
        );
        code
    }

    /// Uses up `code` for App `app_id`: `None` when it is unknown, another
    /// App's, used or expired.
    pub(crate) fn take_code(
        &mut self,
        code: &str,
        app_id: u64,
        now: DateTime<Utc>,
    ) -> Option<Code> {
        let entry = self.codes.get_mut(&token_id(code))?;
        if entry.app_id != app_id || entry.used || now >= entry.expires_at {
            return None;
        }
        entry.used = true;
        Some(entry.clone())
    }

    /// New user access tokens for `app_id` and `login` in the hour before
    /// `now`.
    pub(crate) fn minted_last_hour(&self, app_id: u64, login: &str, now: DateTime<Utc>) -> usize {
        let since = now - TimeDelta::hours(1);
        let login = login.to_lowercase();
        self.minted
            .iter()
            .filter(|(app, user, at)| *app == app_id && *user == login && *at > since)
            .count()
    }

    /// Mints a user access token for `app_id` and the user, with a refresh
    /// token when `expiring`. A new grant (not a refresh) counts towards the
    /// hourly limit. Past [`TOKENS_PER_USER_APP`] live tokens, the oldest are
    /// revoked.
    pub(crate) fn mint_user_token(
        &mut self,
        app_id: u64,
        user_id: u64,
        login: &str,
        expiring: bool,
        counted: bool,
        now: DateTime<Utc>,
    ) -> Minted {
        let issued_at = whole_seconds(now);
        let token = random_token("ghu_", 36);
        let seq = self.next_seq();
        let record = UserTokenRecord {
            id: token_id(&token),
            grant: Grant::UserToken { app_id },
            user_id,
            login: login.to_owned(),
            issued_at,
            expires_at: expiring.then(|| issued_at + ACCESS_LIFETIME),
            revoked: false,
            last_eight: token[token.len() - 8..].to_owned(),
            seq,
        };
        self.access.insert(record.id.clone(), record.clone());
        let refresh_token = expiring.then(|| {
            let refresh = random_token("ghr_", 76);
            let id = token_id(&refresh);
            self.refresh.insert(
                id.clone(),
                RefreshRecord {
                    id,
                    app_id,
                    user_id,
                    login: login.to_owned(),
                    access_token_id: record.id.clone(),
                    issued_at,
                    expires_at: issued_at + REFRESH_LIFETIME,
                    revoked: false,
                },
            );
            refresh
        });
        if counted {
            self.minted.push((app_id, login.to_lowercase(), now));
        }
        self.enforce_cap(app_id, user_id, now);
        Minted {
            token,
            refresh_token,
            record,
        }
    }

    /// Live user access tokens of `app_id` for the user.
    pub(crate) fn live_count(&self, app_id: u64, user_id: u64, now: DateTime<Utc>) -> usize {
        self.access
            .values()
            .filter(|r| r.app_id() == Some(app_id) && r.user_id == user_id && r.live(now))
            .count()
    }

    /// Revokes the oldest live tokens of `app_id` for the user beyond
    /// [`TOKENS_PER_USER_APP`].
    fn enforce_cap(&mut self, app_id: u64, user_id: u64, now: DateTime<Utc>) {
        let mut live: Vec<(u64, String)> = self
            .access
            .values()
            .filter(|r| r.app_id() == Some(app_id) && r.user_id == user_id && r.live(now))
            .map(|r| (r.seq, r.id.clone()))
            .collect();
        live.sort();
        let excess = live.len().saturating_sub(TOKENS_PER_USER_APP);
        for (_, id) in live.into_iter().take(excess) {
            self.revoke(&id);
        }
    }

    /// Mints a classic personal access token with `scopes` (never expires).
    pub(crate) fn mint_personal_access_token(
        &mut self,
        user_id: u64,
        login: &str,
        scopes: Vec<String>,
        now: DateTime<Utc>,
    ) -> Minted {
        let token = random_token("ghp_", 36);
        let seq = self.next_seq();
        let record = UserTokenRecord {
            id: token_id(&token),
            grant: Grant::PersonalAccessToken { scopes },
            user_id,
            login: login.to_owned(),
            issued_at: whole_seconds(now),
            expires_at: None,
            revoked: false,
            last_eight: token[token.len() - 8..].to_owned(),
            seq,
        };
        self.access.insert(record.id.clone(), record.clone());
        Minted {
            token,
            refresh_token: None,
            record,
        }
    }

    /// The record of a `ghu_`/`ghp_` token, checked for revocation and
    /// expiry at `now`.
    pub(crate) fn verify(
        &self,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<&UserTokenRecord, Rejected> {
        let record = self.access.get(&token_id(token)).ok_or(Rejected::Unknown)?;
        if record.revoked {
            Err(Rejected::Revoked)
        } else if record.expires_at.is_some_and(|at| now >= at) {
            Err(Rejected::Expired)
        } else {
            Ok(record)
        }
    }

    /// Uses up refresh token `token` of App `app_id`, revoking the access
    /// token it came with (documented: both stop working). `None` when it
    /// is unknown, another App's, used, revoked or expired.
    pub(crate) fn redeem_refresh(
        &mut self,
        token: &str,
        app_id: u64,
        now: DateTime<Utc>,
    ) -> Option<RefreshRecord> {
        let record = self.refresh.get_mut(&token_id(token))?;
        if record.app_id != app_id || record.revoked || now >= record.expires_at {
            return None;
        }
        record.revoked = true;
        let record = record.clone();
        if let Some(access) = self.access.get_mut(&record.access_token_id) {
            access.revoked = true;
        }
        Some(record)
    }

    /// Revokes access token `id` and its refresh token.
    pub(crate) fn revoke(&mut self, id: &str) {
        if let Some(record) = self.access.get_mut(id) {
            record.revoked = true;
        }
        for refresh in self.refresh.values_mut() {
            if refresh.access_token_id == id {
                refresh.revoked = true;
            }
        }
    }

    /// Revokes every token `app_id` holds for user `user_id` (deleting the
    /// grant).
    pub(crate) fn revoke_grant(&mut self, app_id: u64, user_id: u64) {
        for record in self.access.values_mut() {
            if record.app_id() == Some(app_id) && record.user_id == user_id {
                record.revoked = true;
            }
        }
        for refresh in self.refresh.values_mut() {
            if refresh.app_id == app_id && refresh.user_id == user_id {
                refresh.revoked = true;
            }
        }
    }

    /// Every access token and PAT record, oldest first.
    pub(crate) fn records(&self) -> Vec<&UserTokenRecord> {
        let mut records: Vec<_> = self.access.values().collect();
        records.sort_by_key(|r| r.seq);
        records
    }

    /// Every refresh token record, oldest first.
    pub(crate) fn refresh_records(&self) -> Vec<&RefreshRecord> {
        let mut records: Vec<_> = self.refresh.values().collect();
        records.sort_by(|a, b| a.issued_at.cmp(&b.issued_at).then(a.id.cmp(&b.id)));
        records
    }
}

/// What `POST /__octoemu/tokens/user` (or the handle) mints.
pub(crate) enum MintRequest {
    /// A user access token of App `app_id`.
    UserToken { app_id: u64 },
    /// A classic personal access token with `scopes`.
    PersonalAccessToken { scopes: Vec<String> },
}

/// Mints a user credential for seeded user `login` without the browser: a
/// user access token counts as a new grant (hourly limit, cap) like one
/// from the token endpoint. `Err` names what is unknown.
pub(crate) fn mint_for(
    state: &mut crate::state::State,
    login: &str,
    request: MintRequest,
) -> Result<Minted, String> {
    let now = state.clock.now();
    let user = state
        .world
        .users
        .iter()
        .find(|u| u.login.eq_ignore_ascii_case(login))
        .map(|u| (u.id, u.login.clone()))
        .ok_or_else(|| format!("no user {login:?}"))?;
    match request {
        MintRequest::UserToken { app_id } => {
            let app = state
                .world
                .apps
                .iter()
                .find(|a| a.id == app_id)
                .ok_or_else(|| format!("no App with ID {app_id}"))?;
            let expiring = app.expire_user_tokens;
            Ok(state
                .user_tokens
                .mint_user_token(app_id, user.0, &user.1, expiring, true, now))
        }
        MintRequest::PersonalAccessToken { scopes } => Ok(state
            .user_tokens
            .mint_personal_access_token(user.0, &user.1, scopes, now)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_900_000_000, 0).unwrap()
    }

    #[test]
    fn tokens_have_githubs_shape_and_lifetimes() {
        let mut tokens = UserTokens::default();
        let minted = tokens.mint_user_token(1, 2, "octocat", true, true, now());
        assert!(minted.token.starts_with("ghu_") && minted.token.len() == 40);
        let refresh = minted.refresh_token.clone().unwrap();
        assert!(refresh.starts_with("ghr_") && refresh.len() == 80);
        assert_eq!(minted.record.expires_at, Some(now() + ACCESS_LIFETIME));
        assert!(tokens.verify(&minted.token, now()).is_ok());
        assert_eq!(
            tokens.verify(&minted.token, now() + ACCESS_LIFETIME),
            Err(Rejected::Expired)
        );
        assert!(!octoscrub::scrub(&minted.token).contains(&minted.token[4..]));
        let forever = tokens.mint_user_token(1, 2, "octocat", false, true, now());
        assert!(forever.refresh_token.is_none());
        assert!(
            tokens
                .verify(&forever.token, now() + REFRESH_LIFETIME)
                .is_ok()
        );
        assert_eq!(tokens.minted_last_hour(1, "OctoCat", now()), 2);
        assert_eq!(
            tokens.minted_last_hour(1, "octocat", now() + TimeDelta::hours(1)),
            0
        );
    }

    #[test]
    fn refresh_is_single_use_and_kills_the_access_token() {
        let mut tokens = UserTokens::default();
        let minted = tokens.mint_user_token(1, 2, "octocat", true, true, now());
        let refresh = minted.refresh_token.unwrap();
        assert!(tokens.redeem_refresh(&refresh, 9, now()).is_none());
        assert!(tokens.redeem_refresh(&refresh, 1, now()).is_some());
        assert_eq!(tokens.verify(&minted.token, now()), Err(Rejected::Revoked));
        assert!(tokens.redeem_refresh(&refresh, 1, now()).is_none());
    }

    #[test]
    fn codes_are_single_use_bound_and_short_lived() {
        let mut tokens = UserTokens::default();
        let code = tokens.issue_code(1, 2, "octocat", "http://h/cb", None, now());
        assert_eq!(code.len(), 20);
        assert!(tokens.take_code(&code, 9, now()).is_none());
        assert!(tokens.take_code(&code, 1, now()).is_some());
        assert!(tokens.take_code(&code, 1, now()).is_none());
        let late = tokens.issue_code(1, 2, "octocat", "http://h/cb", None, now());
        assert!(tokens.take_code(&late, 1, now() + CODE_LIFETIME).is_none());
    }

    #[test]
    fn the_eleventh_token_revokes_the_oldest() {
        let mut tokens = UserTokens::default();
        let minted: Vec<_> = (0..11)
            .map(|_| tokens.mint_user_token(1, 2, "octocat", true, false, now()))
            .collect();
        assert_eq!(
            tokens.verify(&minted[0].token, now()),
            Err(Rejected::Revoked)
        );
        for later in &minted[1..] {
            assert!(tokens.verify(&later.token, now()).is_ok());
        }
        let refresh = minted[0].refresh_token.as_deref().unwrap();
        assert!(tokens.redeem_refresh(refresh, 1, now()).is_none());
    }
}
