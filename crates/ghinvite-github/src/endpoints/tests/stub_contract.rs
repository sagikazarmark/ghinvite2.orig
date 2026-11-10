//! Portable metadata contract: changes require reviewing fixture provenance,
//! not silently regenerating a snapshot from a changed implementation.
use crate::endpoints::Endpoint;
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[test]
// covers: TR-12 TR-13 TR-21
fn portable_stub_contract_matches_every_enabled_endpoint_and_its_security_metadata() {
    let contract: Value =
        serde_json::from_str(include_str!("../../../fixtures/stub-contract.json")).unwrap();
    assert_eq!(contract["schema"], 1);
    let rows = contract["endpoints"].as_array().unwrap();
    let enabled = |row: &&Value| match row["feature"].as_str() {
        None => true,
        Some(other) => panic!("unreviewed contract feature {other}"),
    };
    let expected: BTreeSet<_> = rows
        .iter()
        .filter(enabled)
        .map(|row| row["endpoint"].as_str().unwrap())
        .collect();
    let actual: BTreeSet<_> = Endpoint::all()
        .iter()
        .map(|endpoint| format!("{endpoint:?}"))
        .collect();
    assert_eq!(
        actual,
        expected.iter().map(|value| value.to_string()).collect()
    );
    assert_eq!(
        rows.len(),
        rows.iter()
            .map(|r| r["endpoint"].as_str().unwrap())
            .collect::<BTreeSet<_>>()
            .len()
    );
    for endpoint in Endpoint::all() {
        let name = format!("{endpoint:?}");
        let row = rows.iter().find(|row| row["endpoint"] == name).unwrap();
        let metadata = endpoint.meta();
        assert_eq!(row["method"], metadata.method.as_str(), "{name}");
        let template = row["path"].as_str().unwrap();
        let mut path = metadata.path.clone();
        for _ in 0..template.matches("{}").count() {
            path = path.seg("segment");
        }
        assert_eq!(
            path.build().unwrap(),
            template.replace("{}", "segment"),
            "{name}"
        );
        assert_eq!(
            row["credentials"],
            json!(
                metadata
                    .credential
                    .iter()
                    .map(|kind| format!("{kind:?}"))
                    .collect::<Vec<_>>()
            ),
            "{name}"
        );
        assert_eq!(
            row["permission"],
            json!(
                metadata
                    .permission
                    .map(|(permission, access)| [format!("{permission:?}"), format!("{access:?}")])
            ),
            "{name}"
        );
        assert_eq!(row["paginated"], metadata.paginated, "{name}");
        assert_eq!(row["content_creating"], metadata.content_creating, "{name}");
        assert_eq!(row["retry_safe"], metadata.retry_safe, "{name}");
        assert_eq!(row["api_version"], json!(metadata.api_version), "{name}");
        assert_eq!(row["fixture_kind"], "synthetic-provider-wire");
        assert!(!endpoint.retry_reason().is_empty());
    }
}

#[tokio::test]
// covers: TR-12 PG-01 PG-02
async fn installation_request_fixture_preserves_minimal_numeric_requester_and_complete_pages() {
    use super::{app, env};
    use crate::{
        endpoints::list_installation_requests,
        mocks::{Expectation, MockTransport},
        rest::Rest,
        transport::Method,
    };
    let row = |id| json!({"id":id,"account":{"id":9,"login":"org"},"requester":{"id":8},"created_at":"2026-01-01T00:00:00Z"});
    let mut first = Expectation::ok_json(
        Method::Get,
        "https://api.github.test/app/installation-requests?per_page=100",
        json!([row(1)]),
    );
    first.response.headers.insert(
        "link".into(),
        "<https://api.github.test/app/installation-requests?per_page=100&page=2>; rel=\"next\""
            .into(),
    );
    let mock = MockTransport::scripted(vec![
        first,
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/app/installation-requests?per_page=100&page=2",
            json!([row(2)]),
        ),
    ]);
    let environment = env(&mock);
    let credential = app();
    let rows = list_installation_requests(&Rest::new(&environment, &credential))
        .await
        .unwrap();
    assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), [1, 2]);
    assert!(
        rows.iter()
            .all(|row| row.requester.id == 8 && row.account.id == 9)
    );
    mock.assert_exhausted();
}

#[tokio::test]
// covers: TR-12 TR-13
async fn numeric_user_lookup_wire_fixture_unverified() {
    use super::{env, installation};
    use crate::{
        endpoints::get_user_by_id,
        mocks::{Expectation, MockTransport},
        rest::Rest,
        transport::Method,
    };
    // This records P1's selected path, not a live resolution of its open spike.
    let mock = MockTransport::scripted(vec![Expectation::ok_json(
        Method::Get,
        "https://api.github.test/user/8",
        json!({"id":8,"login":"renamed-user"}),
    )]);
    let environment = env(&mock);
    let credential = installation();
    let user = get_user_by_id(&Rest::new(&environment, &credential), 8)
        .await
        .unwrap();
    assert_eq!(user.id, 8);
    assert_eq!(user.login, "renamed-user");
    mock.assert_exhausted();
}
