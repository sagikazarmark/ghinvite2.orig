//! Email-invitation contract, ported from gh-toolkit: the broker is replaced
//! by a `Rest` over an octoauth installation token.
use ghinvite_github::{
    email_invitation::create,
    error::ApiError,
    mocks::{self, Expectation, MockTransport},
    rest::Rest,
    transport::Method,
};
use serde_json::json;

fn setup(response_status: u16, id: u64) -> MockTransport {
    let mut write = Expectation::ok_json(
        Method::Post,
        "https://api.github.test/orgs/acme/invitations",
        json!({"id":id,"created_at":"2026-10-17T00:00:00Z","email":"PRIVATE_EMAIL@example.com","login":"PRIVATE_LOGIN"}),
    )
    .require_header("authorization", "Bearer PRIVATE_IAT")
    .require_body(
        br#"{"email":"Private+tag@example.com","role":"direct_member","team_ids":[7,8]}"#.to_vec(),
    );
    write.response.status = response_status;
    MockTransport::scripted(vec![write])
}

#[tokio::test]
async fn email_post_has_no_user_lookup_membership_read_or_address_in_receipt() {
    let mock = setup(201, 123);
    let env = mocks::env(&mock, "email-contract");
    let credential = mocks::installation_credential(9, 42, "PRIVATE_IAT");
    let receipt = create(
        &Rest::new(&env, &credential),
        "acme",
        "Private+tag@EXAMPLE.com",
        &[7, 8],
    )
    .await
    .unwrap();
    assert_eq!(receipt.id, 123);
    let wire = serde_json::to_value(receipt).unwrap();
    assert_eq!(wire, json!({"id":123,"created_at":"2026-10-17T00:00:00Z"}));
    assert!(!format!("{receipt:?}").contains("PRIVATE"));
    mock.assert_exhausted();
}

#[tokio::test]
async fn non_created_or_zero_identity_never_produces_a_create_receipt() {
    for (status, id) in [(200, 123), (201, 0), (503, 123)] {
        let mock = setup(status, id);
        let env = mocks::env(&mock, "email-contract");
        let credential = mocks::installation_credential(9, 42, "PRIVATE_IAT");
        let error = create(
            &Rest::new(&env, &credential),
            "acme",
            "Private+tag@example.com",
            &[7, 8],
        )
        .await
        .unwrap_err();
        assert!(!format!("{error:?} {error}").contains("PRIVATE"));
        mock.assert_exhausted(); // no automatic POST retry, including a 503
    }
}

#[tokio::test]
async fn invalid_addresses_teams_and_credentials_are_refused_before_dispatch() {
    let mock = MockTransport::scripted(vec![]);
    let env = mocks::env(&mock, "email-contract");
    let credential = mocks::installation_credential(9, 42, "PRIVATE_IAT");
    let rest = Rest::new(&env, &credential);
    for address in ["PRIVATE_ADDRESS", "a@b.com\r\nBcc:PRIVATE@y.com"] {
        let error = create(&rest, "acme", address, &[7]).await.unwrap_err();
        assert_eq!(
            error,
            ApiError::Decode {
                what: "email_invitation_address"
            }
        );
        assert!(!format!("{error:?} {error}").contains("PRIVATE"));
    }
    for teams in [&[0][..], &[7, 7][..]] {
        assert_eq!(
            create(&rest, "acme", "a@example.com", teams)
                .await
                .unwrap_err(),
            ApiError::Decode {
                what: "email_invitation_teams"
            }
        );
    }
    let app = mocks::app_credential(9);
    assert!(matches!(
        create(&Rest::new(&env, &app), "acme", "a@example.com", &[]).await,
        Err(ApiError::Forbidden { .. })
    ));
    mock.assert_exhausted();
}
