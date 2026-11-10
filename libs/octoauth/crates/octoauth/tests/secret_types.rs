//! Secret types stay secret at compile time: no way to serialize, print or
//! copy them by accident. Adding such an impl (e.g. a `#[derive(Serialize)]`)
//! fails this file's build. Their redacted `Debug` output is covered by the
//! crate's unit tests.

use octoauth::{AppCredential, InstallationToken, SigningKey, TokenParts};
use static_assertions::{assert_impl_all, assert_not_impl_any};
use std::fmt::Display;

// The installation token: shareable (`Clone` shares one zeroized secret), but
// never serialized (journals, state, call results; ADR 0003) or displayed.
assert_not_impl_any!(InstallationToken: serde::Serialize, serde::de::DeserializeOwned, Display);
assert_impl_all!(InstallationToken: Clone, Send, Sync, std::fmt::Debug);

// The app's private key: one owner, never copied, serialized or displayed.
assert_not_impl_any!(SigningKey: Clone, serde::Serialize, serde::de::DeserializeOwned, Display);
assert_impl_all!(SigningKey: Send, Sync, std::fmt::Debug);

// The app's signed credential: lent per request, never copied, serialized or
// displayed.
assert_not_impl_any!(AppCredential: Clone, serde::Serialize, serde::de::DeserializeOwned, Display);
assert_impl_all!(AppCredential: Send, Sync, std::fmt::Debug);

// The parts of a token being rebuilt carry the secret in a public field, so
// they move once into `InstallationToken::from_parts` and are never copied.
assert_not_impl_any!(TokenParts: Clone, serde::Serialize, serde::de::DeserializeOwned, Display);

#[test]
fn secret_types_are_checked_at_compile_time() {}
