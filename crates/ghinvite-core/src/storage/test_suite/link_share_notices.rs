use super::*;
use crate::{
    account::{AccountInstallationRef, App},
    budget::RetryReason,
    delivery::{CreateCommand, CreateOutcome, CreateReceipt},
};

pub async fn scenario_link_share_notices<
    S: Storage + ProjectionStorage + super::super::invite_budget::InviteBudgetStorage,
>(
    s: S,
) {
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 100, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(7, "admin")).await.unwrap();
    s.upsert_user(&sample_user(8, "requester")).await.unwrap();
    let link = sample_link(100, 1, 7);
    let request = sample_request(link.id, 8);
    seed(&s, &link, std::slice::from_ref(&request), 1).await;
    let owner = AccountInstallationRef {
        app: App::Repo,
        account_id: 100,
    };
    let mut receipt = CreateReceipt {
        audit_version: crate::delivery_audit::DeliveryAuditVersion::Legacy,
        command: CreateCommand {
            budget_attribution: Default::default(),
            batch: None,
            access_lifetime_seconds: None,
            budget_share_pct: 50,
            delivery_generation: 1,
            invitation_id: Some(GithubInvitationId::new()),
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: "approval".into(),
            account_id: 100,
            installation_id: 1,
            requester_id: 8,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: Permission::Pull,
            approved_at: dt("2026-05-04T12:00:00Z"),
        },
        outcome: CreateOutcome::Throttled {
            until: dt("2026-05-05T12:00:00Z"),
            reason: None,
        },
        revision: 1,
        confirmed_at: None,
    };
    s.project_delivery(&receipt.clone().into()).await.unwrap();
    assert!(
        s.share_limited_links(owner, None).await.unwrap().is_empty(),
        "generic throttling is not link-share exhaustion"
    );
    receipt.revision = 2;
    receipt.outcome = CreateOutcome::Throttled {
        until: dt("2026-05-05T12:00:00Z"),
        reason: Some(RetryReason::LinkShare),
    };
    s.project_delivery(&receipt.clone().into()).await.unwrap();
    s.project_delivery(&receipt.clone().into()).await.unwrap();
    let rows = s.share_limited_links(owner, None).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].link_id, link.id);
    assert_eq!(rows[0].description, link.description);
    assert_eq!(
        s.share_limited_links(owner, Some(link.id)).await.unwrap(),
        rows
    );
    assert!(
        s.share_limited_links(owner, Some(InvitationLinkId::new()))
            .await
            .unwrap()
            .is_empty()
    );
    for wrong in [
        AccountInstallationRef {
            app: App::Org,
            account_id: 100,
        },
        AccountInstallationRef {
            app: App::Repo,
            account_id: 101,
        },
    ] {
        assert!(s.share_limited_links(wrong, None).await.unwrap().is_empty());
    }
    // The next successful delivery revision clears the delayed notice. Replaying
    // the old throttled snapshot cannot bring it back.
    let old = receipt.clone();
    receipt.revision = 3;
    receipt.outcome = CreateOutcome::Created { upstream_id: 99123 };
    receipt.confirmed_at = Some(dt("2026-05-05T12:00:00Z"));
    s.project_delivery(&receipt.clone().into()).await.unwrap();
    s.project_delivery(&old.into()).await.unwrap();
    assert!(s.share_limited_links(owner, None).await.unwrap().is_empty());
}
