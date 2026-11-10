use super::*;
use crate::account::App;
use crate::storage::Error;

pub async fn scenario_link_installation_bindings<S: Storage + ProjectionStorage>(s: S) {
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(501, 9017, "repo-owner")).clone(),
    ))
    .await
    .unwrap();
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(502, 9018, "org-owner")).clone(),
    ))
    .await
    .unwrap();
    s.put_app_installation(&sample_org_installation(501, 9018, "org-owner"))
        .await
        .unwrap();
    s.upsert_user(&sample_user(701, "admin")).await.unwrap();
    let mut org = sample_link(9018, 501, 701);
    org.targets.repos.clear();
    org.targets.organization = Some(crate::targets::OrganizationTarget {
        org_id: 9018,
        org_login: "org-owner".into(),
        grant: crate::targets::MemberGrant::Member,
    });
    assert_eq!(org.primary_app(), App::Org);
    assert_eq!(org.org_installation_id(), Some(501));
    // Equal numeric repo installation501 belongs to another account and must
    // neither authorize nor prevent this organization-app binding.
    seed(&s, &org, &[], 1).await;
    assert_eq!(
        s.get_invitation_link_by_id(org.id).await.unwrap(),
        Some(org.clone())
    );
    let mut mixed = sample_link(9017, 501, 701);
    mixed.targets.repos[0].repo_full_name = "repo-owner/api".into();
    mixed.targets.organization = Some(crate::targets::OrganizationTarget {
        org_id: 9017,
        org_login: "repo-owner".into(),
        grant: crate::targets::MemberGrant::Member,
    });
    mixed.organization_installation_id = Some(502);
    let projection = fixture::envelope(&mixed, 1);
    assert!(
        matches!(
            s.apply_transition(&projection).await,
            Err(Error::ProjectionDependency)
        ),
        "a repo installation with matching ID is not an org parent"
    );
    assert!(
        s.get_invitation_link_by_id(mixed.id)
            .await
            .unwrap()
            .is_none()
    );
    s.put_app_installation(&sample_org_installation(502, 9017, "repo-owner"))
        .await
        .unwrap();
    assert_eq!(mixed.primary_app(), App::Repo);
    assert_eq!(mixed.org_installation_id(), Some(502));
    for _ in 0..3 {
        s.apply_transition(&projection).await.unwrap();
    }
    assert_eq!(
        s.get_invitation_link_by_id(mixed.id).await.unwrap(),
        Some(mixed.clone())
    );
    assert_eq!(
        s.list_invitation_links_for_account(9017).await.unwrap(),
        vec![mixed.clone()]
    );
    let mut wrong = mixed.clone();
    wrong.id = InvitationLinkId::new();
    wrong.organization_installation_id = Some(501);
    assert!(
        matches!(
            s.apply_transition(&fixture::envelope(&wrong, 1)).await,
            Err(Error::ProjectionInvariant(_))
        ),
        "org parent account must agree"
    );
    wrong = org.clone();
    wrong.id = InvitationLinkId::new();
    wrong.installation_id = 502;
    assert!(
        matches!(
            s.apply_transition(&fixture::envelope(&wrong, 1)).await,
            Err(Error::ProjectionInvariant(_))
        ),
        "primary org parent account must agree"
    );
    let mut replacement = projection.clone();
    replacement.link.revision = 2;
    replacement.link.creation.organization_installation_id = Some(501);
    assert!(matches!(
        s.apply_transition(&replacement).await,
        Err(Error::ProjectionInvariant(_))
    ));
    let mut redundant = org.clone();
    redundant.organization_installation_id = Some(501);
    assert!(matches!(
        s.apply_transition(&fixture::envelope(&redundant, 2)).await,
        Err(Error::ProjectionInvariant(_))
    ));
    let mut missing = mixed.clone();
    missing.organization_installation_id = None;
    assert!(matches!(
        s.apply_transition(&fixture::envelope(&missing, 2)).await,
        Err(Error::ProjectionInvariant(_))
    ));
    let mut unsafe_id = mixed.clone();
    unsafe_id.organization_installation_id = Some(super::super::integer::MAX_EXACT + 1);
    assert!(matches!(
        s.apply_transition(&fixture::envelope(&unsafe_id, 2)).await,
        Err(Error::ProjectionInvariant(_))
    ));
    assert_eq!(
        s.get_invitation_link_by_id(mixed.id).await.unwrap(),
        Some(mixed)
    );
    assert_eq!(
        s.get_invitation_link_by_id(org.id).await.unwrap(),
        Some(org)
    );
}
