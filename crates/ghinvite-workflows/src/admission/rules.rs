//! Invitation request admission and lifecycle rules.
//!
//! Pure functions over the bounded records a link command reads. The link
//! object samples time and new identities inside its journaled step, calls
//! these there, and applies the returned after-images in ADR 0003 order.

use chrono::{DateTime, Utc};
use ghinvite_core::audit::EventType;
use ghinvite_core::delivery::CreateCommand;
use ghinvite_core::request_lifecycle::DecisionOutcome;
use ghinvite_core::{GithubInvitationId, RequestId, RequestState};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    AdmissionReceipt, AdmissionResult, Admit, AuditIntent, DecideRequest, DecisionAction,
    LinkSnapshot, PENDING_LIFETIME, ProjectionEnvelope, Rejection, RequestSnapshot,
    TerminalDecision, WorkflowEnvelope,
};
use crate::availability::Eligibility;
use crate::request_lifecycle::ApprovedDispatch;

/// A retained admission outcome, replayed for the same attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct OperationRecord {
    pub(super) input: Admit,
    pub(super) receipt: AdmissionReceipt,
}

/// What admission reads: the link record and the request named by the
/// requester's blocker, if any.
pub(super) struct AdmissionView {
    pub(super) link: LinkSnapshot,
    pub(super) blocking: Option<RequestSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) enum AdmissionOutcome {
    /// Accepted or rejected; retained for the attempt.
    Decided(OperationRecord),
    /// Eligibility could not be confirmed. Nothing is retained, so the same
    /// attempt can retry.
    Undetermined,
}

/// After-images of one admission decision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct AdmissionDecision {
    pub(super) link: LinkSnapshot,
    /// The overdue blocking request, expired by this decision.
    pub(super) expired: Option<RequestSnapshot>,
    /// The admitted request, which becomes the requester's blocker.
    pub(super) request: Option<RequestSnapshot>,
    /// Present whenever a request changed; the link is then rewritten.
    pub(super) projection: Option<ProjectionEnvelope>,
    pub(super) workflow: Option<WorkflowEnvelope>,
    pub(super) outcome: AdmissionOutcome,
}

/// A stored record the rules cannot evaluate. Neither is retryable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub(super) enum RuleError {
    #[error("use counter overflow")]
    UseCounterOverflow,
    #[error("pending deadline missing")]
    PendingDeadlineMissing,
}

/// After-images of one lifecycle evaluation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct LifecycleDecision {
    pub(super) request: RequestSnapshot,
    pub(super) projection: Option<ProjectionEnvelope>,
    pub(super) clear_blocker: bool,
}

pub(super) fn audit(
    kind: EventType,
    target: impl ToString,
    actor_id: Option<u64>,
    at: DateTime<Utc>,
) -> AuditIntent {
    let target_id = target.to_string();
    AuditIntent {
        event_id: format!("{kind}/{target_id}"),
        kind,
        actor_id,
        target_id,
        effective_at: at,
        evaluated_at: at,
    }
}

pub(super) fn projection(
    link: &LinkSnapshot,
    requests: Vec<RequestSnapshot>,
    events: Vec<AuditIntent>,
) -> ProjectionEnvelope {
    ProjectionEnvelope {
        transition_id: format!("link/{}/{}", link.link_id, link.revision),
        link: link.clone(),
        requests,
        events,
    }
}

/// Only an active link consults installation eligibility.
pub(super) fn needs_availability(link: &LinkSnapshot, now: DateTime<Utc>) -> bool {
    link.inactive(now).is_none()
}

/// Rejections decided from link state alone: an inactive link, then one
/// pending-or-approved request per requester.
pub(super) fn local_rejection(
    link: &LinkSnapshot,
    request: Option<&RequestSnapshot>,
    now: DateTime<Utc>,
) -> Option<Rejection> {
    if let Some(inactive) = link.inactive(now) {
        Some(inactive.into())
    } else if request
        .is_some_and(|r| matches!(r.state, RequestState::Pending | RequestState::Approved))
    {
        Some(Rejection::ExistingRequest)
    } else {
        None
    }
}

/// Decide a new admission attempt at `now`. An overdue blocking request
/// expires first, in the same decision; `request_id` names the request an
/// acceptance creates.
pub(super) fn decide_admission(
    view: &AdmissionView,
    input: &Admit,
    eligibility: &Eligibility,
    now: DateTime<Utc>,
    request_id: RequestId,
) -> Result<AdmissionDecision, RuleError> {
    let (availability, availability_unknown) = match eligibility {
        Eligibility::Available => (None, false),
        Eligibility::Unavailable { reason } => (Some(reason.clone()), false),
        Eligibility::Unknown => (None, true),
    };
    let mut link = view.link.clone();
    let mut blocking_request = view.blocking.clone();
    let expiry_event = if let Some(request) = &mut blocking_request {
        transition_request(request, None, now)?
    } else {
        None
    };
    let expired = expiry_event.as_ref().and(blocking_request.clone());
    let reason = local_rejection(&link, blocking_request.as_ref(), now).or(availability);
    let (result, request) = if let Some(reason) = reason {
        (Some(AdmissionResult::Rejected { reason }), None)
    } else if availability_unknown {
        (None, None)
    } else {
        let request = accept(&mut link, input, now, request_id)?;
        (
            Some(AdmissionResult::Accepted {
                request_id,
                state: request.state,
                decision_deadline: request.decision_deadline,
            }),
            Some(request),
        )
    };
    let mut events: Vec<_> = expiry_event.into_iter().collect();
    let mut touched: Vec<_> = expired.iter().cloned().collect();
    if let Some(request) = &request {
        touched.push(request.clone());
        events.extend(acceptance_events(&link, request, now));
    }
    let projection = if touched.is_empty() {
        None
    } else {
        // Rejection plus expiry also gets a unique link revision.
        if request.is_none() {
            link.revision += 1;
        }
        Some(projection(&link, touched, events))
    };
    let workflow = request
        .as_ref()
        .map(|request| WorkflowEnvelope::from_authority(&link, request.clone()));
    Ok(AdmissionDecision {
        link,
        request,
        projection,
        workflow,
        expired,
        outcome: match result {
            Some(result) => AdmissionOutcome::Decided(OperationRecord {
                input: input.clone(),
                receipt: AdmissionReceipt {
                    decided_at: now,
                    result,
                },
            }),
            None => AdmissionOutcome::Undetermined,
        },
    })
}

/// Consume one use of `link` and create the admitted request: pending until
/// its snapshotted deadline, or approved at once when the link needs no
/// approval.
fn accept(
    link: &mut LinkSnapshot,
    input: &Admit,
    now: DateTime<Utc>,
    request_id: RequestId,
) -> Result<RequestSnapshot, RuleError> {
    let state = if link.creation.approval_required {
        RequestState::Pending
    } else {
        RequestState::Approved
    };
    let decision_deadline = link
        .creation
        .approval_required
        .then_some(now + PENDING_LIFETIME);
    link.uses = link
        .uses
        .checked_add(1)
        .ok_or(RuleError::UseCounterOverflow)?;
    link.revision += 1;
    Ok(RequestSnapshot {
        request_id,
        link_id: link.link_id,
        account_id: link.creation.account_id,
        requester_id: input.requester_id,
        justification: input.justification.clone(),
        state,
        admitted_at: now,
        decision_deadline,
        revision: 1,
        decision: (state == RequestState::Approved).then(|| TerminalDecision {
            decision_id: format!("request.approved/{request_id}"),
            decided_by: None,
            effective_at: now,
            evaluated_at: now,
            decline_reason: None,
        }),
    })
}

/// Audit events for an accepted request: its creation, an automatic
/// approval, and the link's exhaustion when this was its last use.
fn acceptance_events(
    link: &LinkSnapshot,
    request: &RequestSnapshot,
    now: DateTime<Utc>,
) -> Vec<AuditIntent> {
    let mut events = vec![audit(
        EventType::RequestCreated,
        request.request_id,
        Some(request.requester_id),
        now,
    )];
    if request.state == RequestState::Approved {
        events.push(audit(
            EventType::RequestApproved,
            request.request_id,
            None,
            now,
        ));
    }
    if link
        .creation
        .max_uses
        .is_some_and(|max| link.uses == u64::from(max))
    {
        events.push(audit(
            EventType::InvitationLinkExhausted,
            link.link_id,
            None,
            now,
        ));
    }
    events
}

/// Evaluate a request's lifecycle at `now`, with an optional account-admin
/// command. `blocker` is the request ID the requester's blocker names.
pub(super) fn decide_lifecycle(
    link: &LinkSnapshot,
    request: &RequestSnapshot,
    blocker: Option<RequestId>,
    command: Option<&DecideRequest>,
    now: DateTime<Utc>,
) -> Result<LifecycleDecision, RuleError> {
    let mut request = request.clone();
    let event = transition_request(&mut request, command, now)?;
    let projection = event.map(|event| {
        let mut envelope = projection(link, vec![request.clone()], vec![event]);
        envelope.transition_id = request.decision.as_ref().unwrap().decision_id.clone();
        envelope
    });
    let clear_blocker = projection.is_some()
        && request.state != RequestState::Approved
        && blocker == Some(request.request_id);
    Ok(LifecycleDecision {
        request,
        projection,
        clear_blocker,
    })
}

/// What an account admin's decision did to `request`, as `transition_request`
/// left it. `was_pending` is its state before the transition.
///
/// A decline matches only under the same reason, so a second decline with a
/// different reason is incompatible rather than already completed.
pub(super) fn decision_outcome(
    was_pending: bool,
    action: &DecisionAction,
    request: &RequestSnapshot,
) -> DecisionOutcome {
    let matching = match action {
        DecisionAction::Approve => request.state == RequestState::Approved,
        DecisionAction::Decline { reason } => {
            request.state == RequestState::Declined
                && request
                    .decision
                    .as_ref()
                    .is_some_and(|d| &d.decline_reason == reason)
        }
    };
    if !matching {
        DecisionOutcome::Incompatible
    } else if was_pending {
        DecisionOutcome::Applied
    } else {
        DecisionOutcome::AlreadyCompleted
    }
}

/// `request` as its requester may see it: a decline reason is admin-only.
pub(super) fn requester_view(mut request: RequestSnapshot) -> RequestSnapshot {
    if let Some(decision) = &mut request.decision {
        decision.decline_reason = None;
    }
    request
}

/// The delivery plan for an approved `request`: one create per repository in
/// the link's scope, in scope order, each under a fresh identity from
/// `new_id`. `None` when the request carries no decision to deliver.
pub(super) fn dispatch_plan(
    dispatch_id: String,
    link: &LinkSnapshot,
    request: &RequestSnapshot,
    mut new_id: impl FnMut() -> GithubInvitationId,
) -> Option<ApprovedDispatch> {
    let decision = request.decision.as_ref()?;
    let commands = link
        .creation
        .repos
        .iter()
        .map(|repo| CreateCommand {
            invitation_id: new_id(),
            link_id: link.link_id,
            request_id: request.request_id,
            approval_id: decision.decision_id.clone(),
            account_id: link.creation.account_id,
            installation_id: link.creation.installation_id,
            requester_id: request.requester_id,
            repo_id: repo.repo_id,
            repo_full_name: repo.repo_full_name.clone(),
            permission: link.creation.permission,
            approved_at: decision.effective_at,
        })
        .collect();
    Some(ApprovedDispatch {
        dispatch_id,
        input: WorkflowEnvelope::from_authority(link, request.clone()),
        commands,
    })
}

/// All callers use the same evaluation-time arbitration, inside their complete
/// journaled decision. Expiration is effective at the snapshotted deadline.
pub(super) fn transition_request(
    request: &mut RequestSnapshot,
    command: Option<&DecideRequest>,
    now: DateTime<Utc>,
) -> Result<Option<AuditIntent>, RuleError> {
    if request.state != RequestState::Pending {
        return Ok(None);
    }
    let deadline = request
        .decision_deadline
        .ok_or(RuleError::PendingDeadlineMissing)?;
    let (state, kind, actor, effective_at, reason) = if now >= deadline {
        (
            RequestState::Expired,
            EventType::RequestExpired,
            None,
            deadline,
            None,
        )
    } else if let Some(command) = command {
        match &command.action {
            DecisionAction::Approve => (
                RequestState::Approved,
                EventType::RequestApproved,
                Some(command.admin.user_id),
                now,
                None,
            ),
            DecisionAction::Decline { reason } => (
                RequestState::Declined,
                EventType::RequestDeclined,
                Some(command.admin.user_id),
                now,
                reason.clone(),
            ),
        }
    } else {
        return Ok(None);
    };
    let mut event = audit(kind, request.request_id, actor, effective_at);
    event.evaluated_at = now;
    request.state = state;
    request.revision += 1;
    request.decision = Some(TerminalDecision {
        decision_id: event.event_id.clone(),
        decided_by: actor,
        effective_at,
        evaluated_at: now,
        decline_reason: reason,
    });
    Ok(Some(event))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::{AccountAdmin, AdmissionOperationId, CreateLink};
    use chrono::{Duration, TimeZone};
    use ghinvite_core::request_lifecycle::LifecycleOperationId;
    use ghinvite_core::{InvitationLinkId, InvitationLinkRepo, Permission};

    const REQUESTER: u64 = 71;
    const ADMIN: u64 = 9;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).unwrap()
    }

    fn link(approval_required: bool) -> LinkSnapshot {
        let link_id = InvitationLinkId::new();
        LinkSnapshot {
            metadata: None,
            link_id,
            creation: CreateLink {
                link_id,
                admin: AccountAdmin {
                    account_id: 5,
                    user_id: ADMIN,
                },
                account_id: 5,
                installation_id: 6,
                description: "Contributors".into(),
                internal_note: None,
                expires_at: None,
                max_uses: None,
                permission: Permission::Push,
                approval_required,
                repos: vec![InvitationLinkRepo {
                    repo_id: 11,
                    repo_full_name: "acme/widgets".into(),
                }],
            },
            invitation_code: "abcd-efgh".into(),
            created_at: now() - Duration::days(30),
            uses: 0,
            revision: 3,
            revoked_at: None,
            revoked_by: None,
        }
    }

    fn admit(link: &LinkSnapshot) -> Admit {
        Admit {
            link_id: link.link_id,
            operation_id: AdmissionOperationId::try_from(RequestId::new().to_string()).unwrap(),
            requester_id: REQUESTER,
            justification: Some("I maintain the docs".into()),
        }
    }

    fn pending(link: &LinkSnapshot, deadline: DateTime<Utc>) -> RequestSnapshot {
        RequestSnapshot {
            request_id: RequestId::new(),
            link_id: link.link_id,
            account_id: link.creation.account_id,
            requester_id: REQUESTER,
            justification: None,
            state: RequestState::Pending,
            admitted_at: deadline - PENDING_LIFETIME,
            decision_deadline: Some(deadline),
            revision: 1,
            decision: None,
        }
    }

    fn with_state(mut request: RequestSnapshot, state: RequestState) -> RequestSnapshot {
        request.state = state;
        request
    }

    fn command(request: &RequestSnapshot, action: DecisionAction) -> DecideRequest {
        DecideRequest {
            link_id: request.link_id,
            request_id: request.request_id,
            operation_id: LifecycleOperationId::try_from(RequestId::new().to_string()).unwrap(),
            admin: AccountAdmin {
                account_id: request.account_id,
                user_id: ADMIN,
            },
            action,
        }
    }

    fn decide(
        link: &LinkSnapshot,
        blocking: Option<RequestSnapshot>,
        eligibility: Eligibility,
    ) -> (AdmissionDecision, Admit, RequestId) {
        let input = admit(link);
        let request_id = RequestId::new();
        let view = AdmissionView {
            link: link.clone(),
            blocking,
        };
        let decision = decide_admission(&view, &input, &eligibility, now(), request_id).unwrap();
        (decision, input, request_id)
    }

    fn receipt(decision: &AdmissionDecision) -> &AdmissionReceipt {
        match &decision.outcome {
            AdmissionOutcome::Decided(operation) => &operation.receipt,
            AdmissionOutcome::Undetermined => panic!("admission was undetermined"),
        }
    }

    fn rejection(decision: &AdmissionDecision) -> Rejection {
        match &receipt(decision).result {
            AdmissionResult::Rejected { reason } => reason.clone(),
            accepted => panic!("expected a rejection, got {accepted:?}"),
        }
    }

    fn kinds(envelope: &ProjectionEnvelope) -> Vec<EventType> {
        envelope.events.iter().map(|event| event.kind).collect()
    }

    #[test]
    fn acceptance_consumes_one_use_and_creates_a_pending_request() {
        let link = link(true);
        let (decision, input, request_id) = decide(&link, None, Eligibility::Available);

        let deadline = now() + PENDING_LIFETIME;
        assert_eq!(
            decision.outcome,
            AdmissionOutcome::Decided(OperationRecord {
                input: input.clone(),
                receipt: AdmissionReceipt {
                    decided_at: now(),
                    result: AdmissionResult::Accepted {
                        request_id,
                        state: RequestState::Pending,
                        decision_deadline: Some(deadline),
                    },
                },
            })
        );
        assert_eq!(decision.link.uses, 1);
        assert_eq!(decision.link.revision, link.revision + 1);
        let request = decision.request.clone().unwrap();
        assert_eq!(
            request,
            RequestSnapshot {
                request_id,
                link_id: link.link_id,
                account_id: 5,
                requester_id: REQUESTER,
                justification: input.justification.clone(),
                state: RequestState::Pending,
                admitted_at: now(),
                decision_deadline: Some(deadline),
                revision: 1,
                decision: None,
            }
        );
        assert_eq!(decision.expired, None);
        assert_eq!(
            decision.workflow,
            Some(WorkflowEnvelope::from_authority(
                &decision.link,
                request.clone()
            ))
        );
    }

    #[test]
    fn acceptance_projects_the_new_link_revision_request_and_creation_event() {
        let link = link(true);
        let (decision, _, request_id) = decide(&link, None, Eligibility::Available);

        let envelope = decision.projection.unwrap();
        assert_eq!(
            envelope.transition_id,
            format!("link/{}/{}", link.link_id, link.revision + 1)
        );
        assert_eq!(envelope.link, decision.link);
        assert_eq!(envelope.requests, vec![decision.request.unwrap()]);
        assert_eq!(
            envelope.events,
            vec![AuditIntent {
                event_id: format!("request.created/{request_id}"),
                kind: EventType::RequestCreated,
                actor_id: Some(REQUESTER),
                target_id: request_id.to_string(),
                effective_at: now(),
                evaluated_at: now(),
            }]
        );
    }

    #[test]
    fn a_link_without_approval_auto_approves_with_no_decision_deadline() {
        let link = link(false);
        let (decision, _, request_id) = decide(&link, None, Eligibility::Available);

        assert_eq!(
            receipt(&decision).result,
            AdmissionResult::Accepted {
                request_id,
                state: RequestState::Approved,
                decision_deadline: None,
            }
        );
        let request = decision.request.unwrap();
        assert_eq!(request.state, RequestState::Approved);
        assert_eq!(request.decision_deadline, None);
        assert_eq!(
            request.decision,
            Some(TerminalDecision {
                decision_id: format!("request.approved/{request_id}"),
                decided_by: None,
                effective_at: now(),
                evaluated_at: now(),
                decline_reason: None,
            })
        );
        let envelope = decision.projection.unwrap();
        assert_eq!(
            kinds(&envelope),
            vec![EventType::RequestCreated, EventType::RequestApproved]
        );
        assert_eq!(envelope.events[1].actor_id, None);
        assert!(decision.workflow.unwrap().request.decision.is_some());
    }

    #[test]
    fn the_last_use_emits_link_exhaustion() {
        let mut link = link(true);
        link.creation.max_uses = Some(2);
        link.uses = 1;
        let (decision, _, _) = decide(&link, None, Eligibility::Available);

        assert_eq!(decision.link.uses, 2);
        let envelope = decision.projection.unwrap();
        assert_eq!(
            kinds(&envelope),
            vec![
                EventType::RequestCreated,
                EventType::InvitationLinkExhausted
            ]
        );
        let exhausted = &envelope.events[1];
        assert_eq!(exhausted.target_id, link.link_id.to_string());
        assert_eq!(
            exhausted.event_id,
            format!("invitation_link.exhausted/{}", link.link_id)
        );
        assert_eq!(exhausted.actor_id, None);
    }

    #[test]
    fn a_use_below_the_limit_does_not_emit_exhaustion() {
        let mut link = link(true);
        link.creation.max_uses = Some(2);
        let (decision, _, _) = decide(&link, None, Eligibility::Available);

        assert_eq!(decision.link.uses, 1);
        assert_eq!(
            kinds(&decision.projection.unwrap()),
            vec![EventType::RequestCreated]
        );
    }

    #[test]
    fn an_exhausted_link_rejects_without_consuming_a_use() {
        let mut link = link(true);
        link.creation.max_uses = Some(2);
        link.uses = 2;
        let (decision, input, _) = decide(&link, None, Eligibility::Available);

        assert_eq!(
            decision.outcome,
            AdmissionOutcome::Decided(OperationRecord {
                input,
                receipt: AdmissionReceipt {
                    decided_at: now(),
                    result: AdmissionResult::Rejected {
                        reason: Rejection::Exhausted
                    },
                },
            })
        );
        assert_eq!(decision.link, link);
        assert_eq!(decision.request, None);
        assert_eq!(decision.projection, None);
        assert_eq!(decision.workflow, None);
    }

    #[test]
    fn inactive_link_rejections_take_precedence_revoked_expired_exhausted() {
        let mut link = link(true);
        link.creation.max_uses = Some(1);
        link.uses = 1;
        link.creation.expires_at = Some(now());
        link.revoked_at = Some(now() - Duration::hours(1));
        let blocker = pending(&link, now() + Duration::days(1));
        let unavailable = Eligibility::Unavailable {
            reason: Rejection::RepositoryUnavailable,
        };

        let (decision, _, _) = decide(&link, Some(blocker.clone()), unavailable.clone());
        assert_eq!(rejection(&decision), Rejection::Revoked);
        link.revoked_at = None;
        let (decision, _, _) = decide(&link, Some(blocker.clone()), unavailable.clone());
        assert_eq!(rejection(&decision), Rejection::Expired);
        link.creation.expires_at = None;
        let (decision, _, _) = decide(&link, Some(blocker), unavailable);
        assert_eq!(rejection(&decision), Rejection::Exhausted);
    }

    #[test]
    fn link_expiry_is_effective_at_its_instant() {
        let mut link = link(true);
        link.creation.expires_at = Some(now() + Duration::milliseconds(1));
        let (decision, _, _) = decide(&link, None, Eligibility::Available);
        assert!(decision.request.is_some());

        link.creation.expires_at = Some(now());
        let (decision, _, _) = decide(&link, None, Eligibility::Available);
        assert_eq!(rejection(&decision), Rejection::Expired);
    }

    #[test]
    fn a_pending_or_approved_request_blocks_its_requester() {
        let link = link(true);
        let blocker = pending(&link, now() + Duration::days(1));
        for blocking in [
            blocker.clone(),
            with_state(blocker.clone(), RequestState::Approved),
        ] {
            let (decision, _, _) = decide(
                &link,
                Some(blocking),
                Eligibility::Unavailable {
                    reason: Rejection::InstallationUnavailable,
                },
            );
            assert_eq!(rejection(&decision), Rejection::ExistingRequest);
            assert_eq!(decision.link, link);
            assert_eq!(decision.expired, None);
            assert_eq!(decision.projection, None);
        }
    }

    #[test]
    fn a_terminal_request_does_not_block_readmission() {
        let link = link(true);
        let old = pending(&link, now() + Duration::days(1));
        for state in [
            RequestState::Declined,
            RequestState::Expired,
            RequestState::Cancelled,
        ] {
            let (decision, _, request_id) = decide(
                &link,
                Some(with_state(old.clone(), state)),
                Eligibility::Available,
            );
            assert_eq!(decision.request.unwrap().request_id, request_id);
            assert_eq!(decision.expired, None);
            assert_eq!(decision.link.uses, 1);
        }
    }

    #[test]
    fn a_blocker_one_instant_before_its_deadline_still_blocks() {
        let link = link(true);
        let blocker = pending(&link, now() + Duration::milliseconds(1));
        let (decision, _, _) = decide(&link, Some(blocker), Eligibility::Available);
        assert_eq!(rejection(&decision), Rejection::ExistingRequest);
        assert_eq!(decision.expired, None);
    }

    #[test]
    fn an_overdue_blocker_expires_at_its_deadline_and_admits_the_new_attempt() {
        let link = link(true);
        let deadline = now();
        let blocker = pending(&link, deadline);
        let (decision, _, request_id) =
            decide(&link, Some(blocker.clone()), Eligibility::Available);

        let expired = decision.expired.clone().unwrap();
        let event_id = format!("request.expired/{}", blocker.request_id);
        assert_eq!(expired.state, RequestState::Expired);
        assert_eq!(expired.revision, blocker.revision + 1);
        assert_eq!(
            expired.decision,
            Some(TerminalDecision {
                decision_id: event_id.clone(),
                decided_by: None,
                effective_at: deadline,
                evaluated_at: now(),
                decline_reason: None,
            })
        );
        assert_eq!(decision.request.clone().unwrap().request_id, request_id);
        assert_eq!(decision.link.uses, 1);
        assert_eq!(decision.link.revision, link.revision + 1);
        let envelope = decision.projection.unwrap();
        assert_eq!(envelope.requests, vec![expired, decision.request.unwrap()]);
        assert_eq!(
            kinds(&envelope),
            vec![EventType::RequestExpired, EventType::RequestCreated]
        );
        assert_eq!(envelope.events[0].event_id, event_id);
        assert_eq!(envelope.events[0].effective_at, deadline);
        assert_eq!(envelope.events[0].actor_id, None);
    }

    #[test]
    fn an_overdue_blocker_expires_even_when_the_new_attempt_is_rejected() {
        let mut link = link(true);
        link.creation.max_uses = Some(1);
        link.uses = 1;
        let deadline = now() - Duration::hours(1);
        let blocker = pending(&link, deadline);
        let (decision, _, _) = decide(&link, Some(blocker), Eligibility::Available);

        assert_eq!(rejection(&decision), Rejection::Exhausted);
        assert_eq!(decision.request, None);
        assert_eq!(decision.workflow, None);
        assert_eq!(decision.link.uses, 1);
        assert_eq!(decision.link.revision, link.revision + 1);
        let expired = decision.expired.unwrap();
        assert_eq!(expired.decision.unwrap().effective_at, deadline);
        let envelope = decision.projection.unwrap();
        assert_eq!(
            envelope.transition_id,
            format!("link/{}/{}", link.link_id, link.revision + 1)
        );
        assert_eq!(kinds(&envelope), vec![EventType::RequestExpired]);
    }

    #[test]
    fn unavailable_eligibility_is_a_retained_rejection() {
        let link = link(true);
        let (decision, _, _) = decide(
            &link,
            None,
            Eligibility::Unavailable {
                reason: Rejection::RepositoryUnavailable,
            },
        );
        assert_eq!(rejection(&decision), Rejection::RepositoryUnavailable);
        assert_eq!(decision.link, link);
        assert_eq!(decision.request, None);
        assert_eq!(decision.projection, None);
    }

    #[test]
    fn unknown_eligibility_retains_no_outcome_and_consumes_no_use() {
        let link = link(true);
        let (decision, _, _) = decide(&link, None, Eligibility::Unknown);
        assert_eq!(decision.outcome, AdmissionOutcome::Undetermined);
        assert_eq!(decision.link, link);
        assert_eq!(decision.request, None);
        assert_eq!(decision.projection, None);
        assert_eq!(decision.workflow, None);
    }

    #[test]
    fn local_rejections_precede_unknown_eligibility() {
        let mut link = link(true);
        link.revoked_at = Some(now());
        let (decision, _, _) = decide(&link, None, Eligibility::Unknown);
        assert_eq!(rejection(&decision), Rejection::Revoked);
    }

    #[test]
    fn unknown_eligibility_still_expires_an_overdue_blocker() {
        let link = link(true);
        let blocker = pending(&link, now());
        let (decision, _, _) = decide(&link, Some(blocker), Eligibility::Unknown);
        assert_eq!(decision.outcome, AdmissionOutcome::Undetermined);
        assert_eq!(decision.expired.unwrap().state, RequestState::Expired);
        assert_eq!(decision.link.revision, link.revision + 1);
        assert_eq!(
            kinds(&decision.projection.unwrap()),
            vec![EventType::RequestExpired]
        );
    }

    #[test]
    fn the_normalized_justification_is_admitted() {
        let link = link(true);
        let mut input = admit(&link);
        input.justification = Some("  I maintain the docs \n".into());
        input.normalize().unwrap();
        let view = AdmissionView {
            link: link.clone(),
            blocking: None,
        };
        let decision = decide_admission(
            &view,
            &input,
            &Eligibility::Available,
            now(),
            RequestId::new(),
        )
        .unwrap();
        assert_eq!(
            decision.request.unwrap().justification.as_deref(),
            Some("I maintain the docs")
        );

        input.justification = Some("   ".into());
        input.normalize().unwrap();
        let decision = decide_admission(
            &view,
            &input,
            &Eligibility::Available,
            now(),
            RequestId::new(),
        )
        .unwrap();
        assert_eq!(decision.request.unwrap().justification, None);
    }

    #[test]
    fn a_use_counter_overflow_fails_the_decision() {
        let mut link = link(true);
        link.uses = u64::MAX;
        let view = AdmissionView {
            link: link.clone(),
            blocking: None,
        };
        let input = admit(&link);
        assert_eq!(
            decide_admission(
                &view,
                &input,
                &Eligibility::Available,
                now(),
                RequestId::new()
            ),
            Err(RuleError::UseCounterOverflow)
        );
    }

    #[test]
    fn only_an_active_link_needs_availability() {
        let mut link = link(true);
        assert!(needs_availability(&link, now()));
        link.revoked_at = Some(now());
        assert!(!needs_availability(&link, now()));
    }

    #[test]
    fn transition_leaves_terminal_requests_unchanged() {
        let link = link(true);
        let request = with_state(pending(&link, now()), RequestState::Declined);
        let mut after = request.clone();
        let approve = command(&request, DecisionAction::Approve);
        assert_eq!(
            transition_request(&mut after, Some(&approve), now()).unwrap(),
            None
        );
        assert_eq!(after, request);
    }

    #[test]
    fn transition_without_a_command_waits_until_the_deadline() {
        let link = link(true);
        let request = pending(&link, now() + Duration::milliseconds(1));
        let mut after = request.clone();
        assert_eq!(transition_request(&mut after, None, now()).unwrap(), None);
        assert_eq!(after, request);
    }

    #[test]
    fn approval_before_the_deadline_is_effective_now() {
        let link = link(true);
        let mut request = pending(&link, now() + Duration::milliseconds(1));
        let approve = command(&request, DecisionAction::Approve);
        let event = transition_request(&mut request, Some(&approve), now())
            .unwrap()
            .unwrap();

        let event_id = format!("request.approved/{}", request.request_id);
        assert_eq!(
            event,
            AuditIntent {
                event_id: event_id.clone(),
                kind: EventType::RequestApproved,
                actor_id: Some(ADMIN),
                target_id: request.request_id.to_string(),
                effective_at: now(),
                evaluated_at: now(),
            }
        );
        assert_eq!(request.state, RequestState::Approved);
        assert_eq!(request.revision, 2);
        assert_eq!(
            request.decision,
            Some(TerminalDecision {
                decision_id: event_id,
                decided_by: Some(ADMIN),
                effective_at: now(),
                evaluated_at: now(),
                decline_reason: None,
            })
        );
    }

    #[test]
    fn decline_keeps_its_reason() {
        let link = link(true);
        let mut request = pending(&link, now() + Duration::days(1));
        let decline = command(
            &request,
            DecisionAction::Decline {
                reason: Some("Not a contributor".into()),
            },
        );
        let event = transition_request(&mut request, Some(&decline), now())
            .unwrap()
            .unwrap();
        assert_eq!(event.kind, EventType::RequestDeclined);
        assert_eq!(request.state, RequestState::Declined);
        let decision = request.decision.unwrap();
        assert_eq!(decision.decided_by, Some(ADMIN));
        assert_eq!(
            decision.decline_reason.as_deref(),
            Some("Not a contributor")
        );
    }

    #[test]
    fn expiry_at_the_deadline_wins_over_a_late_command() {
        let link = link(true);
        let deadline = now() - Duration::minutes(5);
        let mut request = pending(&link, deadline);
        let approve = command(&request, DecisionAction::Approve);
        let event = transition_request(&mut request, Some(&approve), now())
            .unwrap()
            .unwrap();

        assert_eq!(event.kind, EventType::RequestExpired);
        assert_eq!(event.actor_id, None);
        assert_eq!(event.effective_at, deadline);
        assert_eq!(event.evaluated_at, now());
        assert_eq!(request.state, RequestState::Expired);
        let decision = request.decision.unwrap();
        assert_eq!(decision.decided_by, None);
        assert_eq!(decision.effective_at, deadline);
        assert_eq!(decision.evaluated_at, now());
    }

    #[test]
    fn a_pending_request_without_a_deadline_fails_the_transition() {
        let link = link(true);
        let mut request = pending(&link, now());
        request.decision_deadline = None;
        assert_eq!(
            transition_request(&mut request, None, now()),
            Err(RuleError::PendingDeadlineMissing)
        );
    }

    #[test]
    fn lifecycle_release_clears_only_the_blocker_naming_the_request() {
        let link = link(true);
        let request = pending(&link, now() + Duration::days(1));
        let decline = command(&request, DecisionAction::Decline { reason: None });

        let decision = decide_lifecycle(
            &link,
            &request,
            Some(request.request_id),
            Some(&decline),
            now(),
        )
        .unwrap();
        assert!(decision.clear_blocker);
        let envelope = decision.projection.unwrap();
        assert_eq!(
            envelope.transition_id,
            format!("request.declined/{}", request.request_id)
        );
        assert_eq!(envelope.requests, vec![decision.request]);
        assert_eq!(kinds(&envelope), vec![EventType::RequestDeclined]);

        let newer = RequestId::new();
        let decision =
            decide_lifecycle(&link, &request, Some(newer), Some(&decline), now()).unwrap();
        assert!(!decision.clear_blocker);
    }

    #[test]
    fn lifecycle_approval_keeps_the_blocker() {
        let link = link(true);
        let request = pending(&link, now() + Duration::days(1));
        let approve = command(&request, DecisionAction::Approve);
        let decision = decide_lifecycle(
            &link,
            &request,
            Some(request.request_id),
            Some(&approve),
            now(),
        )
        .unwrap();
        assert_eq!(decision.request.state, RequestState::Approved);
        assert!(!decision.clear_blocker);
    }

    #[test]
    fn lifecycle_expiry_releases_the_blocker() {
        let link = link(true);
        let request = pending(&link, now());
        let decision =
            decide_lifecycle(&link, &request, Some(request.request_id), None, now()).unwrap();
        assert_eq!(decision.request.state, RequestState::Expired);
        assert!(decision.clear_blocker);
    }

    #[test]
    fn lifecycle_without_a_change_projects_nothing() {
        let link = link(true);
        let request = pending(&link, now() + Duration::days(1));
        let decision =
            decide_lifecycle(&link, &request, Some(request.request_id), None, now()).unwrap();
        assert_eq!(decision.request, request);
        assert_eq!(decision.projection, None);
        assert!(!decision.clear_blocker);
    }

    fn decided(link: &LinkSnapshot, action: DecisionAction) -> (RequestSnapshot, DecideRequest) {
        let mut request = pending(link, now() + Duration::days(1));
        let command = command(&request, action);
        transition_request(&mut request, Some(&command), now()).unwrap();
        (request, command)
    }

    fn decline(reason: Option<&str>) -> DecisionAction {
        DecisionAction::Decline {
            reason: reason.map(Into::into),
        }
    }

    #[test]
    fn a_decision_that_moved_a_pending_request_is_applied() {
        let link = link(true);
        for action in [DecisionAction::Approve, decline(Some("Not a contributor"))] {
            let (request, command) = decided(&link, action.clone());

            assert_eq!(
                decision_outcome(true, &command.action, &request),
                DecisionOutcome::Applied,
                "action={action:?}"
            );
        }
    }

    #[test]
    fn the_same_decision_on_a_decided_request_is_already_completed() {
        let link = link(true);
        let (request, command) = decided(&link, decline(Some("Not a contributor")));

        assert_eq!(
            decision_outcome(false, &command.action, &request),
            DecisionOutcome::AlreadyCompleted
        );
    }

    #[test]
    fn a_decision_the_request_did_not_take_is_incompatible() {
        let link = link(true);
        let (approved, _) = decided(&link, DecisionAction::Approve);
        let (declined, _) = decided(&link, decline(Some("Not a contributor")));
        let expired = with_state(pending(&link, now()), RequestState::Expired);

        for (request, action) in [
            (&approved, decline(None)),
            (&declined, DecisionAction::Approve),
            // A decline under another reason is another decision.
            (&declined, decline(Some("Spam"))),
            (&declined, decline(None)),
            (&expired, DecisionAction::Approve),
        ] {
            for was_pending in [true, false] {
                assert_eq!(
                    decision_outcome(was_pending, &action, request),
                    DecisionOutcome::Incompatible,
                    "state={:?} action={action:?}",
                    request.state
                );
            }
        }
    }

    #[test]
    fn a_requester_never_sees_the_decline_reason() {
        let link = link(true);
        let (declined, _) = decided(&link, decline(Some("Not a contributor")));

        let seen = requester_view(declined.clone());

        assert_eq!(seen.decision.as_ref().unwrap().decline_reason, None);
        assert_eq!(seen.state, declined.state);
        assert_eq!(
            seen.decision.unwrap().decision_id,
            declined.decision.unwrap().decision_id
        );
    }

    fn two_repo_link() -> LinkSnapshot {
        let mut link = link(true);
        link.creation.repos.push(InvitationLinkRepo {
            repo_id: 12,
            repo_full_name: "acme/gadgets".into(),
        });
        link
    }

    #[test]
    fn a_dispatch_plan_has_one_create_per_repository_in_scope_order() {
        let link = two_repo_link();
        let (request, _) = decided(&link, DecisionAction::Approve);
        let decision = request.decision.clone().unwrap();
        let ids = [
            ghinvite_core::GithubInvitationId::new(),
            ghinvite_core::GithubInvitationId::new(),
        ];
        let mut next = ids.iter().copied();

        let plan = dispatch_plan("dispatch/1".into(), &link, &request, || {
            next.next().unwrap()
        })
        .unwrap();

        assert_eq!(plan.dispatch_id, "dispatch/1");
        assert_eq!(plan.input.request, request);
        let repos: Vec<_> = plan
            .commands
            .iter()
            .map(|c| (c.invitation_id, c.repo_id, c.repo_full_name.as_str()))
            .collect();
        assert_eq!(
            repos,
            vec![(ids[0], 11, "acme/widgets"), (ids[1], 12, "acme/gadgets")]
        );
        for command in &plan.commands {
            assert_eq!(command.link_id, link.link_id);
            assert_eq!(command.request_id, request.request_id);
            assert_eq!(command.approval_id, decision.decision_id);
            assert_eq!(command.approved_at, decision.effective_at);
            assert_eq!(command.account_id, 5);
            assert_eq!(command.installation_id, 6);
            assert_eq!(command.requester_id, REQUESTER);
            assert_eq!(command.permission, Permission::Push);
        }
    }

    #[test]
    fn an_undecided_request_has_no_dispatch_plan() {
        let link = link(true);
        let request = pending(&link, now() + Duration::days(1));

        let plan = dispatch_plan("dispatch/1".into(), &link, &request, || {
            ghinvite_core::GithubInvitationId::new()
        });

        assert!(plan.is_none());
    }
}
