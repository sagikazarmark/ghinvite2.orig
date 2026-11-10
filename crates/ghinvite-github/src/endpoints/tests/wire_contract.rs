//! Synthetic endpoint wire matrix (trimmed from gh-toolkit's). Every typed
//! helper must send exactly the reviewed request and decode every reviewed
//! reply. No live-provider claims.
use super::*;
use crate::{
    credential::{Authorization, Credential, CredentialKind, QuotaBucket, sealed},
    endpoints::organization as org,
    transport::Response,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

pub(super) const USER_AGENT: &str = "portable-wire-contract";

#[derive(Clone, Deserialize)]
pub(super) struct Reply {
    pub status: u16,
    pub body: Option<serde_json::Value>,
}

#[derive(Clone, Deserialize)]
pub(super) struct Pair {
    pub endpoint: String,
    pub feature: Option<String>,
    pub method: String,
    pub path: String,
    pub segments: Vec<String>,
    pub credentials: Vec<String>,
    pub request_body: Option<serde_json::Value>,
    pub request_headers: BTreeMap<String, String>,
    pub response_headers: BTreeMap<String, String>,
    pub authorization: BTreeMap<String, String>,
    pub helper_target: String,
    pub request_body_wire: String,
    pub responses: Vec<Reply>,
    pub source: String,
    pub provenance: String,
}

impl Pair {
    pub fn endpoint(&self) -> Endpoint {
        *Endpoint::all()
            .iter()
            .find(|e| format!("{e:?}") == self.endpoint)
            .expect("fixture endpoint exists")
    }
    pub fn reply(&self, reply: &Reply) -> Response {
        Response {
            status: reply.status,
            headers: self.response_headers.clone(),
            body: reply
                .body
                .as_ref()
                .map(|b| serde_json::to_vec(b).unwrap())
                .unwrap_or_default(),
        }
    }
    /// The bearer value the reviewed request carries for this credential.
    pub fn secret(&self, name: &str) -> String {
        self.authorization[name]
            .strip_prefix("Bearer ")
            .expect("bearer fixture")
            .to_owned()
    }
}

#[derive(Deserialize)]
pub(super) struct Fixtures {
    pub schema: u32,
    pub endpoints: Vec<Pair>,
    pub protocol_cases: Vec<serde_json::Value>,
}

pub(super) fn fixtures() -> Fixtures {
    serde_json::from_str(include_str!("../../../fixtures/http-fixtures.json")).unwrap()
}

/// octoauth's `AppCredential` signs a fresh JWT, so the reviewed fixed
/// app-JWT marker needs a test-private credential of the same kind.
struct FixtureAppJwt(String);

impl sealed::Sealed for FixtureAppJwt {
    fn authorization(&self) -> Authorization<'_> {
        Authorization { value: &self.0 }
    }
}

impl Credential for FixtureAppJwt {
    fn kind(&self) -> CredentialKind {
        CredentialKind::AppJwt
    }
    fn bucket(&self) -> QuotaBucket {
        QuotaBucket::App(crate::ids::AppId::new(123))
    }
}

/// The reviewed credential for `name`: a real installation credential around
/// an octoauth token, or the fixed app-JWT marker.
pub(super) fn credential(pair: &Pair, name: &str) -> Box<dyn Credential> {
    match name {
        "Installation" => Box::new(mocks::installation_credential(123, 77, &pair.secret(name))),
        "AppJwt" => Box::new(FixtureAppJwt(pair.secret(name))),
        _ => panic!("unreviewed credential class"),
    }
}

const REPO: Repository<'static> = Repository {
    owner: "acme",
    name: "api",
};

/// Every helper is selected explicitly: no metadata-derived call substitutes
/// for its request serialization or typed response decoding.
pub(super) async fn dispatch(pair: &Pair, rest: &Rest<'_>) -> Result<String, ApiError> {
    macro_rules! run {
        ($operation:expr) => {
            $operation.await.map(|value| format!("{value:?}"))
        };
    }
    match pair.endpoint() {
        Endpoint::GetOrgMembership => run!(org::organization_membership(rest, "acme", "alice")),
        Endpoint::GetOrg => run!(org::get_organization(rest, "acme")),
        Endpoint::ListOrgInvitations => run!(org::list_invitations(rest, "acme")),
        Endpoint::ListOrgInvitationTeams => run!(org::list_invitation_teams(rest, "acme", 42)),
        Endpoint::ListFailedOrgInvitations => run!(org::list_failed_invitations(rest, "acme")),
        Endpoint::CreateOrgInvitation => run!(org::create_invitation(
            rest,
            "acme",
            &org::OrganizationInvitationInput {
                invitee: org::Invitee::User { invitee_id: 8 },
                role: org::CreateInvitationRole::DirectMember,
                team_ids: vec![20]
            }
        )),
        Endpoint::CancelOrgInvitation => run!(org::cancel_invitation(rest, "acme", 42)),
        Endpoint::RemoveOrgMember => run!(org::remove_organization_member(rest, "acme", "alice")),
        Endpoint::ListOrgTeams => run!(org::list_teams(rest, "acme")),
        Endpoint::GetTeam => run!(org::get_team(rest, "acme", "developers")),
        Endpoint::GetTeamMembership => {
            run!(org::team_membership(rest, "acme", "developers", "alice"))
        }
        Endpoint::PutTeamMembership => run!(org::put_team_membership(
            rest,
            "acme",
            "developers",
            "alice",
            crate::roles::TeamRole::Member
        )),
        Endpoint::RemoveTeamMembership => run!(org::remove_team_membership(
            rest,
            "acme",
            "developers",
            "alice"
        )),
        Endpoint::GetRepository => run!(get_repository(rest, REPO)),
        Endpoint::CollaboratorPermission => run!(collaborator_permission(rest, REPO, "alice")),
        Endpoint::ListCollaborators => run!(list_collaborators(rest, REPO)),
        Endpoint::PutCollaborator => run!(put_collaborator(rest, REPO, "alice", &RepoRole::Write)),
        Endpoint::ListRepoInvitations => run!(list_repo_invitations(rest, REPO)),
        Endpoint::UpdateRepoInvitation => {
            run!(update_repo_invitation(rest, REPO, 42, &RepoRole::Write))
        }
        Endpoint::DeleteRepoInvitation => run!(delete_repo_invitation(rest, REPO, 42)),
        Endpoint::RemoveCollaborator => run!(remove_collaborator(rest, REPO, "alice")),
        Endpoint::GetUserByLogin => run!(get_user_by_login(rest, "alice")),
        Endpoint::GetUserById => run!(get_user_by_id(rest, 8)),
        Endpoint::GetInstallation => run!(get_installation(rest, 77)),
        Endpoint::ListInstallationRepos => run!(list_installation_repos(rest)),
        Endpoint::ListInstallationRequests => run!(list_installation_requests(rest)),
    }
}

fn wire_env(mock: &MockTransport) -> RestEnv {
    mocks::env(mock, USER_AGENT)
}

fn expectation(pair: &Pair, name: &str, response: Response) -> Expectation {
    let mut expectation = Expectation::status(
        crate::transport::Method::Get,
        format!("{}{}", mocks::API_BASE, pair.helper_target),
        200,
    )
    .require_header("authorization", pair.authorization[name].clone())
    .require_body(pair.request_body_wire.as_bytes().to_vec());
    expectation.method = match pair.method.as_str() {
        "GET" => Method::Get,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "PATCH" => Method::Patch,
        "DELETE" => Method::Delete,
        _ => panic!("unreviewed method"),
    };
    for (name, value) in &pair.request_headers {
        expectation = expectation.require_header(name, value.clone());
    }
    expectation.response = response;
    expectation
}

#[test]
fn wire_pairs_cover_every_endpoint_credential_and_success_status() {
    let fixture = fixtures();
    assert_eq!(fixture.schema, 1);
    let rows: BTreeSet<_> = fixture
        .endpoints
        .iter()
        .map(|p| p.endpoint.clone())
        .collect();
    let endpoints: BTreeSet<_> = Endpoint::all().iter().map(|e| format!("{e:?}")).collect();
    assert_eq!(rows, endpoints);
    assert_eq!(fixture.endpoints.len(), Endpoint::all().len());
    let metadata: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/stub-contract.json")).unwrap();
    for pair in &fixture.endpoints {
        let row = metadata["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["endpoint"] == pair.endpoint)
            .unwrap();
        assert_eq!(row["method"], pair.method);
        assert_eq!(row["path"], pair.path);
        assert_eq!(row["credentials"], json!(pair.credentials));
        assert_eq!(row["feature"], json!(pair.feature));
        assert_eq!(
            row["success_statuses"],
            json!(pair.responses.iter().map(|r| r.status).collect::<Vec<_>>())
        );
        assert_eq!(pair.path.matches("{}").count(), pair.segments.len());
        assert!(!pair.source.is_empty() && pair.provenance.contains("synthetic"));
        assert!(!pair.responses.is_empty());
        assert_eq!(
            pair.authorization.keys().cloned().collect::<BTreeSet<_>>(),
            pair.credentials.iter().cloned().collect()
        );
        assert_eq!(
            pair.request_body_wire.is_empty(),
            pair.request_body.is_none()
        );
    }
}

#[tokio::test]
async fn every_typed_helper_sends_the_reviewed_request_and_decodes_every_reply() {
    for pair in fixtures().endpoints {
        for name in &pair.credentials {
            for reply in &pair.responses {
                let mock =
                    MockTransport::scripted(vec![expectation(&pair, name, pair.reply(reply))]);
                let env = wire_env(&mock);
                let credential = credential(&pair, name);
                let result = dispatch(&pair, &Rest::new(&env, &*credential)).await;
                assert!(
                    result.is_ok(),
                    "typed fixture failed for {}: {:?}",
                    pair.endpoint,
                    result.err()
                );
                mock.assert_exhausted();
            }
        }
    }
}

#[tokio::test]
async fn protocol_cases_keep_their_header_semantics() {
    let fixture = fixtures();
    assert_eq!(fixture.protocol_cases.len(), 5);
    for row in &fixture.protocol_cases {
        let pair = fixture
            .endpoints
            .iter()
            .find(|p| p.endpoint == row["endpoint"])
            .unwrap();
        let name = row["credential"].as_str().unwrap();
        let response = Response {
            status: row["status"].as_u64().unwrap() as u16,
            headers: serde_json::from_value(row["headers"].clone()).unwrap(),
            body: serde_json::to_vec(&row["body"]).unwrap(),
        };
        let mock = MockTransport::scripted(vec![expectation(pair, name, response)]);
        let env = wire_env(&mock);
        let credential = credential(pair, name);
        let error = dispatch(pair, &Rest::new(&env, &*credential))
            .await
            .unwrap_err();
        assert_eq!(
            error.code(),
            row["expected_error"].as_str().unwrap(),
            "{}",
            row["name"]
        );
        match row["name"].as_str().unwrap() {
            "sso-partial" => assert!(
                matches!(&error, ApiError::Partial { org_ids, .. } if org_ids.as_slice() == [crate::ids::OrgId::new(100)])
            ),
            "sso-required" => assert_eq!(
                error,
                ApiError::Forbidden {
                    reason: ForbiddenReason::SsoRequired
                }
            ),
            "secondary-limit" => assert_eq!(
                error.suggested_wait(),
                Some(std::time::Duration::from_secs(2))
            ),
            "foreign-redirect" => assert_eq!(error, ApiError::Moved { status: 307 }),
            _ => {}
        }
        assert!(!format!("{error} {error:?}").contains("foreign.invalid"));
        mock.assert_exhausted();
    }
}

/// A gateway or proxy can reflect our request into an error body or a header.
/// Neither the credential nor the reflected text may reach an error.
#[tokio::test]
async fn reflected_credentials_never_reach_errors() {
    for pair in fixtures().endpoints {
        for name in &pair.credentials {
            let secret = pair.secret(name);
            for status in [401, 403, 404, 422, 500, 502] {
                let response = Response {
                    status,
                    headers: [
                        ("x-reflected".to_owned(), secret.clone()),
                        ("location".to_owned(), format!("https://x.test/{secret}")),
                    ]
                    .into(),
                    body: serde_json::to_vec(&json!({
                        "message": format!("Authorization: Bearer {secret}"),
                        "errors": [{"code": secret, "message": secret}],
                    }))
                    .unwrap(),
                };
                let mock = MockTransport::scripted(vec![expectation(&pair, name, response)]);
                let env = wire_env(&mock);
                let credential = credential(&pair, name);
                let error = dispatch(&pair, &Rest::new(&env, &*credential))
                    .await
                    .unwrap_err();
                let rendered = format!("{error} {error:?}");
                assert!(!rendered.contains(&secret), "{}", pair.endpoint);
                assert!(!rendered.contains("WIRE_MATRIX"), "{}", pair.endpoint);
                mock.assert_exhausted();
            }
        }
    }
}
