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

/// Logical application identity is stable across independently configured numeric App IDs.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum App {
    Repo,
    Org,
}
impl App {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Org => "org",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Installation {
    pub app: App,
    /// Monotonic authoritative projection sequence; never a wall-clock timestamp.
    pub revision: u64,
    pub account: Account,
    pub suspended_at: Option<DateTime<Utc>>,
    pub permissions: std::collections::BTreeMap<String, InstallationPermission>,
    pub installed_by_user_id: Option<u64>,
    pub installed_by_login: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstallationPermission {
    Read,
    Write,
}
impl Installation {
    pub fn repo(account: Account) -> Self {
        Self {
            app: App::Repo,
            revision: 1,
            account,
            suspended_at: None,
            permissions: Default::default(),
            installed_by_user_id: None,
            installed_by_login: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountSettings {
    pub account_id: u64,
    pub revision: u64,
    pub repo_admin_delegation: bool,
    pub changed_by: u64,
    pub changed_at: DateTime<Utc>,
}

/// Verified app-JWT facts deliberately have no installing-owner field. A setup
/// callback proves installation identity, never who installed the app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstallationFacts {
    pub app: App,
    pub account: Account,
    pub suspended_at: Option<DateTime<Utc>>,
    pub permissions: std::collections::BTreeMap<String, InstallationPermission>,
}
impl InstallationFacts {
    pub fn into_snapshot(self, revision: u64) -> Installation {
        Installation {
            app: self.app,
            revision,
            account: self.account,
            suspended_at: self.suspended_at,
            permissions: self.permissions,
            installed_by_user_id: None,
            installed_by_login: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OnboardInput {
    pub operation_id: crate::admission::AdmissionOperationId,
    pub verified: InstallationFacts,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstallationWebhookAction {
    Created,
    NewPermissionsAccepted,
    Deleted,
    Suspend,
    Unsuspend,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstallationWebhookInput {
    pub delivery_id: String,
    pub updated_at: DateTime<Utc>,
    pub action: InstallationWebhookAction,
    pub verified: InstallationFacts,
    pub sender_id: u64,
    pub sender_login: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetDelegationInput {
    pub operation_id: crate::admission::AdmissionOperationId,
    pub actor_user_id: u64,
    pub on: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DelegationRefusal {
    NotOwner,
    OrgAppRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstallationRef {
    pub app: App,
    pub installation_id: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountInstallationRef {
    pub app: App,
    pub account_id: u64,
}
fn app_key(app: App, id: u64) -> Result<String, &'static str> {
    if id == 0 || id > crate::storage::integer::MAX_EXACT {
        return Err("invalid app identity");
    }
    Ok(format!("{}:{id}", app.as_str()))
}
fn parse_app_key(value: &str) -> Result<(App, u64), &'static str> {
    let (app, id) = value.split_once(':').ok_or("invalid app identity")?;
    let app = match app {
        "repo" => App::Repo,
        "org" => App::Org,
        _ => return Err("invalid app identity"),
    };
    let id = id.parse::<u64>().map_err(|_| "invalid app identity")?;
    if app_key(app, id)? != value {
        return Err("invalid app identity");
    }
    Ok((app, id))
}
impl InstallationRef {
    pub fn key(self) -> Result<String, &'static str> {
        app_key(self.app, self.installation_id)
    }
}
impl AccountInstallationRef {
    pub fn key(self) -> Result<String, &'static str> {
        app_key(self.app, self.account_id)
    }
}
impl FromStr for InstallationRef {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (app, installation_id) = parse_app_key(value)?;
        Ok(Self {
            app,
            installation_id,
        })
    }
}
impl FromStr for AccountInstallationRef {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (app, account_id) = parse_app_key(value)?;
        Ok(Self { app, account_id })
    }
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
    fn app_keys_are_canonical_and_scoped() {
        for (raw, app, id) in [
            ("repo:123", App::Repo, 123),
            ("org:9007199254740991", App::Org, 9007199254740991),
        ] {
            let installation: InstallationRef = raw.parse().unwrap();
            let account: AccountInstallationRef = raw.parse().unwrap();
            assert_eq!(
                installation,
                InstallationRef {
                    app,
                    installation_id: id
                }
            );
            assert_eq!(
                account,
                AccountInstallationRef {
                    app,
                    account_id: id
                }
            );
            assert_eq!(installation.key().unwrap(), raw);
            assert_eq!(account.key().unwrap(), raw);
        }
        for raw in [
            "123",
            "other:123",
            "repo:0",
            "repo:01",
            "repo:+1",
            "repo:1:2",
            "org:9007199254740992",
        ] {
            assert!(raw.parse::<InstallationRef>().is_err());
            assert!(raw.parse::<AccountInstallationRef>().is_err());
        }
    }

    #[test]
    fn verified_facts_cannot_claim_installing_owner() {
        let value = serde_json::json!({"app":"repo","account":{
            "installation_id":1,"account_id":2,"account_login":"owner","account_type":"User",
            "installed_at":"2026-05-01T00:00:00Z","uninstalled_at":null,"selected_repos":"all"
        },"suspended_at":null,"permissions":{}});
        let facts: InstallationFacts = serde_json::from_value(value.clone()).unwrap();
        assert!(facts.into_snapshot(1).installed_by_user_id.is_none());
        for field in ["installed_by_user_id", "installed_by_login"] {
            let mut forged = value.clone();
            forged[field] = serde_json::json!(123);
            assert!(serde_json::from_value::<InstallationFacts>(forged).is_err());
        }
    }

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
