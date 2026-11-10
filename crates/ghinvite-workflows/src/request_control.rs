//! Durable request-side inbox for full link control snapshots. It does not
//! decide request outcomes or synchronously wait for deliveries.
use ghinvite_core::{
    delivery_control::{ControlEnvelope, DeliveryKind, Identity, LinkControl},
    request_control::{ControlOutcome, ControlReceipt, RequestControl},
    request_lifecycle::InitializeRequest,
};
use restate_sdk::{
    context::{ContextClient, ContextReadState, ContextWriteState, ObjectContext},
    errors::TerminalError,
    serde::Json,
};
use serde::{Deserialize, Serialize};

const CURRENT: &str = "link_control/current";
#[derive(Clone, Serialize, Deserialize)]
struct Record {
    input: RequestControl,
    receipt: ControlReceipt,
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "link control conflict")
}
fn invalid() -> TerminalError {
    TerminalError::new_with_code(400, "invalid link control")
}

fn bind(input: &RequestControl, initialized: &InitializeRequest) -> Result<(), TerminalError> {
    if input.request_id != initialized.request.request_id
        || Some(input.link_id) != initialized.request.link_id
        || input.admitted_at != initialized.request.admitted_at
        || input.invitation_lifetime_seconds != initialized.invitation_lifetime_seconds
    {
        return Err(conflict());
    }
    Ok(())
}

fn evaluate(
    input: &RequestControl,
    current: Option<&RequestControl>,
) -> Result<ControlReceipt, TerminalError> {
    ghinvite_core::delivery_control::validate_link_control(
        input.revision,
        input.hold_epoch,
        input.at,
        &input.control,
    )
    .map_err(|_| invalid())?;
    if !(3600..=604800).contains(&input.invitation_lifetime_seconds) {
        return Err(invalid());
    }
    if let Some(old) = current {
        if input.request_id != old.request_id
            || input.link_id != old.link_id
            || input.admitted_at != old.admitted_at
            || input.invitation_lifetime_seconds != old.invitation_lifetime_seconds
        {
            return Err(conflict());
        }
        if input.revision == old.revision && input != old {
            return Err(conflict());
        }
        if input.revision < old.revision {
            return Ok(ControlReceipt {
                outcome: ControlOutcome::IgnoredStale,
                revision: old.revision,
            });
        }
        if input != old {
            if matches!(old.control, LinkControl::Revoked { .. })
                || input.hold_epoch < old.hold_epoch
            {
                return Err(conflict());
            }
            let window = match &input.control {
                LinkControl::AutoPause { window } => Some(window),
                LinkControl::Revoked { abort_hold, .. } => abort_hold.as_ref(),
                _ => None,
            };
            if let Some(window) = window {
                let continuing = matches!(&old.control, LinkControl::AutoPause { window: previous } if previous == window);
                if window.epoch <= old.hold_epoch && !continuing {
                    return Err(conflict());
                }
            }
        }
    }
    Ok(ControlReceipt {
        outcome: ControlOutcome::Applied,
        revision: input.revision,
    })
}

pub(crate) async fn apply(
    ctx: &ObjectContext<'_>,
    input: RequestControl,
) -> Result<ControlReceipt, TerminalError> {
    if ctx.key() != input.request_id.to_string() {
        return Err(TerminalError::new_with_code(404, "request not found"));
    }
    let key = format!(
        "link_control/operation/{}",
        String::from(input.operation_id.clone())
    );
    if let Some(Json(record)) = ctx.get::<Json<Record>>(&key).await? {
        return if record.input == input {
            Ok(record.receipt)
        } else {
            Err(conflict())
        };
    }
    let initialized = ctx.get::<Json<InitializeRequest>>("input").await?;
    if let Some(Json(initialized)) = &initialized {
        bind(&input, initialized)?;
    }
    let current = ctx.get::<Json<RequestControl>>(CURRENT).await?;
    let receipt = evaluate(&input, current.as_ref().map(|j| &j.0))?;
    if receipt.outcome == ControlOutcome::Applied {
        ctx.set(CURRENT, Json(input.clone()));
        if let Some(Json(initialized)) = &initialized {
            forward_initialized(ctx, &input, initialized).await?;
        }
    }
    ctx.set(
        &key,
        Json(Record {
            input,
            receipt: receipt.clone(),
        }),
    );
    Ok(receipt)
}

/// Compare a pre-initialization inbox before setting any request state.
pub(crate) async fn validate_initialization(
    ctx: &ObjectContext<'_>,
    initialized: &InitializeRequest,
) -> Result<(), TerminalError> {
    if let Some(Json(input)) = ctx.get::<Json<RequestControl>>(CURRENT).await? {
        bind(&input, initialized)?;
    }
    Ok(())
}

/// Send the current snapshot before this delivery's first create invocation.
/// A later owner command sends updated snapshots even after dispatch completed.
pub(crate) async fn before_create(
    ctx: &ObjectContext<'_>,
    initialized: &InitializeRequest,
    repo_id: u64,
    generation: u64,
) -> Result<(), TerminalError> {
    if let Some(Json(input)) = ctx.get::<Json<RequestControl>>(CURRENT).await? {
        bind(&input, initialized)?;
        forward(ctx, &input, DeliveryKind::Repository, repo_id, generation).await?;
    }
    Ok(())
}

async fn forward(
    ctx: &ObjectContext<'_>,
    input: &RequestControl,
    kind: DeliveryKind,
    repo_id: u64,
    generation: u64,
) -> Result<(), TerminalError> {
    let identity = Identity {
        link_id: Some(input.link_id),
        batch: None,
        request_id: input.request_id,
        kind,
        target_id: repo_id,
        delivery_generation: generation,
    };
    let payload = crate::delivery_control::DeliveryControlInput {
        envelope: ControlEnvelope {
            identity,
            operation_id: input.operation_id.clone(),
            revision: input.revision,
            hold_epoch: input.hold_epoch,
            at: input.at,
            control: input.control.clone(),
        },
        admitted_at: input.admitted_at,
        invitation_lifetime_seconds: input.invitation_lifetime_seconds,
    };
    let key = ghinvite_core::delivery::delivery_key(input.request_id, repo_id, generation);
    match kind {
        DeliveryKind::Repository => {
            ctx.object_client::<crate::delivery::RepositoryDeliveryClient>(key)
                .control(Json(payload))
                .send()
                .await?;
        }
        DeliveryKind::Organization => {
            ctx.object_client::<crate::org_delivery::OrganizationDeliveryClient>(key)
                .control(Json(payload))
                .send()
                .await?;
        }
    }
    Ok(())
}

pub(crate) async fn before_organization_create(
    ctx: &ObjectContext<'_>,
    initialized: &InitializeRequest,
    org_id: u64,
    generation: u64,
) -> Result<(), TerminalError> {
    if let Some(Json(input)) = ctx.get::<Json<RequestControl>>(CURRENT).await? {
        bind(&input, initialized)?;
        forward(ctx, &input, DeliveryKind::Organization, org_id, generation).await?;
    } else if let Some(batch) = initialized.request.batch {
        // Batch requests have no link-control owner. Their immutable approved
        // initialization supplies the initial S0 snapshot for every generation;
        // later batch cancellation uses the separate exact-generation stop.
        if initialized.request.link_id.is_some() {
            return Err(conflict());
        }
        let input = crate::delivery_control::DeliveryControlInput {
            envelope: ControlEnvelope {
                identity: Identity {
                    link_id: None,
                    batch: Some(batch),
                    request_id: initialized.request.request_id,
                    kind: DeliveryKind::Organization,
                    target_id: org_id,
                    delivery_generation: generation,
                },
                operation_id: initialized
                    .request
                    .request_id
                    .to_string()
                    .try_into()
                    .map_err(|_| invalid())?,
                revision: 1,
                hold_epoch: 0,
                at: initialized.request.admitted_at,
                control: LinkControl::Active,
            },
            admitted_at: initialized.request.admitted_at,
            invitation_lifetime_seconds: initialized.invitation_lifetime_seconds,
        };
        ctx.object_client::<crate::org_delivery::OrganizationDeliveryClient>(
            ghinvite_core::delivery::delivery_key(
                initialized.request.request_id,
                org_id,
                generation,
            ),
        )
        .control(Json(input))
        .send()
        .await?;
    }
    Ok(())
}

pub(crate) async fn after_initialize(
    ctx: &ObjectContext<'_>,
    initialized: &InitializeRequest,
) -> Result<(), TerminalError> {
    if let Some(Json(input)) = ctx.get::<Json<RequestControl>>(CURRENT).await? {
        bind(&input, initialized)?;
        forward_initialized(ctx, &input, initialized).await?;
    }
    Ok(())
}

/// Current M0 has only generation1, but control routing must follow retained
/// dispatch facts if later work adds reissue. Never guess a next generation.
async fn forward_initialized(
    ctx: &ObjectContext<'_>,
    input: &RequestControl,
    initialized: &InitializeRequest,
) -> Result<(), TerminalError> {
    use ghinvite_core::request_lifecycle::SubmittedCommand;
    let plan = crate::request_reissue::plan(ctx).await?.map(Json);
    let mut submitted = Vec::new();
    for target in &initialized.targets.repos {
        if let Some(Json(command)) = ctx
            .get::<Json<SubmittedCommand>>(&format!("submitted/{}", target.repo_id))
            .await?
        {
            submitted.push(command.command);
        }
    }
    for (repo, generation) in destinations(
        initialized,
        plan.as_ref().map(|j| j.0.commands.as_slice()),
        &submitted,
    )? {
        forward(ctx, input, DeliveryKind::Repository, repo, generation).await?;
    }
    let submitted_org = if initialized.targets.organization.is_some() {
        ctx.get::<Json<ghinvite_core::request_lifecycle::SubmittedOrganizationCommand>>(
            "submitted/organization",
        )
        .await?
        .map(|j| j.0)
    } else {
        None
    };
    for (org, generation) in organization_destinations(
        initialized,
        plan.as_ref().map(|j| &j.0),
        submitted_org.as_ref(),
    )? {
        forward(ctx, input, DeliveryKind::Organization, org, generation).await?;
    }
    Ok(())
}

pub(crate) async fn cascade_manifest(
    ctx: &restate_sdk::context::SharedObjectContext<'_>,
    query: ghinvite_core::revocation::RequestCascadeQuery,
) -> Result<ghinvite_core::revocation::RequestCascadeManifest, TerminalError> {
    use ghinvite_core::{request_lifecycle::SubmittedCommand, revocation::RequestCascadeManifest};
    let initialized = ctx
        .get::<Json<InitializeRequest>>("input")
        .await?
        .ok_or_else(|| TerminalError::new_with_code(503, "request initialization unavailable"))?
        .0;
    if initialized.request.link_id != Some(query.link_id)
        || initialized.request.request_id != query.request_id
    {
        return Err(conflict());
    }
    let current = crate::request_reissue::shared_request(ctx)
        .await?
        .map(Json)
        .ok_or_else(conflict)?
        .0;
    let plan = crate::request_reissue::shared_plan(ctx).await?.map(Json);
    let mut submitted = Vec::new();
    for target in &initialized.targets.repos {
        if let Some(Json(command)) = ctx
            .get::<Json<SubmittedCommand>>(&format!("submitted/{}", target.repo_id))
            .await?
        {
            submitted.push(command.command);
        }
    }
    let mut deliveries: Vec<_> = destinations(
        &initialized,
        plan.as_ref().map(|j| j.0.commands.as_slice()),
        &submitted,
    )?
    .into_iter()
    .map(|(target_id, delivery_generation)| Identity {
        link_id: Some(query.link_id),
        batch: None,
        request_id: query.request_id,
        kind: DeliveryKind::Repository,
        target_id,
        delivery_generation,
    })
    .collect();
    let submitted_org = if initialized.targets.organization.is_some() {
        ctx.get::<Json<ghinvite_core::request_lifecycle::SubmittedOrganizationCommand>>(
            "submitted/organization",
        )
        .await?
        .map(|j| j.0)
    } else {
        None
    };
    deliveries.extend(
        organization_destinations(
            &initialized,
            plan.as_ref().map(|j| &j.0),
            submitted_org.as_ref(),
        )?
        .into_iter()
        .map(|(target_id, delivery_generation)| Identity {
            link_id: Some(query.link_id),
            batch: None,
            request_id: query.request_id,
            kind: DeliveryKind::Organization,
            target_id,
            delivery_generation,
        }),
    );
    let observed_control_revision = ctx
        .get::<Json<RequestControl>>(CURRENT)
        .await?
        .map(|j| j.0.revision)
        .unwrap_or(0);
    Ok(RequestCascadeManifest {
        request_id: query.request_id,
        state: current.state,
        observed_control_revision,
        deliveries,
    })
}

fn organization_destinations(
    initialized: &InitializeRequest,
    planned: Option<&ghinvite_core::request_lifecycle::ApprovedDispatch>,
    submitted: Option<&ghinvite_core::request_lifecycle::SubmittedOrganizationCommand>,
) -> Result<std::collections::BTreeSet<(u64, u64)>, TerminalError> {
    let mut result = std::collections::BTreeSet::new();
    let Some(target) = initialized.targets.organization.as_ref() else {
        if planned.is_some_and(|p| p.organization_command.is_some()) || submitted.is_some() {
            return Err(conflict());
        }
        return Ok(result);
    };
    if planned.is_none() {
        result.insert((target.org_id, 1));
    }
    if planned.is_some_and(|p| p.organization_command.is_none()) {
        return Err(conflict());
    }
    for command in planned
        .and_then(|p| p.organization_command.as_ref())
        .into_iter()
        .chain(submitted.map(|s| &s.command))
    {
        if command.validate().is_err()
            || command.link_id != initialized.request.link_id
            || command.request_id != initialized.request.request_id
            || command.account_id != initialized.request.account_id
            || Some(command.installation_id) != initialized.org_installation_id()
            || command.requester_id != initialized.request.requester_id
            || command.organization != *target
            || command.teams != initialized.targets.teams
            || command.budget_share_pct != initialized.budget_share_pct
        {
            return Err(conflict());
        }
        result.insert((target.org_id, command.delivery_generation));
    }
    Ok(result)
}

fn destinations(
    initialized: &InitializeRequest,
    planned: Option<&[ghinvite_core::delivery::CreateCommand]>,
    submitted: &[ghinvite_core::delivery::CreateCommand],
) -> Result<std::collections::BTreeSet<(u64, u64)>, TerminalError> {
    let mut destinations = std::collections::BTreeSet::new();
    if planned.is_none() {
        destinations.extend(
            initialized
                .targets
                .repos
                .iter()
                .map(|target| (target.repo_id, 1)),
        );
    }
    for command in planned.unwrap_or_default().iter().chain(submitted) {
        if command.link_id != initialized.request.link_id
            || command.request_id != initialized.request.request_id
            || command.account_id != initialized.request.account_id
            || command.installation_id != initialized.installation_id
            || command.requester_id != initialized.request.requester_id
            || command.delivery_generation == 0
            || command.delivery_generation > ghinvite_core::storage::integer::MAX_EXACT
            || !initialized.targets.repos.iter().any(|target| {
                target.repo_id == command.repo_id && target.grant == command.permission.into()
            })
        {
            return Err(conflict());
        }
        destinations.insert((command.repo_id, command.delivery_generation));
    }
    Ok(destinations)
}

pub(crate) async fn pending_cancellation(
    ctx: &ObjectContext<'_>,
) -> Result<Option<RequestControl>, TerminalError> {
    Ok(ctx
        .get::<Json<RequestControl>>(CURRENT)
        .await?
        .map(|j| j.0)
        .filter(|input| {
            matches!(
                input.control,
                LinkControl::Revoked {
                    decline_pending_requests: true,
                    ..
                }
            )
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use ghinvite_core::delivery_control::HoldWindow;
    fn control(revision: u64, hold_epoch: u64, control: LinkControl) -> RequestControl {
        let now = Utc::now();
        RequestControl {
            link_id: ghinvite_core::InvitationLinkId::new(),
            request_id: ghinvite_core::RequestId::new(),
            operation_id: ghinvite_core::RequestId::new()
                .to_string()
                .try_into()
                .unwrap(),
            revision,
            hold_epoch,
            at: now,
            control,
            admitted_at: now - Duration::minutes(1),
            invitation_lifetime_seconds: 604800,
        }
    }
    fn next(old: &RequestControl, revision: u64, control: LinkControl) -> RequestControl {
        RequestControl {
            revision,
            control,
            operation_id: ghinvite_core::RequestId::new()
                .to_string()
                .try_into()
                .unwrap(),
            ..old.clone()
        }
    }
    #[test]
    fn immutable_admission_facts_bind_even_stale_controls() {
        let old = control(5, 0, LinkControl::Active);
        for mut incoming in [
            next(&old, 4, LinkControl::ManualPause),
            next(&old, 6, LinkControl::ManualPause),
        ] {
            incoming.invitation_lifetime_seconds = 3600;
            assert!(evaluate(&incoming, Some(&old)).is_err());
            incoming.invitation_lifetime_seconds = old.invitation_lifetime_seconds;
            incoming.admitted_at += Duration::seconds(1);
            assert!(evaluate(&incoming, Some(&old)).is_err());
            incoming.admitted_at = old.admitted_at;
            incoming.link_id = ghinvite_core::InvitationLinkId::new();
            assert!(evaluate(&incoming, Some(&old)).is_err());
        }
    }
    #[test]
    fn stale_and_exact_snapshots_are_distinct_from_revision_conflicts() {
        let old = control(5, 0, LinkControl::Active);
        assert_eq!(
            evaluate(&old, Some(&old)).unwrap().outcome,
            ControlOutcome::Applied
        );
        assert_eq!(
            evaluate(&next(&old, 4, LinkControl::ManualPause), Some(&old)).unwrap(),
            ControlReceipt {
                outcome: ControlOutcome::IgnoredStale,
                revision: 5
            }
        );
        assert!(evaluate(&next(&old, 5, LinkControl::Active), Some(&old)).is_err());
        assert!(evaluate(&next(&old, 0, LinkControl::Active), Some(&old)).is_err());
    }
    #[test]
    fn release_fences_delayed_holds_and_revocation_cannot_resurrect_them() {
        let mut held = control(4, 4, LinkControl::Active);
        let window = HoldWindow {
            epoch: 4,
            window_start: held.admitted_at,
            paused_at: held.at,
        };
        held.control = LinkControl::AutoPause {
            window: window.clone(),
        };
        let released = next(&held, 5, LinkControl::Active);
        assert!(evaluate(&released, Some(&held)).is_ok());
        assert_eq!(
            evaluate(&held, Some(&released)).unwrap().outcome,
            ControlOutcome::IgnoredStale
        );
        assert!(evaluate(&next(&released, 6, held.control.clone()), Some(&released)).is_err());
        let revoke = next(
            &released,
            6,
            LinkControl::Revoked {
                cancel_pending_invitations: false,
                decline_pending_requests: false,
                abort_hold: Some(window),
            },
        );
        assert!(evaluate(&revoke, Some(&released)).is_err());
        assert!(
            evaluate(&revoke, Some(&held)).is_ok(),
            "an active hold can be aborted by full snapshot"
        );
        assert!(evaluate(&next(&revoke, 7, LinkControl::Active), Some(&revoke)).is_err());
    }
    #[test]
    fn routing_follows_every_retained_generation_without_inventing_reissues() {
        let control = control(2, 0, LinkControl::Active);
        let initialized: InitializeRequest = serde_json::from_value(serde_json::json!({
            "request":{"request_id":control.request_id,"link_id":control.link_id,"account_id":100,"requester_id":8,"justification":null,"state":"pending","admitted_at":control.admitted_at,"decision_deadline":null,"revision":1},
            "installation_id":1,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]},"approval_required":true,"invitation_lifetime_seconds":604800
        })).unwrap();
        let command = |generation| ghinvite_core::delivery::CreateCommand {
            batch: None,
            budget_attribution: Default::default(),
            access_lifetime_seconds: None,
            budget_share_pct: 50,
            delivery_generation: generation,
            invitation_id: Some(ghinvite_core::GithubInvitationId::new()),
            link_id: Some(control.link_id),
            request_id: control.request_id,
            approval_id: "approval".into(),
            account_id: 100,
            installation_id: 1,
            requester_id: 8,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: ghinvite_core::Permission::Push,
            approved_at: control.admitted_at,
        };
        assert_eq!(
            destinations(&initialized, None, &[])
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            vec![(10, 1)]
        );
        assert_eq!(
            destinations(&initialized, Some(&[command(3)]), &[command(2)])
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            vec![(10, 2), (10, 3)]
        );
        assert_eq!(
            destinations(&initialized, Some(&[command(3)]), &[command(3)])
                .unwrap()
                .len(),
            1
        );
        let mut wrong = command(4);
        wrong.request_id = ghinvite_core::RequestId::new();
        assert!(destinations(&initialized, Some(&[wrong]), &[]).is_err());
        assert!(destinations(&initialized, Some(&[command(0)]), &[]).is_err());
    }
    #[test]
    fn organization_routing_preserves_kind_and_rejects_changed_immutable_targets() {
        use ghinvite_core::request_lifecycle::{ApprovedDispatch, SubmittedOrganizationCommand};
        let control = control(2, 0, LinkControl::Active);
        let initialized: InitializeRequest = serde_json::from_value(serde_json::json!({
            "request":{"request_id":control.request_id,"link_id":control.link_id,"account_id":100,"requester_id":8,"justification":null,"state":"pending","admitted_at":control.admitted_at,"decision_deadline":null,"revision":1},
            "installation_id":1,"organization_installation_id":2,"budget_share_pct":17,
            "targets":{"organization":{"org_id":100,"org_login":"acme","grant":"member"},"teams":[{"team_id":50,"team_slug":"devs","grant":"member"}],"repos":[{"repo_id":100,"repo_full_name":"acme/api","grant":"write"}]},"approval_required":true,"invitation_lifetime_seconds":604800
        })).unwrap();
        assert!(initialized.validate_installations().is_ok());
        assert_eq!(
            organization_destinations(&initialized, None, None).unwrap(),
            [(100, 1)].into()
        );
        assert_eq!(
            destinations(&initialized, None, &[]).unwrap(),
            [(100, 1)].into()
        );
        let command = ghinvite_core::org_delivery::OrganizationCreateCommand {
            batch: None,
            budget_attribution: Default::default(),
            access_lifetime_seconds: None,
            invitation_id: Some(ghinvite_core::GithubInvitationId::new()),
            link_id: Some(control.link_id),
            request_id: control.request_id,
            approval_id: "approval".into(),
            account_id: 100,
            installation_id: 2,
            requester_id: 8,
            organization: initialized.targets.organization.clone().unwrap(),
            teams: initialized.targets.teams.clone(),
            delivery_generation: 3,
            approved_at: control.admitted_at,
            budget_share_pct: 17,
        };
        let plan = ApprovedDispatch {
            dispatch_id: "dispatch".into(),
            input: initialized.clone(),
            commands: vec![],
            organization_command: Some(command.clone()),
        };
        let mut submitted = SubmittedOrganizationCommand {
            command: command.clone(),
            invocation_id: "retained".into(),
        };
        submitted.command.delivery_generation = 2;
        assert_eq!(
            organization_destinations(&initialized, Some(&plan), Some(&submitted)).unwrap(),
            [(100, 2), (100, 3)].into()
        );
        for mutation in 0..5 {
            let mut wrong = plan.clone();
            let c = wrong.organization_command.as_mut().unwrap();
            match mutation {
                0 => c.installation_id = 1,
                1 => c.teams[0].team_id = 51,
                2 => c.organization.org_login = "other".into(),
                3 => c.budget_share_pct = 50,
                _ => c.delivery_generation = 0,
            }
            assert!(
                organization_destinations(&initialized, Some(&wrong), None).is_err(),
                "mutation {mutation}"
            );
        }
        let mut missing = plan.clone();
        missing.organization_command = None;
        assert!(organization_destinations(&initialized, Some(&missing), None).is_err());
        let mut repo_only = initialized;
        repo_only.targets.organization = None;
        repo_only.targets.teams.clear();
        repo_only.organization_installation_id = None;
        assert!(organization_destinations(&repo_only, Some(&plan), None).is_err());
        let value = serde_json::to_value(repo_only).unwrap();
        assert!(
            value.get("organization_installation_id").is_none(),
            "old repo wire input remains unchanged"
        );
    }
}
