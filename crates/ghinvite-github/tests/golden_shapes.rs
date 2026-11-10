//! The serde shapes ghinvite persists must be byte-identical to gh-toolkit's
//! (PD-9). GM-00's golden fixtures were written by gh-toolkit's types; each
//! value is decoded here with this crate's type and encoded back.
use ghinvite_github::{
    endpoints::{
        authority::{Membership, RepositoryCursor, RepositoryIdentity, RepositoryPage},
        authority_teams::{TeamCursor, TeamIdentity, TeamPage, TeamSelection},
    },
    payloads::{GhAppInstallation, GhInstallationPermission, GhRepositorySelection, GhUser},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::path::PathBuf;

fn fixture(relative: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap()
}

/// Decode `value` as `T` and encode it again.
fn roundtrip<T: Serialize + DeserializeOwned>(value: &Value) -> Value {
    let decoded: T = serde_json::from_value(value.clone())
        .unwrap_or_else(|error| panic!("decode {}: {error}", std::any::type_name::<T>()));
    serde_json::to_value(decoded).unwrap()
}

#[track_caller]
fn assert_identical<T: Serialize + DeserializeOwned>(value: &Value) {
    assert_eq!(
        &roundtrip::<T>(value),
        value,
        "{}",
        std::any::type_name::<T>()
    );
}

/// The authority-cache entry under `key`; its value sits in `value`.
fn entry<'a>(entries: &'a Value, key: &str) -> &'a Value {
    &entries
        .get(key)
        .unwrap_or_else(|| panic!("fixture entry {key}"))["value"]
}

#[test]
fn console_authority_cache_entries_keep_their_shapes() {
    let entries = fixture("../ghinvite-web/golden/authority_cache_console.json");
    for key in ["authz:org:2:0:teams-0", "authz:org:2:0:teams-1"] {
        assert_identical::<TeamPage>(entry(&entries, key));
    }
    assert_identical::<TeamIdentity>(entry(&entries, "authz:org:2:0:team-101"));
    assert_identical::<Membership>(entry(&entries, "authz:org:2:8:membership"));
    for key in ["authz:repo:1:0:page-0", "authz:repo:1:0:page-1"] {
        assert_identical::<RepositoryPage>(entry(&entries, key));
    }
    // Written as a `json!({"id","login"})` literal and read back as `GhUser`
    // (GM-00 finding 4): the optional fields must default.
    let identity = entry(&entries, "authz:org:2:0:identity");
    assert_eq!(identity, &json!({"id":100,"login":"acme"}));
    let user: GhUser = serde_json::from_value(identity.clone()).unwrap();
    assert_eq!((user.id, user.login.as_str()), (100, "acme"));
    assert_eq!((user.avatar_url, user.account_type), (None, None));
    // gh-toolkit encodes the absent fields as nulls; so does this crate.
    assert_eq!(
        roundtrip::<GhUser>(identity),
        json!({"id":100,"login":"acme","avatar_url":null,"type":null})
    );
}

#[test]
fn preview_authority_cache_entries_keep_their_shapes() {
    let organization = fixture("../ghinvite-web/golden/authority_cache_preview_organization.json");
    let repository = fixture("../ghinvite-web/golden/authority_cache_preview_repository.json");
    let mut seen = [0usize; 4];
    for entries in [&organization, &repository] {
        for (key, entry) in entries.as_object().unwrap() {
            // Bulk-preview entries wrap their value once more.
            let value = if key.starts_with("authz:bulk:") {
                &entry["value"]["value"]
            } else {
                &entry["value"]
            };
            if value.get("kind").is_some() {
                continue; // ghinvite's own membership/role facts
            }
            if value.get("organization").is_some() && value.get("slug").is_some() {
                assert_identical::<TeamIdentity>(value);
                seen[0] += 1;
            } else if value.get("organization").is_some() {
                assert_identical::<Membership>(value);
                seen[1] += 1;
            } else if value.get("full_name").is_some() {
                assert_identical::<RepositoryIdentity>(value);
                seen[2] += 1;
            } else if value.get("created_at").is_some() {
                assert_identical::<GhUser>(value);
                seen[3] += 1;
            } else {
                // The org identity literal, as in the console cache.
                assert_eq!(value, &json!({"id":100,"login":"acme"}));
                serde_json::from_value::<GhUser>(value.clone()).unwrap();
            }
        }
    }
    assert_eq!(seen, [1, 1, 1, 2], "every shape is covered");
}

/// Cursors are newtypes over relative paths and serialize as the bare string
/// (GM-00 finding 3). They are re-validated before any request; see
/// `authority_page_cursor_never_changes_endpoint_and_membership_is_installation_only`.
#[test]
fn cursors_are_bare_relative_path_strings() {
    let entries = fixture("../ghinvite-web/golden/authority_cache_console.json");
    let teams = entry(&entries, "authz:org:2:0:teams-0")["next"].clone();
    assert_eq!(teams, json!("/orgs/acme/teams?per_page=100&page=2"));
    assert_identical::<TeamCursor>(&teams);
    let repos = entry(&entries, "authz:repo:1:0:page-0")["next"].clone();
    assert_eq!(
        repos,
        json!("/installation/repositories?per_page=100&page=2")
    );
    assert_identical::<RepositoryCursor>(&repos);
    assert_identical::<Option<TeamCursor>>(&Value::Null);
    let selection: TeamSelection =
        serde_json::from_value(json!({"id":1,"slug":"s","name":"n"})).unwrap();
    assert_eq!(selection.ldap_dn, None);
}

#[test]
fn verify_onboard_identity_journal_keeps_its_shape() {
    let present = fixture("../ghinvite-workflows/golden/verify_onboard_identity.json");
    assert_identical::<GhAppInstallation>(&present);
    assert_identical::<Option<GhAppInstallation>>(&present);
    let absent = fixture("../ghinvite-workflows/golden/verify_onboard_identity_absent.json");
    assert_eq!(absent, Value::Null);
    assert_identical::<Option<GhAppInstallation>>(&absent);
    let installation: GhAppInstallation = serde_json::from_value(present).unwrap();
    assert_eq!(
        installation.repository_selection,
        GhRepositorySelection::Selected
    );
    assert_eq!(
        installation.permissions["administration"],
        GhInstallationPermission::Write
    );
    assert_eq!(
        installation.permissions["members"],
        GhInstallationPermission::Read
    );
    assert_eq!(
        installation.account.account_type.as_deref(),
        Some("Organization")
    );
    assert!(installation.suspended_at.is_some());
}

#[test]
fn enum_spellings_are_snake_case() {
    for (selection, wire) in [
        (GhRepositorySelection::All, "all"),
        (GhRepositorySelection::Selected, "selected"),
    ] {
        assert_eq!(serde_json::to_value(selection).unwrap(), json!(wire));
    }
    for (permission, wire) in [
        (GhInstallationPermission::Read, "read"),
        (GhInstallationPermission::Write, "write"),
    ] {
        assert_eq!(serde_json::to_value(permission).unwrap(), json!(wire));
    }
}
