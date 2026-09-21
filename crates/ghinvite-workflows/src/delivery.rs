//! Receiving-side create receipts. Private, versioned ingress; ADR 0004.
use crate::AppState;
use ghinvite_core::{
    InvitationState,
    delivery::{CreateCommand, CreateOutcome, CreateReceipt},
    storage::Error,
};
use restate_sdk::{
    context::{
        Context, ContextClient, ContextReadState, ContextSideEffects, ContextWriteState,
        ObjectContext, RunFuture, SharedObjectContext,
    },
    endpoint::Builder,
    errors::{HandlerError, TerminalError},
    serde::Json,
};

#[restate_sdk::object]
pub trait GithubCreate {
    async fn create(input: Json<CreateCommand>) -> Result<Json<CreateReceipt>, TerminalError>;
    async fn recheck(input: Json<CreateCommand>) -> Result<(), TerminalError>;
    #[shared]
    async fn status() -> Result<Json<Option<CreateReceipt>>, TerminalError>;
}

pub struct GithubCreateImpl {
    state: AppState,
    #[cfg(feature = "integration")]
    faults: Option<std::sync::Arc<DeliveryFaults>>,
}

#[cfg(feature = "integration")]
#[derive(Default)]
pub struct DeliveryFaults {
    pub lose_http_result: std::sync::atomic::AtomicBool,
    pub lose_projection_ack: std::sync::atomic::AtomicBool,
    pub http_result_losses: std::sync::atomic::AtomicUsize,
}

pub fn bind(builder: Builder, state: AppState) -> Builder {
    builder
        .bind(
            GithubCreateImpl {
                state,
                #[cfg(feature = "integration")]
                faults: None,
            }
            .serve(),
        )
        .bind(DeliveryRecoveryImpl.serve())
}

#[cfg(feature = "integration")]
pub fn bind_with_faults(
    builder: Builder,
    state: AppState,
    faults: std::sync::Arc<DeliveryFaults>,
) -> Builder {
    builder
        .bind(
            GithubCreateImpl {
                state,
                faults: Some(faults),
            }
            .serve(),
        )
        .bind(DeliveryRecoveryImpl.serve())
}

#[restate_sdk::service]
pub trait DeliveryRecovery {
    async fn recover(
        input: Json<crate::admission::RequestStatus>,
    ) -> Result<Json<crate::request_lifecycle::DeliveryStatus>, TerminalError>;
}
pub struct DeliveryRecoveryImpl;
impl DeliveryRecovery for DeliveryRecoveryImpl {
    async fn recover(
        &self,
        ctx: Context<'_>,
        Json(query): Json<crate::admission::RequestStatus>,
    ) -> Result<Json<crate::request_lifecycle::DeliveryStatus>, TerminalError> {
        let link =
            ctx.object_client::<crate::admission::InvitationLinkClient>(query.link_id.to_string());
        let Json(plan) = link.prepare_dispatch(Json(query.clone())).call().await?;
        for command in &plan.commands {
            let receiver =
                ctx.object_client::<GithubCreateClient>(command.invitation_id.to_string());
            let Json(receipt) = receiver.status().call().await?;
            if let Some(receipt) = receipt
                && receipt.command != *command
            {
                return Err(TerminalError::new_with_code(
                    409,
                    "receiving identity conflict",
                ));
            }
            // Confirmed replay also repairs a missing SQL projection. It
            // cannot reissue PUT; uncertain replay only reconciles.
            let invocation_id = receiver
                .create(Json(command.clone()))
                .send()
                .await?
                .invocation_id()
                .to_owned();
            link.record_submitted(Json(crate::request_lifecycle::SubmittedCommand {
                command: command.clone(),
                invocation_id,
            }))
            .call()
            .await?;
        }
        link.delivery_status(Json(query)).call().await
    }
}

impl GithubCreate for GithubCreateImpl {
    async fn recheck(
        &self,
        ctx: ObjectContext<'_>,
        Json(command): Json<CreateCommand>,
    ) -> Result<(), TerminalError> {
        let previous = ctx
            .get::<Json<CreateCommand>>("v1/input")
            .await?
            .map(|c| c.0);
        if ctx.key() != command.invitation_id.to_string() || previous.as_ref() != Some(&command) {
            return Err(TerminalError::new_with_code(
                409,
                "recheck identity conflict",
            ));
        }
        ctx.clear("v1/recheck_scheduled");
        ctx.object_client::<GithubCreateClient>(command.invitation_id.to_string())
            .create(Json(command))
            .send()
            .await?;
        Ok(())
    }
    async fn status(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<CreateReceipt>>, TerminalError> {
        Ok(Json(
            ctx.get::<Json<CreateReceipt>>("v1/receipt")
                .await?
                .map(|r| r.0),
        ))
    }

    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        Json(command): Json<CreateCommand>,
    ) -> Result<Json<CreateReceipt>, TerminalError> {
        if ctx.key() != command.invitation_id.to_string() || command.requester_id == 0 {
            return Err(TerminalError::new_with_code(400, "invalid create command"));
        }
        if let Some(Json(old)) = ctx.get::<Json<CreateCommand>>("v1/input").await?
            && old != command
        {
            return Err(TerminalError::new_with_code(409, "create input conflict"));
        }
        if command.version != 1 {
            return Err(TerminalError::new_with_code(
                400,
                "unsupported command version",
            ));
        }
        let mut previous = ctx
            .get::<Json<CreateReceipt>>("v1/receipt")
            .await?
            .map(|r| r.0);
        if let Some(receipt) = &mut previous
            && receipt.outcome.confirmed()
        {
            retain_recovery_time(&ctx, receipt).await?;
            ctx.run(|| async { project(&self.state, receipt).await })
                .name("repair_create_projection")
                .await?;
            return Ok(Json(receipt.clone()));
        }
        let Json(plan) = ctx
            .object_client::<crate::admission::InvitationLinkClient>(command.link_id.to_string())
            .prepare_dispatch(Json(crate::admission::RequestStatus {
                link_id: command.link_id,
                request_id: command.request_id,
                requester_id: command.requester_id,
            }))
            .call()
            .await?;
        if !plan.commands.contains(&command) {
            return Err(TerminalError::new_with_code(
                409,
                "command differs from approved plan",
            ));
        }
        ctx.set("v1/input", Json(command.clone()));
        let Json(Attempt {
            mut receipt,
            throttled_for_secs,
        }) = ctx
            .run(|| async {
                let attempted = attempt(&self.state, &command, previous.is_none()).await?;
                #[cfg(feature = "integration")]
                if let Some(faults) = &self.faults {
                    use std::sync::atomic::Ordering;
                    if attempted.receipt.outcome.confirmed()
                        && faults.lose_http_result.swap(false, Ordering::SeqCst)
                    {
                        faults.http_result_losses.fetch_add(1, Ordering::SeqCst);
                        return Err(std::io::Error::other(
                            "fixture: GitHub result lost before journal acknowledgement",
                        )
                        .into());
                    }
                }
                Ok::<_, HandlerError>(Json(attempted))
            })
            .name("guarded_github_create")
            .await?;
        receipt.revision = previous.as_ref().map_or(1, |r| r.revision + 1);
        if matches!(
            previous.as_ref().map(|r| &r.outcome),
            Some(CreateOutcome::OutcomeUnknown)
        ) && matches!(receipt.outcome, CreateOutcome::Blocked { .. })
        {
            receipt.outcome = CreateOutcome::OutcomeUnknown;
        }
        ctx.set("v1/receipt", Json(receipt.clone()));
        ctx.run(|| async {
            project(&self.state, &receipt).await?;
            #[cfg(feature = "integration")]
            if let Some(faults) = &self.faults {
                if faults
                    .lose_projection_ack
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(std::io::Error::other(
                        "fixture: SQL committed but acknowledgement lost",
                    )
                    .into());
                }
            }
            Ok::<_, HandlerError>(())
        })
        .name("project_create_receipt")
        .await?;
        // A throttle clears on GitHub's own schedule, usually long before a
        // missing dependency does, so it rechecks on the bounded wait GitHub
        // asked for. It earns that recheck even where the outcome stayed
        // unknown: rereading is safe, and it is the only way a delivery GitHub
        // would not let us read ever resolves on its own. Either way the object
        // lock is released first — the wait is a continuation, not a held retry.
        // Exactly one recheck stands per invitation. A sooner one does not
        // supersede a pending later one: a timer already sent cannot be
        // withdrawn, so superseding means two timers, and telling which is
        // stale needs a due time and a token on `recheck` — retained protocol,
        // for a case only explicit recovery during a blocked window reaches.
        // It costs that recovery the pending hour; it settles nothing wrongly.
        let blocked = matches!(receipt.outcome, CreateOutcome::Blocked { .. });
        if (blocked || throttled_for_secs.is_some())
            && ctx.get::<bool>("v1/recheck_scheduled").await?.is_none()
        {
            let wait =
                throttled_for_secs.map_or(BLOCKED_RECHECK_INTERVAL, std::time::Duration::from_secs);
            ctx.set("v1/recheck_scheduled", true);
            ctx.object_client::<GithubCreateClient>(command.invitation_id.to_string())
                .recheck(Json(command))
                .send_after(wait)
                .await?;
        }
        Ok(Json(receipt))
    }
}

/// One create attempt's receipt, plus the bounded wait to recheck after when
/// GitHub throttled it rather than deciding it.
///
/// The receipt is flattened so this reads back from journals written before
/// throttling was classified: their `guarded_github_create` value is a bare
/// receipt, which deserializes here with no wait.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Attempt {
    #[serde(flatten)]
    receipt: CreateReceipt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    throttled_for_secs: Option<u64>,
}

impl From<CreateReceipt> for Attempt {
    fn from(receipt: CreateReceipt) -> Self {
        Self {
            receipt,
            throttled_for_secs: None,
        }
    }
}

impl Attempt {
    /// Pair a receipt with the bounded wait GitHub's throttling guidance
    /// allows. Only a receipt reporting no confirmed outcome carries one, so a
    /// wait can never shorten the life of a settled delivery.
    fn throttled_by(receipt: CreateReceipt, error: &ghinvite_github::Error) -> Self {
        let retry_after = error.rate_limit().and_then(|limit| limit.retry_after);
        Self {
            throttled_for_secs: (!receipt.outcome.confirmed())
                .then(|| crate::throttle::backoff(retry_after).as_secs()),
            receipt,
        }
    }
}

/// How long a create waits before rechecking the dependency that blocked it.
/// Throttling replaces this with GitHub's own guidance, which the same bound
/// caps, so an unexplained block never rechecks more slowly than a named one.
const BLOCKED_RECHECK_INTERVAL: std::time::Duration = crate::throttle::MAXIMUM;

async fn attempt(
    state: &AppState,
    command: &CreateCommand,
    has_no_retained_receipt: bool,
) -> Result<Attempt, HandlerError> {
    let dependency = || Error::ProjectionDependency;
    let request = state
        .storage
        .get_invitation_request(command.request_id)
        .await?
        .ok_or_else(dependency)?;
    let link = state
        .storage
        .get_invitation_link_by_id(command.link_id)
        .await?
        .ok_or_else(dependency)?;
    state
        .storage
        .get_user(command.requester_id)
        .await?
        .ok_or_else(dependency)?;
    if request.invitation_link_id != command.link_id
        || request.requester_id != command.requester_id
        || link.account_id != command.account_id
        || link.permission != command.permission
        || !link
            .repos
            .iter()
            .any(|r| r.repo_id == command.repo_id && r.repo_full_name == command.repo_full_name)
    {
        return Err(TerminalError::new_with_code(409, "projected create identity conflict").into());
    }
    if request.state != ghinvite_core::RequestState::Approved {
        return Err(Error::ProjectionDependency.into());
    }
    if let Some(existing) = state
        .storage
        .get_github_invitation(command.invitation_id)
        .await?
    {
        if existing.invitation_request_id != command.request_id
            || existing.repo_id != command.repo_id
        {
            return Err(TerminalError::new_with_code(409, "imported invitation conflict").into());
        }
        // Historical Sent + upstream ID is affirmative create evidence. Other
        // lifecycle states alone cannot reconstruct an original create outcome.
        if let Some(upstream_id) = existing.github_invitation_id {
            return Ok(CreateReceipt {
                command: command.clone(),
                outcome: CreateOutcome::Created { upstream_id },
                revision: 1,
                confirmed_at: Some(chrono::Utc::now()),
                recovered: true,
            }
            .into());
        }
        // Fence ambiguous Sending/terminal rows before reconciliation.
        if has_no_retained_receipt {
            state.storage.claim_delivery_attempt(command).await?;
        }
    }
    let attempted = state
        .storage
        .delivery_attempt_exists(command.invitation_id)
        .await?;
    let blocked = |reason: &str| CreateReceipt {
        command: command.clone(),
        outcome: if attempted {
            CreateOutcome::OutcomeUnknown
        } else {
            CreateOutcome::Blocked {
                reason: reason.into(),
            }
        },
        revision: 1,
        confirmed_at: None,
        recovered: false,
    };
    let Some(account) = state
        .storage
        .get_active_installation_by_account_id(command.account_id)
        .await?
    else {
        return Ok(blocked("installation unavailable").into());
    };
    let repo = ghinvite_core::RepositoryIdentity::parse(command.repo_full_name.clone())
        .map_err(|_| TerminalError::new_with_code(400, "invalid repository"))?;
    if let ghinvite_core::SelectedRepos::Subset(ids) = &account.selected_repos
        && !ids.contains(&command.repo_id)
    {
        return Ok(blocked("repository unavailable").into());
    }
    // A prerequisite GitHub would not answer for is not a prerequisite that went
    // away: it blocks on GitHub's wait rather than on the unavailability cadence.
    let unread = |error: &ghinvite_github::Error, unavailable: &str| match error.rate_limit() {
        Some(_) => {
            Attempt::throttled_by(blocked("GitHub throttled a delivery prerequisite"), error)
        }
        None => blocked(unavailable).into(),
    };
    match state
        .github
        .get_repo(account.installation_id, repo.owner(), repo.name())
        .await
    {
        Ok(r) if r.id == command.repo_id => (),
        Err(error) => {
            return Ok(unread(
                &error,
                "repository unavailable or identity unverified",
            ));
        }
        // A repository that answered with a different identity is verified as
        // the wrong one, not unread.
        Ok(_) => return Ok(blocked("repository unavailable or identity unverified").into()),
    };
    let user = match state
        .github
        .verified_user(account.installation_id, command.requester_id)
        .await
    {
        Ok(user) => user,
        Err(error) => return Ok(unread(&error, "requester identity unverified")),
    };
    // A database write fence protects retries of this run closure even when
    // GitHub succeeded but Restate never journaled its response. It never expires.
    let generation = state.storage.claim_delivery_attempt(command).await?;
    let mut throttled_by = None;
    let outcome = if let Some(generation) = generation {
        match state
            .github
            .add_collaborator(
                account.installation_id,
                repo.owner(),
                repo.name(),
                &user.login,
                command.permission,
            )
            .await
        {
            Ok(Some(upstream_id)) => CreateOutcome::Created { upstream_id },
            Ok(None) => CreateOutcome::AlreadyCollaborator,
            // A throttled response is GitHub refusing the PUT outright, so it is
            // as explicit a rejection as a 403: the write did not happen and
            // this exact generation may be claimed again. Only a *response*
            // reaches this arm — a transport failure falls through to
            // `OutcomeUnknown` below and keeps the fence, so throttling can
            // never release an ambiguous PUT.
            Err(error @ ghinvite_github::Error::RateLimited { .. }) => {
                state
                    .storage
                    .reject_delivery_attempt(command.invitation_id, generation)
                    .await?;
                throttled_by = Some(error);
                CreateOutcome::Blocked {
                    reason: "GitHub throttled delivery".into(),
                }
            }
            // The remaining authorization rejections. A 429 never lands here:
            // the status is throttling evidence, so it arrives above.
            Err(ghinvite_github::Error::Status {
                status: 401 | 403 | 404,
                ..
            }) => {
                state
                    .storage
                    .reject_delivery_attempt(command.invitation_id, generation)
                    .await?;
                CreateOutcome::Blocked {
                    reason: "GitHub rejected access".into(),
                }
            }
            Err(ghinvite_github::Error::Status {
                status: status @ (400 | 409 | 422),
                ..
            }) => CreateOutcome::Failed { status },
            Err(_) => CreateOutcome::OutcomeUnknown,
        }
    } else {
        // Reconciliation is read-only. Absence, API failure, or a subsequently
        // declined invitation never permits another write of this operation.
        let pending = state
            .github
            .list_invitations(account.installation_id, repo.owner(), repo.name())
            .await;
        // Reading is safe to repeat, so a throttled read earns the same bounded
        // recheck a throttled write does — whichever of the two reads it was.
        // The outcome stays unknown either way: a limit is not evidence about
        // the original PUT.
        let mut unread = |error: ghinvite_github::Error| {
            if error.rate_limit().is_some() {
                throttled_by = Some(error);
            }
            CreateOutcome::OutcomeUnknown
        };
        match pending {
            Ok(items) => match items.iter().find(|i| {
                i.invitee.id == command.requester_id
                    && permission_matches(&i.permissions, command.permission)
            }) {
                Some(item) => CreateOutcome::Created {
                    upstream_id: item.id,
                },
                None => match state
                    .github
                    .collaborator_permission(
                        account.installation_id,
                        repo.owner(),
                        repo.name(),
                        &user.login,
                    )
                    .await
                {
                    Ok((id, permission))
                        if id == command.requester_id
                            && permission_matches(&permission, command.permission) =>
                    {
                        CreateOutcome::AlreadyCollaborator
                    }
                    // Membership evidence that named somebody else is read, not
                    // unread: it reports no limit and schedules nothing.
                    Ok(_) => CreateOutcome::OutcomeUnknown,
                    Err(error) => unread(error),
                },
            },
            Err(error) => unread(error),
        }
    };
    let receipt = CreateReceipt {
        command: command.clone(),
        confirmed_at: outcome.confirmed().then(chrono::Utc::now),
        recovered: false,
        outcome,
        revision: 1,
    };
    Ok(match &throttled_by {
        Some(error) => Attempt::throttled_by(receipt, error),
        None => receipt.into(),
    })
}

fn permission_matches(value: &str, permission: ghinvite_core::Permission) -> bool {
    value == permission.to_string()
        || value
            == match permission {
                ghinvite_core::Permission::Pull => "read",
                ghinvite_core::Permission::Push => "write",
                ghinvite_core::Permission::Triage => "triage",
                ghinvite_core::Permission::Maintain => "maintain",
                ghinvite_core::Permission::Admin => "admin",
            }
}

async fn retain_recovery_time(
    ctx: &ObjectContext<'_>,
    receipt: &mut CreateReceipt,
) -> Result<(), TerminalError> {
    if receipt.outcome.confirmed() && receipt.confirmed_at.is_none() {
        let Json(at) = ctx
            .run(|| async { Ok::<_, HandlerError>(Json(chrono::Utc::now())) })
            .name("observe_retained_create_outcome")
            .await?;
        receipt.confirmed_at = Some(at);
        receipt.recovered = true;
        receipt.revision += 1;
        ctx.set("v1/receipt", Json(receipt.clone()));
    }
    Ok(())
}

async fn project(state: &AppState, receipt: &CreateReceipt) -> Result<(), HandlerError> {
    let command = &receipt.command;
    if let Some(existing) = state
        .storage
        .get_github_invitation(command.invitation_id)
        .await?
    {
        if existing.invitation_request_id != command.request_id
            || existing.repo_id != command.repo_id
        {
            return Err(Error::ProjectionInvariant("invitation identity conflict".into()).into());
        }
    } else {
        state
            .storage
            .insert_github_invitation(&ghinvite_core::GithubInvitation {
                id: command.invitation_id,
                invitation_request_id: command.request_id,
                repo_id: command.repo_id,
                github_invitation_id: None,
                state: InvitationState::Sending,
                error_message: None,
                created_at: command.approved_at,
                updated_at: command.approved_at,
            })
            .await?;
    }
    state.storage.project_delivery(receipt).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        dt, fixture_github_client, invitation_item, invitation_page, refusal,
        seed_pending_invitation, token_mint,
    };
    use ghinvite_core::GithubInvitationId;
    use ghinvite_core::delivery::CreateCommand;
    use ghinvite_github::mocks::{Expectation, MockTransport};
    use ghinvite_github::transport::Method;
    use std::sync::Arc;

    /// A pending invitation on the same repository for somebody else, so page
    /// one carries no evidence about this command's requester.
    fn other_requester_item(id: u64) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "invitee": {"id": 41, "login": "bob"},
            "permissions": "write",
            "created_at": "2026-05-04T13:00:00Z"
        })
    }

    /// GitHub calls `attempt` makes before it reaches the pending-invitation
    /// listing: repository identity, then requester identity and its address.
    fn identity_expectations() -> Vec<Expectation> {
        vec![
            token_mint(9),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/repos/acme/api",
                serde_json::json!({"id": 10, "full_name": "acme/api", "private": true}),
            ),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/user/8",
                serde_json::json!({"id": 8, "login": "alice"}),
            ),
            Expectation::ok_json(
                Method::Get,
                "https://api.github.test/users/alice",
                serde_json::json!({"id": 8, "login": "alice"}),
            ),
        ]
    }

    /// Seed the projections `attempt` reads, leaving the delivery fence
    /// unclaimed so the first guarded PUT may go out.
    async fn state_with_pending_create(mock: MockTransport) -> (AppState, CreateCommand) {
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let state = AppState::new(storage.clone(), fixture_github_client(Arc::new(mock)));
        let seeded = seed_pending_invitation(&storage, "acme/api").await;
        let command = CreateCommand {
            version: 1,
            invitation_id: GithubInvitationId::new(),
            link_id: seeded.link_id,
            request_id: seeded.request_id,
            approval_id: "approval-1".into(),
            account_id: 100,
            installation_id: 9,
            requester_id: 8,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: ghinvite_core::Permission::Push,
            approved_at: dt("2026-05-04T13:00:00Z"),
        };
        (state, command)
    }

    /// The PUT `attempt` issues once the identity checks have passed, answered
    /// with `status`, `headers` and a GitHub-shaped `{"message": ...}` body.
    fn put_collaborator(status: u16, headers: &[(&str, &str)], message: &str) -> Vec<Expectation> {
        let mut script = identity_expectations();
        script.push(Expectation {
            method: Method::Put,
            url: "https://api.github.test/repos/acme/api/collaborators/alice".into(),
            required_headers: Default::default(),
            expected_body: None,
            response: refusal(status, headers, message),
        });
        script
    }

    /// Whether the delivery fence would authorize another PUT for this command.
    async fn fence_is_open(state: &AppState, command: &CreateCommand) -> bool {
        state
            .storage
            .claim_delivery_attempt(command)
            .await
            .unwrap()
            .is_some()
    }

    #[tokio::test]
    async fn throttled_put_blocks_on_githubs_own_schedule() {
        let mock = MockTransport::scripted(put_collaborator(
            403,
            &[("retry-after", "45")],
            "You have exceeded a secondary rate limit",
        ));
        let (state, command) = state_with_pending_create(mock.clone()).await;

        let attempted = attempt(&state, &command, true).await.unwrap();

        assert_eq!(
            attempted.receipt.outcome,
            CreateOutcome::Blocked {
                reason: "GitHub throttled delivery".into()
            }
        );
        assert_eq!(attempted.throttled_for_secs, Some(45));
        // An explicit throttle refused the PUT outright, so this generation is
        // released and the recheck may claim it again.
        assert!(fence_is_open(&state, &command).await);
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn permission_denied_put_blocks_without_throttle_guidance() {
        let mock = MockTransport::scripted(put_collaborator(
            403,
            &[("x-ratelimit-remaining", "4998")],
            "Resource not accessible by integration",
        ));
        let (state, command) = state_with_pending_create(mock.clone()).await;

        let attempted = attempt(&state, &command, true).await.unwrap();

        assert_eq!(
            attempted.receipt.outcome,
            CreateOutcome::Blocked {
                reason: "GitHub rejected access".into()
            }
        );
        // Nothing to wait out: this recheck keeps the slow dependency cadence.
        assert_eq!(attempted.throttled_for_secs, None);
        assert!(fence_is_open(&state, &command).await);
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn ambiguous_put_is_never_read_as_throttling() {
        // 502 carries no rate-limit evidence, so it stays uncertain: the PUT may
        // have been applied, and neither the fence nor a throttled retry may
        // assume otherwise.
        let mock = MockTransport::scripted(put_collaborator(502, &[], "Bad gateway"));
        let (state, command) = state_with_pending_create(mock.clone()).await;

        let attempted = attempt(&state, &command, true).await.unwrap();

        assert_eq!(attempted.receipt.outcome, CreateOutcome::OutcomeUnknown);
        assert_eq!(attempted.throttled_for_secs, None);
        assert!(!fence_is_open(&state, &command).await);
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn throttled_prerequisite_blocks_before_the_fence_is_claimed() {
        let mock = MockTransport::scripted(vec![
            token_mint(9),
            Expectation {
                method: Method::Get,
                url: "https://api.github.test/repos/acme/api".into(),
                required_headers: Default::default(),
                expected_body: None,
                response: refusal(403, &[("retry-after", "30")], "API rate limit exceeded"),
            },
        ]);
        let (state, command) = state_with_pending_create(mock.clone()).await;

        let attempted = attempt(&state, &command, true).await.unwrap();

        assert_eq!(
            attempted.receipt.outcome,
            CreateOutcome::Blocked {
                reason: "GitHub throttled a delivery prerequisite".into()
            }
        );
        assert_eq!(attempted.throttled_for_secs, Some(30));
        // A prerequisite that was never read is not a repository that went away,
        // and no PUT was attempted, so the fence is untouched.
        assert!(fence_is_open(&state, &command).await);
        mock.assert_exhausted();
    }

    #[test]
    fn receipt_journaled_before_throttling_reads_back_with_no_wait() {
        let receipt = serde_json::json!({
            "command": {
                "version": 1,
                "invitation_id": GithubInvitationId::new(),
                "link_id": ghinvite_core::InvitationLinkId::new(),
                "request_id": ghinvite_core::RequestId::new(),
                "approval_id": "approval-1",
                "account_id": 100,
                "installation_id": 9,
                "requester_id": 8,
                "repo_id": 10,
                "repo_full_name": "acme/api",
                "permission": "push",
                "approved_at": "2026-05-04T13:00:00Z",
            },
            "outcome": {"kind": "created", "upstream_id": 9977},
            "revision": 1,
            "confirmed_at": "2026-05-04T13:01:00Z",
        });
        let attempted: Attempt = serde_json::from_value(receipt.clone()).unwrap();
        assert_eq!(attempted.throttled_for_secs, None);
        assert_eq!(
            attempted.receipt.outcome,
            CreateOutcome::Created { upstream_id: 9977 }
        );
        // And a receipt with no wait still journals in the old shape.
        assert_eq!(serde_json::to_value(&attempted).unwrap(), receipt);
    }

    /// Seed the projections `attempt` reads, then burn the delivery claim the
    /// way a create whose GitHub response was never journaled does. The retry
    /// can no longer write, so it has to recover the outcome by observation.
    async fn state_with_retained_create(mock: MockTransport) -> (AppState, CreateCommand) {
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let state = AppState::new(storage.clone(), fixture_github_client(Arc::new(mock)));
        let seeded = seed_pending_invitation(&storage, "acme/api").await;
        let command = CreateCommand {
            version: 1,
            // A create whose invitation row was never projected: the recovery
            // path has no stored upstream ID to short-circuit on.
            invitation_id: GithubInvitationId::new(),
            link_id: seeded.link_id,
            request_id: seeded.request_id,
            approval_id: "approval-1".into(),
            account_id: 100,
            installation_id: 9,
            requester_id: 8,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: ghinvite_core::Permission::Push,
            approved_at: dt("2026-05-04T13:00:00Z"),
        };
        assert!(
            state
                .storage
                .claim_delivery_attempt(&command)
                .await
                .unwrap()
                .is_some()
        );
        (state, command)
    }

    #[tokio::test]
    async fn retained_create_is_recovered_from_a_later_page() {
        let mut script = identity_expectations();
        script.extend([
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([other_requester_item(7001)]),
                Some("https://api.github.test/repos/acme/api/invitations?per_page=100&page=2"),
            ),
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100&page=2",
                serde_json::json!([invitation_item(9977)]),
                None,
            ),
        ]);
        let mock = MockTransport::scripted(script);
        let (state, command) = state_with_retained_create(mock.clone()).await;

        let attempted = attempt(&state, &command, false).await.unwrap();

        assert_eq!(
            attempted.receipt.outcome,
            CreateOutcome::Created { upstream_id: 9977 }
        );
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn retained_create_stays_unknown_when_a_later_page_fails() {
        let mut script = identity_expectations();
        script.extend([
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([other_requester_item(7001)]),
                Some("https://api.github.test/repos/acme/api/invitations?per_page=100&page=2"),
            ),
            Expectation::status(
                Method::Get,
                "https://api.github.test/repos/acme/api/invitations?per_page=100&page=2",
                502,
            ),
        ]);
        let mock = MockTransport::scripted(script);
        let (state, command) = state_with_retained_create(mock.clone()).await;

        let attempted = attempt(&state, &command, false).await.unwrap();

        // Half a listing is not evidence of absence, so the collaborator probe
        // never runs and the create stays unknown.
        assert_eq!(attempted.receipt.outcome, CreateOutcome::OutcomeUnknown);
        // A 502 says nothing about when reading might work, so nothing is
        // scheduled: recovery reconciles this one.
        assert_eq!(attempted.throttled_for_secs, None);
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn retained_create_rechecks_a_throttled_reconciliation() {
        let mut script = identity_expectations();
        script.push(Expectation {
            method: Method::Get,
            url: "https://api.github.test/repos/acme/api/invitations?per_page=100".into(),
            required_headers: Default::default(),
            expected_body: None,
            response: refusal(403, &[("retry-after", "90")], "API rate limit exceeded"),
        });
        let mock = MockTransport::scripted(script);
        let (state, command) = state_with_retained_create(mock.clone()).await;

        let attempted = attempt(&state, &command, false).await.unwrap();

        // The fence stays burned and the outcome stays unknown — a limit is no
        // evidence about the original PUT — but rereading is safe, so this one
        // is worth rechecking once GitHub says the limit has cleared.
        assert_eq!(attempted.receipt.outcome, CreateOutcome::OutcomeUnknown);
        assert_eq!(attempted.throttled_for_secs, Some(90));
        assert!(!fence_is_open(&state, &command).await);
        mock.assert_exhausted();
    }

    #[tokio::test]
    async fn retained_create_rechecks_a_throttled_membership_probe() {
        // The listing is complete and shows no invitation, so absence is real
        // and the membership probe follows — and that read is the throttled one.
        let mut script = identity_expectations();
        script.extend([
            invitation_page(
                "https://api.github.test/repos/acme/api/invitations?per_page=100",
                serde_json::json!([other_requester_item(7001)]),
                None,
            ),
            Expectation {
                method: Method::Get,
                url: "https://api.github.test/repos/acme/api/collaborators/alice/permission".into(),
                required_headers: Default::default(),
                expected_body: None,
                response: refusal(403, &[("retry-after", "20")], "API rate limit exceeded"),
            },
        ]);
        let mock = MockTransport::scripted(script);
        let (state, command) = state_with_retained_create(mock.clone()).await;

        let attempted = attempt(&state, &command, false).await.unwrap();

        assert_eq!(attempted.receipt.outcome, CreateOutcome::OutcomeUnknown);
        assert_eq!(attempted.throttled_for_secs, Some(20));
        mock.assert_exhausted();
    }
}
