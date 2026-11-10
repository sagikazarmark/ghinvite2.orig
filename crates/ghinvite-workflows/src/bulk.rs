//! Retained review and exact submission outbox. No upstream write happens before
//! Submit; ordinary request/email/link owners retain each downstream identity.
pub(crate) mod items;
mod progress;
use crate::{AppState, request_owner::InvitationRequestClient};
use ghinvite_core::{
    audit::EventType,
    bulk::Review,
    bulk_commands::*,
    storage::bulk::{BulkAudit, BulkStorage, Projection},
};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
        RunFuture, SharedObjectContext,
    },
    endpoint::Builder,
    errors::{HandlerError, TerminalError},
    serde::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub struct BulkInviteBatch {
    state: AppState,
}
pub struct BulkProjection {
    storage: Arc<dyn BulkStorage>,
}
pub fn bind(builder: Builder, state: AppState, storage: Arc<dyn BulkStorage>) -> Builder {
    builder
        .bind(BulkInviteBatch { state })
        .bind(BulkProjection { storage })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "command", rename_all = "snake_case")]
enum Operation {
    Create(Box<CreateBatch>),
    Edit(EditBatch),
    Submit(SubmitBatch),
}
impl Operation {
    fn id(&self) -> String {
        String::from(match self {
            Self::Create(c) => c.operation_id.clone(),
            Self::Edit(c) => c.operation_id.clone(),
            Self::Submit(c) => c.operation_id.clone(),
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    input: Operation,
    result: Result<Review, ghinvite_core::bulk::Refusal>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Retained {
    review: Review,
    operation: Receipt,
    events: Vec<BulkAudit>,
    pending: bool,
    #[serde(default)]
    handoff: Handoff,
}
/// Counts only durable downstream enqueue acknowledgements. Older pending heads
/// default to zero and recover already-started email owners by exact command.
#[derive(Clone, Default, Serialize, Deserialize)]
struct Handoff {
    requests: u16,
    emails: u16,
    personal_links: u16,
}
const CURRENT: &str = "batch/current";
const CANCEL: &str = "batch/cancellation";
#[derive(Clone, Serialize, Deserialize)]
struct Cancellation {
    input: CancelBatch,
    review: Review,
    at: chrono::DateTime<chrono::Utc>,
    receipt: Option<CancelBatchReceipt>,
    pending: bool,
}
fn invalid() -> TerminalError {
    TerminalError::new_with_code(400, "invalid bulk command")
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "bulk operation conflict")
}
fn missing() -> TerminalError {
    TerminalError::new_with_code(404, "batch not found")
}
fn refusal(error: ghinvite_core::bulk::Refusal) -> TerminalError {
    TerminalError::new_with_code(409, error.to_string())
}
fn authorize(query: &BatchQuery, review: &Review) -> Result<(), TerminalError> {
    if query.batch_id != review.manifest.batch_id
        || query.admin.account_id != review.manifest.admin.account_id
        || query.admin.user_id == 0
        || query.admin.user_id > ghinvite_core::storage::integer::MAX_EXACT
    {
        return Err(missing());
    }
    Ok(())
}
async fn current(ctx: &ObjectContext<'_>) -> Result<Retained, TerminalError> {
    ctx.get::<Json<Retained>>(CURRENT)
        .await?
        .map(|j| j.0)
        .ok_or_else(missing)
}
fn save(ctx: &ObjectContext<'_>, retained: &Retained) {
    ctx.set(CURRENT, Json(retained.clone()));
}

impl BulkInviteBatch {
    async fn stop(
        &self,
        ctx: &ObjectContext<'_>,
        cancellation: &mut Cancellation,
    ) -> Result<CancelBatchReceipt, TerminalError> {
        if !cancellation.pending {
            let key = format!(
                "batch/cancel/{}",
                String::from(cancellation.input.operation_id.clone())
            );
            ctx.set(&key, Json(cancellation.clone()));
            return cancellation.receipt.clone().ok_or_else(conflict);
        }
        let receipt = if let Some(receipt) = &cancellation.receipt {
            receipt.clone()
        } else {
            let submission = cancellation
                .review
                .submitted
                .as_ref()
                .ok_or_else(conflict)?;
            let mut receipt = CancelBatchReceipt {
                batch_id: cancellation.input.query.batch_id,
                stopped: true,
                aborted: 0,
                already_written: 0,
                already_completed: 0,
                uncertain: 0,
            };
            for row in &submission.requests {
                let command = ghinvite_core::bulk_cancel::CancelRequest {
                    operation_id: String::from(cancellation.input.operation_id.clone())
                        .try_into()
                        .map_err(|_| invalid())?,
                    admin: cancellation.input.query.admin.clone(),
                    input: initialization(&cancellation.review, row).map_err(|_| invalid())?,
                };
                let result = ctx
                    .object_client::<InvitationRequestClient>(row.request_id.to_string())
                    .cancel_batch(Json(command))
                    .call()
                    .await?
                    .0;
                if result.request_id != row.request_id {
                    return Err(conflict());
                }
                receipt.stopped &= result.stopped;
                receipt.aborted += u32::from(result.aborted);
                receipt.already_written += u32::from(result.already_written);
                receipt.already_completed += u32::from(result.already_completed);
                receipt.uncertain += u32::from(result.uncertain);
            }
            for command in &submission.emails {
                use ghinvite_core::email_delivery::EmailCreateOutcome as Outcome;
                let result = ctx
                    .object_client::<crate::email_delivery::EmailInviteDeliveryClient>(
                        command.delivery_key(),
                    )
                    .cancel(Json(command.clone()))
                    .call()
                    .await?
                    .0;
                if result.command != *command {
                    return Err(conflict());
                }
                match result.outcome {
                    Outcome::Aborted {
                        reason: ghinvite_core::delivery::AbortReason::BatchCancelled,
                    } => receipt.aborted += 1,
                    Outcome::Created { .. } => receipt.already_written += 1,
                    Outcome::Failed { .. } | Outcome::Aborted { .. } => {
                        receipt.already_completed += 1
                    }
                    Outcome::OutcomeUnknown => receipt.uncertain += 1,
                    _ => return Err(conflict()),
                }
            }
            cancellation.receipt = Some(receipt.clone());
            ctx.set(CANCEL, Json(cancellation.clone()));
            receipt
        };
        ctx.object_client::<BulkProjectionClient>(ctx.key())
            .apply(Json(Projection {
                review: cancellation.review.clone(),
                events: vec![BulkAudit {
                    operation_id: cancellation.input.operation_id.clone(),
                    actor: cancellation.input.query.admin.clone(),
                    at: cancellation.at,
                    revision: cancellation.review.revision,
                    kind: EventType::BulkBatchCancelled,
                    removed_rows: Default::default(),
                }],
            }))
            .send()
            .await?;
        cancellation.pending = false;
        ctx.set(CANCEL, Json(cancellation.clone()));
        let key = format!(
            "batch/cancel/{}",
            String::from(cancellation.input.operation_id.clone())
        );
        ctx.set(&key, Json(cancellation.clone()));
        Ok(receipt)
    }
    fn capabilities(&self, review: &Review) -> Result<(), TerminalError> {
        let targets = &review.manifest.targets;
        if (targets.organization.is_some() && self.state.organization_delivery.is_none())
            || (review.manifest.access_lifetime_seconds.is_some()
                && !self.state.access_removal_enabled)
            || (review
                .manifest
                .entries
                .iter()
                .any(|r| matches!(r, ghinvite_core::bulk::Entry::Email { .. }))
                && self.state.email_deliveries.is_none())
        {
            return Err(TerminalError::new_with_code(
                503,
                "batch capabilities unavailable",
            ));
        }
        Ok(())
    }
    async fn flush(
        &self,
        ctx: &ObjectContext<'_>,
        retained: &mut Retained,
    ) -> Result<(), TerminalError> {
        if !retained.pending {
            return Ok(());
        }
        // Archive the exact operation receipt before a successor can replace the
        // current record. A kill after the atomic head write still retains it.
        let key = format!("batch/operation/{}", retained.operation.input.id());
        if let Some(Json(old)) = ctx.get::<Json<Receipt>>(&key).await? {
            if old.input != retained.operation.input || old.result != retained.operation.result {
                return Err(conflict());
            }
        } else {
            ctx.set(&key, Json(retained.operation.clone()));
        }
        ctx.object_client::<BulkProjectionClient>(ctx.key())
            .apply(Json(Projection {
                review: retained.review.clone(),
                events: retained.events.clone(),
            }))
            .send()
            .await?;
        if let Operation::Submit(command) = &retained.operation.input {
            let review = &retained.review;
            let submission = review.submitted.as_ref().ok_or_else(conflict)?;
            if usize::from(retained.handoff.requests) > submission.requests.len()
                || usize::from(retained.handoff.emails) > submission.emails.len()
                || usize::from(retained.handoff.personal_links) > submission.personal_links.len()
            {
                return Err(conflict());
            }
            for (index, row) in submission
                .requests
                .iter()
                .enumerate()
                .skip(usize::from(retained.handoff.requests))
            {
                let user = ghinvite_core::User {
                    user_id: row.requester_id,
                    login: row.requester_login.clone(),
                    avatar_url: None,
                    github_created_at: None,
                    last_seen_at: review.rows[usize::from(row.source.row) - 1]
                        .evidence
                        .observed_at,
                };
                ctx.run(|| async {
                    self.state
                        .storage
                        .upsert_user(&user)
                        .await
                        .map_err(HandlerError::from)
                })
                .name("batch_request_profile")
                .await?;
                let input = initialization(review, row).map_err(|_| invalid())?;
                ctx.object_client::<InvitationRequestClient>(row.request_id.to_string())
                    .initialize_batch(Json(input))
                    .send()
                    .await?;
                retained.handoff.requests = u16::try_from(index + 1).map_err(|_| conflict())?;
                save(ctx, retained);
            }
            for (index, email) in submission
                .emails
                .iter()
                .enumerate()
                .skip(usize::from(retained.handoff.emails))
            {
                let dependencies = self.state.email_deliveries.as_ref().ok_or_else(invalid)?;
                let binding = ghinvite_core::storage::email_vault::EmailVaultBinding {
                    email_ref: email.email_ref,
                    batch: email.batch,
                    account_id: email.account_id,
                    installation_id: email.installation_id,
                };
                let owner = ctx.object_client::<crate::email_delivery::EmailInviteDeliveryClient>(
                    email.delivery_key(),
                );
                let existing = owner.status().call().await?.0;
                if let Some(existing) = existing {
                    if existing.command != *email {
                        return Err(conflict());
                    }
                    // A prior enqueue may have completed before the cursor save.
                    // The owner proves the exact handoff even after ciphertext
                    // retirement/purge. Never reactivate that recovery record.
                } else {
                    let at = crate::clock::now(ctx, "batch_email_activation_time").await?;
                    ctx.run(|| async {
                        // A definite false is unavailable recovery, not
                        // a transport failure. The email owner retains its precise
                        // Blocked reason without a fence/write; batch replay and
                        // cancellation must still make progress. Database errors
                        // remain retryable and never masquerade as false.
                        dependencies
                            .records
                            .activate_email(&binding, at.timestamp())
                            .await?;
                        Ok::<(), HandlerError>(())
                    })
                    .name("batch_email_activation")
                    .await?;
                }
                // Also drains a completed child's interrupted projection/retirement.
                owner.create(Json(email.clone())).send().await?;
                retained.handoff.emails = u16::try_from(index + 1).map_err(|_| conflict())?;
                save(ctx, retained);
            }
            for (index, personal) in submission
                .personal_links
                .iter()
                .enumerate()
                .skip(usize::from(retained.handoff.personal_links))
            {
                let create = personal
                    .creation_for_admin(
                        &review.manifest,
                        submission.submitted_at,
                        &command.query.admin,
                    )
                    .map_err(|_| invalid())?;
                ctx.object_client::<crate::admission::InvitationLinkClient>(
                    personal.link_id.to_string(),
                )
                .create(Json(create))
                .send()
                .await?;
                retained.handoff.personal_links =
                    u16::try_from(index + 1).map_err(|_| conflict())?;
                save(ctx, retained);
            }
        }
        retained.pending = false;
        retained.events.clear();
        save(ctx, retained);
        Ok(())
    }
    async fn replay(
        &self,
        ctx: &ObjectContext<'_>,
        operation: &Operation,
    ) -> Result<Option<Review>, TerminalError> {
        if let Some(Json(mut retained)) = ctx.get::<Json<Retained>>(CURRENT).await? {
            self.flush(ctx, &mut retained).await?;
        }
        let key = format!("batch/operation/{}", operation.id());
        if let Some(Json(old)) = ctx.get::<Json<Receipt>>(&key).await? {
            if old.input != *operation {
                return Err(conflict());
            }
            return old.result.map(Some).map_err(refusal);
        }
        Ok(None)
    }
    async fn accept(
        &self,
        ctx: &ObjectContext<'_>,
        operation: Operation,
        review: Review,
        events: Vec<BulkAudit>,
    ) -> Result<Json<Review>, TerminalError> {
        let mut retained = Retained {
            operation: Receipt {
                input: operation,
                result: Ok(review.clone()),
            },
            review: review.clone(),
            events,
            pending: true,
            handoff: Handoff::default(),
        };
        save(ctx, &retained);
        self.flush(ctx, &mut retained).await?;
        Ok(Json(review))
    }
    fn refuse(
        &self,
        ctx: &ObjectContext<'_>,
        operation: Operation,
        error: ghinvite_core::bulk::Refusal,
    ) -> TerminalError {
        let key = format!("batch/operation/{}", operation.id());
        ctx.set(
            &key,
            Json(Receipt {
                input: operation,
                result: Err(error),
            }),
        );
        refusal(error)
    }
}

#[restate_sdk::object]
impl BulkInviteBatch {
    #[handler]
    async fn cancel_remaining(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<CancelBatch>,
    ) -> Result<Json<CancelBatchReceipt>, TerminalError> {
        if ctx.key() != input.query.batch_id.to_string() {
            return Err(missing());
        }
        let mut retained = current(&ctx).await?;
        authorize(&input.query, &retained.review)?;
        self.flush(&ctx, &mut retained).await?;
        if retained.review.submitted.is_none() {
            return Err(TerminalError::new_with_code(409, "batch not submitted"));
        }
        // Drain an interrupted stop before a different operation can replace its
        // retained aggregate. Child stop receipts bind every command exactly.
        if let Some(Json(mut old)) = ctx.get::<Json<Cancellation>>(CANCEL).await? {
            self.stop(&ctx, &mut old).await?;
        }
        let key = format!("batch/cancel/{}", String::from(input.operation_id.clone()));
        if let Some(Json(old)) = ctx.get::<Json<Cancellation>>(&key).await? {
            if old.input != input {
                return Err(conflict());
            }
            return Ok(Json(old.receipt.ok_or_else(conflict)?));
        }
        let mut cancellation = Cancellation {
            input,
            review: retained.review,
            at: crate::clock::now(&ctx, "bulk_cancel_time").await?,
            receipt: None,
            pending: true,
        };
        ctx.set(CANCEL, Json(cancellation.clone()));
        Ok(Json(self.stop(&ctx, &mut cancellation).await?))
    }
    #[handler]
    async fn on_item_progress(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ItemProgressNotice>,
    ) -> Result<(), TerminalError> {
        match items::observe(&ctx, input.clone()).await {
            Err(error) if error.code() >= 500 => {
                // A call error is itself journaled. A fresh one-way invocation
                // can read the recovered owner instead of replaying that error.
                ctx.object_client::<BulkInviteBatchClient>(ctx.key())
                    .on_item_progress(Json(input))
                    .send_after(std::time::Duration::from_secs(60))
                    .await?;
                Ok(())
            }
            result => result,
        }
    }

    #[handler]
    async fn review(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<BatchQuery>,
    ) -> Result<Json<Review>, TerminalError> {
        if ctx.key() != query.batch_id.to_string() {
            return Err(missing());
        }
        let retained = ctx
            .get::<Json<Retained>>(CURRENT)
            .await?
            .map(|j| j.0)
            .ok_or_else(missing)?;
        authorize(&query, &retained.review)?;
        Ok(Json(retained.review))
    }

    #[handler]
    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<CreateBatch>,
    ) -> Result<Json<Review>, TerminalError> {
        input.review.validate().map_err(|_| invalid())?;
        if ctx.key() != input.review.manifest.batch_id.to_string()
            || input.review.revision != 1
            || input.review.submitted.is_some()
            || !input.review.removed.is_empty()
            || !input.review.personal_link_rows.is_empty()
        {
            return Err(invalid());
        }
        self.capabilities(&input.review)?;
        let operation = Operation::Create(Box::new(input.clone()));
        if let Some(result) = self.replay(&ctx, &operation).await? {
            return Ok(Json(result));
        }
        if ctx.get::<Json<Retained>>(CURRENT).await?.is_some() {
            return Err(conflict());
        }
        let at = crate::clock::now(&ctx, "bulk_create_time").await?;
        if input.review.reviewed_at > at {
            return Err(invalid());
        }
        let event = BulkAudit {
            operation_id: input.operation_id,
            actor: input.review.manifest.admin.clone(),
            at,
            revision: 1,
            kind: EventType::BulkBatchCreated,
            removed_rows: Default::default(),
        };
        self.accept(&ctx, operation, input.review, vec![event])
            .await
    }
    #[handler]
    async fn remove_rows(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<EditBatch>,
    ) -> Result<Json<Review>, TerminalError> {
        if ctx.key() != input.query.batch_id.to_string() {
            return Err(missing());
        }
        let operation = Operation::Edit(input.clone());
        let mut retained = current(&ctx).await?;
        authorize(&input.query, &retained.review)?;
        if let Some(result) = self.replay(&ctx, &operation).await? {
            return Ok(Json(result));
        }
        retained = current(&ctx).await?;
        let removed = input
            .remove
            .difference(&retained.review.removed)
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        if let Err(error) = retained.review.edit(
            input.expected_revision,
            input.remove,
            input.personal_link_rows,
        ) {
            return Err(self.refuse(&ctx, operation, error));
        }
        let events = if removed.is_empty() {
            vec![]
        } else {
            vec![BulkAudit {
                operation_id: input.operation_id,
                actor: input.query.admin,
                at: crate::clock::now(&ctx, "bulk_edit_time").await?,
                revision: retained.review.revision,
                kind: EventType::BulkBatchRowsRemoved,
                removed_rows: removed,
            }]
        };
        self.accept(&ctx, operation, retained.review, events).await
    }
    #[handler]
    async fn submit(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<SubmitBatch>,
    ) -> Result<Json<Review>, TerminalError> {
        if ctx.key() != input.query.batch_id.to_string() {
            return Err(missing());
        }
        let operation = Operation::Submit(input.clone());
        let mut retained = current(&ctx).await?;
        authorize(&input.query, &retained.review)?;
        if input.query.admin.user_id != input.submission.submitted_by {
            return Err(invalid());
        }
        if let Some(result) = self.replay(&ctx, &operation).await? {
            return Ok(Json(result));
        }
        retained = current(&ctx).await?;
        self.capabilities(&retained.review)?;
        let at = crate::clock::now(&ctx, "bulk_submit_time").await?;
        if input.submission.submitted_at > at {
            return Err(invalid());
        }
        if let Err(error) = retained
            .review
            .submit(input.expected_revision, input.submission.clone())
        {
            return Err(self.refuse(&ctx, operation, error));
        }
        let event = BulkAudit {
            operation_id: input.operation_id,
            actor: input.query.admin,
            at: input.submission.submitted_at,
            revision: retained.review.revision,
            kind: EventType::BulkBatchSubmitted,
            removed_rows: Default::default(),
        };
        self.accept(&ctx, operation, retained.review, vec![event])
            .await
    }
    #[handler]
    async fn progress(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(query): Json<BatchQuery>,
    ) -> Result<Json<BatchProgress>, TerminalError> {
        progress::page(
            ctx,
            ProgressPageQuery {
                query,
                cursor: None,
            },
        )
        .await
    }
    #[handler]
    async fn progress_page(
        &self,
        ctx: SharedObjectContext<'_>,
        Json(input): Json<ProgressPageQuery>,
    ) -> Result<Json<BatchProgress>, TerminalError> {
        progress::page(ctx, input).await
    }
}

#[restate_sdk::object]
impl BulkProjection {
    #[handler]
    async fn apply(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<Projection>,
    ) -> Result<(), TerminalError> {
        if ctx.key() != input.review.manifest.batch_id.to_string() {
            return Err(invalid());
        }
        ctx.run(|| async {
            self.storage
                .project_bulk_projection(&input)
                .await
                .map_err(HandlerError::from)
        })
        .name("apply_bulk_projection")
        .await
    }
}
