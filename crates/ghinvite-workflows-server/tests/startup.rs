//! Invalid canonical app config must stop the real binary before touching SQL.
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn rejected(case: &str, values: &[(&str, &str)], field: &str) {
    let path =
        std::env::temp_dir().join(format!("p1-startup-{}-{case}.sqlite", std::process::id()));
    assert!(!path.exists());
    let mut command = Command::new(env!("CARGO_BIN_EXE_ghinvite-workflows"));
    command
        .env_clear()
        .env("GHINVITE_DATABASE_PATH", &path)
        .envs(values.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("invalid startup configuration must terminate before serving");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let diagnostics = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        diagnostics.contains(field),
        "expected fixed configuration field"
    );
    assert!(!diagnostics.contains("SECRET_MARKER"));
    assert!(
        !path.exists(),
        "invalid app config must fail before storage initialization"
    );
}

#[test]
fn native_startup_rejects_missing_config_even_with_legacy_credentials() {
    rejected(
        "legacy",
        &[
            ("GHINVITE_GITHUB_APP_ID", "123"),
            (
                "GHINVITE_GITHUB_APP_PRIVATE_KEY",
                include_str!("../../../test-fixtures/github-app-rsa.pem"),
            ),
        ],
        "P1_HOST",
    );
}

#[test]
fn native_startup_rejects_bad_keys_without_echoing_or_opening_database() {
    rejected(
        "bad-key",
        &[
            ("P1_HOST", "p1.example.com"),
            ("GH_REPO_APP_ID", "123"),
            ("GH_REPO_CLIENT_ID", "Iv.actual-repo"),
            ("GH_REPO_PRIVATE_KEYS", "primary:SECRET_MARKER"),
        ],
        "GH_REPO_PRIVATE_KEYS",
    );
}

#[tokio::test]
async fn native_startup_accepts_explicit_registry_fixture_and_serves_discovery() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::net::{TcpListener, TcpStream};
    struct Running(std::process::Child);
    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for (stage, organization_enabled, bulk_enabled) in [
        ("repo", false, false),
        ("repo-and-org", false, false),
        ("repo-and-org", true, false),
        ("repo", false, true),
        ("repo-and-org", true, true),
    ] {
        let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reserved.local_addr().unwrap();
        drop(reserved);
        let key = format!(
            "fixture:{}",
            STANDARD.encode(include_bytes!("../../../test-fixtures/github-app-rsa.pem"))
        );
        let mut server = Running(
            Command::new(env!("CARGO_BIN_EXE_ghinvite-workflows"))
                .arg("--local-unsigned")
                .env_clear()
                .env("P1_HOST", "p1.example.com")
                .env("GH_REPO_APP_ID", "123")
                .env("GH_REPO_CLIENT_ID", "Iv.explicit-fixture")
                .env("GH_REPO_PRIVATE_KEYS", &key)
                .env("P1_APP_STAGE", stage)
                .env("GH_ORG_DELIVERY_ENABLED", organization_enabled.to_string())
                .env("GH_BULK_ENABLED",bulk_enabled.to_string())
                .env("P1_EMAIL_KEYRING","kekring1;name=p1-email;current=1;hwm=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.6b3451284885d811")
                .env("GH_ORG_APP_ID", "456")
                .env("GH_ORG_CLIENT_ID", "Iv.org-fixture")
                .env("GH_ORG_PRIVATE_KEYS", &key)
                .env("GHINVITE_LISTEN_ADDR", address.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                server.0.try_wait().unwrap().is_none(),
                "valid explicit config must start"
            );
            if TcpStream::connect(address).is_ok() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "valid config must serve before deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // The native Restate SDK server speaks HTTP/2 prior knowledge.
        let response = reqwest::Client::builder()
            .http2_prior_knowledge()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap()
            .get(format!("http://{address}/discover"))
            .header("accept", "application/vnd.restate.endpointmanifest.v3+json")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let body = response.text().await.unwrap();
        assert!(body.contains("InvitationLink") && body.contains("Installation"));
        let manifest: serde_json::Value = serde_json::from_str(&body).unwrap();
        let services = manifest["services"].as_array().unwrap();
        for (service, enabled) in [
            ("OrganizationDelivery", organization_enabled),
            ("InviteBudget", organization_enabled || bulk_enabled),
            ("BulkInviteBatch", bulk_enabled),
            ("EmailInviteDelivery", bulk_enabled && organization_enabled),
        ] {
            assert_eq!(
                services.iter().any(|s| s["name"] == service),
                enabled,
                "{service} composition must follow the explicit setting"
            );
        }
    }
}

#[test]
fn native_identity_configuration_fails_before_storage() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let key = format!(
        "fixture:{}",
        STANDARD.encode(include_bytes!("../../../test-fixtures/github-app-rsa.pem"))
    );
    let common = [
        ("P1_HOST", "p1.example.com"),
        ("GH_REPO_APP_ID", "123"),
        ("GH_REPO_CLIENT_ID", "Iv.explicit-fixture"),
        ("GH_REPO_PRIVATE_KEYS", key.as_str()),
    ];
    rejected("missing-identity", &common, "RESTATE_IDENTITY_KEYS");
    for (case, value) in [
        ("empty-identity", ""),
        ("bad-identity", "publickeyv1_SECRET_MARKER"),
    ] {
        let mut values = common.to_vec();
        values.push(("RESTATE_IDENTITY_KEYS", value));
        rejected(case, &values, "RESTATE_IDENTITY_KEYS");
    }
    let mut legacy = common.to_vec();
    legacy.push((
        "RESTATE_IDENTITY_KEY",
        "publickeyv1_ChjENKeMvCtRnqG2mrBK1HmPKufgFUc98K8B3ononQvp",
    ));
    rejected("legacy-identity", &legacy, "RESTATE_IDENTITY_KEYS");
}

#[test]
fn native_registry_stage_rejects_invalid_or_incomplete_org_before_storage() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    rejected(
        "stage-invalid",
        &[("P1_APP_STAGE", "SECRET_MARKER")],
        "P1_APP_STAGE",
    );
    let key = format!(
        "fixture:{}",
        STANDARD.encode(include_bytes!("../../../test-fixtures/github-app-rsa.pem"))
    );
    let common = [
        ("P1_APP_STAGE", "repo-and-org"),
        ("P1_HOST", "p1.example.com"),
        ("GH_REPO_APP_ID", "123"),
        ("GH_REPO_CLIENT_ID", "Iv.repo"),
        ("GH_REPO_PRIVATE_KEYS", key.as_str()),
    ];
    rejected("stage-org-missing", &common, "GH_ORG_APP_ID");
    let mut partial = common.to_vec();
    partial.extend([("GH_ORG_APP_ID", "456"), ("GH_ORG_CLIENT_ID", "Iv.org")]);
    rejected("stage-org-partial", &partial, "GH_ORG_PRIVATE_KEYS");
}

#[cfg(unix)]
#[test]
fn native_stage_non_unicode_is_invalid_not_absent() {
    use std::os::unix::ffi::OsStringExt;
    let output = Command::new(env!("CARGO_BIN_EXE_ghinvite-workflows"))
        .env_clear()
        .env(
            "P1_APP_STAGE",
            std::ffi::OsString::from_vec(b"SECRET_MARKER\xff".to_vec()),
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    let diagnostics = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(diagnostics.contains("P1_APP_STAGE"));
    assert!(!diagnostics.contains("SECRET_MARKER"));
}

#[test]
fn organization_delivery_activation_rejects_bad_value_and_missing_org_stage_before_storage() {
    for (case, value) in [
        ("org-delivery-invalid", "SECRET_MARKER"),
        ("org-delivery-no-stage", "true"),
    ] {
        rejected(
            case,
            &[("GH_ORG_DELIVERY_ENABLED", value)],
            "GH_ORG_DELIVERY_ENABLED",
        );
    }
}

#[test]
fn bulk_configuration_is_explicit_and_email_keyring_fails_before_sql() {
    rejected(
        "reissue-invalid",
        &[("GH_REISSUE_ENABLED", "SECRET_MARKER")],
        "GH_REISSUE_ENABLED",
    );
    rejected(
        "bulk-invalid",
        &[("GH_BULK_ENABLED", "SECRET_MARKER")],
        "GH_BULK_ENABLED",
    );
    rejected(
        "email-ring-invalid",
        &[("P1_EMAIL_KEYRING", "SECRET_MARKER")],
        "P1_EMAIL_KEYRING",
    );
    rejected(
        "email-ring-required",
        &[
            ("P1_APP_STAGE", "repo-and-org"),
            ("GH_ORG_DELIVERY_ENABLED", "true"),
            ("GH_BULK_ENABLED", "true"),
        ],
        "P1_EMAIL_KEYRING",
    );
}
