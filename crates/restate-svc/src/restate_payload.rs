//! Helpers for plugging Rust types into Restate's I/O framing without going
//! through the `Json<T>` newtype wrapper (which would force a
//! `schemars::JsonSchema` derive — extra dep we don't otherwise need).
//!
//! Use [`impl_restate_json_payload!`] on each input/output type used by a
//! handler signature. The macro implements `restate_sdk::serde::{Serialize,
//! Deserialize, PayloadMetadata}` over the type's existing `serde` derives,
//! shipping payloads as `application/json`.

/// Implement Restate's framing traits for a type that already derives
/// `serde::{Serialize, Deserialize}`. Use at module scope alongside the type
/// definition.
///
/// The implementation ships payloads as `application/json` and stubs out
/// `json_schema()` with an empty object — Restate UI/discovery still works,
/// just without rich field-level types. If/when a handler wants richer
/// schemas, switch to `schemars::JsonSchema` + `restate_sdk::serde::Json<T>`.
#[macro_export]
macro_rules! impl_restate_json_payload {
    ($t:ty) => {
        impl ::restate_sdk::serde::Serialize for $t {
            type Error = ::serde_json::Error;
            fn serialize(&self) -> ::std::result::Result<::bytes::Bytes, Self::Error> {
                ::serde_json::to_vec(self).map(::bytes::Bytes::from)
            }
        }
        impl ::restate_sdk::serde::Deserialize for $t {
            type Error = ::serde_json::Error;
            fn deserialize(bytes: &mut ::bytes::Bytes) -> ::std::result::Result<Self, Self::Error> {
                ::serde_json::from_slice(bytes)
            }
        }
        impl ::restate_sdk::serde::PayloadMetadata for $t {
            fn json_schema() -> ::std::option::Option<::serde_json::Value> {
                ::std::option::Option::Some(::serde_json::json!({}))
            }
            fn input_metadata() -> ::restate_sdk::serde::InputMetadata {
                ::restate_sdk::serde::InputMetadata::default()
            }
            fn output_metadata() -> ::restate_sdk::serde::OutputMetadata {
                ::restate_sdk::serde::OutputMetadata::default()
            }
        }
    };
}
