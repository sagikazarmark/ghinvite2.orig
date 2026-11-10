//! Scripted private authority responses prove web presentation, not owner policy.
use super::*;
use ghinvite_core::admission::{Attempt, Rejection, RequesterPage};

fn page(fixture: &DeliveryFixture, state: RequestState) -> RequesterPage {
    let mut request = fixture.authority.requests().into_iter().next().unwrap();
    request.state = state;
    let link = fixture.authority.link(request.link_id).unwrap();
    RequesterPage {
        two_factor_required: None,
        email_domains: vec![],
        paused: false,
        rerequest_after: None,
        link_id: request.link_id.expect("linked fixture"),
        targets: link.creation.targets,
        approval_required: false,
        can_start_fresh: state == RequestState::Lapsed,
        attempt: None,
        request: Some(request),
    }
}

#[tokio::test]
async fn lapsed_history_allows_only_authorized_fresh_request_and_displays_durable_limit() {
    let fixture = fixture().await;
    let cookie = sign_in(&fixture.app).await;
    let mut page = page(&fixture, RequestState::Lapsed);
    let original_decision = page.request.as_ref().unwrap().decision.clone();
    fixture.authority.set_requester_page(page.clone());
    let html = status_html(&fixture, &cookie).await;
    assert!(html.contains("Invitation lapsed"));
    assert!(html.contains("Request again"));
    assert!(html.contains("one per seven days"));
    assert!(html.contains("short invitation lifetime requires another admin review"));
    assert!(
        html.contains("acme/waiting"),
        "retained repository history remains visible"
    );
    assert!(!html.contains("<h2 class=\"text-xl font-semibold\">Approved</h2>"));
    assert_eq!(fixture.authority.requests()[0].decision, original_decision);
    assert!(
        !fixture
            .authority
            .calls()
            .iter()
            .any(|method| method == "admit")
    );

    page.can_start_fresh = false;
    page.rerequest_after = Some("2099-01-02T03:04:05Z".parse().unwrap());
    fixture.authority.set_requester_page(page.clone());
    let html = status_html(&fixture, &cookie).await;
    assert!(html.contains("You can request again after 2099-01-02T03:04:05+00:00"));
    assert!(!html.contains(">Request again</"));
    assert!(!html.contains("action=\"/join?operation_id="));
    page.rerequest_after = None;
    fixture.authority.set_requester_page(page);
    let html = status_html(&fixture, &cookie).await;
    assert!(html.contains("This link is not accepting another request right now"));
}

#[tokio::test]
async fn rerequest_actual_pending_state_and_limit_receipt_are_not_presented_as_autoapproved() {
    let fixture = fixture().await;
    let cookie = sign_in(&fixture.app).await;
    let mut page = page(&fixture, RequestState::Pending);
    fixture.authority.set_requester_page(page.clone());
    let html = status_html(&fixture, &cookie).await;
    assert!(html.contains("Awaiting review"));
    assert!(!html.contains("Your request is approved"));
    assert!(!html.contains("Requests are automatically approved"));
    assert!(html.contains("Account admins review your request"));
    let link = fixture.authority.link(page.link_id).unwrap();
    let retry_after = "2099-01-02T03:04:05Z".parse().unwrap();
    page.attempt = Some(Attempt {
        input: Admit {
            domain_evidence: None,
            link_id: page.link_id,
            operation_id: RequestId::new().to_string().try_into().unwrap(),
            requester_id: REQUESTER_ID,
            secret_sha256: link.secret.sha256,
            secret_generation: link.secret.secret_generation,
            justification: None,
            scope_evidence: vec![],
        },
        receipt: Some(AdmissionReceipt {
            decided_at: Utc::now(),
            result: AdmissionResult::Rejected {
                reason: Rejection::RerequestLimited { retry_after },
            },
        }),
    });
    fixture.authority.set_requester_page(page);
    let html = status_html(&fixture, &cookie).await;
    assert!(html.contains("You can request again after 2099-01-02T03:04:05+00:00"));
    assert!(html.contains("a later request must use a new attempt"));
    assert!(!html.contains("action=\"/join?operation_id="));
}

/// Browser fixture only: SQL may still show earlier delivery observations.
/// The script controls the current owner state, not Lapsed aggregation policy.
pub(super) fn browser_page(fixture: &DeliveryFixture, limited: bool) {
    let mut page = page(fixture, RequestState::Lapsed);
    page.can_start_fresh = !limited;
    page.rerequest_after = limited.then(|| "2099-01-02T03:04:05Z".parse().unwrap());
    fixture
        .authority
        .seed_request(page.request.as_ref().unwrap().clone());
    fixture.authority.set_requester_page(page);
}
