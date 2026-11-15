//! AC-APP-1 and the inactive P1 app deployment configuration contract.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ghinvite_web::config::apps::{
    AppStage, InstalledApp, InstalledSlot, parse_installed_web, parse_mint_apps, parse_web_apps,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn values() -> BTreeMap<&'static str, String> {
    let key = format!(
        "first:{};previous:{}",
        STANDARD.encode(include_bytes!("../../../test-fixtures/github-app-rsa.pem")),
        STANDARD.encode(include_bytes!("../../../test-fixtures/github-app-rsa.pem"))
    );
    BTreeMap::from([
        ("P1_HOST", "p1.example.com".into()),
        ("GH_SIGNIN_CLIENT_ID", "Iv.signin".into()),
        ("GH_SIGNIN_CLIENT_SECRET", "SIGNIN_SECRET_MARKER".into()),
        ("GH_REPO_APP_ID", "123".into()),
        ("GH_REPO_CLIENT_ID", "Iv.repo".into()),
        ("GH_REPO_PRIVATE_KEYS", key.clone()),
        (
            "GH_REPO_WEBHOOK_SECRETS",
            "REPO_SECRET_NEW;REPO_SECRET_OLD".into(),
        ),
        ("GH_ORG_APP_ID", "456".into()),
        ("GH_ORG_CLIENT_ID", "Iv.org".into()),
        ("GH_ORG_PRIVATE_KEYS", key),
        (
            "GH_ORG_WEBHOOK_SECRETS",
            "ORG_SECRET_NEW;ORG_SECRET_OLD".into(),
        ),
    ])
}
/// Each installed app's slot, App ID and user agent.
fn summary(apps: &[InstalledApp]) -> Vec<(InstalledSlot, u64, String)> {
    apps.iter()
        .map(|app| (app.slot, app.app.id().get(), app.user_agent.clone()))
        .collect()
}

fn both() -> Vec<(InstalledSlot, u64, String)> {
    vec![
        (InstalledSlot::Repo, 123, "p1/repo".into()),
        (InstalledSlot::Org, 456, "p1/org".into()),
    ]
}

#[test]
fn ac_app_1_manifests_have_exact_permissions_events_and_routes() {
    let cases = [
        (
            "signin",
            include_str!("../../../deploy/github-apps/signin.manifest.json"),
            json!({"emails":"read"}),
            json!([]),
        ),
        (
            "repo",
            include_str!("../../../deploy/github-apps/repo.manifest.json"),
            json!({"administration":"write","metadata":"read"}),
            json!([]),
        ),
        (
            "org",
            include_str!("../../../deploy/github-apps/org.manifest.json"),
            json!({"members":"write","metadata":"read"}),
            json!(["organization", "membership"]),
        ),
    ];
    for (slot, source, permissions, events) in cases {
        let m: Value = serde_json::from_str(source).unwrap();
        assert_eq!(m["default_permissions"], permissions, "{slot}");
        assert_eq!(m["default_events"], events, "{slot}");
        assert_eq!(m["public"], true);
        assert_eq!(m["request_oauth_on_install"], false);
        assert_eq!(m["url"], "https://p1.example.com");
        assert!(m.get("client_secret").is_none());
        assert!(m.get("private_key").is_none());
        if slot == "signin" {
            assert_eq!(m["hook_attributes"], json!({"active":false}));
            assert_eq!(
                m["callback_urls"],
                json!(["https://p1.example.com/oauth/callback"])
            );
            assert!(m.get("setup_url").is_none());
        } else {
            assert_eq!(
                m["hook_attributes"],
                json!({"active":true,"url":format!("https://p1.example.com/webhooks/{slot}")})
            );
            assert_eq!(
                m["setup_url"],
                format!("https://p1.example.com/setup/{slot}")
            );
            assert_eq!(m["setup_on_update"], true);
            assert!(m.get("callback_urls").is_none());
        }
    }
}

#[test]
fn web_apps_are_purpose_separated_without_signin_numeric_id() {
    let input = values();
    let cfg = parse_web_apps(AppStage::RepoAndOrg, |key| input.get(key).cloned()).unwrap();
    // The sign-in app is OAuth only: no App ID, key or webhook.
    assert_eq!(summary(&cfg.installed), both());
    assert_eq!(
        cfg.webhooks.slots().collect::<Vec<_>>(),
        [InstalledSlot::Repo, InstalledSlot::Org]
    );
    for (slot, id) in [(InstalledSlot::Repo, 123), (InstalledSlot::Org, 456)] {
        let webhook = cfg.webhooks.get(slot).unwrap();
        assert_eq!(webhook.app_id, id);
        assert_eq!(webhook.secrets.len(), 2);
    }
    assert_eq!(cfg.signin.client_id, "Iv.signin");
    assert_eq!(
        cfg.signin.redirect_uri.as_str(),
        "https://p1.example.com/oauth/callback"
    );
    assert!(cfg.signin.web_base_url.is_none() && cfg.signin.api_base_url.is_none());
    ghinvite_web::signin::SignInService::new(&cfg.signin).unwrap();
    let diagnostics = format!("{cfg:?}");
    for marker in [
        "SIGNIN_SECRET_MARKER",
        "REPO_SECRET_NEW",
        "ORG_SECRET_NEW",
        "BEGIN PRIVATE KEY",
        "MIIEvg",
    ] {
        assert!(!diagnostics.contains(marker));
    }
}

#[test]
fn mint_apps_never_request_web_only_credentials() {
    let input = values();
    let mut requested = BTreeSet::new();
    let cfg = parse_mint_apps(AppStage::RepoAndOrg, |key| {
        assert!(!key.contains("SIGNIN") && !key.contains("WEBHOOK") && key != "P1_KEYRING");
        requested.insert(key);
        input.get(key).cloned()
    })
    .unwrap();
    assert_eq!(
        requested,
        BTreeSet::from([
            "P1_HOST",
            "GH_REPO_APP_ID",
            "GH_REPO_CLIENT_ID",
            "GH_REPO_PRIVATE_KEYS",
            "GH_ORG_APP_ID",
            "GH_ORG_CLIENT_ID",
            "GH_ORG_PRIVATE_KEYS"
        ])
    );
    assert_eq!(summary(&cfg.installed), both());
    assert_eq!(cfg.base_url.as_str(), "https://p1.example.com/");
}

#[test]
fn org_stage_requires_all_fields_and_repo_stage_does_not_read_them() {
    let mut input = values();
    input.retain(|key, _| !key.starts_with("GH_ORG"));
    let cfg = parse_web_apps(AppStage::Repo, |key| {
        assert!(!key.starts_with("GH_ORG"));
        input.get(key).cloned()
    })
    .unwrap();
    assert_eq!(summary(&cfg.installed), both()[..1]);
    assert!(cfg.webhooks.get(InstalledSlot::Org).is_none());
    assert_eq!(
        parse_web_apps(AppStage::RepoAndOrg, |key| input.get(key).cloned())
            .unwrap_err()
            .field,
        "GH_ORG_APP_ID"
    );
    for missing in values().keys() {
        let mut input = values();
        input.remove(missing);
        assert_eq!(
            parse_web_apps(AppStage::RepoAndOrg, |key| input.get(key).cloned())
                .unwrap_err()
                .field,
            *missing
        );
    }
}

#[test]
fn malformed_configuration_reports_only_fixed_variable_names() {
    let cases = [
        ("P1_HOST", "https://p1.example.com"),
        ("P1_HOST", "user@p1.example.com"),
        ("P1_HOST", "p1.example.com/path"),
        ("P1_HOST", "p1.example.com:443"),
        ("P1_HOST", "p1.example.com?secret"),
        ("P1_HOST", "p1.example.com."),
        ("P1_HOST", "p1..example.com"),
        ("P1_HOST", "P1.example.com"),
        ("P1_HOST", "127.0.0.1"),
        ("GH_REPO_APP_ID", "0"),
        ("GH_REPO_APP_ID", "0123"),
        ("GH_REPO_APP_ID", "18446744073709551616"),
        ("GH_REPO_CLIENT_ID", "CLIENT_SECRET_MARKER\n"),
        ("GH_SIGNIN_CLIENT_SECRET", "SIGNIN_SECRET_MARKER\n"),
        // octologin accepts printable ASCII without spaces only.
        ("GH_SIGNIN_CLIENT_SECRET", "SIGNIN_SECRET MARKER"),
        ("GH_REPO_PRIVATE_KEYS", "key:PRIVATE_KEY_MARKER"),
        ("GH_ORG_WEBHOOK_SECRETS", "WEBHOOK_SECRET_MARKER;"),
        (
            "GH_ORG_WEBHOOK_SECRETS",
            "WEBHOOK_SECRET_MARKER;WEBHOOK_SECRET_MARKER",
        ),
    ];
    for (field, value) in cases {
        let mut input = values();
        input.insert(field, value.into());
        let error =
            parse_web_apps(AppStage::RepoAndOrg, |key| input.get(key).cloned()).unwrap_err();
        assert_eq!(error.field, field);
        assert_eq!(
            error.to_string(),
            format!("invalid or missing deployment configuration: {field}")
        );
        assert!(!format!("{error:?}").contains("MARKER"));
    }
    let mut input = values();
    input.insert(
        "GH_REPO_PRIVATE_KEYS",
        format!(
            "key:{}",
            STANDARD
                .encode("-----BEGIN RSA PRIVATE KEY-----\nMARKER\n-----END RSA PRIVATE KEY-----")
        ),
    );
    assert_eq!(
        parse_web_apps(AppStage::Repo, |key| input.get(key).cloned())
            .unwrap_err()
            .field,
        "GH_REPO_PRIVATE_KEYS"
    );
}

#[test]
fn duplicate_app_and_client_identities_fail_atomically() {
    for (field, value, expected) in [
        ("GH_ORG_APP_ID", "123", "GH_REPO_APP_ID/GH_ORG_APP_ID"),
        (
            "GH_REPO_CLIENT_ID",
            "Iv.signin",
            "GH_SIGNIN_CLIENT_ID/GH_REPO_CLIENT_ID/GH_ORG_CLIENT_ID",
        ),
        (
            "GH_ORG_CLIENT_ID",
            "Iv.repo",
            "GH_SIGNIN_CLIENT_ID/GH_REPO_CLIENT_ID/GH_ORG_CLIENT_ID",
        ),
    ] {
        let mut input = values();
        input.insert(field, value.into());
        assert_eq!(
            parse_web_apps(AppStage::RepoAndOrg, |key| input.get(key).cloned())
                .unwrap_err()
                .field,
            expected
        );
    }
}

#[test]
fn installed_web_apps_require_only_installed_app_credentials() {
    let input = values();
    let apps = parse_installed_web(AppStage::Repo, |key| {
        assert!(
            key.starts_with("GH_REPO_"),
            "unexpected credential read: {key}"
        );
        input.get(key).cloned()
    })
    .unwrap();
    assert_eq!(summary(&apps.installed), both()[..1]);
    assert_eq!(
        apps.webhooks.slots().collect::<Vec<_>>(),
        [InstalledSlot::Repo]
    );
    let repo = apps.webhooks.get(InstalledSlot::Repo).unwrap();
    assert_eq!((repo.app_id, repo.secrets.len()), (123, 2));
    for missing in [
        "GH_REPO_APP_ID",
        "GH_REPO_CLIENT_ID",
        "GH_REPO_PRIVATE_KEYS",
        "GH_REPO_WEBHOOK_SECRETS",
    ] {
        let err = parse_installed_web(AppStage::Repo, |key| {
            if key == missing {
                None
            } else {
                input.get(key).cloned()
            }
        })
        .unwrap_err();
        assert_eq!(err.field, missing);
    }
}

fn production_values() -> BTreeMap<&'static str, String> {
    let mut input = values();
    input.extend([
        ("P1_KEYRING", "kekring1;name=p1;current=1;hwm=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.9fdab21f330c89b7".into()),
        ("RESTATE_INGRESS_URL", "https://ingress.example.com/prefix".into()),
        ("RESTATE_INGRESS_TOKEN", "INGRESS_SECRET_MARKER".into()),
        ("GHINVITE_GITHUB_INSTALL_URL", "https://github.com/apps/example/installations/new".into()),
    ]);
    input
}

#[test]
fn production_web_requires_canonical_authenticated_config_and_the_signin_app() {
    let input = production_values();
    let config =
        ghinvite_web::WebConfig::for_production(|field| input.get(field).cloned()).unwrap();
    assert_eq!(config.base_url, "https://p1.example.com");
    let signin = config.signin_apps.as_ref().unwrap();
    assert_eq!(
        signin.redirect_uri.as_str(),
        "https://p1.example.com/oauth/callback"
    );
    assert_eq!(signin.client_id, "Iv.signin");
    assert!(config.cookie_secure);
    let printed = format!("{config:?} {:?}", config.restate_auth);
    assert!(!printed.contains("SECRET_MARKER"));
    for field in [
        "P1_HOST",
        "RESTATE_INGRESS_URL",
        "RESTATE_INGRESS_TOKEN",
        "P1_KEYRING",
        "GH_REPO_PRIVATE_KEYS",
        "GH_REPO_WEBHOOK_SECRETS",
        "GH_SIGNIN_CLIENT_ID",
        "GH_SIGNIN_CLIENT_SECRET",
        "GHINVITE_GITHUB_INSTALL_URL",
    ] {
        let mut missing = input.clone();
        missing.remove(field);
        // Legacy aliases and unsigned mode cannot supply a production default.
        missing.insert("GHINVITE_BASE_URL", "https://legacy.example".into());
        missing.insert("GHINVITE_RESTATE_INGRESS", "http://localhost:8080".into());
        missing.insert("GHINVITE_RESTATE_AUTH", "local-unauthenticated".into());
        let error =
            ghinvite_web::WebConfig::for_production(|key| missing.get(key).cloned()).unwrap_err();
        assert!(error.to_string().contains(field), "{field}: {error}");
        assert!(!format!("{error:?} {error}").contains("SECRET_MARKER"));
    }
}

#[test]
fn production_web_rejects_ambiguous_urls_and_malformed_secrets_with_field_only_errors() {
    for (field, bad) in [
        ("P1_HOST", "https://example.com"),
        ("P1_HOST", "EXAMPLE.com"),
        ("P1_HOST", "example.com:443"),
        ("P1_HOST", "127.0.0.1"),
        ("RESTATE_INGRESS_URL", "http://127.0.0.1:8080"),
        ("RESTATE_INGRESS_URL", "https://SECRET_MARKER@example.com"),
        ("RESTATE_INGRESS_URL", "https://example.com?SECRET_MARKER"),
        ("RESTATE_INGRESS_URL", "https://example.com/#SECRET_MARKER"),
        ("RESTATE_INGRESS_URL", "https://EXAMPLE.com"),
        ("RESTATE_INGRESS_URL", "https://example.com."),
        ("RESTATE_INGRESS_URL", "https://example.com/SECRET_MARKER\0"),
        ("RESTATE_INGRESS_URL", "https://example.com\\SECRET_MARKER"),
        ("RESTATE_INGRESS_URL", " https://example.com"),
        ("RESTATE_INGRESS_TOKEN", ""),
        ("RESTATE_INGRESS_TOKEN", "SECRET_MARKER\n"),
        ("RESTATE_INGRESS_TOKEN", "SECRET MARKER"),
        ("GH_SIGNIN_CLIENT_SECRET", "SECRET_MARKER\n"),
        ("GH_SIGNIN_CLIENT_SECRET", "   "),
        ("GH_SIGNIN_CLIENT_ID", "SECRET_MARKER/invalid"),
        (
            "GHINVITE_GITHUB_INSTALL_URL",
            "https://evil.example/apps/x/installations/new",
        ),
        ("P1_OPERATOR_IDS", "9007199254740992"),
    ] {
        let mut input = production_values();
        input.insert(field, bad.into());
        let error =
            ghinvite_web::WebConfig::for_production(|key| input.get(key).cloned()).unwrap_err();
        let rendered = format!("{error:?} {error}");
        assert!(rendered.contains(field), "{rendered}");
        assert!(!rendered.contains("SECRET_MARKER"), "{rendered}");
    }
}

#[test]
fn explicit_app_stage_defaults_repo_and_requires_complete_org_without_downgrade() {
    assert_eq!(AppStage::from_config(None).unwrap(), AppStage::Repo);
    assert_eq!(AppStage::from_config(Some("repo")).unwrap(), AppStage::Repo);
    assert_eq!(
        AppStage::from_config(Some("repo-and-org")).unwrap(),
        AppStage::RepoAndOrg
    );
    for value in ["", "Repo", "repo-and-org ", "SECRET_MARKER", "org"] {
        let error = AppStage::from_config(Some(value)).unwrap_err();
        assert_eq!(error.field, "P1_APP_STAGE");
        assert!(!error.to_string().contains(value) || value.is_empty());
    }
    for (value, want) in [
        (None, vec![InstalledSlot::Repo]),
        (Some("repo"), vec![InstalledSlot::Repo]),
        (
            Some("repo-and-org"),
            vec![InstalledSlot::Repo, InstalledSlot::Org],
        ),
    ] {
        let mut input = production_values();
        if let Some(value) = value {
            input.insert("P1_APP_STAGE", value.into());
        }
        let config =
            ghinvite_web::WebConfig::for_production(|field| input.get(field).cloned()).unwrap();
        let installed = config
            .github_apps
            .iter()
            .map(|app| app.slot)
            .collect::<Vec<_>>();
        assert_eq!(installed, want);
        assert_eq!(config.webhooks.unwrap().slots().collect::<Vec<_>>(), want);
        assert!(config.signin_apps.is_some());
    }
    for field in [
        "GH_ORG_APP_ID",
        "GH_ORG_CLIENT_ID",
        "GH_ORG_PRIVATE_KEYS",
        "GH_ORG_WEBHOOK_SECRETS",
    ] {
        let mut input = production_values();
        input.insert("P1_APP_STAGE", "repo-and-org".into());
        input.remove(field);
        let error = ghinvite_web::WebConfig::for_production(|key| input.get(key).cloned())
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            format!("invalid or missing deployment configuration: {field}")
        );
    }
}
