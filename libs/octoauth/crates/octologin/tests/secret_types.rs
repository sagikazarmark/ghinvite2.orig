//! Secret-bearing types stay secret at compile time: adding a way to
//! serialize, print or copy them fails this file's build.

use octologin::{AuthorizeUrl, PendingAuthorization, SignIn, UserToken};
use static_assertions::assert_not_impl_any;
use std::fmt::Display;

// The user's token: one owner, revoked by value.
assert_not_impl_any!(UserToken: Clone, serde::Serialize, serde::de::DeserializeOwned, Display);
// The PKCE verifier and state: kept server side, consumed by `verify`.
assert_not_impl_any!(PendingAuthorization: Clone, serde::Serialize, serde::de::DeserializeOwned, Display);
// Carries the state and PKCE challenge.
assert_not_impl_any!(AuthorizeUrl: Clone, serde::Serialize, Display);
// Holds the client secret.
assert_not_impl_any!(SignIn: Clone, serde::Serialize, Display);

#[test]
fn secret_types_are_checked_at_compile_time() {}
