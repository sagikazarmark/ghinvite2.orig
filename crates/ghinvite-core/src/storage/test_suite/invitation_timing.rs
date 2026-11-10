use super::*;
use crate::delivery::{CreateOutcome, CreateReceipt, DeliverySnapshot, InvitationTiming};

pub async fn scenario_invitation_timing<S: Storage + ProjectionStorage>(s: S) {
    s.put_app_installation(&crate::account::Installation::repo(sample_account(
        1, 9001, "acme",
    )))
    .await
    .unwrap();
    s.put_app_installation(&sample_org_installation(1, 9001, "acme"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(701, "requester")).await.unwrap();
    let mut link = sample_link(9001, 1, 701);
    link.organization_installation_id = Some(1);
    link.targets.organization = Some(crate::targets::OrganizationTarget {
        org_id: 9001,
        org_login: "acme".into(),
        grant: crate::targets::MemberGrant::Member,
    });
    let request = sample_request(link.id, 701);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    let at = dt("2026-05-04T13:00:00Z");
    let command=serde_json::from_value(serde_json::json!({"link_id":link.id,"request_id":request.id,"approval_id":"approval","account_id":9001,"installation_id":1,"requester_id":701,"repo_id":link.targets.repos[0].repo_id,"repo_full_name":link.targets.repos[0].repo_full_name,"permission":"pull","approved_at":at})).unwrap();
    let original: DeliverySnapshot = CreateReceipt {
        audit_version: crate::delivery_audit::DeliveryAuditVersion::CanonicalV2,
        command,
        outcome: CreateOutcome::Created { upstream_id: 99 },
        revision: 1,
        confirmed_at: Some(at),
    }
    .into();
    assert!(
        serde_json::to_value(&original)
            .unwrap()
            .get("invitation_timing")
            .is_none()
    );
    s.project_delivery(&original).await.unwrap();
    let mut next = original.clone();
    let mut timing =
        InvitationTiming::observed(99, at, at + chrono::Duration::seconds(1), at, true).unwrap();
    timing.owned_lifetime_seconds = Some(3600);
    assert!(next.retain_invitation_timing(timing.clone()));
    for _ in 0..2 {
        s.project_delivery(&next).await.unwrap();
    }
    s.project_delivery(&original).await.unwrap();
    let stored = s
        .get_delivery(&original.create.command.delivery_key())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored, next);
    assert_eq!(stored.create, original.create);
    assert_eq!(
        stored.invitation_expires_at(),
        Some(at + chrono::Duration::hours(1))
    );
    for changed in [
        None,
        Some(InvitationTiming {
            created_at: at + chrono::Duration::nanoseconds(1),
            ..timing.clone()
        }),
        Some(InvitationTiming {
            upstream_id: 100,
            ..timing.clone()
        }),
    ] {
        let mut invalid = next.clone();
        invalid.revision += 1;
        invalid.invitation_timing = changed;
        assert!(s.project_delivery(&invalid).await.is_err());
        assert_eq!(
            s.get_delivery(&original.create.command.delivery_key())
                .await
                .unwrap(),
            Some(next.clone())
        );
    }
    let command:crate::org_delivery::OrganizationCreateCommand=serde_json::from_value(serde_json::json!({"link_id":link.id,"request_id":request.id,"approval_id":"approval","account_id":9001,"installation_id":1,"requester_id":701,"organization":link.targets.organization,"teams":[],"approved_at":at})).unwrap();
    let mut org = crate::org_delivery::OrganizationDeliverySnapshot::planned(command).unwrap();
    org.create.outcome = crate::org_delivery::OrganizationCreateOutcome::AlreadyPending {
        upstream_id: 44,
        role: crate::org_delivery::OrganizationInvitationRole::DirectMember,
    };
    org.create.confirmed_at = Some(at);
    org.create.revision = 2;
    org.revision = 2;
    s.project_organization_delivery(&org).await.unwrap();
    let receipt = org.create.clone();
    let foreign_at = at - chrono::Duration::days(2);
    assert!(org.retain_invitation_timing(
        InvitationTiming::observed(44, foreign_at, at, at, false).unwrap()
    ));
    for _ in 0..2 {
        s.project_organization_delivery(&org).await.unwrap();
    }
    assert_eq!(org.create, receipt);
    assert_eq!(
        org.invitation_expires_at(),
        Some(foreign_at + chrono::Duration::days(7))
    );
    let mut invalid = org.clone();
    invalid.revision += 1;
    invalid.invitation_timing = None;
    assert!(s.project_organization_delivery(&invalid).await.is_err());
}
