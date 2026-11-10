//! Notifications are wakeups. Only current, command-bound owner reads supply facts.
use super::*;
use ghinvite_core::{
    bulk_commands::*,
    request_completion::{Target, TargetKind, TargetResult},
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn key(row: u16) -> String {
    format!("batch/item/{row}")
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) enum Item {
    Request {
        request_id: ghinvite_core::RequestId,
        targets: BTreeSet<Target>,
        results: Vec<TargetResult>,
    },
    Email(Box<ghinvite_core::email_delivery::EmailDeliverySnapshot>),
}
pub(super) fn targets(
    plan: &ghinvite_core::request_lifecycle::ApprovedDispatch,
) -> BTreeSet<Target> {
    let mut targets: BTreeSet<_> = plan
        .commands
        .iter()
        .map(|c| Target {
            kind: TargetKind::Repository,
            id: c.repo_id,
            generation: c.delivery_generation,
        })
        .collect();
    if let Some(c) = &plan.organization_command {
        targets.insert(Target {
            kind: TargetKind::Organization,
            id: c.organization.org_id,
            generation: c.delivery_generation,
        });
        targets.extend(c.teams.iter().map(|t| Target {
            kind: TargetKind::Team,
            id: t.team_id,
            generation: c.delivery_generation,
        }));
    }
    targets
}
/// Reissue advances lifecycle revision while preserving original approval and
/// every source/target/guardrail field. Those fields remain exact-match inputs.
pub(super) fn matches_initialization(
    actual: &ghinvite_core::request_lifecycle::InitializeRequest,
    expected: &ghinvite_core::request_lifecycle::InitializeRequest,
) -> bool {
    if actual.request.state != ghinvite_core::RequestState::Approved
        || actual.request.revision < expected.request.revision
    {
        return false;
    }
    let mut normalized = actual.clone();
    normalized.request.revision = expected.request.revision;
    normalized == *expected
}
pub(super) async fn observe(
    ctx: &ObjectContext<'_>,
    input: ItemProgressNotice,
) -> Result<(), TerminalError> {
    let source = input.source();
    if source.validate().is_err() || ctx.key() != source.batch_id.to_string() {
        return Err(invalid());
    }
    let retained = current(ctx).await?;
    let review = &retained.review;
    let submission = review.submitted.as_ref().ok_or_else(conflict)?;
    let next = match input {
        ItemProgressNotice::Request {
            source,
            request_id,
            delivery,
        } => {
            let row = submission
                .requests
                .iter()
                .find(|r| r.source == source && r.request_id == request_id)
                .ok_or_else(conflict)?;
            let Some(plan) = ctx
                .object_client::<InvitationRequestClient>(request_id.to_string())
                .admin_dispatch(Json(ghinvite_core::request_lifecycle::AdminRequestStatus {
                    request_id,
                    admin: review.manifest.admin.clone(),
                }))
                .call()
                .await?
                .0
            else {
                return Ok(());
            };
            let expected = initialization(review, row).map_err(|_| conflict())?;
            if !matches_initialization(&plan.input, &expected) {
                return Err(conflict());
            }
            let expected_targets = targets(&plan);
            let mut observed = vec![];
            if let Some(notice) = delivery {
                use ghinvite_core::delivery_control::DeliveryKind;
                match notice.kind {
                    DeliveryKind::Repository => {
                        let Some(c) = plan.commands.iter().find(|c| {
                            c.repo_id == notice.target_id
                                && c.delivery_generation == notice.delivery_generation
                        }) else {
                            return Ok(());
                        };
                        if let Some(snapshot) = ctx
                            .object_client::<crate::delivery::RepositoryDeliveryClient>(
                                c.delivery_key(),
                            )
                            .status()
                            .call()
                            .await?
                            .0
                        {
                            if snapshot.create.command != *c {
                                return Err(conflict());
                            }
                            crate::delivery::validate_notice_snapshot(
                                &c.delivery_key(),
                                &snapshot,
                            )?;
                            observed
                                .extend(ghinvite_core::request_completion::repository(&snapshot));
                        }
                    }
                    DeliveryKind::Organization => {
                        let Some(c) = plan.organization_command.as_ref().filter(|c| {
                            c.organization.org_id == notice.target_id
                                && c.delivery_generation == notice.delivery_generation
                        }) else {
                            return Ok(());
                        };
                        if let Some(snapshot) = ctx
                            .object_client::<crate::org_delivery::OrganizationDeliveryClient>(
                                c.delivery_key(),
                            )
                            .status()
                            .call()
                            .await?
                            .0
                        {
                            if snapshot.create.command != *c {
                                return Err(conflict());
                            }
                            snapshot.validate().map_err(|_| conflict())?;
                            observed = ghinvite_core::request_completion::organization(&snapshot);
                        }
                    }
                }
            }
            let mut results = BTreeMap::new();
            if let Some(Json(Item::Request {
                request_id: old,
                results: old_results,
                ..
            })) = ctx.get::<Json<Item>>(&key(source.row)).await?
            {
                if old != request_id {
                    return Err(conflict());
                }
                results.extend(
                    old_results
                        .into_iter()
                        .filter(|r| expected_targets.contains(&r.target))
                        .map(|r| (r.target, r.result)),
                );
            }
            for r in observed {
                if !expected_targets.contains(&r.target)
                    || results.get(&r.target).is_some_and(|old| *old != r.result)
                {
                    return Err(conflict());
                }
                results.insert(r.target, r.result);
            }
            Item::Request {
                request_id,
                targets: expected_targets,
                results: results
                    .into_iter()
                    .map(|(target, result)| TargetResult { target, result })
                    .collect(),
            }
        }
        ItemProgressNotice::Email { source } => {
            let command = submission
                .emails
                .iter()
                .find(|c| c.batch == source)
                .ok_or_else(conflict)?;
            let Some(snapshot) = ctx
                .object_client::<crate::email_delivery::EmailInviteDeliveryClient>(
                    command.delivery_key(),
                )
                .status()
                .call()
                .await?
                .0
            else {
                return Ok(());
            };
            snapshot.validate().map_err(|_| conflict())?;
            if snapshot.command != *command {
                return Err(conflict());
            }
            if let Some(Json(Item::Email(old))) = ctx.get::<Json<Item>>(&key(source.row)).await? {
                if old.command != snapshot.command
                    || (old.revision == snapshot.revision && *old != snapshot)
                {
                    return Err(conflict());
                }
                if old.revision > snapshot.revision {
                    return Ok(());
                }
            }
            Item::Email(Box::new(snapshot))
        }
    };
    ctx.set(&key(source.row), Json(next));
    Ok(())
}

pub(crate) async fn notify_request(
    ctx: &ObjectContext<'_>,
    request: &ghinvite_core::request_lifecycle::RequestSnapshot,
    delivery: Option<ghinvite_core::request_completion::DeliveryNotice>,
) -> Result<(), TerminalError> {
    if let Some(source) = request.batch {
        ctx.object_client::<super::BulkInviteBatchClient>(source.batch_id.to_string())
            .on_item_progress(Json(ItemProgressNotice::Request {
                source,
                request_id: request.request_id,
                delivery,
            }))
            .send()
            .await?;
    }
    Ok(())
}
