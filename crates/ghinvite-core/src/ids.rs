use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
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

        impl Default for $name {
            fn default() -> Self {
                Self::new()
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

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn schema_id() -> Cow<'static, str> {
                concat!(module_path!(), "::", stringify!($name)).into()
            }

            fn inline_schema() -> bool {
                true
            }

            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "pattern": "^[0-9A-HJKMNP-TV-Za-hjkmnp-tv-z]{26}$"
                })
            }
        }
    };
}

ulid_newtype!(InvitationLinkId);
ulid_newtype!(RequestId);
ulid_newtype!(GithubInvitationId);
ulid_newtype!(AuditEventId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_roundtrip_through_string() {
        let id = InvitationLinkId::new();
        let s = id.to_string();
        let parsed: InvitationLinkId = s.parse().expect("roundtrip");
        assert_eq!(id, parsed);
    }

    #[test]
    fn distinct_id_types_are_not_interchangeable() {
        // This is a compile-time guarantee; we just check the types differ at runtime.
        let _link: InvitationLinkId = InvitationLinkId::new();
        let _req: RequestId = RequestId::new();
        // Uncommenting this line should cause a compile error:
        // let _: InvitationLinkId = _req;
    }

    #[test]
    fn serde_transparent_to_string() {
        let id = RequestId::new();
        let json = serde_json::to_string(&id).unwrap();
        // Ulid serializes as a 26-char base32 string.
        assert!(json.starts_with('"') && json.ends_with('"'));
        assert_eq!(json.trim_matches('"').len(), 26);
    }

    #[test]
    fn id_schema_matches_ulid_string_wire_format() {
        let schema = schemars::schema_for!(InvitationLinkId).to_value();
        assert_eq!(
            schema.get("type").and_then(serde_json::Value::as_str),
            Some("string")
        );
        assert_eq!(
            schema.get("pattern").and_then(serde_json::Value::as_str),
            Some("^[0-9A-HJKMNP-TV-Za-hjkmnp-tv-z]{26}$")
        );
    }
}
