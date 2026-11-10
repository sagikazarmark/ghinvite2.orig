//! Called only after requester capability or Console target authorization.
use ghinvite_core::{
    InvitationLinkId, RequestId, request_lifecycle::RequestStatus,
    storage::org_delivery_reads::OrgDeliveryQuery, targets::Targets,
};
use ghinvite_ui::organization_delivery::OrganizationDeliveryView;

pub(crate) async fn load(
    state: &crate::AppState,
    targets: &Targets,
    link_id: InvitationLinkId,
    request_id: RequestId,
    requester_id: u64,
) -> Option<OrganizationDeliveryView> {
    load_views(state, targets, link_id, request_id, requester_id, false)
        .await
        .0
}

pub(crate) async fn load_console(
    state: &crate::AppState,
    targets: &Targets,
    link_id: InvitationLinkId,
    request_id: RequestId,
    requester_id: u64,
) -> (
    Option<OrganizationDeliveryView>,
    Vec<OrganizationDeliveryView>,
) {
    load_views(state, targets, link_id, request_id, requester_id, true).await
}

async fn load_views(
    state: &crate::AppState,
    targets: &Targets,
    link_id: InvitationLinkId,
    request_id: RequestId,
    requester_id: u64,
    include_history: bool,
) -> (
    Option<OrganizationDeliveryView>,
    Vec<OrganizationDeliveryView>,
) {
    let Some(org) = targets.organization.as_ref() else {
        return (None, vec![]);
    };
    let snapshots = state
        .storage
        .list_organization_deliveries(&OrgDeliveryQuery {
            account_id: org.org_id,
            link_id,
            request_id,
        })
        .await;
    let progress = state
        .request_authority
        .organization_delivery_progress(RequestStatus {
            link_id,
            request_id,
            requester_id,
        })
        .await;
    let unavailable = snapshots.is_err() || progress.is_err();
    let progress = progress.as_ref().ok().and_then(Option::as_ref).filter(|p| {
        let mut ids = p.team_ids.clone();
        ids.sort_unstable();
        let mut expected: Vec<_> = targets.teams.iter().map(|t| t.team_id).collect();
        expected.sort_unstable();
        p.org_id == org.org_id && ids == expected
    });
    let snapshots = snapshots.as_deref().unwrap_or_default();
    let current = ghinvite_ui::organization_delivery::presentation(
        targets,
        if progress.is_some() { snapshots } else { &[] },
        progress,
        unavailable,
    );
    // SQL receipts remain useful Console history when the request owner is
    // unavailable or has advanced. They never select the active generation.
    let history = if include_history {
        snapshots
            .iter()
            .filter(|s| {
                s.create.command.organization == *org
                    && s.create.command.teams == targets.teams
                    && progress.is_none_or(|p| {
                        p.delivery_generation != s.create.command.delivery_generation
                    })
            })
            .filter_map(|snapshot| {
                ghinvite_ui::organization_delivery::presentation(
                    targets,
                    std::slice::from_ref(snapshot),
                    None,
                    unavailable,
                )
            })
            .collect()
    } else {
        vec![]
    };
    (current, history)
}
