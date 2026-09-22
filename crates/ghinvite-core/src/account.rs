use chrono::{DateTime, Utc};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

/// GitHub account type. The two variants are GitHub-defined and stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
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
///
/// Wire format (matches [`crate::storage::installations::encode_selected_repos`]):
/// - `All` ⇒ JSON string `"all"`
/// - `Subset(v)` ⇒ JSON array of integers
#[derive(Clone, Debug, PartialEq)]
pub enum SelectedRepos {
    All,
    Subset(Vec<u64>),
}

impl Serialize for SelectedRepos {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::All => s.serialize_str("all"),
            Self::Subset(v) => v.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for SelectedRepos {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            S(String),
            V(Vec<u64>),
        }
        match Repr::deserialize(d)? {
            Repr::S(s) if s == "all" => Ok(Self::All),
            Repr::S(other) => Err(D::Error::custom(format!(
                "selected_repos string must be \"all\", got {other:?}"
            ))),
            Repr::V(v) => Ok(Self::Subset(v)),
        }
    }
}

impl JsonSchema for SelectedRepos {
    fn schema_name() -> Cow<'static, str> {
        "SelectedRepos".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::SelectedRepos").into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "oneOf": [
                {
                    "type": "string",
                    "const": "all"
                },
                {
                    "type": "array",
                    "items": {
                        "type": "integer",
                        "format": "uint64",
                        "minimum": 0
                    }
                }
            ]
        })
    }
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

    #[test]
    fn selected_repos_serde_all_is_string() {
        let json = serde_json::to_string(&SelectedRepos::All).unwrap();
        assert_eq!(json, "\"all\"");
        let back: SelectedRepos = serde_json::from_str(&json).unwrap();
        assert_eq!(back, SelectedRepos::All);
    }

    #[test]
    fn selected_repos_serde_subset_is_array() {
        let s = SelectedRepos::Subset(vec![10, 20, 30]);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "[10,20,30]");
        let back: SelectedRepos = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn selected_repos_serde_rejects_other_strings() {
        let err = serde_json::from_str::<SelectedRepos>("\"none\"").unwrap_err();
        assert!(err.to_string().contains("selected_repos"));
    }

    #[test]
    fn selected_repos_schema_matches_wire_format() {
        let schema = schemars::schema_for!(SelectedRepos).to_value();
        let variants = schema
            .get("oneOf")
            .and_then(serde_json::Value::as_array)
            .expect("SelectedRepos schema should use oneOf");

        assert!(variants.iter().any(|variant| {
            variant.get("type") == Some(&serde_json::json!("string"))
                && variant.get("const") == Some(&serde_json::json!("all"))
        }));
        assert!(variants.iter().any(|variant| {
            variant.get("type") == Some(&serde_json::json!("array"))
                && variant.pointer("/items/type") == Some(&serde_json::json!("integer"))
                && variant.pointer("/items/format") == Some(&serde_json::json!("uint64"))
        }));
    }
}
