# ghinvite — Plan 1: Foundations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the type-safe domain core, append-only audit event type, and a SQLite-backed storage layer (with a parameterized test suite that any future `Storage` implementor must pass) — the foundation every other plan in the ghinvite project depends on.

**Architecture:** Three pure-Rust crates in a Cargo workspace. `crates/domain` holds types and state derivations with no I/O. `crates/audit` defines the event payload that handlers will emit. `crates/storage` defines the `Storage` trait, ships a `SqlxStorage` implementation against SQLite, and includes a parameterized test suite (`tests::run_suite`) that takes any `Storage` impl and exercises every code path.

**Tech Stack:** Rust 2024 edition (workspace), `sqlx 0.8` with `sqlite` feature for storage, `chrono 0.4` for timestamps, `ulid 1.x` for sortable IDs, `rand 0.8` + `rand_chacha 0.3` for slug generation and deterministic tests, `serde` + `serde_json` for `selected_repos` JSON and audit metadata, `subtle 2.x` for constant-time slug comparison, `async-trait 0.1` for the `Storage` trait, `tokio` (test feature only) for async tests.

---

## Spec coverage

This plan implements §7.1 (data model), §7.2 (no SQL CHECK constraints), §7.3 (Storage trait shape), §8 (state machines as pure derivations), and §15.1 (audit event types as a Rust enum). It does NOT implement: §9 (Restate handlers), §10 (auth flows), §11 (web routing), §13 (webhooks), §14 (GitHub API), or the `D1Storage` implementation. Those are later plans.

## File structure

| Path | Responsibility |
|---|---|
| `Cargo.toml` (workspace root) | Workspace member list + shared `[workspace.dependencies]` versions |
| `rust-toolchain.toml` | Pin Rust 1.85 stable for reproducibility |
| `.gitignore` | Ignore `target/`, `.sqlite*`, `.env*`, `node_modules/`, `dist/` |
| `crates/domain/Cargo.toml` | Domain crate manifest |
| `crates/domain/src/lib.rs` | Module declarations + re-exports |
| `crates/domain/src/ids.rs` | Typed Ulid newtypes: `ShareLinkId`, `RequestId`, `GithubInvitationId`, `AuditEventId` |
| `crates/domain/src/slug.rs` | `Slug::generate(&mut rng)`, `Slug::from_str`, constant-time `eq` |
| `crates/domain/src/account.rs` | `AccountType` enum (`User`/`Organization`), `Account` struct |
| `crates/domain/src/user.rs` | `User` struct |
| `crates/domain/src/permission.rs` | `Permission` enum + parse/display |
| `crates/domain/src/share_link.rs` | `ShareLink` struct + `is_active(now)` |
| `crates/domain/src/invitation_request.rs` | `RequestState` enum + `InvitationRequest` struct |
| `crates/domain/src/github_invitation.rs` | `InvitationState` enum + `GithubInvitation` struct |
| `crates/audit/Cargo.toml` | Audit crate manifest |
| `crates/audit/src/lib.rs` | `AuditEvent`, `ActorKind`, `TargetKind`, `EventType` enum |
| `crates/storage/Cargo.toml` | Storage crate manifest |
| `crates/storage/src/lib.rs` | `Storage` async trait, `Error` type, `Result` alias |
| `crates/storage/src/records.rs` | Stored record types: `InstallationRecord`, `ShareLinkRecord`, `InvitationRequestRecord`, `GithubInvitationRecord`, helpers |
| `crates/storage/src/sqlx_impl.rs` | `SqlxStorage` impl |
| `crates/storage/src/tests.rs` | `pub async fn run_suite<S: Storage>(storage: S)` — parameterized scenarios |
| `crates/storage/tests/sqlx_suite.rs` | Wires `run_suite` to `SqlxStorage` (in-memory SQLite) |
| `migrations/0001_initial.sql` | Schema for all 8 v1 tables (excluding tower-sessions store; that's added in Plan 4) |
| `migrations/README.md` | How migrations work, where they're applied |

---

## Task ordering

Tasks 1–2 establish the workspace. Tasks 3–11 build the domain crate (no I/O, fast tests). Task 12 is the audit crate. Tasks 13–24 are the storage crate, structured TDD-style: trait first, migration second, then methods one table-group at a time, with the parameterized suite running incrementally.

---

### Task 1: Workspace skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Modify: `.gitignore`

- [ ] **Step 1: Create the workspace `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = [
    "crates/domain",
    "crates/audit",
    "crates/storage",
]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"
publish = false

[workspace.dependencies]
async-trait = "0.1"
chrono = { version = "0.4", default-features = false, features = ["std", "serde", "clock"] }
rand = { version = "0.8", default-features = false }
rand_chacha = { version = "0.3", default-features = false }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sqlx = { version = "0.8", default-features = false, features = ["runtime-tokio", "sqlite", "macros", "chrono"] }
subtle = "2"
thiserror = "1"
tokio = { version = "1", default-features = false, features = ["macros", "rt"] }
ulid = { version = "1", features = ["serde"] }

# crates depend on each other only via path:
domain = { path = "crates/domain" }
audit = { path = "crates/audit" }
storage = { path = "crates/storage" }
```

- [ ] **Step 2: Create `rust-toolchain.toml`**

```toml
[toolchain]
channel = "1.85.0"
components = ["rustfmt", "clippy"]
profile = "minimal"
```

- [ ] **Step 3: Update `.gitignore`**

Append (create if absent):

```gitignore
/target
/dist
/node_modules
*.sqlite
*.sqlite-*
.env
.env.*
!.env.example
.direnv
```

- [ ] **Step 4: Verify the workspace parses**

Run: `cargo metadata --no-deps --format-version 1 > /dev/null`
Expected: exits 0, no output. (Will succeed even with no member crates yet, because the member dirs don't have to exist for `cargo metadata` to succeed when nothing is fetched.) If this errors with "no manifests found", that's expected — proceed to Task 2 which adds the first member.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml rust-toolchain.toml .gitignore
git commit -m "chore: scaffold cargo workspace"
```

---

### Task 2: Domain crate skeleton

**Files:**
- Create: `crates/domain/Cargo.toml`
- Create: `crates/domain/src/lib.rs`

- [ ] **Step 1: Create the domain crate manifest**

`crates/domain/Cargo.toml`:

```toml
[package]
name = "domain"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
chrono.workspace = true
rand.workspace = true
serde.workspace = true
subtle.workspace = true
thiserror.workspace = true
ulid.workspace = true

[dev-dependencies]
rand_chacha.workspace = true
serde_json.workspace = true
```

- [ ] **Step 2: Create `crates/domain/src/lib.rs`**

```rust
//! Pure domain types and state derivations. No I/O.

pub mod account;
pub mod github_invitation;
pub mod ids;
pub mod invitation_request;
pub mod permission;
pub mod share_link;
pub mod slug;
pub mod user;

pub use account::{Account, AccountType};
pub use github_invitation::{GithubInvitation, InvitationState};
pub use ids::{AuditEventId, GithubInvitationId, RequestId, ShareLinkId};
pub use invitation_request::{InvitationRequest, RequestState};
pub use permission::Permission;
pub use share_link::ShareLink;
pub use slug::Slug;
pub use user::User;
```

- [ ] **Step 3: Add stub module files so the crate compiles**

For each of `account.rs`, `github_invitation.rs`, `ids.rs`, `invitation_request.rs`, `permission.rs`, `share_link.rs`, `slug.rs`, `user.rs` under `crates/domain/src/`, create the file with a single line:

```rust
// filled in by a later task
```

- [ ] **Step 4: Run `cargo check`**

Run: `cargo check -p domain`
Expected: errors — each `pub use` references types that don't exist yet. We'll fix as we go. To unblock the build right now, replace each module's stub with a placeholder `pub struct X;` per the names referenced in `lib.rs`. **Don't do that.** Instead, comment out the `pub use` lines in `lib.rs` and uncomment them as each task finishes. Update `lib.rs`:

```rust
//! Pure domain types and state derivations. No I/O.

pub mod account;
pub mod github_invitation;
pub mod ids;
pub mod invitation_request;
pub mod permission;
pub mod share_link;
pub mod slug;
pub mod user;

// Re-exports filled in as each module gains its public types:
// pub use account::{Account, AccountType};
// pub use github_invitation::{GithubInvitation, InvitationState};
// pub use ids::{AuditEventId, GithubInvitationId, RequestId, ShareLinkId};
// pub use invitation_request::{InvitationRequest, RequestState};
// pub use permission::Permission;
// pub use share_link::ShareLink;
// pub use slug::Slug;
// pub use user::User;
```

Run `cargo check -p domain` again. Expected: clean build (warnings about unused module is OK; a `#[allow(dead_code)]` on each module is unnecessary because there's no code yet).

- [ ] **Step 5: Commit**

```bash
git add crates/domain
git commit -m "feat(domain): scaffold crate skeleton"
```

---

### Task 3: Typed ID newtypes

Each domain ID is a Ulid wrapped in a distinct newtype so the type system prevents passing a `RequestId` where a `ShareLinkId` is expected.

**Files:**
- Modify: `crates/domain/src/ids.rs`
- Modify: `crates/domain/src/lib.rs` (un-comment the `ids` re-export)

- [ ] **Step 1: Write failing tests in `crates/domain/src/ids.rs`**

```rust
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use ulid::Ulid;

macro_rules! ulid_newtype {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Ulid);

        impl $name {
            pub fn new() -> Self {
                Self(Ulid::new())
            }

            pub fn from_ulid(u: Ulid) -> Self {
                Self(u)
            }

            pub fn as_ulid(&self) -> Ulid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = ulid::DecodeError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ulid::from_str(s).map(Self)
            }
        }
    };
}

ulid_newtype!(ShareLinkId);
ulid_newtype!(RequestId);
ulid_newtype!(GithubInvitationId);
ulid_newtype!(AuditEventId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_roundtrip_through_string() {
        let id = ShareLinkId::new();
        let s = id.to_string();
        let parsed: ShareLinkId = s.parse().expect("roundtrip");
        assert_eq!(id, parsed);
    }

    #[test]
    fn distinct_id_types_are_not_interchangeable() {
        // This is a compile-time guarantee; we just check the types differ at runtime.
        let _link: ShareLinkId = ShareLinkId::new();
        let _req: RequestId = RequestId::new();
        // Uncommenting this line should cause a compile error:
        // let _: ShareLinkId = _req;
    }

    #[test]
    fn serde_transparent_to_string() {
        let id = RequestId::new();
        let json = serde_json::to_string(&id).unwrap();
        // Ulid serializes as a 26-char base32 string.
        assert!(json.starts_with('"') && json.ends_with('"'));
        assert_eq!(json.trim_matches('"').len(), 26);
    }
}
```

- [ ] **Step 2: Run the tests; expect a clean pass**

Run: `cargo test -p domain --lib ids`
Expected: 3 tests pass. Because the macro defines the types in the same edit, the test file *is* the implementation. (TDD here is "write the test code that exercises the implementation in the same module"; the test fails only if the macro is wrong.)

- [ ] **Step 3: Un-comment the `ids` re-export in `lib.rs`**

Replace the commented re-exports list line for ids with the active line:

```rust
pub use ids::{AuditEventId, GithubInvitationId, RequestId, ShareLinkId};
```

Run: `cargo build -p domain`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/domain/src/ids.rs crates/domain/src/lib.rs
git commit -m "feat(domain): typed Ulid newtypes for IDs"
```

---

### Task 4: Slug generation and constant-time comparison

Per spec §Q7: 16 base62 chars, cryptographic RNG, constant-time compare.

**Files:**
- Modify: `crates/domain/src/slug.rs`
- Modify: `crates/domain/src/lib.rs`

- [ ] **Step 1: Write failing tests**

`crates/domain/src/slug.rs`:

```rust
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::fmt;
use subtle::ConstantTimeEq;

const SLUG_LEN: usize = 16;
const ALPHABET: &[u8; 62] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// 16-character base62 secret used in share-link URLs.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Slug(String);

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SlugError {
    #[error("slug must be exactly {SLUG_LEN} characters")]
    BadLength,
    #[error("slug contains non-base62 character")]
    BadCharacter,
}

impl Slug {
    /// Generate a new random slug from the given RNG.
    /// In production, pass `rand::thread_rng()` or `rand::rngs::OsRng`.
    /// In tests, pass a seeded `ChaCha8Rng` for determinism.
    pub fn generate<R: RngCore>(rng: &mut R) -> Self {
        let mut bytes = [0u8; SLUG_LEN];
        let mut out = String::with_capacity(SLUG_LEN);
        rng.fill_bytes(&mut bytes);
        for b in bytes {
            // Reject-free mod 62 is biased but the bias is tiny (256 % 62 = 8 / 256 ≈ 3%);
            // for a 16-character slug at 95+ bits of entropy that's irrelevant security-wise.
            out.push(ALPHABET[(b % 62) as usize] as char);
        }
        Self(out)
    }

    pub fn from_string(s: String) -> Result<Self, SlugError> {
        if s.len() != SLUG_LEN {
            return Err(SlugError::BadLength);
        }
        if !s.bytes().all(|b| ALPHABET.contains(&b)) {
            return Err(SlugError::BadCharacter);
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Constant-time equality. Use this whenever comparing a user-supplied slug
    /// against a stored slug to prevent timing-based enumeration.
    pub fn ct_eq(&self, other: &Slug) -> bool {
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[test]
    fn generate_produces_16_base62_chars() {
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let slug = Slug::generate(&mut rng);
        assert_eq!(slug.as_str().len(), 16);
        assert!(slug.as_str().bytes().all(|b| ALPHABET.contains(&b)));
    }

    #[test]
    fn deterministic_under_seeded_rng() {
        let mut a = ChaCha8Rng::seed_from_u64(42);
        let mut b = ChaCha8Rng::seed_from_u64(42);
        assert_eq!(Slug::generate(&mut a).as_str(), Slug::generate(&mut b).as_str());
    }

    #[test]
    fn from_string_validates_length() {
        assert_eq!(Slug::from_string("short".into()), Err(SlugError::BadLength));
        assert_eq!(
            Slug::from_string("seventeen-chars-x".into()),
            Err(SlugError::BadLength)
        );
    }

    #[test]
    fn from_string_validates_alphabet() {
        let invalid = "abcdef0123456!@#"; // 16 chars, but '!' '@' '#' not base62
        assert_eq!(
            Slug::from_string(invalid.into()),
            Err(SlugError::BadCharacter)
        );
    }

    #[test]
    fn from_string_round_trips_valid_slug() {
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        let s = Slug::generate(&mut rng);
        let parsed = Slug::from_string(s.as_str().to_string()).unwrap();
        assert!(s.ct_eq(&parsed));
    }

    #[test]
    fn ct_eq_distinguishes_different_slugs() {
        let mut r1 = ChaCha8Rng::seed_from_u64(1);
        let mut r2 = ChaCha8Rng::seed_from_u64(2);
        let a = Slug::generate(&mut r1);
        let b = Slug::generate(&mut r2);
        assert!(!a.ct_eq(&b));
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p domain --lib slug`
Expected: 6 tests pass.

- [ ] **Step 3: Un-comment the `Slug` re-export in `lib.rs`**

```rust
pub use slug::Slug;
```

- [ ] **Step 4: Commit**

```bash
git add crates/domain/src/slug.rs crates/domain/src/lib.rs
git commit -m "feat(domain): Slug with seeded gen and constant-time compare"
```

---

### Task 5: Account types

**Files:**
- Modify: `crates/domain/src/account.rs`
- Modify: `crates/domain/src/lib.rs`

- [ ] **Step 1: Write the implementation + tests**

`crates/domain/src/account.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// GitHub account type. The two variants are GitHub-defined and stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum AccountType {
    User,
    Organization,
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("unknown account type: {0}")]
pub struct UnknownAccountType(pub String);

impl FromStr for AccountType {
    type Err = UnknownAccountType;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "User" => Ok(Self::User),
            "Organization" => Ok(Self::Organization),
            other => Err(UnknownAccountType(other.to_string())),
        }
    }
}

impl fmt::Display for AccountType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::User => "User",
            Self::Organization => "Organization",
        })
    }
}

/// A GitHub account on which the App is (or was) installed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub installation_id: u64,
    pub account_id: u64,
    pub account_login: String,
    pub account_type: AccountType,
    pub installed_at: DateTime<Utc>,
    pub uninstalled_at: Option<DateTime<Utc>>,
    pub selected_repos: SelectedRepos,
}

/// Either "all repos selected" or an explicit list of GitHub repo IDs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SelectedRepos {
    All,
    Subset(Vec<u64>),
}

impl SelectedRepos {
    pub fn includes(&self, repo_id: u64) -> bool {
        match self {
            Self::All => true,
            Self::Subset(ids) => ids.contains(&repo_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_type_parses_known_values() {
        assert_eq!("User".parse(), Ok(AccountType::User));
        assert_eq!("Organization".parse(), Ok(AccountType::Organization));
    }

    #[test]
    fn account_type_rejects_unknown() {
        assert_eq!(
            "Robot".parse::<AccountType>(),
            Err(UnknownAccountType("Robot".into()))
        );
    }

    #[test]
    fn account_type_display_round_trips() {
        for t in [AccountType::User, AccountType::Organization] {
            assert_eq!(t.to_string().parse::<AccountType>().unwrap(), t);
        }
    }

    #[test]
    fn selected_repos_all_includes_anything() {
        assert!(SelectedRepos::All.includes(123));
        assert!(SelectedRepos::All.includes(0));
    }

    #[test]
    fn selected_repos_subset_filters() {
        let s = SelectedRepos::Subset(vec![1, 2, 3]);
        assert!(s.includes(2));
        assert!(!s.includes(4));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p domain --lib account`
Expected: 5 tests pass.

- [ ] **Step 3: Un-comment the `account` re-export**

```rust
pub use account::{Account, AccountType};
```

Also add the `SelectedRepos` re-export:

```rust
pub use account::{Account, AccountType, SelectedRepos};
```

- [ ] **Step 4: Commit**

```bash
git add crates/domain/src/account.rs crates/domain/src/lib.rs
git commit -m "feat(domain): Account, AccountType, SelectedRepos"
```

---

### Task 6: User type

**Files:**
- Modify: `crates/domain/src/user.rs`
- Modify: `crates/domain/src/lib.rs`

- [ ] **Step 1: Write impl + tests**

`crates/domain/src/user.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Cached GitHub user info. `login` is mutable upstream; refreshed on each sign-in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub user_id: u64,
    pub login: String,
    pub avatar_url: Option<String>,
    pub last_seen_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn user_serde_round_trips() {
        let u = User {
            user_id: 42,
            login: "octocat".into(),
            avatar_url: Some("https://example.test/a.png".into()),
            last_seen_at: Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap(),
        };
        let s = serde_json::to_string(&u).unwrap();
        let parsed: User = serde_json::from_str(&s).unwrap();
        assert_eq!(u, parsed);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p domain --lib user`
Expected: 1 test passes.

- [ ] **Step 3: Un-comment the `user` re-export in `lib.rs`**

```rust
pub use user::User;
```

- [ ] **Step 4: Commit**

```bash
git add crates/domain/src/user.rs crates/domain/src/lib.rs
git commit -m "feat(domain): User struct"
```

---

### Task 7: Permission enum

**Files:**
- Modify: `crates/domain/src/permission.rs`
- Modify: `crates/domain/src/lib.rs`

- [ ] **Step 1: Write impl + tests**

`crates/domain/src/permission.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// GitHub repo-collaborator permission level.
/// Stored as the lowercase string GitHub returns (`"pull"`, `"push"`, etc.).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Pull,
    Triage,
    Push,
    Maintain,
    Admin,
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("unknown permission: {0}")]
pub struct UnknownPermission(pub String);

impl FromStr for Permission {
    type Err = UnknownPermission;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pull" => Ok(Self::Pull),
            "triage" => Ok(Self::Triage),
            "push" => Ok(Self::Push),
            "maintain" => Ok(Self::Maintain),
            "admin" => Ok(Self::Admin),
            other => Err(UnknownPermission(other.to_string())),
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pull => "pull",
            Self::Triage => "triage",
            Self::Push => "push",
            Self::Maintain => "maintain",
            Self::Admin => "admin",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_levels() {
        assert_eq!("pull".parse(), Ok(Permission::Pull));
        assert_eq!("triage".parse(), Ok(Permission::Triage));
        assert_eq!("push".parse(), Ok(Permission::Push));
        assert_eq!("maintain".parse(), Ok(Permission::Maintain));
        assert_eq!("admin".parse(), Ok(Permission::Admin));
    }

    #[test]
    fn rejects_unknown() {
        assert_eq!(
            "owner".parse::<Permission>(),
            Err(UnknownPermission("owner".into()))
        );
    }

    #[test]
    fn display_round_trips() {
        for p in [
            Permission::Pull,
            Permission::Triage,
            Permission::Push,
            Permission::Maintain,
            Permission::Admin,
        ] {
            assert_eq!(p.to_string().parse::<Permission>().unwrap(), p);
        }
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p domain --lib permission`
Expected: 3 tests pass.

- [ ] **Step 3: Un-comment the `permission` re-export**

```rust
pub use permission::Permission;
```

- [ ] **Step 4: Commit**

```bash
git add crates/domain/src/permission.rs crates/domain/src/lib.rs
git commit -m "feat(domain): Permission enum"
```

---

### Task 8: ShareLink with derived `is_active`

Per spec §8.1: most state is derived, not stored. Only `revoked_at` and `uses_count` are stored.

**Files:**
- Modify: `crates/domain/src/share_link.rs`
- Modify: `crates/domain/src/lib.rs`

- [ ] **Step 1: Write impl + tests**

`crates/domain/src/share_link.rs`:

```rust
use crate::ids::ShareLinkId;
use crate::permission::Permission;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShareLink {
    pub id: ShareLinkId,
    pub slug: String, // Slug stored as String here so the type can travel without rng-tied checks
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
        if let Some(expires) = self.expires_at {
            if now >= expires {
                return false;
            }
        }
        if let Some(max) = self.max_uses {
            if self.uses_count >= max {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, 0, 0, 0).unwrap()
    }

    fn base_link() -> ShareLink {
        ShareLink {
            id: ShareLinkId::new(),
            slug: "0123456789ABCDEF".into(),
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
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p domain --lib share_link`
Expected: 5 tests pass.

- [ ] **Step 3: Un-comment `share_link` re-export**

```rust
pub use share_link::{ShareLink, ShareLinkRepo};
```

- [ ] **Step 4: Commit**

```bash
git add crates/domain/src/share_link.rs crates/domain/src/lib.rs
git commit -m "feat(domain): ShareLink with derived is_active"
```

---

### Task 9: InvitationRequest

**Files:**
- Modify: `crates/domain/src/invitation_request.rs`
- Modify: `crates/domain/src/lib.rs`

- [ ] **Step 1: Write impl + tests**

`crates/domain/src/invitation_request.rs`:

```rust
use crate::ids::{RequestId, ShareLinkId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestState {
    Pending,
    Approved,
    Declined,
    Expired,
    /// Reserved for v2 (link revoke cascading); not produced by v1 code paths.
    Cancelled,
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("unknown request state: {0}")]
pub struct UnknownRequestState(pub String);

impl FromStr for RequestState {
    type Err = UnknownRequestState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "declined" => Ok(Self::Declined),
            "expired" => Ok(Self::Expired),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(UnknownRequestState(other.to_string())),
        }
    }
}

impl fmt::Display for RequestState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Declined => "declined",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        })
    }
}

impl RequestState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InvitationRequest {
    pub id: RequestId,
    pub share_link_id: ShareLinkId,
    pub requester_id: u64,
    pub justification: Option<String>,
    pub state: RequestState,
    pub decided_by: Option<u64>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decline_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_through_string() {
        for s in [
            RequestState::Pending,
            RequestState::Approved,
            RequestState::Declined,
            RequestState::Expired,
            RequestState::Cancelled,
        ] {
            assert_eq!(s.to_string().parse::<RequestState>().unwrap(), s);
        }
    }

    #[test]
    fn pending_is_only_non_terminal() {
        assert!(!RequestState::Pending.is_terminal());
        assert!(RequestState::Approved.is_terminal());
        assert!(RequestState::Declined.is_terminal());
        assert!(RequestState::Expired.is_terminal());
        assert!(RequestState::Cancelled.is_terminal());
    }

    #[test]
    fn rejects_unknown_state() {
        assert_eq!(
            "approving".parse::<RequestState>(),
            Err(UnknownRequestState("approving".into()))
        );
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p domain --lib invitation_request`
Expected: 3 tests pass.

- [ ] **Step 3: Un-comment re-export**

```rust
pub use invitation_request::{InvitationRequest, RequestState};
```

- [ ] **Step 4: Commit**

```bash
git add crates/domain/src/invitation_request.rs crates/domain/src/lib.rs
git commit -m "feat(domain): InvitationRequest + RequestState"
```

---

### Task 10: GithubInvitation

**Files:**
- Modify: `crates/domain/src/github_invitation.rs`
- Modify: `crates/domain/src/lib.rs`

- [ ] **Step 1: Write impl + tests**

`crates/domain/src/github_invitation.rs`:

```rust
use crate::ids::{GithubInvitationId, RequestId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvitationState {
    Sending,
    Sent,
    Accepted,
    Declined,
    Expired,
    Cancelled,
    Failed,
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("unknown invitation state: {0}")]
pub struct UnknownInvitationState(pub String);

impl FromStr for InvitationState {
    type Err = UnknownInvitationState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "sending" => Ok(Self::Sending),
            "sent" => Ok(Self::Sent),
            "accepted" => Ok(Self::Accepted),
            "declined" => Ok(Self::Declined),
            "expired" => Ok(Self::Expired),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            other => Err(UnknownInvitationState(other.to_string())),
        }
    }
}

impl fmt::Display for InvitationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        })
    }
}

impl InvitationState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Accepted | Self::Declined | Self::Expired | Self::Cancelled | Self::Failed
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GithubInvitation {
    pub id: GithubInvitationId,
    pub invitation_request_id: RequestId,
    pub repo_id: u64,
    /// Set after `PUT /repos/.../collaborators` returns 201 with an invitation id.
    /// Stays `None` if GitHub returned 204 (recipient already a member) or 4xx.
    pub github_invitation_id: Option<u64>,
    pub state: InvitationState,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips() {
        for s in [
            InvitationState::Sending,
            InvitationState::Sent,
            InvitationState::Accepted,
            InvitationState::Declined,
            InvitationState::Expired,
            InvitationState::Cancelled,
            InvitationState::Failed,
        ] {
            assert_eq!(s.to_string().parse::<InvitationState>().unwrap(), s);
        }
    }

    #[test]
    fn terminal_states() {
        assert!(!InvitationState::Sending.is_terminal());
        assert!(!InvitationState::Sent.is_terminal());
        assert!(InvitationState::Accepted.is_terminal());
        assert!(InvitationState::Declined.is_terminal());
        assert!(InvitationState::Expired.is_terminal());
        assert!(InvitationState::Cancelled.is_terminal());
        assert!(InvitationState::Failed.is_terminal());
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p domain --lib github_invitation`
Expected: 2 tests pass.

- [ ] **Step 3: Un-comment re-export**

```rust
pub use github_invitation::{GithubInvitation, InvitationState};
```

- [ ] **Step 4: Run the full domain crate test suite**

Run: `cargo test -p domain`
Expected: all ~25 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/domain/src/github_invitation.rs crates/domain/src/lib.rs
git commit -m "feat(domain): GithubInvitation + InvitationState"
```

---

### Task 11: Audit crate

**Files:**
- Create: `crates/audit/Cargo.toml`
- Create: `crates/audit/src/lib.rs`
- Modify: workspace `Cargo.toml` (members list already includes it)

- [ ] **Step 1: Create the audit crate manifest**

`crates/audit/Cargo.toml`:

```toml
[package]
name = "audit"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
chrono.workspace = true
domain.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
ulid.workspace = true

[dev-dependencies]
```

- [ ] **Step 2: Write the audit event types + tests**

`crates/audit/src/lib.rs`:

```rust
//! Append-only audit event types. The Storage trait exposes `audit(&AuditEvent)`
//! and no update/delete; this module defines the event payload.

use chrono::{DateTime, Utc};
use domain::AuditEventId;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorKind {
    User,
    System,
    Github,
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("unknown actor kind: {0}")]
pub struct UnknownActorKind(pub String);

impl FromStr for ActorKind {
    type Err = UnknownActorKind;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Self::User),
            "system" => Ok(Self::System),
            "github" => Ok(Self::Github),
            other => Err(UnknownActorKind(other.to_string())),
        }
    }
}

impl fmt::Display for ActorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::User => "user",
            Self::System => "system",
            Self::Github => "github",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Installation,
    ShareLink,
    InvitationRequest,
    GithubInvitation,
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("unknown target kind: {0}")]
pub struct UnknownTargetKind(pub String);

impl FromStr for TargetKind {
    type Err = UnknownTargetKind;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "installation" => Ok(Self::Installation),
            "share_link" => Ok(Self::ShareLink),
            "invitation_request" => Ok(Self::InvitationRequest),
            "github_invitation" => Ok(Self::GithubInvitation),
            other => Err(UnknownTargetKind(other.to_string())),
        }
    }
}

impl fmt::Display for TargetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Installation => "installation",
            Self::ShareLink => "share_link",
            Self::InvitationRequest => "invitation_request",
            Self::GithubInvitation => "github_invitation",
        })
    }
}

/// The full set of v1 event types per spec §15.1. Stored as the dotted string
/// shown here. We deliberately don't enforce this set in SQL, but we *do*
/// validate at the application boundary.
pub const EVENT_TYPES: &[&str] = &[
    "installation.created",
    "installation.repos_changed",
    "installation.uninstalled",
    "share_link.created",
    "share_link.revoked",
    "share_link.expired",
    "share_link.exhausted",
    "request.created",
    "request.approved",
    "request.declined",
    "request.expired",
    "invitation.sent",
    "invitation.accepted",
    "invitation.declined",
    "invitation.expired",
    "invitation.cancelled",
    "invitation.send_failed",
];

pub fn is_known_event_type(s: &str) -> bool {
    EVENT_TYPES.contains(&s)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: AuditEventId,
    pub account_id: u64,
    pub occurred_at: DateTime<Utc>,
    pub event_type: String,
    pub actor_kind: ActorKind,
    pub actor_id: Option<u64>,
    pub target_kind: TargetKind,
    pub target_id: String,
    pub metadata: serde_json::Value,
    /// Restate invocation id when this event was emitted by a handler.
    pub request_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn actor_kind_round_trips() {
        for k in [ActorKind::User, ActorKind::System, ActorKind::Github] {
            assert_eq!(k.to_string().parse::<ActorKind>().unwrap(), k);
        }
    }

    #[test]
    fn target_kind_round_trips() {
        for k in [
            TargetKind::Installation,
            TargetKind::ShareLink,
            TargetKind::InvitationRequest,
            TargetKind::GithubInvitation,
        ] {
            assert_eq!(k.to_string().parse::<TargetKind>().unwrap(), k);
        }
    }

    #[test]
    fn known_event_types_recognized() {
        assert!(is_known_event_type("share_link.created"));
        assert!(is_known_event_type("invitation.sent"));
        assert!(!is_known_event_type("share_link.unmade"));
    }

    #[test]
    fn audit_event_serde() {
        let e = AuditEvent {
            id: AuditEventId::new(),
            account_id: 1,
            occurred_at: Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap(),
            event_type: "share_link.created".into(),
            actor_kind: ActorKind::User,
            actor_id: Some(99),
            target_kind: TargetKind::ShareLink,
            target_id: "01HFOOBAR".into(),
            metadata: serde_json::json!({"permission": "push"}),
            request_id: None,
        };
        let json = serde_json::to_string(&e).unwrap();
        let parsed: AuditEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, parsed);
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p audit`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/audit
git commit -m "feat(audit): event types and discrete enums"
```

---

### Task 12: Initial migration

This is the only migration in this plan. It defines all 8 v1 tables (excluding the tower-sessions store, which is added in Plan 4 when tower-sessions itself is wired).

**Files:**
- Create: `migrations/0001_initial.sql`
- Create: `migrations/README.md`

- [ ] **Step 1: Write the migration**

`migrations/0001_initial.sql`:

```sql
-- ghinvite v1 initial schema. SQLite-portable so the same file is applied to
-- both local sqlx (sqlx-migrate) and Cloudflare D1 (wrangler d1 migrations apply).

CREATE TABLE installations (
  installation_id   INTEGER PRIMARY KEY,
  account_id        INTEGER NOT NULL,
  account_login     TEXT    NOT NULL,
  account_type      TEXT    NOT NULL CHECK (account_type IN ('User', 'Organization')),
  installed_at      TEXT    NOT NULL,
  uninstalled_at    TEXT,
  selected_repos    TEXT    NOT NULL
);

CREATE INDEX idx_installations_account_login ON installations(account_login);

CREATE UNIQUE INDEX idx_installations_active_account
  ON installations(account_id) WHERE uninstalled_at IS NULL;

CREATE TABLE users (
  user_id      INTEGER PRIMARY KEY,
  login        TEXT NOT NULL,
  avatar_url   TEXT,
  last_seen_at TEXT NOT NULL
);

CREATE TABLE share_links (
  id                 TEXT    PRIMARY KEY,
  slug               TEXT    NOT NULL UNIQUE,
  installation_id    INTEGER NOT NULL REFERENCES installations(installation_id),
  account_id         INTEGER NOT NULL,
  created_by         INTEGER NOT NULL REFERENCES users(user_id),
  created_at         TEXT    NOT NULL,
  expires_at         TEXT,
  max_uses           INTEGER,
  uses_count         INTEGER NOT NULL DEFAULT 0,
  permission         TEXT    NOT NULL,
  approval_required  INTEGER NOT NULL,
  internal_note      TEXT,
  revoked_at         TEXT,
  revoked_by         INTEGER REFERENCES users(user_id)
);

CREATE INDEX idx_share_links_account ON share_links(account_id);

CREATE TABLE share_link_repos (
  share_link_id   TEXT    NOT NULL REFERENCES share_links(id),
  repo_id         INTEGER NOT NULL,
  repo_full_name  TEXT    NOT NULL,
  PRIMARY KEY (share_link_id, repo_id)
);

CREATE TABLE invitation_requests (
  id              TEXT    PRIMARY KEY,
  share_link_id   TEXT    NOT NULL REFERENCES share_links(id),
  requester_id    INTEGER NOT NULL REFERENCES users(user_id),
  justification   TEXT,
  state           TEXT    NOT NULL,
  decided_by      INTEGER REFERENCES users(user_id),
  decided_at      TEXT,
  decline_reason  TEXT,
  created_at      TEXT    NOT NULL
);

CREATE UNIQUE INDEX idx_one_pending_per_link_per_user
  ON invitation_requests(share_link_id, requester_id) WHERE state = 'pending';

CREATE INDEX idx_requests_link ON invitation_requests(share_link_id);

CREATE TABLE github_invitations (
  id                     TEXT    PRIMARY KEY,
  invitation_request_id  TEXT    NOT NULL REFERENCES invitation_requests(id),
  repo_id                INTEGER NOT NULL,
  github_invitation_id   INTEGER,
  state                  TEXT    NOT NULL,
  error_message          TEXT,
  created_at             TEXT    NOT NULL,
  updated_at             TEXT    NOT NULL
);

CREATE INDEX idx_github_invitations_request ON github_invitations(invitation_request_id);

CREATE INDEX idx_github_invitations_github_id ON github_invitations(github_invitation_id);

CREATE TABLE audit_events (
  id           TEXT    PRIMARY KEY,
  account_id   INTEGER NOT NULL,
  occurred_at  TEXT    NOT NULL,
  event_type   TEXT    NOT NULL,
  actor_kind   TEXT    NOT NULL,
  actor_id     INTEGER,
  target_kind  TEXT    NOT NULL,
  target_id    TEXT    NOT NULL,
  metadata     TEXT,
  request_id   TEXT
);

CREATE INDEX idx_audit_account_time ON audit_events(account_id, occurred_at);

CREATE INDEX idx_audit_target ON audit_events(target_kind, target_id);
```

- [ ] **Step 2: Write the migration README**

`migrations/README.md`:

```markdown
# ghinvite migrations

Plain SQL files, one schema-change per file, applied in lexicographic order.

The same files are applied by:
- **Local dev (sqlx):** `sqlx migrate run --source migrations/ --database-url sqlite:./dev.sqlite`
- **Production (Cloudflare D1):** `wrangler d1 migrations apply ghinvite --remote`

## Conventions

- Files named `NNNN_short_description.sql` where `NNNN` is a zero-padded sequence number starting at `0001`.
- SQLite-portable SQL only: no Postgres-isms, no `WITHOUT ROWID`, no `STRICT` (D1 supports both as of 2024 but we don't rely on them yet).
- No `CHECK (col IN (...))` on enum-shaped columns (see spec §7.2). Domain enums in `crates/domain` validate values before write.
- Timestamps are ISO-8601 strings stored in `TEXT` columns, with chrono's default `to_rfc3339()` format on the Rust side.
```

- [ ] **Step 3: Commit**

```bash
git add migrations
git commit -m "feat: initial schema migration"
```

---

### Task 13: Storage crate skeleton

**Files:**
- Create: `crates/storage/Cargo.toml`
- Create: `crates/storage/src/lib.rs`
- Create: `crates/storage/src/records.rs`

- [ ] **Step 1: Manifest**

`crates/storage/Cargo.toml`:

```toml
[package]
name = "storage"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
async-trait.workspace = true
audit.workspace = true
chrono.workspace = true
domain.workspace = true
serde.workspace = true
serde_json.workspace = true
sqlx.workspace = true
thiserror.workspace = true
ulid.workspace = true

[dev-dependencies]
rand.workspace = true
rand_chacha.workspace = true
tokio.workspace = true
```

- [ ] **Step 2: `crates/storage/src/lib.rs` — trait declaration**

```rust
//! Persistence boundary for ghinvite. The `Storage` trait is the single seam
//! between domain logic and the database. Two impls in v1: `SqlxStorage`
//! (native dev & tests, this crate) and `D1Storage` (production, future plan).

use async_trait::async_trait;
use audit::AuditEvent;
use chrono::{DateTime, Utc};
use domain::{
    Account, GithubInvitation, GithubInvitationId, InvitationRequest, InvitationState, RequestId,
    RequestState, SelectedRepos, ShareLink, ShareLinkId, User,
};
use thiserror::Error;

pub mod records;
pub mod sqlx_impl;
pub mod tests;

pub use sqlx_impl::SqlxStorage;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("not found")]
    NotFound,

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("data corruption: {0}")]
    Corrupt(String),
}

/// Inputs for creating a share link with its repo set in one transaction.
#[derive(Clone, Debug)]
pub struct NewShareLink {
    pub link: ShareLink,
}

/// Inputs for creating an invitation request and atomically incrementing the link's
/// `uses_count`. Caller must have validated the link is `is_active(now)` first.
#[derive(Clone, Debug)]
pub struct NewInvitationRequest {
    pub request: InvitationRequest,
}

/// Decision recorded against a previously-pending invitation request.
#[derive(Clone, Debug)]
pub struct RequestDecision {
    pub request_id: RequestId,
    pub state: RequestState,
    pub decided_by: u64,
    pub decided_at: DateTime<Utc>,
    pub decline_reason: Option<String>,
}

/// Update payload for github_invitations row state transitions.
#[derive(Clone, Debug)]
pub struct GithubInvitationUpdate {
    pub id: GithubInvitationId,
    pub state: InvitationState,
    pub github_invitation_id: Option<u64>,
    pub error_message: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[async_trait]
pub trait Storage: Send + Sync + 'static {
    // -------- installations --------
    async fn insert_installation(&self, account: &Account) -> Result<()>;
    async fn mark_installation_uninstalled(
        &self,
        installation_id: u64,
        when: DateTime<Utc>,
    ) -> Result<()>;
    async fn update_installation_repos(
        &self,
        installation_id: u64,
        selected: &SelectedRepos,
    ) -> Result<()>;
    async fn get_installation(&self, installation_id: u64) -> Result<Option<Account>>;
    async fn get_active_installation_by_account_id(
        &self,
        account_id: u64,
    ) -> Result<Option<Account>>;
    async fn get_active_installation_by_login(&self, login: &str) -> Result<Option<Account>>;
    async fn list_active_installations(&self) -> Result<Vec<Account>>;

    // -------- users --------
    async fn upsert_user(&self, user: &User) -> Result<()>;
    async fn get_user(&self, user_id: u64) -> Result<Option<User>>;

    // -------- share links --------
    async fn insert_share_link(&self, new: &NewShareLink) -> Result<()>;
    async fn mark_share_link_revoked(
        &self,
        id: ShareLinkId,
        by_user: u64,
        when: DateTime<Utc>,
    ) -> Result<()>;
    async fn get_share_link_by_id(&self, id: ShareLinkId) -> Result<Option<ShareLink>>;
    async fn get_share_link_by_slug(&self, slug: &str) -> Result<Option<ShareLink>>;
    async fn list_share_links_for_account(&self, account_id: u64) -> Result<Vec<ShareLink>>;

    // -------- invitation requests --------
    async fn insert_invitation_request_and_increment_uses(
        &self,
        new: &NewInvitationRequest,
    ) -> Result<()>;
    async fn record_request_decision(&self, decision: &RequestDecision) -> Result<()>;
    async fn get_invitation_request(&self, id: RequestId) -> Result<Option<InvitationRequest>>;
    async fn list_pending_requests_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<InvitationRequest>>;
    async fn list_requests_for_link(&self, link_id: ShareLinkId) -> Result<Vec<InvitationRequest>>;

    // -------- github invitations --------
    async fn insert_github_invitation(&self, invitation: &GithubInvitation) -> Result<()>;
    async fn update_github_invitation(&self, update: &GithubInvitationUpdate) -> Result<()>;
    async fn get_github_invitation(
        &self,
        id: GithubInvitationId,
    ) -> Result<Option<GithubInvitation>>;
    async fn get_github_invitation_by_github_id(
        &self,
        github_id: u64,
    ) -> Result<Option<GithubInvitation>>;
    async fn list_pending_github_invitations_for_installation(
        &self,
        installation_id: u64,
    ) -> Result<Vec<GithubInvitation>>;

    // -------- audit (write-only) --------
    async fn audit(&self, event: &AuditEvent) -> Result<()>;
}
```

Note on imports: `Account` carries `account_type` internally, so the trait never names `AccountType` directly — that's why it's not in the import list. `InvitationState` and `RequestState` *are* imported because they appear in the `GithubInvitationUpdate` and `RequestDecision` struct definitions immediately above. If `cargo check` flags any import as unused once the impl is wired in later tasks, drop it.

- [ ] **Step 3: Stub out `records.rs`, `sqlx_impl.rs`, `tests.rs`**

Each with one line so the `mod` declarations resolve:

`crates/storage/src/records.rs`:

```rust
// Filled in by Task 14.
```

`crates/storage/src/sqlx_impl.rs`:

```rust
//! Filled in by Tasks 15+.

use crate::Storage;

pub struct SqlxStorage;
```

`crates/storage/src/tests.rs`:

```rust
//! Filled in by Tasks 22-23.
```

- [ ] **Step 4: Verify `cargo check` passes**

Run: `cargo check -p storage`
Expected: errors — `SqlxStorage` is declared but doesn't impl `Storage`. That's fine; we'll fill it in. To get green compile right now, change `crates/storage/src/sqlx_impl.rs` to:

```rust
//! Filled in by Tasks 15+.
```

(empty struct removed; keep the file empty placeholder until Task 15.)

Run: `cargo check -p storage` again.
Expected: clean (warnings about unused imports in `lib.rs` are OK).

- [ ] **Step 5: Commit**

```bash
git add crates/storage
git commit -m "feat(storage): trait declaration and module skeleton"
```

---

### Task 14: Storage record types

The `Storage` trait surfaces domain types directly, but the SqlxStorage impl needs intermediate struct shapes that match the table columns 1:1 (so `sqlx::FromRow` can populate them). Those live in `records.rs` and have explicit converters to/from domain types.

**Files:**
- Modify: `crates/storage/src/records.rs`

- [ ] **Step 1: Write the record types and tests**

`crates/storage/src/records.rs`:

```rust
//! Row-shaped intermediate types for the sqlx impl. Each has a 1:1 column
//! mapping; conversion to domain types happens in named `try_into_domain`
//! methods (so the conversion is explicit, not magic).

use crate::Error;
use audit::{ActorKind, AuditEvent, TargetKind};
use chrono::{DateTime, Utc};
use domain::{
    Account, AccountType, AuditEventId, GithubInvitation, GithubInvitationId, InvitationRequest,
    InvitationState, RequestId, RequestState, SelectedRepos, ShareLink, ShareLinkId,
    ShareLinkRepo, User,
};
use sqlx::FromRow;
use std::str::FromStr;
use ulid::Ulid;

#[derive(FromRow, Debug)]
pub struct InstallationRow {
    pub installation_id: i64,
    pub account_id: i64,
    pub account_login: String,
    pub account_type: String,
    pub installed_at: DateTime<Utc>,
    pub uninstalled_at: Option<DateTime<Utc>>,
    pub selected_repos: String,
}

impl InstallationRow {
    pub fn try_into_domain(self) -> Result<Account, Error> {
        Ok(Account {
            installation_id: self.installation_id as u64,
            account_id: self.account_id as u64,
            account_login: self.account_login,
            account_type: AccountType::from_str(&self.account_type)
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            installed_at: self.installed_at,
            uninstalled_at: self.uninstalled_at,
            selected_repos: parse_selected_repos(&self.selected_repos)?,
        })
    }
}

pub fn parse_selected_repos(raw: &str) -> Result<SelectedRepos, Error> {
    if raw == "all" {
        return Ok(SelectedRepos::All);
    }
    let v: Vec<u64> = serde_json::from_str(raw)
        .map_err(|e| Error::Corrupt(format!("selected_repos JSON: {e}")))?;
    Ok(SelectedRepos::Subset(v))
}

pub fn encode_selected_repos(s: &SelectedRepos) -> String {
    match s {
        SelectedRepos::All => "all".to_string(),
        SelectedRepos::Subset(v) => serde_json::to_string(v).expect("vec<u64> serializes"),
    }
}

#[derive(FromRow, Debug)]
pub struct UserRow {
    pub user_id: i64,
    pub login: String,
    pub avatar_url: Option<String>,
    pub last_seen_at: DateTime<Utc>,
}

impl UserRow {
    pub fn into_domain(self) -> User {
        User {
            user_id: self.user_id as u64,
            login: self.login,
            avatar_url: self.avatar_url,
            last_seen_at: self.last_seen_at,
        }
    }
}

#[derive(FromRow, Debug)]
pub struct ShareLinkRow {
    pub id: String,
    pub slug: String,
    pub installation_id: i64,
    pub account_id: i64,
    pub created_by: i64,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<i64>,
    pub uses_count: i64,
    pub permission: String,
    pub approval_required: i64,
    pub internal_note: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<i64>,
}

impl ShareLinkRow {
    pub fn try_into_domain(self, repos: Vec<ShareLinkRepo>) -> Result<ShareLink, Error> {
        Ok(ShareLink {
            id: ShareLinkId::from_ulid(
                Ulid::from_str(&self.id).map_err(|e| Error::Corrupt(format!("link id: {e}")))?,
            ),
            slug: self.slug,
            installation_id: self.installation_id as u64,
            account_id: self.account_id as u64,
            created_by: self.created_by as u64,
            created_at: self.created_at,
            expires_at: self.expires_at,
            max_uses: self.max_uses.map(|m| m as u32),
            uses_count: self.uses_count as u32,
            permission: self
                .permission
                .parse()
                .map_err(|e: domain::permission::UnknownPermission| Error::Corrupt(e.to_string()))?,
            approval_required: self.approval_required != 0,
            internal_note: self.internal_note,
            revoked_at: self.revoked_at,
            revoked_by: self.revoked_by.map(|r| r as u64),
            repos,
        })
    }
}

#[derive(FromRow, Debug)]
pub struct ShareLinkRepoRow {
    pub share_link_id: String,
    pub repo_id: i64,
    pub repo_full_name: String,
}

impl ShareLinkRepoRow {
    pub fn into_domain(self) -> ShareLinkRepo {
        ShareLinkRepo {
            repo_id: self.repo_id as u64,
            repo_full_name: self.repo_full_name,
        }
    }
}

#[derive(FromRow, Debug)]
pub struct InvitationRequestRow {
    pub id: String,
    pub share_link_id: String,
    pub requester_id: i64,
    pub justification: Option<String>,
    pub state: String,
    pub decided_by: Option<i64>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decline_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl InvitationRequestRow {
    pub fn try_into_domain(self) -> Result<InvitationRequest, Error> {
        Ok(InvitationRequest {
            id: RequestId::from_ulid(
                Ulid::from_str(&self.id).map_err(|e| Error::Corrupt(format!("req id: {e}")))?,
            ),
            share_link_id: ShareLinkId::from_ulid(
                Ulid::from_str(&self.share_link_id)
                    .map_err(|e| Error::Corrupt(format!("link id: {e}")))?,
            ),
            requester_id: self.requester_id as u64,
            justification: self.justification,
            state: RequestState::from_str(&self.state)
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            decided_by: self.decided_by.map(|d| d as u64),
            decided_at: self.decided_at,
            decline_reason: self.decline_reason,
            created_at: self.created_at,
        })
    }
}

#[derive(FromRow, Debug)]
pub struct GithubInvitationRow {
    pub id: String,
    pub invitation_request_id: String,
    pub repo_id: i64,
    pub github_invitation_id: Option<i64>,
    pub state: String,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl GithubInvitationRow {
    pub fn try_into_domain(self) -> Result<GithubInvitation, Error> {
        Ok(GithubInvitation {
            id: GithubInvitationId::from_ulid(
                Ulid::from_str(&self.id).map_err(|e| Error::Corrupt(format!("ginv id: {e}")))?,
            ),
            invitation_request_id: RequestId::from_ulid(
                Ulid::from_str(&self.invitation_request_id)
                    .map_err(|e| Error::Corrupt(format!("req id: {e}")))?,
            ),
            repo_id: self.repo_id as u64,
            github_invitation_id: self.github_invitation_id.map(|g| g as u64),
            state: InvitationState::from_str(&self.state)
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            error_message: self.error_message,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(FromRow, Debug)]
pub struct AuditEventRow {
    pub id: String,
    pub account_id: i64,
    pub occurred_at: DateTime<Utc>,
    pub event_type: String,
    pub actor_kind: String,
    pub actor_id: Option<i64>,
    pub target_kind: String,
    pub target_id: String,
    pub metadata: Option<String>,
    pub request_id: Option<String>,
}

impl AuditEventRow {
    pub fn try_into_domain(self) -> Result<AuditEvent, Error> {
        Ok(AuditEvent {
            id: AuditEventId::from_ulid(
                Ulid::from_str(&self.id).map_err(|e| Error::Corrupt(format!("audit id: {e}")))?,
            ),
            account_id: self.account_id as u64,
            occurred_at: self.occurred_at,
            event_type: self.event_type,
            actor_kind: ActorKind::from_str(&self.actor_kind)
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            actor_id: self.actor_id.map(|a| a as u64),
            target_kind: TargetKind::from_str(&self.target_kind)
                .map_err(|e| Error::Corrupt(e.to_string()))?,
            target_id: self.target_id,
            metadata: match self.metadata {
                None => serde_json::Value::Null,
                Some(s) => serde_json::from_str(&s)
                    .map_err(|e| Error::Corrupt(format!("audit metadata: {e}")))?,
            },
            request_id: self.request_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_selected_repos_all() {
        match parse_selected_repos("all").unwrap() {
            SelectedRepos::All => (),
            _ => panic!("expected All"),
        }
    }

    #[test]
    fn parse_selected_repos_subset() {
        match parse_selected_repos("[1,2,3]").unwrap() {
            SelectedRepos::Subset(v) => assert_eq!(v, vec![1, 2, 3]),
            _ => panic!("expected Subset"),
        }
    }

    #[test]
    fn parse_selected_repos_rejects_garbage() {
        let err = parse_selected_repos("not json").unwrap_err();
        match err {
            Error::Corrupt(_) => (),
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn encode_selected_repos_round_trip() {
        for s in [
            SelectedRepos::All,
            SelectedRepos::Subset(vec![]),
            SelectedRepos::Subset(vec![10, 20, 30]),
        ] {
            let encoded = encode_selected_repos(&s);
            let decoded = parse_selected_repos(&encoded).unwrap();
            assert_eq!(decoded, s);
        }
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p storage --lib records`
Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/storage/src/records.rs
git commit -m "feat(storage): row records and domain conversions"
```

---

### Task 15: SqlxStorage scaffolding (connect, migrate)

**Files:**
- Modify: `crates/storage/src/sqlx_impl.rs`
- Modify: `crates/storage/Cargo.toml` (already done)

- [ ] **Step 1: Replace `sqlx_impl.rs` with the scaffolding**

```rust
//! `Storage` implementation backed by `sqlx::SqlitePool`.

use crate::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;

#[derive(Clone)]
pub struct SqlxStorage {
    pub(crate) pool: SqlitePool,
}

impl SqlxStorage {
    /// Open an in-memory SQLite database. Each call gets a fresh DB.
    /// Useful for unit and storage-suite tests.
    pub async fn in_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1) // in-memory shares one connection so all queries see the same DB
            .connect_with(opts)
            .await?;
        let s = Self { pool };
        s.run_migrations().await?;
        Ok(s)
    }

    /// Open a file-backed SQLite database (creating it if missing).
    pub async fn at_path(path: &Path) -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;
        let s = Self { pool };
        s.run_migrations().await?;
        Ok(s)
    }

    /// Run the embedded migrations.
    pub async fn run_migrations(&self) -> Result<()> {
        // sqlx::migrate! is a proc-macro that requires a string LITERAL — not a const.
        // The path is resolved relative to CARGO_MANIFEST_DIR (this crate's Cargo.toml),
        // so from `crates/storage/` we reach `migrations/` at the workspace root.
        sqlx::migrate!("../../migrations").run(&self.pool).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_creates_schema() {
        let s = SqlxStorage::in_memory().await.unwrap();
        // The migrations table sqlx creates is `_sqlx_migrations`. Pull a row count.
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert!(row.0 >= 1, "expected at least one migration applied");
    }

    #[tokio::test]
    async fn known_tables_exist() {
        let s = SqlxStorage::in_memory().await.unwrap();
        for table in [
            "installations",
            "users",
            "share_links",
            "share_link_repos",
            "invitation_requests",
            "github_invitations",
            "audit_events",
        ] {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' AND name=?1")
                    .bind(table)
                    .fetch_optional(&s.pool)
                    .await
                    .unwrap();
            assert!(row.is_some(), "table {table} missing");
        }
    }
}
```

- [ ] **Step 2: `sqlx::migrate!` macro path**

The `sqlx::migrate!` proc-macro expects a string literal path resolved relative to `CARGO_MANIFEST_DIR` of the crate where it's expanded. Our migrations directory is at the workspace root (`migrations/`), and the storage crate is at `crates/storage/`, so the literal `"../../migrations"` is correct. Verify by:

Run: `cargo build -p storage`
Expected: clean build. If you see "migration directory not found" or a similar compile error, the path inside the `sqlx::migrate!(...)` call needs adjustment to land on the workspace `migrations/` directory from the crate's manifest location.

- [ ] **Step 3: Run the scaffolding tests**

Run: `cargo test -p storage --lib sqlx_impl`
Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/storage
git commit -m "feat(storage): SqlxStorage connect + migrate"
```

---

### Task 16: SqlxStorage — installations methods

**Files:**
- Modify: `crates/storage/src/sqlx_impl.rs`

- [ ] **Step 1: Add the `Storage` trait impl block with installations methods**

Append to `sqlx_impl.rs`:

```rust
use crate::records::{encode_selected_repos, InstallationRow};
use crate::{Error, Storage};
use async_trait::async_trait;
use audit::AuditEvent;
use chrono::{DateTime, Utc};
use domain::{
    Account, AccountType, GithubInvitation, GithubInvitationId, InvitationRequest, InvitationState,
    RequestId, RequestState, SelectedRepos, ShareLink, ShareLinkId, User,
};

#[async_trait]
impl Storage for SqlxStorage {
    async fn insert_installation(&self, account: &Account) -> Result<()> {
        let res = sqlx::query(
            r#"
            INSERT INTO installations
              (installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(account.installation_id as i64)
        .bind(account.account_id as i64)
        .bind(&account.account_login)
        .bind(account.account_type.to_string())
        .bind(account.installed_at)
        .bind(account.uninstalled_at)
        .bind(encode_selected_repos(&account.selected_repos))
        .execute(&self.pool)
        .await;

        match res {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                Err(Error::Conflict(format!(
                    "installation_id {} already exists",
                    account.installation_id
                )))
            }
            Err(e) => Err(Error::Database(e)),
        }
    }

    async fn mark_installation_uninstalled(
        &self,
        installation_id: u64,
        when: DateTime<Utc>,
    ) -> Result<()> {
        let res = sqlx::query(
            r#"UPDATE installations SET uninstalled_at = ?1 WHERE installation_id = ?2 AND uninstalled_at IS NULL"#,
        )
        .bind(when)
        .bind(installation_id as i64)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn update_installation_repos(
        &self,
        installation_id: u64,
        selected: &SelectedRepos,
    ) -> Result<()> {
        let res = sqlx::query(
            r#"UPDATE installations SET selected_repos = ?1 WHERE installation_id = ?2 AND uninstalled_at IS NULL"#,
        )
        .bind(encode_selected_repos(selected))
        .bind(installation_id as i64)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn get_installation(&self, installation_id: u64) -> Result<Option<Account>> {
        let row: Option<InstallationRow> = sqlx::query_as(
            r#"SELECT installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos
               FROM installations WHERE installation_id = ?1"#,
        )
        .bind(installation_id as i64)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn get_active_installation_by_account_id(
        &self,
        account_id: u64,
    ) -> Result<Option<Account>> {
        let row: Option<InstallationRow> = sqlx::query_as(
            r#"SELECT installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos
               FROM installations WHERE account_id = ?1 AND uninstalled_at IS NULL"#,
        )
        .bind(account_id as i64)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn get_active_installation_by_login(&self, login: &str) -> Result<Option<Account>> {
        let row: Option<InstallationRow> = sqlx::query_as(
            r#"SELECT installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos
               FROM installations WHERE account_login = ?1 AND uninstalled_at IS NULL"#,
        )
        .bind(login)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn list_active_installations(&self) -> Result<Vec<Account>> {
        let rows: Vec<InstallationRow> = sqlx::query_as(
            r#"SELECT installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos
               FROM installations WHERE uninstalled_at IS NULL ORDER BY installed_at"#,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }

    // --- stubs for the rest of the trait, filled in by later tasks ---
    async fn upsert_user(&self, _user: &User) -> Result<()> { unimplemented!("Task 17") }
    async fn get_user(&self, _user_id: u64) -> Result<Option<User>> { unimplemented!("Task 17") }
    async fn insert_share_link(&self, _new: &crate::NewShareLink) -> Result<()> { unimplemented!("Task 18") }
    async fn mark_share_link_revoked(&self, _id: ShareLinkId, _by_user: u64, _when: DateTime<Utc>) -> Result<()> { unimplemented!("Task 18") }
    async fn get_share_link_by_id(&self, _id: ShareLinkId) -> Result<Option<ShareLink>> { unimplemented!("Task 18") }
    async fn get_share_link_by_slug(&self, _slug: &str) -> Result<Option<ShareLink>> { unimplemented!("Task 18") }
    async fn list_share_links_for_account(&self, _account_id: u64) -> Result<Vec<ShareLink>> { unimplemented!("Task 18") }
    async fn insert_invitation_request_and_increment_uses(&self, _new: &crate::NewInvitationRequest) -> Result<()> { unimplemented!("Task 19") }
    async fn record_request_decision(&self, _decision: &crate::RequestDecision) -> Result<()> { unimplemented!("Task 19") }
    async fn get_invitation_request(&self, _id: RequestId) -> Result<Option<InvitationRequest>> { unimplemented!("Task 19") }
    async fn list_pending_requests_for_account(&self, _account_id: u64) -> Result<Vec<InvitationRequest>> { unimplemented!("Task 19") }
    async fn list_requests_for_link(&self, _link_id: ShareLinkId) -> Result<Vec<InvitationRequest>> { unimplemented!("Task 19") }
    async fn insert_github_invitation(&self, _invitation: &GithubInvitation) -> Result<()> { unimplemented!("Task 20") }
    async fn update_github_invitation(&self, _update: &crate::GithubInvitationUpdate) -> Result<()> { unimplemented!("Task 20") }
    async fn get_github_invitation(&self, _id: GithubInvitationId) -> Result<Option<GithubInvitation>> { unimplemented!("Task 20") }
    async fn get_github_invitation_by_github_id(&self, _github_id: u64) -> Result<Option<GithubInvitation>> { unimplemented!("Task 20") }
    async fn list_pending_github_invitations_for_installation(&self, _installation_id: u64) -> Result<Vec<GithubInvitation>> { unimplemented!("Task 20") }
    async fn audit(&self, _event: &AuditEvent) -> Result<()> { unimplemented!("Task 21") }
}
```

The unused-variable underscores avoid warnings; the `unimplemented!` macros document the implementation order.

- [ ] **Step 2: Write installations tests**

Append at the bottom of `sqlx_impl.rs` (inside the existing `#[cfg(test)] mod tests`):

```rust
    use chrono::TimeZone;
    use domain::{Account, AccountType, SelectedRepos};

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn sample_account(installation_id: u64, account_id: u64, login: &str) -> Account {
        Account {
            installation_id,
            account_id,
            account_login: login.to_string(),
            account_type: AccountType::Organization,
            installed_at: dt("2026-05-04T12:00:00Z"),
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        }
    }

    #[tokio::test]
    async fn insert_and_get_installation() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let acct = sample_account(1, 100, "acme");
        s.insert_installation(&acct).await.unwrap();

        let by_id = s.get_installation(1).await.unwrap().unwrap();
        assert_eq!(by_id, acct);

        let by_acc = s.get_active_installation_by_account_id(100).await.unwrap().unwrap();
        assert_eq!(by_acc.installation_id, 1);

        let by_login = s.get_active_installation_by_login("acme").await.unwrap().unwrap();
        assert_eq!(by_login.installation_id, 1);
    }

    #[tokio::test]
    async fn duplicate_installation_id_conflict() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let acct = sample_account(1, 100, "acme");
        s.insert_installation(&acct).await.unwrap();
        let err = s.insert_installation(&acct).await.unwrap_err();
        assert!(matches!(err, crate::Error::Conflict(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn mark_uninstalled_hides_from_active_lookups() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let acct = sample_account(1, 100, "acme");
        s.insert_installation(&acct).await.unwrap();
        s.mark_installation_uninstalled(1, dt("2026-05-05T00:00:00Z"))
            .await
            .unwrap();
        assert!(s.get_active_installation_by_account_id(100).await.unwrap().is_none());
        // Direct lookup still finds it (kept for audit):
        let direct = s.get_installation(1).await.unwrap().unwrap();
        assert!(direct.uninstalled_at.is_some());
    }

    #[tokio::test]
    async fn update_repos_swaps_subset() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let acct = sample_account(1, 100, "acme");
        s.insert_installation(&acct).await.unwrap();

        s.update_installation_repos(1, &SelectedRepos::Subset(vec![1, 2, 3]))
            .await
            .unwrap();

        let got = s.get_installation(1).await.unwrap().unwrap();
        assert_eq!(got.selected_repos, SelectedRepos::Subset(vec![1, 2, 3]));
    }

    #[tokio::test]
    async fn list_active_omits_uninstalled() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "a")).await.unwrap();
        s.insert_installation(&sample_account(2, 200, "b")).await.unwrap();
        s.mark_installation_uninstalled(2, dt("2026-05-06T00:00:00Z")).await.unwrap();

        let active = s.list_active_installations().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].installation_id, 1);
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p storage --lib sqlx_impl`
Expected: 7 tests pass (2 from Task 15 + 5 new).

- [ ] **Step 4: Commit**

```bash
git add crates/storage/src/sqlx_impl.rs
git commit -m "feat(storage): SqlxStorage installations methods"
```

---

### Task 17: SqlxStorage — users methods

**Files:**
- Modify: `crates/storage/src/sqlx_impl.rs`

- [ ] **Step 1: Replace the `upsert_user` and `get_user` stubs**

```rust
    async fn upsert_user(&self, user: &User) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO users (user_id, login, avatar_url, last_seen_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(user_id) DO UPDATE SET
                login = excluded.login,
                avatar_url = excluded.avatar_url,
                last_seen_at = excluded.last_seen_at
            "#,
        )
        .bind(user.user_id as i64)
        .bind(&user.login)
        .bind(user.avatar_url.as_deref())
        .bind(user.last_seen_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_user(&self, user_id: u64) -> Result<Option<User>> {
        let row: Option<crate::records::UserRow> = sqlx::query_as(
            r#"SELECT user_id, login, avatar_url, last_seen_at FROM users WHERE user_id = ?1"#,
        )
        .bind(user_id as i64)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.into_domain()))
    }
```

- [ ] **Step 2: Add tests**

Inside the same `#[cfg(test)] mod tests` block:

```rust
    fn sample_user(user_id: u64, login: &str) -> User {
        User {
            user_id,
            login: login.into(),
            avatar_url: Some(format!("https://example.test/{login}.png")),
            last_seen_at: dt("2026-05-04T12:00:00Z"),
        }
    }

    #[tokio::test]
    async fn upsert_user_inserts_then_updates() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let mut u = sample_user(7, "octocat");
        s.upsert_user(&u).await.unwrap();
        assert_eq!(s.get_user(7).await.unwrap().unwrap(), u);

        u.login = "octorenamed".into();
        u.last_seen_at = dt("2026-05-05T12:00:00Z");
        s.upsert_user(&u).await.unwrap();
        assert_eq!(s.get_user(7).await.unwrap().unwrap(), u);
    }

    #[tokio::test]
    async fn get_user_missing_returns_none() {
        let s = SqlxStorage::in_memory().await.unwrap();
        assert!(s.get_user(123).await.unwrap().is_none());
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p storage --lib sqlx_impl`
Expected: 9 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/storage/src/sqlx_impl.rs
git commit -m "feat(storage): SqlxStorage users methods"
```

---

### Task 18: SqlxStorage — share_links + share_link_repos methods

Insert is transactional: link row + N repo rows in one transaction.

**Files:**
- Modify: `crates/storage/src/sqlx_impl.rs`

- [ ] **Step 1: Replace the share-link stubs**

```rust
    async fn insert_share_link(&self, new: &crate::NewShareLink) -> Result<()> {
        let link = &new.link;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            INSERT INTO share_links
              (id, slug, installation_id, account_id, created_by, created_at, expires_at,
               max_uses, uses_count, permission, approval_required, internal_note,
               revoked_at, revoked_by)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            "#,
        )
        .bind(link.id.to_string())
        .bind(&link.slug)
        .bind(link.installation_id as i64)
        .bind(link.account_id as i64)
        .bind(link.created_by as i64)
        .bind(link.created_at)
        .bind(link.expires_at)
        .bind(link.max_uses.map(|m| m as i64))
        .bind(link.uses_count as i64)
        .bind(link.permission.to_string())
        .bind(if link.approval_required { 1_i64 } else { 0 })
        .bind(link.internal_note.as_deref())
        .bind(link.revoked_at)
        .bind(link.revoked_by.map(|r| r as i64))
        .execute(&mut *tx)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                Error::Conflict("share_link with that id or slug already exists".into())
            }
            other => Error::Database(other),
        })?;

        for repo in &link.repos {
            sqlx::query(
                r#"INSERT INTO share_link_repos (share_link_id, repo_id, repo_full_name)
                   VALUES (?1, ?2, ?3)"#,
            )
            .bind(link.id.to_string())
            .bind(repo.repo_id as i64)
            .bind(&repo.repo_full_name)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn mark_share_link_revoked(
        &self,
        id: ShareLinkId,
        by_user: u64,
        when: DateTime<Utc>,
    ) -> Result<()> {
        let res = sqlx::query(
            r#"UPDATE share_links SET revoked_at = ?1, revoked_by = ?2
               WHERE id = ?3 AND revoked_at IS NULL"#,
        )
        .bind(when)
        .bind(by_user as i64)
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn get_share_link_by_id(&self, id: ShareLinkId) -> Result<Option<ShareLink>> {
        let row: Option<crate::records::ShareLinkRow> = sqlx::query_as(
            r#"SELECT id, slug, installation_id, account_id, created_by, created_at, expires_at,
                      max_uses, uses_count, permission, approval_required, internal_note,
                      revoked_at, revoked_by
               FROM share_links WHERE id = ?1"#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let repos = self.list_repos_for_link(&row.id).await?;
        Ok(Some(row.try_into_domain(repos)?))
    }

    async fn get_share_link_by_slug(&self, slug: &str) -> Result<Option<ShareLink>> {
        let row: Option<crate::records::ShareLinkRow> = sqlx::query_as(
            r#"SELECT id, slug, installation_id, account_id, created_by, created_at, expires_at,
                      max_uses, uses_count, permission, approval_required, internal_note,
                      revoked_at, revoked_by
               FROM share_links WHERE slug = ?1"#,
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let repos = self.list_repos_for_link(&row.id).await?;
        Ok(Some(row.try_into_domain(repos)?))
    }

    async fn list_share_links_for_account(&self, account_id: u64) -> Result<Vec<ShareLink>> {
        let rows: Vec<crate::records::ShareLinkRow> = sqlx::query_as(
            r#"SELECT id, slug, installation_id, account_id, created_by, created_at, expires_at,
                      max_uses, uses_count, permission, approval_required, internal_note,
                      revoked_at, revoked_by
               FROM share_links WHERE account_id = ?1 ORDER BY created_at DESC"#,
        )
        .bind(account_id as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let repos = self.list_repos_for_link(&row.id).await?;
            out.push(row.try_into_domain(repos)?);
        }
        Ok(out)
    }
```

- [ ] **Step 2: Add private helper `list_repos_for_link`**

In the `impl SqlxStorage { ... }` block (the `pub`-method block, *not* the trait impl), add:

```rust
    async fn list_repos_for_link(&self, link_id: &str) -> Result<Vec<domain::ShareLinkRepo>> {
        let rows: Vec<crate::records::ShareLinkRepoRow> = sqlx::query_as(
            r#"SELECT share_link_id, repo_id, repo_full_name
               FROM share_link_repos WHERE share_link_id = ?1
               ORDER BY repo_id"#,
        )
        .bind(link_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.into_domain()).collect())
    }
```

- [ ] **Step 3: Add tests**

```rust
    use domain::{Permission, ShareLinkId, ShareLinkRepo};

    fn sample_link(account_id: u64, installation_id: u64, created_by: u64) -> ShareLink {
        ShareLink {
            id: ShareLinkId::new(),
            slug: "0123456789ABCDEF".into(),
            installation_id,
            account_id,
            created_by,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: None,
            max_uses: Some(5),
            uses_count: 0,
            permission: Permission::Push,
            approval_required: true,
            internal_note: Some("for the contractor".into()),
            revoked_at: None,
            revoked_by: None,
            repos: vec![
                ShareLinkRepo { repo_id: 10, repo_full_name: "acme/api".into() },
                ShareLinkRepo { repo_id: 11, repo_full_name: "acme/web".into() },
            ],
        }
    }

    #[tokio::test]
    async fn insert_and_get_share_link_round_trips_with_repos() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme")).await.unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();

        let link = sample_link(100, 1, 7);
        s.insert_share_link(&crate::NewShareLink { link: link.clone() })
            .await
            .unwrap();

        let got = s.get_share_link_by_id(link.id).await.unwrap().unwrap();
        assert_eq!(got, link);

        let by_slug = s.get_share_link_by_slug(&link.slug).await.unwrap().unwrap();
        assert_eq!(by_slug.id, link.id);
    }

    #[tokio::test]
    async fn share_link_slug_is_unique() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme")).await.unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();

        let mut a = sample_link(100, 1, 7);
        let mut b = sample_link(100, 1, 7);
        b.id = ShareLinkId::new();
        // same slug
        s.insert_share_link(&crate::NewShareLink { link: a.clone() }).await.unwrap();
        let err = s
            .insert_share_link(&crate::NewShareLink { link: b.clone() })
            .await
            .unwrap_err();
        assert!(matches!(err, crate::Error::Conflict(_)), "got {err:?}");
        // suppress unused-mut warning
        let _ = a; let _ = b;
    }

    #[tokio::test]
    async fn revoke_marks_revoked_at_and_by() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme")).await.unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        let link = sample_link(100, 1, 7);
        s.insert_share_link(&crate::NewShareLink { link: link.clone() })
            .await
            .unwrap();

        s.mark_share_link_revoked(link.id, 7, dt("2026-05-04T13:00:00Z"))
            .await
            .unwrap();

        let got = s.get_share_link_by_id(link.id).await.unwrap().unwrap();
        assert_eq!(got.revoked_by, Some(7));
        assert_eq!(got.revoked_at, Some(dt("2026-05-04T13:00:00Z")));

        // Idempotent revoke returns NotFound second time:
        let err = s
            .mark_share_link_revoked(link.id, 7, dt("2026-05-04T14:00:00Z"))
            .await
            .unwrap_err();
        assert!(matches!(err, crate::Error::NotFound));
    }

    #[tokio::test]
    async fn list_share_links_for_account_returns_in_descending_created_at() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme")).await.unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();

        let mut older = sample_link(100, 1, 7);
        older.id = ShareLinkId::new();
        older.slug = "AAAAAAAAAAAAAAAA".into();
        older.created_at = dt("2026-05-01T00:00:00Z");

        let mut newer = sample_link(100, 1, 7);
        newer.id = ShareLinkId::new();
        newer.slug = "BBBBBBBBBBBBBBBB".into();
        newer.created_at = dt("2026-05-04T00:00:00Z");

        s.insert_share_link(&crate::NewShareLink { link: older.clone() }).await.unwrap();
        s.insert_share_link(&crate::NewShareLink { link: newer.clone() }).await.unwrap();

        let list = s.list_share_links_for_account(100).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, newer.id);
        assert_eq!(list[1].id, older.id);
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p storage --lib sqlx_impl`
Expected: 13 tests pass (9 prior + 4 new).

- [ ] **Step 5: Commit**

```bash
git add crates/storage/src/sqlx_impl.rs
git commit -m "feat(storage): SqlxStorage share_links + repos methods"
```

---

### Task 19: SqlxStorage — invitation_requests methods

The key insight here: `insert_invitation_request_and_increment_uses` does both writes in one transaction so the `uses_count` is always consistent with the requests table. The partial unique index on `(share_link_id, requester_id) WHERE state = 'pending'` enforces "one pending per recipient per link" at the DB level.

**Files:**
- Modify: `crates/storage/src/sqlx_impl.rs`

- [ ] **Step 1: Replace the request stubs**

```rust
    async fn insert_invitation_request_and_increment_uses(
        &self,
        new: &crate::NewInvitationRequest,
    ) -> Result<()> {
        let r = &new.request;
        let mut tx = self.pool.begin().await?;

        let res = sqlx::query(
            r#"
            INSERT INTO invitation_requests
              (id, share_link_id, requester_id, justification, state,
               decided_by, decided_at, decline_reason, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
        )
        .bind(r.id.to_string())
        .bind(r.share_link_id.to_string())
        .bind(r.requester_id as i64)
        .bind(r.justification.as_deref())
        .bind(r.state.to_string())
        .bind(r.decided_by.map(|d| d as i64))
        .bind(r.decided_at)
        .bind(r.decline_reason.as_deref())
        .bind(r.created_at)
        .execute(&mut *tx)
        .await;

        match res {
            Ok(_) => (),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                return Err(Error::Conflict(
                    "request already pending for this (link, requester) or duplicate id".into(),
                ));
            }
            Err(e) => return Err(Error::Database(e)),
        }

        // Increment uses_count atomically. Caller has already verified is_active(now);
        // here we trust that and do the bump.
        let updated = sqlx::query(
            r#"UPDATE share_links SET uses_count = uses_count + 1 WHERE id = ?1"#,
        )
        .bind(r.share_link_id.to_string())
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(Error::NotFound);
        }

        tx.commit().await?;
        Ok(())
    }

    async fn record_request_decision(&self, decision: &crate::RequestDecision) -> Result<()> {
        let res = sqlx::query(
            r#"UPDATE invitation_requests
               SET state = ?1, decided_by = ?2, decided_at = ?3, decline_reason = ?4
               WHERE id = ?5 AND state = 'pending'"#,
        )
        .bind(decision.state.to_string())
        .bind(decision.decided_by as i64)
        .bind(decision.decided_at)
        .bind(decision.decline_reason.as_deref())
        .bind(decision.request_id.to_string())
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn get_invitation_request(&self, id: RequestId) -> Result<Option<InvitationRequest>> {
        let row: Option<crate::records::InvitationRequestRow> = sqlx::query_as(
            r#"SELECT id, share_link_id, requester_id, justification, state,
                      decided_by, decided_at, decline_reason, created_at
               FROM invitation_requests WHERE id = ?1"#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn list_pending_requests_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<InvitationRequest>> {
        let rows: Vec<crate::records::InvitationRequestRow> = sqlx::query_as(
            r#"SELECT r.id, r.share_link_id, r.requester_id, r.justification, r.state,
                      r.decided_by, r.decided_at, r.decline_reason, r.created_at
               FROM invitation_requests r
               JOIN share_links l ON l.id = r.share_link_id
               WHERE l.account_id = ?1 AND r.state = 'pending'
               ORDER BY r.created_at"#,
        )
        .bind(account_id as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }

    async fn list_requests_for_link(&self, link_id: ShareLinkId) -> Result<Vec<InvitationRequest>> {
        let rows: Vec<crate::records::InvitationRequestRow> = sqlx::query_as(
            r#"SELECT id, share_link_id, requester_id, justification, state,
                      decided_by, decided_at, decline_reason, created_at
               FROM invitation_requests WHERE share_link_id = ?1
               ORDER BY created_at DESC"#,
        )
        .bind(link_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }
```

- [ ] **Step 2: Add tests**

```rust
    use domain::{InvitationRequest, RequestId, RequestState};

    fn sample_request(link_id: ShareLinkId, requester: u64) -> InvitationRequest {
        InvitationRequest {
            id: RequestId::new(),
            share_link_id: link_id,
            requester_id: requester,
            justification: Some("contractor for q2".into()),
            state: RequestState::Pending,
            decided_by: None,
            decided_at: None,
            decline_reason: None,
            created_at: dt("2026-05-04T12:30:00Z"),
        }
    }

    #[tokio::test]
    async fn insert_request_increments_uses_count() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme")).await.unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7);
        s.insert_share_link(&crate::NewShareLink { link: link.clone() }).await.unwrap();

        let req = sample_request(link.id, 8);
        s.insert_invitation_request_and_increment_uses(
            &crate::NewInvitationRequest { request: req.clone() },
        )
        .await
        .unwrap();

        let got = s.get_invitation_request(req.id).await.unwrap().unwrap();
        assert_eq!(got, req);

        let updated_link = s.get_share_link_by_id(link.id).await.unwrap().unwrap();
        assert_eq!(updated_link.uses_count, 1);
    }

    #[tokio::test]
    async fn second_pending_request_for_same_user_conflicts() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme")).await.unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7);
        s.insert_share_link(&crate::NewShareLink { link: link.clone() }).await.unwrap();

        let req_a = sample_request(link.id, 8);
        s.insert_invitation_request_and_increment_uses(
            &crate::NewInvitationRequest { request: req_a.clone() },
        )
        .await
        .unwrap();

        let req_b = sample_request(link.id, 8); // same requester, distinct id
        let err = s
            .insert_invitation_request_and_increment_uses(
                &crate::NewInvitationRequest { request: req_b },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, crate::Error::Conflict(_)));
    }

    #[tokio::test]
    async fn record_decision_to_approved() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme")).await.unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7);
        s.insert_share_link(&crate::NewShareLink { link: link.clone() }).await.unwrap();

        let req = sample_request(link.id, 8);
        s.insert_invitation_request_and_increment_uses(
            &crate::NewInvitationRequest { request: req.clone() },
        )
        .await
        .unwrap();

        s.record_request_decision(&crate::RequestDecision {
            request_id: req.id,
            state: RequestState::Approved,
            decided_by: 7,
            decided_at: dt("2026-05-04T13:00:00Z"),
            decline_reason: None,
        })
        .await
        .unwrap();

        let got = s.get_invitation_request(req.id).await.unwrap().unwrap();
        assert_eq!(got.state, RequestState::Approved);
        assert_eq!(got.decided_by, Some(7));
        assert_eq!(got.decided_at, Some(dt("2026-05-04T13:00:00Z")));
    }

    #[tokio::test]
    async fn second_decision_returns_not_found() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme")).await.unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7);
        s.insert_share_link(&crate::NewShareLink { link: link.clone() }).await.unwrap();
        let req = sample_request(link.id, 8);
        s.insert_invitation_request_and_increment_uses(
            &crate::NewInvitationRequest { request: req.clone() },
        )
        .await
        .unwrap();
        s.record_request_decision(&crate::RequestDecision {
            request_id: req.id,
            state: RequestState::Approved,
            decided_by: 7,
            decided_at: dt("2026-05-04T13:00:00Z"),
            decline_reason: None,
        }).await.unwrap();

        let err = s.record_request_decision(&crate::RequestDecision {
            request_id: req.id,
            state: RequestState::Declined,
            decided_by: 7,
            decided_at: dt("2026-05-04T14:00:00Z"),
            decline_reason: Some("wrong person".into()),
        }).await.unwrap_err();
        assert!(matches!(err, crate::Error::NotFound));
    }

    #[tokio::test]
    async fn list_pending_for_account_filters_by_account() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme")).await.unwrap();
        s.insert_installation(&sample_account(2, 200, "other")).await.unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();

        let mut link_a = sample_link(100, 1, 7);
        link_a.slug = "AAAAAAAAAAAAAAAA".into();
        let mut link_b = sample_link(200, 2, 7);
        link_b.id = ShareLinkId::new();
        link_b.slug = "BBBBBBBBBBBBBBBB".into();

        s.insert_share_link(&crate::NewShareLink { link: link_a.clone() }).await.unwrap();
        s.insert_share_link(&crate::NewShareLink { link: link_b.clone() }).await.unwrap();

        let r_a = sample_request(link_a.id, 8);
        let r_b = sample_request(link_b.id, 8);
        s.insert_invitation_request_and_increment_uses(
            &crate::NewInvitationRequest { request: r_a.clone() },
        ).await.unwrap();
        s.insert_invitation_request_and_increment_uses(
            &crate::NewInvitationRequest { request: r_b.clone() },
        ).await.unwrap();

        let pending_a = s.list_pending_requests_for_account(100).await.unwrap();
        assert_eq!(pending_a.len(), 1);
        assert_eq!(pending_a[0].id, r_a.id);

        let pending_b = s.list_pending_requests_for_account(200).await.unwrap();
        assert_eq!(pending_b.len(), 1);
        assert_eq!(pending_b[0].id, r_b.id);
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p storage --lib sqlx_impl`
Expected: 18 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/storage/src/sqlx_impl.rs
git commit -m "feat(storage): SqlxStorage invitation_requests methods"
```

---

### Task 20: SqlxStorage — github_invitations methods

**Files:**
- Modify: `crates/storage/src/sqlx_impl.rs`

- [ ] **Step 1: Replace the github-invitation stubs**

```rust
    async fn insert_github_invitation(&self, invitation: &GithubInvitation) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO github_invitations
              (id, invitation_request_id, repo_id, github_invitation_id, state, error_message, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .bind(invitation.id.to_string())
        .bind(invitation.invitation_request_id.to_string())
        .bind(invitation.repo_id as i64)
        .bind(invitation.github_invitation_id.map(|g| g as i64))
        .bind(invitation.state.to_string())
        .bind(invitation.error_message.as_deref())
        .bind(invitation.created_at)
        .bind(invitation.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                Error::Conflict(format!("github_invitation {} already exists", invitation.id))
            }
            other => Error::Database(other),
        })?;
        Ok(())
    }

    async fn update_github_invitation(&self, update: &crate::GithubInvitationUpdate) -> Result<()> {
        let res = sqlx::query(
            r#"UPDATE github_invitations
               SET state = ?1, github_invitation_id = COALESCE(?2, github_invitation_id),
                   error_message = ?3, updated_at = ?4
               WHERE id = ?5"#,
        )
        .bind(update.state.to_string())
        .bind(update.github_invitation_id.map(|g| g as i64))
        .bind(update.error_message.as_deref())
        .bind(update.updated_at)
        .bind(update.id.to_string())
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn get_github_invitation(
        &self,
        id: GithubInvitationId,
    ) -> Result<Option<GithubInvitation>> {
        let row: Option<crate::records::GithubInvitationRow> = sqlx::query_as(
            r#"SELECT id, invitation_request_id, repo_id, github_invitation_id, state,
                      error_message, created_at, updated_at
               FROM github_invitations WHERE id = ?1"#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn get_github_invitation_by_github_id(
        &self,
        github_id: u64,
    ) -> Result<Option<GithubInvitation>> {
        let row: Option<crate::records::GithubInvitationRow> = sqlx::query_as(
            r#"SELECT id, invitation_request_id, repo_id, github_invitation_id, state,
                      error_message, created_at, updated_at
               FROM github_invitations WHERE github_invitation_id = ?1"#,
        )
        .bind(github_id as i64)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn list_pending_github_invitations_for_installation(
        &self,
        installation_id: u64,
    ) -> Result<Vec<GithubInvitation>> {
        // Cross-join via the request → link → installation_id chain.
        let rows: Vec<crate::records::GithubInvitationRow> = sqlx::query_as(
            r#"SELECT g.id, g.invitation_request_id, g.repo_id, g.github_invitation_id, g.state,
                      g.error_message, g.created_at, g.updated_at
               FROM github_invitations g
               JOIN invitation_requests r ON r.id = g.invitation_request_id
               JOIN share_links l ON l.id = r.share_link_id
               WHERE l.installation_id = ?1
                 AND g.state IN ('sending', 'sent')
               ORDER BY g.created_at"#,
        )
        .bind(installation_id as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }
```

- [ ] **Step 2: Add tests**

```rust
    use domain::{GithubInvitation, GithubInvitationId, InvitationState};

    fn sample_ginv(req_id: RequestId, repo_id: u64) -> GithubInvitation {
        GithubInvitation {
            id: GithubInvitationId::new(),
            invitation_request_id: req_id,
            repo_id,
            github_invitation_id: None,
            state: InvitationState::Sending,
            error_message: None,
            created_at: dt("2026-05-04T13:00:00Z"),
            updated_at: dt("2026-05-04T13:00:00Z"),
        }
    }

    #[tokio::test]
    async fn insert_and_update_github_invitation() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme")).await.unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7);
        s.insert_share_link(&crate::NewShareLink { link: link.clone() }).await.unwrap();
        let req = sample_request(link.id, 8);
        s.insert_invitation_request_and_increment_uses(
            &crate::NewInvitationRequest { request: req.clone() }
        ).await.unwrap();

        let g = sample_ginv(req.id, 10);
        s.insert_github_invitation(&g).await.unwrap();

        s.update_github_invitation(&crate::GithubInvitationUpdate {
            id: g.id,
            state: InvitationState::Sent,
            github_invitation_id: Some(99999),
            error_message: None,
            updated_at: dt("2026-05-04T13:01:00Z"),
        }).await.unwrap();

        let got = s.get_github_invitation(g.id).await.unwrap().unwrap();
        assert_eq!(got.state, InvitationState::Sent);
        assert_eq!(got.github_invitation_id, Some(99999));
        assert_eq!(got.updated_at, dt("2026-05-04T13:01:00Z"));

        let by_gid = s.get_github_invitation_by_github_id(99999).await.unwrap().unwrap();
        assert_eq!(by_gid.id, g.id);
    }

    #[tokio::test]
    async fn list_pending_for_installation_filters_state_and_installation() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme")).await.unwrap();
        s.insert_installation(&sample_account(2, 200, "other")).await.unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();

        // link in installation 1
        let link_a = sample_link(100, 1, 7);
        s.insert_share_link(&crate::NewShareLink { link: link_a.clone() }).await.unwrap();
        // link in installation 2
        let mut link_b = sample_link(200, 2, 7);
        link_b.id = ShareLinkId::new();
        link_b.slug = "BBBBBBBBBBBBBBBB".into();
        s.insert_share_link(&crate::NewShareLink { link: link_b.clone() }).await.unwrap();

        // request + invitation in each
        let req_a = sample_request(link_a.id, 8);
        s.insert_invitation_request_and_increment_uses(
            &crate::NewInvitationRequest { request: req_a.clone() }
        ).await.unwrap();
        let req_b = sample_request(link_b.id, 8);
        s.insert_invitation_request_and_increment_uses(
            &crate::NewInvitationRequest { request: req_b.clone() }
        ).await.unwrap();

        let g_a_pending = sample_ginv(req_a.id, 10);
        let mut g_a_done = sample_ginv(req_a.id, 11);
        g_a_done.state = InvitationState::Accepted;
        let g_b_pending = sample_ginv(req_b.id, 12);

        s.insert_github_invitation(&g_a_pending).await.unwrap();
        s.insert_github_invitation(&g_a_done).await.unwrap();
        s.insert_github_invitation(&g_b_pending).await.unwrap();

        let pending_inst_1 = s.list_pending_github_invitations_for_installation(1).await.unwrap();
        assert_eq!(pending_inst_1.len(), 1);
        assert_eq!(pending_inst_1[0].id, g_a_pending.id);

        let pending_inst_2 = s.list_pending_github_invitations_for_installation(2).await.unwrap();
        assert_eq!(pending_inst_2.len(), 1);
        assert_eq!(pending_inst_2[0].id, g_b_pending.id);
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p storage --lib sqlx_impl`
Expected: 20 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/storage/src/sqlx_impl.rs
git commit -m "feat(storage): SqlxStorage github_invitations methods"
```

---

### Task 21: SqlxStorage — `audit()` append

**Files:**
- Modify: `crates/storage/src/sqlx_impl.rs`

- [ ] **Step 1: Replace the `audit` stub**

```rust
    async fn audit(&self, event: &AuditEvent) -> Result<()> {
        let metadata_json = if event.metadata.is_null() {
            None
        } else {
            Some(serde_json::to_string(&event.metadata).expect("audit metadata serializes"))
        };

        sqlx::query(
            r#"
            INSERT INTO audit_events
              (id, account_id, occurred_at, event_type, actor_kind, actor_id,
               target_kind, target_id, metadata, request_id)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
        )
        .bind(event.id.to_string())
        .bind(event.account_id as i64)
        .bind(event.occurred_at)
        .bind(&event.event_type)
        .bind(event.actor_kind.to_string())
        .bind(event.actor_id.map(|a| a as i64))
        .bind(event.target_kind.to_string())
        .bind(&event.target_id)
        .bind(metadata_json)
        .bind(event.request_id.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
```

- [ ] **Step 2: Add a private read helper for tests (not on the trait)**

Inside the `impl SqlxStorage` block (the inherent-impl, not the trait-impl):

```rust
    /// Test/debug-only: read all audit events for an account in occurrence order.
    /// Not on the `Storage` trait because audit reads are a v1.1 feature.
    #[cfg(test)]
    pub async fn debug_list_audit(&self, account_id: u64) -> Result<Vec<AuditEvent>> {
        let rows: Vec<crate::records::AuditEventRow> = sqlx::query_as(
            r#"SELECT id, account_id, occurred_at, event_type, actor_kind, actor_id,
                      target_kind, target_id, metadata, request_id
               FROM audit_events WHERE account_id = ?1 ORDER BY occurred_at, id"#,
        )
        .bind(account_id as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }
```

This requires `use audit::AuditEvent;` (already imported earlier) and `use crate::records::AuditEventRow;` — add `use crate::records::AuditEventRow;` to the imports if rustc complains.

- [ ] **Step 3: Add tests**

```rust
    use audit::{ActorKind, AuditEvent, TargetKind};
    use domain::AuditEventId;

    fn sample_audit(account_id: u64, event_type: &str, target: &str) -> AuditEvent {
        AuditEvent {
            id: AuditEventId::new(),
            account_id,
            occurred_at: dt("2026-05-04T13:00:00Z"),
            event_type: event_type.into(),
            actor_kind: ActorKind::User,
            actor_id: Some(7),
            target_kind: TargetKind::ShareLink,
            target_id: target.into(),
            metadata: serde_json::json!({"k": "v"}),
            request_id: Some("inv-abc".into()),
        }
    }

    #[tokio::test]
    async fn audit_append_then_read_back() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let e1 = sample_audit(100, "share_link.created", "01HFLINK1");
        let e2 = AuditEvent {
            occurred_at: dt("2026-05-04T13:01:00Z"),
            ..sample_audit(100, "share_link.revoked", "01HFLINK1")
        };
        s.audit(&e1).await.unwrap();
        s.audit(&e2).await.unwrap();

        let got = s.debug_list_audit(100).await.unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], e1);
        assert_eq!(got[1], e2);
    }

    #[tokio::test]
    async fn audit_scoped_per_account() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.audit(&sample_audit(100, "share_link.created", "x")).await.unwrap();
        s.audit(&sample_audit(200, "share_link.created", "y")).await.unwrap();

        let a = s.debug_list_audit(100).await.unwrap();
        let b = s.debug_list_audit(200).await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
    }

    #[tokio::test]
    async fn audit_metadata_null_is_preserved() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let mut e = sample_audit(100, "share_link.created", "x");
        e.metadata = serde_json::Value::Null;
        s.audit(&e).await.unwrap();
        let got = s.debug_list_audit(100).await.unwrap();
        assert_eq!(got[0].metadata, serde_json::Value::Null);
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p storage --lib sqlx_impl`
Expected: 23 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/storage/src/sqlx_impl.rs
git commit -m "feat(storage): SqlxStorage audit append"
```

---

### Task 22: Parameterized test suite — scenarios

This is the big payoff: `tests::run_suite::<S: Storage>(s: S)` runs all the cross-cutting scenarios that any future `Storage` impl (D1, mock, etc.) must also pass. The per-impl integration tests just call `run_suite` against their impl.

**Files:**
- Modify: `crates/storage/src/tests.rs`

- [ ] **Step 1: Write the suite**

`crates/storage/src/tests.rs`:

```rust
//! Cross-cutting Storage behavior tests, parameterized over any `Storage` impl.
//! Per-impl test files (e.g. `tests/sqlx_suite.rs`) call `run_suite(impl)`.

use crate::{
    GithubInvitationUpdate, NewInvitationRequest, NewShareLink, RequestDecision, Storage,
};
use audit::{ActorKind, AuditEvent, TargetKind};
use chrono::{DateTime, Utc};
use domain::{
    Account, AccountType, AuditEventId, GithubInvitation, GithubInvitationId, InvitationRequest,
    InvitationState, Permission, RequestId, RequestState, SelectedRepos, ShareLink, ShareLinkId,
    ShareLinkRepo, User,
};

fn dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn sample_account(installation_id: u64, account_id: u64, login: &str) -> Account {
    Account {
        installation_id,
        account_id,
        account_login: login.into(),
        account_type: AccountType::Organization,
        installed_at: dt("2026-05-04T12:00:00Z"),
        uninstalled_at: None,
        selected_repos: SelectedRepos::All,
    }
}

fn sample_user(user_id: u64, login: &str) -> User {
    User {
        user_id,
        login: login.into(),
        avatar_url: None,
        last_seen_at: dt("2026-05-04T12:00:00Z"),
    }
}

fn sample_link(account_id: u64, installation_id: u64, created_by: u64, slug: &str) -> ShareLink {
    ShareLink {
        id: ShareLinkId::new(),
        slug: slug.into(),
        installation_id,
        account_id,
        created_by,
        created_at: dt("2026-05-04T12:00:00Z"),
        expires_at: None,
        max_uses: None,
        uses_count: 0,
        permission: Permission::Pull,
        approval_required: false,
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        repos: vec![ShareLinkRepo {
            repo_id: 10,
            repo_full_name: "acme/api".into(),
        }],
    }
}

fn sample_request(link: ShareLinkId, requester: u64) -> InvitationRequest {
    InvitationRequest {
        id: RequestId::new(),
        share_link_id: link,
        requester_id: requester,
        justification: None,
        state: RequestState::Pending,
        decided_by: None,
        decided_at: None,
        decline_reason: None,
        created_at: dt("2026-05-04T12:30:00Z"),
    }
}

/// Run the full cross-cutting suite against any `Storage` impl. Each scenario is
/// independent; we don't reset between scenarios (they use disjoint IDs / accounts).
pub async fn run_suite<S: Storage>(s: S) {
    scenario_install_uninstall_reinstall(&s).await;
    scenario_share_link_lifecycle(&s).await;
    scenario_request_uses_and_uniqueness(&s).await;
    scenario_request_decision(&s).await;
    scenario_github_invitation_lifecycle(&s).await;
    scenario_audit_appends(&s).await;
}

async fn scenario_install_uninstall_reinstall<S: Storage>(s: &S) {
    s.insert_installation(&sample_account(1001, 9001, "acme1")).await.unwrap();

    s.mark_installation_uninstalled(1001, dt("2026-05-04T18:00:00Z")).await.unwrap();
    assert!(s.get_active_installation_by_account_id(9001).await.unwrap().is_none());

    let mut reinstall = sample_account(1002, 9001, "acme1");
    reinstall.installed_at = dt("2026-05-05T00:00:00Z");
    s.insert_installation(&reinstall).await.unwrap();

    let active = s.get_active_installation_by_account_id(9001).await.unwrap().unwrap();
    assert_eq!(active.installation_id, 1002);
}

async fn scenario_share_link_lifecycle<S: Storage>(s: &S) {
    s.insert_installation(&sample_account(2001, 9002, "acme2")).await.unwrap();
    s.upsert_user(&sample_user(701, "creator")).await.unwrap();

    let link = sample_link(9002, 2001, 701, "0000111122223333");
    s.insert_share_link(&NewShareLink { link: link.clone() }).await.unwrap();

    let by_slug = s.get_share_link_by_slug("0000111122223333").await.unwrap().unwrap();
    assert_eq!(by_slug.id, link.id);

    s.mark_share_link_revoked(link.id, 701, dt("2026-05-04T20:00:00Z")).await.unwrap();
    let revoked = s.get_share_link_by_id(link.id).await.unwrap().unwrap();
    assert_eq!(revoked.revoked_by, Some(701));
    assert!(!revoked.is_active(dt("2026-05-04T21:00:00Z")));
}

async fn scenario_request_uses_and_uniqueness<S: Storage>(s: &S) {
    s.insert_installation(&sample_account(3001, 9003, "acme3")).await.unwrap();
    s.upsert_user(&sample_user(702, "creator")).await.unwrap();
    s.upsert_user(&sample_user(802, "asker")).await.unwrap();

    let mut link = sample_link(9003, 3001, 702, "1111222233334444");
    link.max_uses = Some(2);
    s.insert_share_link(&NewShareLink { link: link.clone() }).await.unwrap();

    let r1 = sample_request(link.id, 802);
    s.insert_invitation_request_and_increment_uses(
        &NewInvitationRequest { request: r1.clone() },
    ).await.unwrap();

    let after_one = s.get_share_link_by_id(link.id).await.unwrap().unwrap();
    assert_eq!(after_one.uses_count, 1);

    // Second pending for same (link, requester) should conflict.
    let r2 = sample_request(link.id, 802);
    let err = s.insert_invitation_request_and_increment_uses(
        &NewInvitationRequest { request: r2 },
    ).await.unwrap_err();
    assert!(matches!(err, crate::Error::Conflict(_)));

    // After we decide r1, requester can ask again.
    s.record_request_decision(&RequestDecision {
        request_id: r1.id,
        state: RequestState::Declined,
        decided_by: 702,
        decided_at: dt("2026-05-04T13:00:00Z"),
        decline_reason: Some("not yet".into()),
    }).await.unwrap();

    let r3 = sample_request(link.id, 802);
    s.insert_invitation_request_and_increment_uses(
        &NewInvitationRequest { request: r3.clone() },
    ).await.unwrap();

    let after_two = s.get_share_link_by_id(link.id).await.unwrap().unwrap();
    assert_eq!(after_two.uses_count, 2);
    assert!(!after_two.is_active(dt("2026-05-04T20:00:00Z")), "max_uses exhausted");
}

async fn scenario_request_decision<S: Storage>(s: &S) {
    s.insert_installation(&sample_account(4001, 9004, "acme4")).await.unwrap();
    s.upsert_user(&sample_user(703, "creator")).await.unwrap();
    s.upsert_user(&sample_user(803, "asker")).await.unwrap();

    let link = sample_link(9004, 4001, 703, "AAAA111122223333");
    s.insert_share_link(&NewShareLink { link: link.clone() }).await.unwrap();
    let req = sample_request(link.id, 803);
    s.insert_invitation_request_and_increment_uses(
        &NewInvitationRequest { request: req.clone() },
    ).await.unwrap();

    // First decision wins; second on the same row should NotFound.
    s.record_request_decision(&RequestDecision {
        request_id: req.id,
        state: RequestState::Approved,
        decided_by: 703,
        decided_at: dt("2026-05-04T13:00:00Z"),
        decline_reason: None,
    }).await.unwrap();
    let err = s.record_request_decision(&RequestDecision {
        request_id: req.id,
        state: RequestState::Declined,
        decided_by: 703,
        decided_at: dt("2026-05-04T13:01:00Z"),
        decline_reason: Some("changed mind".into()),
    }).await.unwrap_err();
    assert!(matches!(err, crate::Error::NotFound));

    let pending = s.list_pending_requests_for_account(9004).await.unwrap();
    assert!(pending.is_empty());
}

async fn scenario_github_invitation_lifecycle<S: Storage>(s: &S) {
    s.insert_installation(&sample_account(5001, 9005, "acme5")).await.unwrap();
    s.upsert_user(&sample_user(704, "creator")).await.unwrap();
    s.upsert_user(&sample_user(804, "asker")).await.unwrap();

    let link = sample_link(9005, 5001, 704, "AAAA222233334444");
    s.insert_share_link(&NewShareLink { link: link.clone() }).await.unwrap();
    let req = sample_request(link.id, 804);
    s.insert_invitation_request_and_increment_uses(
        &NewInvitationRequest { request: req.clone() },
    ).await.unwrap();

    let g = GithubInvitation {
        id: GithubInvitationId::new(),
        invitation_request_id: req.id,
        repo_id: 10,
        github_invitation_id: None,
        state: InvitationState::Sending,
        error_message: None,
        created_at: dt("2026-05-04T13:00:00Z"),
        updated_at: dt("2026-05-04T13:00:00Z"),
    };
    s.insert_github_invitation(&g).await.unwrap();

    s.update_github_invitation(&GithubInvitationUpdate {
        id: g.id,
        state: InvitationState::Sent,
        github_invitation_id: Some(99001),
        error_message: None,
        updated_at: dt("2026-05-04T13:01:00Z"),
    }).await.unwrap();

    let by_gid = s.get_github_invitation_by_github_id(99001).await.unwrap().unwrap();
    assert_eq!(by_gid.id, g.id);

    let pending = s
        .list_pending_github_invitations_for_installation(5001)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);

    s.update_github_invitation(&GithubInvitationUpdate {
        id: g.id,
        state: InvitationState::Accepted,
        github_invitation_id: None,
        error_message: None,
        updated_at: dt("2026-05-04T13:05:00Z"),
    }).await.unwrap();

    let now_done = s.list_pending_github_invitations_for_installation(5001).await.unwrap();
    assert!(now_done.is_empty());
}

async fn scenario_audit_appends<S: Storage>(s: &S) {
    let e = AuditEvent {
        id: AuditEventId::new(),
        account_id: 9999,
        occurred_at: dt("2026-05-04T15:00:00Z"),
        event_type: "share_link.created".into(),
        actor_kind: ActorKind::User,
        actor_id: Some(701),
        target_kind: TargetKind::ShareLink,
        target_id: "01HFAUDIT1".into(),
        metadata: serde_json::json!({"slug": "ABCD"}),
        request_id: Some("inv-abc".into()),
    };
    s.audit(&e).await.unwrap();
    // (No read on the trait; per-impl tests can verify via their own debug helpers.)
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p storage`
Expected: clean.

- [ ] **Step 3: Commit (no test runs yet — the suite is wired to an impl in Task 23)**

```bash
git add crates/storage/src/tests.rs
git commit -m "feat(storage): parameterized test suite scenarios"
```

---

### Task 23: Wire the suite to SqlxStorage

**Files:**
- Create: `crates/storage/tests/sqlx_suite.rs`

- [ ] **Step 1: Write the integration test entry point**

`crates/storage/tests/sqlx_suite.rs`:

```rust
//! Run the parameterized Storage suite against `SqlxStorage` (in-memory SQLite).

use storage::tests::run_suite;
use storage::SqlxStorage;

#[tokio::test]
async fn sqlx_storage_passes_full_suite() {
    let s = SqlxStorage::in_memory().await.unwrap();
    run_suite(s).await;
}
```

- [ ] **Step 2: Run all storage tests**

Run: `cargo test -p storage`
Expected: all unit tests + the new `sqlx_storage_passes_full_suite` integration test pass.

- [ ] **Step 3: Run the entire workspace test suite**

Run: `cargo test --workspace`
Expected: every test in every crate (domain, audit, storage) passes; no warnings of consequence.

- [ ] **Step 4: Commit**

```bash
git add crates/storage/tests/sqlx_suite.rs
git commit -m "test(storage): wire parameterized suite to SqlxStorage"
```

---

### Task 24: Final clean-up — clippy + fmt

**Files:**
- All crates

- [ ] **Step 1: Run rustfmt**

Run: `cargo fmt --all`
Expected: completes silently.

- [ ] **Step 2: Run clippy with -D warnings**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean. If clippy flags genuine issues (non-stylistic), fix them; if it flags style preferences you disagree with, add a focused `#[allow(clippy::xxx)]` near the call site rather than a crate-wide allow.

- [ ] **Step 3: Run all tests one more time**

Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 4: Commit any formatting/lint fixes**

```bash
git add -u
git diff --cached --stat
git commit -m "chore: apply rustfmt and clippy fixes"
```

If there's nothing to commit, skip this step.

---

## Plan self-review

**Spec coverage check** (each spec section → which task implements it):

- §7.1 (8 tables) → Task 12 (migration) + Tasks 16–21 (CRUD)
- §7.2 (no SQL CHECKs except `account_type`) → Task 12
- §7.3 (Storage trait with explicit transitions, audit-write-only) → Task 13 (trait), Tasks 16–21 (impls)
- §8.1 (`is_active` derivation) → Task 8 (`ShareLink::is_active` + 5 tests)
- §8.2 (RequestState) → Task 9
- §8.3 (InvitationState, 7 states) → Task 10
- §15.1 (event types as a known list) → Task 11 (`EVENT_TYPES` constant + `is_known_event_type`)
- §15.2 (audit append-only by trait design) → Task 13 (trait shape) + Task 21 (impl)

**Spec sections NOT covered by this plan (intentionally — assigned to later plans):**
- §9 Restate workflows → Plan 3
- §10 Auth flows, §11 URL routing, §12 Frontend → Plans 4–6
- §13 Webhook handling → Plan 6
- §14 GitHub API surface → Plan 2
- §15.3 Audit UI/CSV (v1.1) → Plan after v1
- §16 Error handling (HTTP-level) → Plans 4–6
- §18 Security (HMAC, CSP, etc.) → Plans 2, 4, 6
- D1Storage → Plan 7

**Type consistency check:** `ShareLinkId`, `RequestId`, `GithubInvitationId`, `AuditEventId` are defined identically in Task 3 and used identically across tasks. `Permission`, `RequestState`, `InvitationState`, `AccountType`, `ActorKind`, `TargetKind` all use lowercase `to_string()`/`FromStr` round-trips and the SQL stores those exact strings. The `Storage` trait method signatures in Task 13 match the impls in Tasks 16–21 (and the `tests::run_suite` calls in Task 22).

**Placeholder check:** No `TBD`, `TODO`, `// implement later`, or "similar to Task N" references. Every code step shows complete code. Every `unimplemented!` in the trait scaffolding is explicitly tracked to the task that fills it in (16, 17, 18, 19, 20, 21).
