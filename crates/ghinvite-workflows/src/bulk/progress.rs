//! Bounded owner observations. The cursor indexes the immutable submission,
//! never a guessed delivery generation or a projected delivery result.
use super::*;
const MAX_CALLS: usize = 64;
enum Row<'a> {
    Request(&'a ghinvite_core::bulk::RowRequest),
    Email(&'a ghinvite_core::email_delivery::EmailCreateCommand),
}
impl Row<'_> {
    fn number(&self) -> u16 {
        match self {
            Self::Request(row) => row.source.row,
            Self::Email(command) => command.batch.row,
        }
    }
}
fn cursor(review: &Review, row: u16, target_offset: usize) -> ProgressCursor {
    ProgressCursor {
        review_revision: review.revision,
        row,
        target_offset: target_offset as u16,
    }
}
pub(super) async fn page(
    ctx: SharedObjectContext<'_>,
    input: ProgressPageQuery,
) -> Result<Json<BatchProgress>, TerminalError> {
    let retained = ctx
        .get::<Json<Retained>>(CURRENT)
        .await?
        .map(|j| j.0)
        .ok_or_else(missing)?;
    authorize(&input.query, &retained.review)?;
    if ctx.key() != input.query.batch_id.to_string() {
        return Err(missing());
    }
    let review = &retained.review;
    let mut rows = vec![];
    if let Some(submission) = &review.submitted {
        rows.extend(submission.requests.iter().map(Row::Request));
        rows.extend(submission.emails.iter().map(Row::Email));
    }
    rows.sort_by_key(Row::number);
    let start = if let Some(c) = &input.cursor {
        if c.review_revision != review.revision {
            return Err(conflict());
        }
        let index = rows
            .iter()
            .position(|row| row.number() == c.row)
            .ok_or_else(invalid)?;
        let count = match rows[index] {
            Row::Request(_) => {
                review.manifest.targets.repos.len()
                    + usize::from(review.manifest.targets.organization.is_some())
            }
            Row::Email(_) => 1,
        };
        if usize::from(c.target_offset) >= count {
            return Err(invalid());
        }
        if matches!(rows[index], Row::Email(_)) && c.target_offset != 0 {
            return Err(invalid());
        }
        index
    } else {
        0
    };
    let mut calls = 0;
    let mut requests = vec![];
    let mut emails = vec![];
    let mut next_cursor = None;
    for (index, row) in rows.iter().enumerate().skip(start) {
        let offset = if index == start {
            input
                .cursor
                .as_ref()
                .map_or(0, |c| usize::from(c.target_offset))
        } else {
            0
        };
        if calls == MAX_CALLS {
            next_cursor = Some(cursor(review, row.number(), offset));
            break;
        }
        match row {
            Row::Request(row) => {
                calls += 1;
                let mut progress = RequestProgress {
                    source: row.source,
                    request_id: row.request_id,
                    initialized: Observation::Pending,
                    final_results: vec![],
                    repositories: vec![],
                    organization: None,
                    next_target_offset: None,
                };
                match ctx
                    .object_client::<InvitationRequestClient>(row.request_id.to_string())
                    .admin_dispatch(Json(ghinvite_core::request_lifecycle::AdminRequestStatus {
                        request_id: row.request_id,
                        admin: input.query.admin.clone(),
                    }))
                    .call()
                    .await
                {
                    Ok(Json(Some(plan))) => {
                        let expected = ghinvite_core::bulk_commands::initialization(review, row)
                            .map_err(|_| conflict())?;
                        if !items::matches_initialization(&plan.input, &expected)
                            || plan.commands.len() != expected.targets.repos.len()
                            || plan.organization_command.is_some()
                                != expected.targets.organization.is_some()
                        {
                            return Err(conflict());
                        }
                        for (command, target) in plan.commands.iter().zip(&expected.targets.repos) {
                            if command.request_id != row.request_id
                                || command.batch != Some(row.source)
                                || command.link_id.is_some()
                                || command.account_id != expected.request.account_id
                                || command.installation_id != expected.installation_id
                                || command.requester_id != row.requester_id
                                || command.repo_id != target.repo_id
                                || command.repo_full_name != target.repo_full_name
                                || command.permission != target.grant.into()
                                || command.delivery_generation == 0
                            {
                                return Err(conflict());
                            }
                        }
                        if let Some(command) = &plan.organization_command
                            && (command.validate().is_err()
                                || command.request_id != row.request_id
                                || command.batch != Some(row.source)
                                || command.link_id.is_some()
                                || command.account_id != expected.request.account_id
                                || command.requester_id != row.requester_id
                                || Some(command.installation_id) != expected.org_installation_id()
                                || Some(&command.organization)
                                    != expected.targets.organization.as_ref()
                                || command.teams != expected.targets.teams)
                        {
                            return Err(conflict());
                        }
                        if let Some(Json(items::Item::Request {
                            request_id,
                            targets,
                            results,
                        })) = ctx
                            .get::<Json<items::Item>>(&items::key(row.source.row))
                            .await?
                        {
                            // A delayed reset callback cannot present old results as current.
                            if request_id == row.request_id && targets == items::targets(&plan) {
                                progress.final_results = results;
                            }
                        }
                        progress.initialized = Observation::Observed(());
                        let count =
                            plan.commands.len() + usize::from(plan.organization_command.is_some());
                        for target_index in offset..count {
                            if calls == MAX_CALLS {
                                progress.next_target_offset = Some(target_index as u16);
                                next_cursor = Some(cursor(review, row.source.row, target_index));
                                break;
                            }
                            calls += 1;
                            if let Some(command) = plan.commands.get(target_index) {
                                let observation = match ctx
                                    .object_client::<crate::delivery::RepositoryDeliveryClient>(
                                        command.delivery_key(),
                                    )
                                    .status()
                                    .call()
                                    .await
                                {
                                    Ok(Json(Some(snapshot)))
                                        if snapshot.create.command == *command =>
                                    {
                                        Observation::Observed(snapshot)
                                    }
                                    Ok(Json(Some(_))) => return Err(conflict()),
                                    Ok(Json(None)) => Observation::Pending,
                                    Err(_) => Observation::Unavailable,
                                };
                                progress.repositories.push(RepositoryProgress {
                                    repo_id: command.repo_id,
                                    delivery_generation: command.delivery_generation,
                                    observation,
                                });
                            } else {
                                let command =
                                    plan.organization_command.as_ref().ok_or_else(conflict)?;
                                progress.organization = Some(match ctx.object_client::<crate::org_delivery::OrganizationDeliveryClient>(command.delivery_key()).status().call().await {
                                    Ok(Json(Some(snapshot))) if snapshot.create.command == *command => Observation::Observed(snapshot),
                                    Ok(Json(Some(_))) => return Err(conflict()),
                                    Ok(Json(None)) => Observation::Pending, Err(_) => Observation::Unavailable,
                                });
                            }
                        }
                    }
                    Ok(Json(None)) => {}
                    Err(_) => progress.initialized = Observation::Unavailable,
                }
                requests.push(progress);
            }
            Row::Email(command) => {
                calls += 1;
                let observation = match ctx
                    .object_client::<crate::email_delivery::EmailInviteDeliveryClient>(
                        command.delivery_key(),
                    )
                    .status()
                    .call()
                    .await
                {
                    Ok(Json(Some(snapshot))) if snapshot.command == **command => {
                        Observation::Observed(snapshot)
                    }
                    Ok(Json(Some(_))) => return Err(conflict()),
                    Ok(Json(None)) => Observation::Pending,
                    Err(_) => Observation::Unavailable,
                };
                emails.push(EmailProgress {
                    source: command.batch,
                    observation,
                });
            }
        }
        if next_cursor.is_some() {
            break;
        }
    }
    let cancellation = ctx.get::<Json<Cancellation>>(CANCEL).await?.map(|j| j.0);
    Ok(Json(BatchProgress {
        review: retained.review,
        handoff_pending: retained.pending,
        requests,
        emails,
        next_cursor,
        cancellation_pending: cancellation.as_ref().is_some_and(|c| c.pending),
        cancellation: cancellation.and_then(|c| c.receipt),
    }))
}
