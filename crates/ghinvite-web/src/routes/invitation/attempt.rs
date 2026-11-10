//! Native forms for admission attempts, with optional SQL delivery observations.
use crate::attempt_continuations::{AttemptContinuations, Retention, Scope};
use crate::link_authority::AuthorityError;
use crate::{WebError, session::Session};
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
};
use dioxus::prelude::*;
use ghinvite_core::admission::{
    AdmissionOperationId, AdmissionReceipt, AdmissionResult, Admit, Rejection, RequesterPage,
};

fn attempt_url(_code: &str, id: &str) -> String {
    format!("/join?operation_id={id}")
}

const RETRY_UNKNOWN: &str =
    "Outcome unknown. Retry this same attempt to confirm whether your request was accepted.";
const FRESH_ATTEMPT: &str =
    "This is a fresh attempt. The original attempt remains recoverable below.";

/// Submitted attempt input that cannot become an [`Admit`] command.
enum InvalidInput {
    OperationId,
    Justification,
}

impl From<InvalidInput> for WebError {
    fn from(invalid: InvalidInput) -> Self {
        WebError::BadRequest(match invalid {
            InvalidInput::OperationId => "Missing or invalid operation ID. Return to the invitation link to start a fresh attempt.".into(),
            InvalidInput::Justification => "Justification is too long.".into(),
        })
    }
}

/// Submitted input retained as a continuation before ingress. The requester
/// and canonical link identity are implied by the continuation scope.
#[derive(serde::Serialize, serde::Deserialize)]
struct AttemptInput {
    operation_id: AdmissionOperationId,
    justification: Option<String>,
    secret_sha256: ghinvite_core::SecretHash,
    secret_generation: u32,
    scope_evidence: Vec<ghinvite_core::admission::ScopeEvidence>,
    #[serde(default)]
    domain_evidence: Option<ghinvite_core::email_domain::DomainEvidence>,
}

impl From<&Admit> for AttemptInput {
    fn from(command: &Admit) -> Self {
        Self {
            operation_id: command.operation_id.clone(),
            justification: command.justification.clone(),
            secret_sha256: command.secret_sha256,
            secret_generation: command.secret_generation,
            scope_evidence: command.scope_evidence.clone(),
            domain_evidence: command.domain_evidence.clone(),
        }
    }
}

/// Parse and normalize submitted input before any network call.
fn admit_command(
    capability: &crate::session::PendingLink,
    operation_id: &str,
    requester_id: u64,
    justification: Option<String>,
) -> Result<Admit, InvalidInput> {
    let operation_id = AdmissionOperationId::try_from(operation_id.to_owned())
        .map_err(|_| InvalidInput::OperationId)?;
    let mut command = Admit {
        domain_evidence: None,
        link_id: capability.link_id,
        secret_sha256: capability.secret_sha256,
        secret_generation: capability.secret_generation,
        scope_evidence: vec![],
        operation_id,
        requester_id,
        justification,
    };
    command
        .normalize()
        .map_err(|_| InvalidInput::Justification)?;
    Ok(command)
}

pub async fn page(
    state: &crate::AppState,
    tower: &tower_sessions::Session,
    session: &Session,
    capability: &crate::session::PendingLink,
    operation: Option<&str>,
    fresh: bool,
) -> Response {
    let link_id = capability.link_id;
    let canonical = link_id.to_string();
    let code = canonical.as_str();
    let authority = &state.link_authority;
    let operation_id = match operation
        .map(|id| AdmissionOperationId::try_from(id.to_owned()))
        .transpose()
    {
        Ok(id) => id,
        Err(_) => return safe_error(code, WebError::BadRequest("Invalid operation ID.".into())),
    };
    let local = match load_local(state, tower, session, code, operation).await {
        Ok(local) => local,
        Err(error) => return safe_error(code, error),
    };
    let retained_capability =
        local
            .as_ref()
            .filter(|_| operation.is_some())
            .map(|input| crate::session::PendingLink {
                link_id,
                secret_sha256: input.secret_sha256,
                secret_generation: input.secret_generation,
            });
    let page = match authority
        .requester_page(
            retained_capability.as_ref().unwrap_or(capability),
            session.user_id,
            operation_id,
        )
        .await
    {
        Ok(page) => page,
        Err(error) => {
            if let Some(local) = local {
                // The exact attempt may never have reached ingress. Recover
                // current eligibility independently, without submitting it.
                if matches!(error, AuthorityError::Missing)
                    && let Ok(summary) = authority
                        .requester_page(capability, session.user_id, None)
                        .await
                {
                    if fresh
                        && summary.can_start_fresh
                        && let Some(response) = super::domains::gate(state, session, &summary)
                    {
                        return response;
                    }
                    let original = attempt_url(code, &String::from(local.operation_id.clone()));
                    let (id, justification, message, mode) = if fresh && summary.can_start_fresh {
                        let id = ghinvite_core::RequestId::new().to_string();
                        (id, None, FRESH_ATTEMPT, FormMode::Fresh)
                    } else {
                        let id = String::from(local.operation_id.clone());
                        let justification = local.justification.as_deref();
                        (id, justification, RETRY_UNKNOWN, FormMode::Retry)
                    };
                    return View {
                        id: &id,
                        justification,
                        page: Some(&summary),
                        message,
                        original: Some(&original),
                        mode,
                        status: StatusCode::OK,
                        delivery: vec![],
                        organization_delivery: None,
                    }
                    .render(session, code);
                }
                if matches!(error, AuthorityError::Missing | AuthorityError::Unknown(_)) {
                    return unknown(session, code, &local);
                }
            }
            return page_error(session, code, error);
        }
    };
    if operation.is_none()
        && let Some(local) = local.as_ref()
    {
        let local_id = String::from(local.operation_id.clone());
        if page
            .attempt
            .as_ref()
            .is_none_or(|a| a.input.operation_id != local.operation_id)
        {
            // Prefer local only if its own authoritative outcome is still
            // unresolved. A completed old browser record cannot hide a newer
            // request from another browser. Fresh navigation retains this path.
            let unresolved = match authority
                .requester_page(
                    capability,
                    session.user_id,
                    Some(local.operation_id.clone()),
                )
                .await
            {
                Ok(known) => known.attempt.as_ref().is_none_or(|a| a.receipt.is_none()),
                Err(AuthorityError::Missing | AuthorityError::Unknown(_)) => true,
                Err(_) => false,
            };
            if unresolved {
                if fresh
                    && page.can_start_fresh
                    && let Some(response) = super::domains::gate(state, session, &page)
                {
                    return response;
                }
                let original = attempt_url(code, &local_id);
                let fresh_id = ghinvite_core::RequestId::new().to_string();
                return View {
                    id: if fresh && page.can_start_fresh {
                        &fresh_id
                    } else {
                        &local_id
                    },
                    justification: if fresh && page.can_start_fresh {
                        None
                    } else {
                        local.justification.as_deref()
                    },
                    page: Some(&page),
                    message: if fresh && page.can_start_fresh {
                        FRESH_ATTEMPT
                    } else {
                        RETRY_UNKNOWN
                    },
                    original: Some(&original),
                    mode: if fresh && page.can_start_fresh {
                        FormMode::Fresh
                    } else {
                        FormMode::Retry
                    },
                    status: StatusCode::OK,
                    delivery: vec![],
                    organization_delivery: None,
                }
                .render(session, code);
            }
        }
    }
    if page
        .attempt
        .as_ref()
        .and_then(|attempt| attempt.receipt.as_ref())
        .is_some_and(|receipt| {
            matches!(
                receipt.result,
                AdmissionResult::Rejected {
                    reason: Rejection::NotAvailable
                }
            )
        })
    {
        return super::invitation_not_found_response(session);
    }
    if !page.can_start_fresh && !page.paused && page.attempt.is_none() && page.request.is_none() {
        return super::invitation_not_found_response(session);
    }
    // A generic visit may start anew after reauthentication. An explicit old
    // operation still answers its immutable original domain refusal.
    let domain_refusal = page
        .attempt
        .as_ref()
        .and_then(|a| a.receipt.as_ref())
        .is_some_and(|r| {
            matches!(
                r.result,
                AdmissionResult::Rejected {
                    reason: Rejection::DomainEvidenceMissing
                        | Rejection::DomainEvidenceStale
                        | Rejection::DomainMismatch { .. }
                }
            )
        });
    let fresh = fresh
        || (operation.is_none()
            && domain_refusal
            && page.can_start_fresh
            && state.config.signin_apps.is_some()
            && session.domain_evidence.as_ref().is_some_and(|e| {
                e.check(&page.email_domains, page.link_id, chrono::Utc::now())
                    .is_ok()
            }));
    if !fresh && let Some(receipt) = page.attempt.as_ref().and_then(|a| a.receipt.as_ref()) {
        match &receipt.result {
            AdmissionResult::Rejected {
                reason: Rejection::DomainEvidenceMissing | Rejection::DomainEvidenceStale,
            } => {
                return super::domains::prompt(
                    session,
                    &page.email_domains,
                    false,
                    state.config.signin_apps.is_none(),
                );
            }
            AdmissionResult::Rejected {
                reason: Rejection::DomainMismatch { required },
            } => {
                return super::domains::prompt(
                    session,
                    required,
                    true,
                    state.config.signin_apps.is_none(),
                );
            }
            _ => {}
        }
    }
    if page.request.as_ref().is_none_or(|r| {
        !matches!(
            r.state,
            ghinvite_core::RequestState::Pending | ghinvite_core::RequestState::Approved
        )
    }) && (fresh
        || page
            .attempt
            .as_ref()
            .and_then(|a| a.receipt.as_ref())
            .is_none())
        && let Some(response) = super::domains::gate(state, session, &page)
    {
        return response;
    }
    let fresh = fresh && page.can_start_fresh;
    let original = page
        .attempt
        .as_ref()
        .map(|a| attempt_url(code, &String::from(a.input.operation_id.clone())));
    let (id, justification, receipt) = match &page.attempt {
        Some(attempt) if !fresh => (
            String::from(attempt.input.operation_id.clone()),
            local
                .as_ref()
                .filter(|local| local.operation_id == attempt.input.operation_id)
                .map(|local| local.justification.clone())
                .unwrap_or_else(|| attempt.input.justification.clone()),
            attempt.receipt.clone(),
        ),
        _ => (ghinvite_core::RequestId::new().to_string(), None, None),
    };
    let message = receipt.as_ref().map(receipt_copy).unwrap_or_else(|| {
        if page.attempt.is_some() && !fresh {
            RETRY_UNKNOWN.into()
        } else if fresh {
            FRESH_ATTEMPT.into()
        } else if page.request.is_some() {
            "Your existing request is shown below.".into()
        } else if page.paused {
            "This link isn't accepting requests right now.".into()
        } else if page.targets.organization.is_some() {
            "Review the organization, team and repository targets and submit your request.".into()
        } else {
            "Review the repositories and submit your request.".into()
        }
    });
    let mut delivery = Vec::new();
    let mut organization_delivery = None;
    if let Some(request) = &page.request
        && matches!(
            request.state,
            ghinvite_core::RequestState::Approved | ghinvite_core::RequestState::Lapsed
        )
    {
        organization_delivery = crate::organization_delivery::load(
            state,
            &page.targets,
            page.link_id,
            request.request_id,
            session.user_id,
        )
        .await;
        let progress = state
            .request_authority
            .delivery_progress(ghinvite_core::request_lifecycle::RequestStatus {
                link_id: page.link_id,
                request_id: request.request_id,
                requester_id: session.user_id,
            })
            .await
            .ok();
        let receipts = state
            .storage
            .list_delivery_for_request(request.request_id)
            .await
            .ok();
        for target in &page.targets.repos {
            let repo = &ghinvite_core::InvitationLinkRepo {
                repo_id: target.repo_id,
                repo_full_name: target.repo_full_name.clone(),
            };
            let generation = progress
                .as_deref()
                .unwrap_or_default()
                .iter()
                .find(|p| p.repo_id == repo.repo_id)
                .map(|p| p.delivery_generation)
                .filter(|g| *g > 0);
            let current = match generation {
                Some(generation) => {
                    state
                        .delivery_authority
                        .snapshot(request.request_id, repo.repo_id, generation)
                        .await
                }
                None => Err(crate::link_authority::AuthorityError::Missing),
            };
            let matches_scope = |s: &ghinvite_core::delivery::DeliverySnapshot| {
                s.create.command.request_id == request.request_id
                    && s.create.command.link_id == Some(page.link_id)
                    && s.create.command.account_id == request.account_id
                    && s.create.command.requester_id == session.user_id
                    && s.create.command.repo_id == repo.repo_id
                    && s.create.command.permission == target.grant.into()
                    && Some(s.create.command.delivery_generation) == generation
            };
            let current = current.ok().flatten().filter(matches_scope);
            // Absence or a mismatched owner response is not a current
            // observation, even when an exact-generation projection survives.
            let unavailable = current.is_none();
            let projected: Vec<_> = receipts
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter(|s| matches_scope(s))
                .cloned()
                .collect();
            let current = current
                .as_ref()
                .map(std::slice::from_ref)
                .unwrap_or(&projected);
            let row = ghinvite_ui::invitation::delivery_presentation(
                repo,
                current,
                progress.as_deref().unwrap_or_default(),
                unavailable || current.is_empty() || progress.is_none(),
            );
            delivery.push(row);
        }
    }
    let mode = if receipt.is_some() || (!page.can_start_fresh && page.attempt.is_none()) {
        FormMode::Closed
    } else if page.attempt.is_some() && !fresh {
        FormMode::Retry
    } else {
        FormMode::Fresh
    };
    View {
        id: &id,
        justification: justification.as_deref(),
        page: Some(&page),
        message: &message,
        original: original.as_deref(),
        mode,
        status: StatusCode::OK,
        delivery,
        organization_delivery,
    }
    .render(session, code)
}

pub async fn submit(
    state: &crate::AppState,
    tower: &tower_sessions::Session,
    session: &Session,
    capability: &crate::session::PendingLink,
    operation: &str,
    justification: Option<String>,
) -> Response {
    let authority = &state.link_authority;
    let link_id = capability.link_id;
    let canonical = link_id.to_string();
    let code = canonical.as_str();
    // Validate identity and normalize input before any network call.
    let invalid_justification = ghinvite_ui::request_form::justification_error(
        justification.as_deref().unwrap_or_default(),
    )
    .is_some();
    let local = match load_local(state, tower, session, code, Some(operation)).await {
        Ok(local) => local,
        Err(error) => return safe_error(code, error),
    };
    let retained_capability = local.as_ref().map(|input| crate::session::PendingLink {
        link_id,
        secret_sha256: input.secret_sha256,
        secret_generation: input.secret_generation,
    });
    let mut command = match admit_command(
        retained_capability.as_ref().unwrap_or(capability),
        operation,
        session.user_id,
        justification.clone(),
    ) {
        Ok(command) => command,
        Err(InvalidInput::Justification) if invalid_justification => {
            let page = match authority
                .requester_page(capability, session.user_id, None)
                .await
            {
                Ok(page) => page,
                Err(error) => return page_error(session, code, error),
            };
            if !page.can_start_fresh {
                return self::page(state, tower, session, capability, None, false).await;
            }
            return View {
                id: operation,
                justification: justification.as_deref(),
                page: Some(&page),
                message: "Your request has not been submitted. Correct the justification below.",
                original: None,
                mode: FormMode::Validation,
                status: StatusCode::BAD_REQUEST,
                delivery: vec![],
                organization_delivery: None,
            }
            .render(session, code);
        }
        Err(invalid) => return safe_error(code, invalid.into()),
    };
    if let Some(input) = local.as_ref() {
        command.scope_evidence.clone_from(&input.scope_evidence);
        command.domain_evidence.clone_from(&input.domain_evidence);
    } else if state.config.signin_apps.is_some() {
        let page = match authority
            .requester_page(capability, session.user_id, None)
            .await
        {
            Ok(page) => page,
            Err(error) => return page_error(session, code, error),
        };
        if !page.email_domains.is_empty() {
            // Exact existing attempts retain their original evidence, including stale evidence.
            let original = match authority
                .requester_page(
                    capability,
                    session.user_id,
                    Some(command.operation_id.clone()),
                )
                .await
            {
                Ok(page) => page.attempt,
                Err(AuthorityError::Missing) => None,
                Err(error) => return page_error(session, code, error),
            };
            if let Some(original) = original {
                command.domain_evidence = original.input.domain_evidence;
            } else {
                if let Some(response) = super::domains::gate(state, session, &page) {
                    return response;
                }
                command.domain_evidence.clone_from(&session.domain_evidence);
            }
        }
    }
    // Retain the input before ingress: if the request never reaches the
    // authority, this is all that recovers the attempt.
    let mut input = AttemptInput::from(&command);
    input.justification = justification;
    let id = String::from(input.operation_id.clone());
    let retained = async {
        AttemptContinuations::new(state)
            .retain(&scope(tower, session, code)?, &id, None, &input)
            .await
    };
    match retained.await {
        Ok(Retention::Retained) => {}
        Ok(Retention::Conflict(original)) => {
            let canonical = admit_command(
                &crate::session::PendingLink {
                    link_id,
                    secret_sha256: original.secret_sha256,
                    secret_generation: original.secret_generation,
                },
                &id,
                session.user_id,
                original.justification.clone(),
            );
            if !canonical.is_ok_and(|mut recovered| {
                recovered.scope_evidence = original.scope_evidence;
                recovered.domain_evidence = original.domain_evidence;
                recovered == command
            }) {
                return conflict(session, code, &input);
            }
        }
        Ok(Retention::Bound(_)) => return conflict(session, code, &input),
        Err(error) => return safe_error(code, error),
    }
    // Persist recovery input before admission. If the response is lost, the
    // link's per-user pointer and bookmark URL both recover this attempt.
    let outcome = match authority.prepare(command.clone()).await {
        Ok(attempt) => match attempt.receipt {
            Some(receipt) => Ok(receipt),
            None => authority.admit(command.clone()).await,
        },
        Err(error) => Err(error),
    };
    match outcome {
        Ok(receipt) => match receipt.result {
            AdmissionResult::Accepted { .. } => {
                let id = String::from(command.operation_id.clone());
                Redirect::to(&attempt_url(code, &id)).into_response()
            }
            AdmissionResult::Rejected {
                reason: Rejection::NotAvailable,
            } => super::invitation_not_found_response(session),
            AdmissionResult::Rejected {
                reason: Rejection::DomainEvidenceMissing | Rejection::DomainEvidenceStale,
            } => super::domains::prompt(session, &[], false, state.config.signin_apps.is_none()),
            AdmissionResult::Rejected {
                reason: Rejection::DomainMismatch { ref required },
            } => super::domains::prompt(session, required, true, false),
            AdmissionResult::Rejected { .. } => render_attempt(
                session,
                code,
                &(&command).into(),
                &receipt_copy(&receipt),
                FormMode::Closed,
                StatusCode::CONFLICT,
            ),
        },
        Err(AuthorityError::Unknown(_)) => unknown(session, code, &input),
        Err(error) => failed(session, code, &command, error),
    }
}

/// Render what the authority's answer means for this attempt.
fn failed(session: &Session, code: &str, command: &Admit, error: AuthorityError) -> Response {
    match error {
        AuthorityError::AuthorizationUnavailable => {
            crate::WebError::ConsoleAuthority.into_response()
        }
        AuthorityError::Invalid | AuthorityError::Missing => safe_error(code, error.into()),
        AuthorityError::Conflict => conflict(session, code, &command.into()),
        AuthorityError::Unknown(_) => unknown(session, code, &command.into()),
    }
}

/// The attempt's operation ID is bound to different input.
fn conflict(session: &Session, code: &str, input: &AttemptInput) -> Response {
    render_attempt(
        session,
        code,
        input,
        "Operation conflict. This ID is bound to different input. Recover the original attempt to check its result and whether a fresh attempt is available.",
        FormMode::Closed,
        StatusCode::CONFLICT,
    )
}

/// Whether the attempt was admitted is unknown; offer only the same attempt.
fn unknown(session: &Session, code: &str, input: &AttemptInput) -> Response {
    render_attempt(
        session,
        code,
        input,
        "Outcome unknown. Your request may have been accepted. Retry this same attempt or check its status.",
        FormMode::Retry,
        StatusCode::BAD_GATEWAY,
    )
}

/// Render one attempt's own input, without the link's page, linking back to
/// its attempt URL.
fn render_attempt(
    session: &Session,
    code: &str,
    input: &AttemptInput,
    message: &str,
    mode: FormMode,
    status: StatusCode,
) -> Response {
    let id = String::from(input.operation_id.clone());
    View {
        id: &id,
        justification: input.justification.as_deref(),
        page: None,
        message,
        original: Some(&attempt_url(code, &id)),
        mode,
        status,
        delivery: vec![],
        organization_delivery: None,
    }
    .render(session, code)
}

fn scope(tower: &tower_sessions::Session, session: &Session, code: &str) -> crate::Result<Scope> {
    Scope::invitation(tower, session.user_id, code)
}

/// The recovery input for an attempt that may never have reached the
/// authority, which also retains attempts once they do.
async fn load_local(
    state: &crate::AppState,
    tower: &tower_sessions::Session,
    session: &Session,
    code: &str,
    operation: Option<&str>,
) -> crate::Result<Option<AttemptInput>> {
    let Ok(scope) = scope(tower, session, code) else {
        return Ok(None);
    };
    let continuations = AttemptContinuations::new(state);
    let result = match operation {
        Some(id) => {
            let Ok(id) = AdmissionOperationId::try_from(id.to_owned()) else {
                return Ok(None);
            };
            continuations.load(&scope, &String::from(id)).await
        }
        None => continuations.latest(&scope).await,
    };
    match result {
        // Crypto failures must never turn a retained attempt into fresh input.
        Err(
            error @ (WebError::KeyringRetry | WebError::KeyringConfig | WebError::Unauthenticated),
        ) => Err(error),
        // Preserve the existing authoritative recovery path on storage failure.
        Err(_) => Ok(None),
        Ok(local) => Ok(local),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum FormMode {
    Fresh,
    Validation,
    Retry,
    Closed,
}

/// Render a failure that interrupted the invitation request flow, sending the
/// visitor back to the invitation link they were working through rather than
/// to a generic page.
pub(crate) fn safe_error(_code: &str, error: WebError) -> Response {
    error.into_response_with_recovery("/join", "Back to the invitation link")
}

fn page_error(session: &Session, code: &str, error: AuthorityError) -> Response {
    match error {
        AuthorityError::AuthorizationUnavailable => {
            crate::WebError::ConsoleAuthority.into_response()
        }
        AuthorityError::Missing => super::invitation_not_found_response(session),
        error => safe_error(code, error.into()),
    }
}

fn receipt_copy(receipt: &AdmissionReceipt) -> String {
    match &receipt.result {
        AdmissionResult::Accepted {
            request_id,
            decision_deadline,
            ..
        } => format!(
            "Request accepted at {}. Request ID: {request_id}.{} Acceptance does not guarantee GitHub invitation delivery.",
            receipt.decided_at,
            decision_deadline
                .map(|d| format!(" Decision deadline: {d}."))
                .unwrap_or_default()
        ),
        AdmissionResult::Rejected { reason } => format!(
            "Request not accepted: {}. This attempt's result is final.",
            match reason {
                Rejection::DomainEvidenceMissing | Rejection::DomainEvidenceStale =>
                    "confirm your email domain with a fresh sign-in before starting a new attempt",
                Rejection::DomainMismatch { .. } =>
                    "your verified email domain does not match this link",
                Rejection::Paused =>
                    "this invitation link is paused; try again after the admin resumes it",
                Rejection::NotAvailable => "the invitation is not available",
                Rejection::ScopeUnavailable => "the requested access is currently unavailable",
                Rejection::ExistingRequest => "you already have a pending or approved request",
                Rejection::AlreadyPending => "you already have a request awaiting review",
                Rejection::AlreadyApproved => "your existing request is already approved",
                Rejection::RerequestLimited { retry_after } =>
                    return format!(
                        "You can request again after {}. This attempt’s result is final; a later request must use a new attempt.",
                        retry_after.to_rfc3339()
                    ),
                Rejection::InstallationUnavailable => "the GitHub installation is unavailable",
                Rejection::RepositoryUnavailable =>
                    "one or more repositories in this invitation link are unavailable",
            }
        ),
    }
}

/// What the attempt page shows for one attempt.
struct View<'a> {
    id: &'a str,
    justification: Option<&'a str>,
    page: Option<&'a RequesterPage>,
    message: &'a str,
    original: Option<&'a str>,
    mode: FormMode,
    status: StatusCode,
    delivery: Vec<ghinvite_ui::invitation::DeliveryPresentation>,
    organization_delivery: Option<ghinvite_ui::organization_delivery::OrganizationDeliveryView>,
}

impl View<'_> {
    fn render(self, session: &Session, _code: &str) -> Response {
        let id = self.id.to_owned();
        let justification = self.justification.unwrap_or_default().to_owned();
        let page = self.page.cloned();
        let message = self.message.to_owned();
        let original = self.original.map(str::to_owned);
        let (mode, status, delivery) = (self.mode, self.status, self.delivery);
        let organization_delivery = self.organization_delivery;
        let has_organization = page
            .as_ref()
            .is_some_and(|p| p.targets.organization.is_some());
        let title = if has_organization {
            "Request access"
        } else {
            "Request repository access"
        };
        let login = session.login.clone();
        let refresh_seconds = page
            .as_ref()
            .and_then(|p| p.request.as_ref())
            .filter(|r| r.state == ghinvite_core::RequestState::Pending)
            .map(|_| ghinvite_ui::invitation::PENDING_REFRESH_SECONDS);
        let html = crate::render::render_with_csrf(session.csrf_token.clone(), move || {
            rsx! {
                ghinvite_ui::layouts::InvitationLayout {
                    signed_in_login: Some(login.clone()), title: format!("{title} · ghinvite"),
                    account_login: None, active_nav: None, flash: None,
                    refresh_seconds,
                    div { class: "space-y-5",
                    h1 { class: "text-2xl font-semibold tracking-tight", "{title}" }
                    ghinvite_ui::invitation::IdentityConfirmation {
                        login: login.clone(),
                        return_to: "/join".to_owned(),
                    }
                    if let Some(page) = &page {
                        ghinvite_ui::invitation::AccessSummary {
                            targets: page.targets.clone(), approval_required: page.approval_required || page.request.as_ref().is_some_and(|r| r.state == ghinvite_core::RequestState::Pending),
                        }
                        if page.targets.organization.is_some() && page.two_factor_required == Some(true) {
                            p { class: "alert alert-info", role: "note", "This organization requires two-factor authentication; you'll need it to accept." }
                        }
                    }
                    p { class: "alert alert-info", role: "status", "{message}" }
                    if let Some(page) = &page {
                        if page.paused && (page.request.is_some() || page.attempt.is_some()) {
                            p { class: "alert alert-warning", role: "status", "This link isn't accepting requests right now. Existing requests continue." }
                        }
                        if let Some(request) = &page.request {
                            p { "Current request status: {request.state}" }
                            if request.state == ghinvite_core::RequestState::Declined {
                                if let Some(message) = request.decision.as_ref().and_then(|d|d.requester_message.as_ref()) {
                                    p { class: "whitespace-pre-wrap", "Message from the reviewer: {message}" }
                                }
                            }
                            if request.state == ghinvite_core::RequestState::Cancelled {
                                p { role: "status", "Your request was cancelled because this invitation link was revoked." }
                            }
                            if request.state == ghinvite_core::RequestState::Lapsed {
                                h2 { class: "text-xl font-semibold", "Invitation lapsed" }
                                p { "Your invitation expired before it was accepted. The original approval and recorded delivery history remain below." }
                                p { "A new request consumes another use and must satisfy this link’s current rules. Re-requests are limited to one per seven days; a short invitation lifetime requires another admin review." }
                                if let Some(retry_after) = page.rerequest_after {
                                    p { role: "status", "You can request again after {retry_after.to_rfc3339()}. The link must still accept requests then." }
                                }
                                if page.can_start_fresh && mode != FormMode::Fresh {
                                    a { class: "btn btn-primary", href: "/join?fresh=true", "Request again" }
                                } else if !page.can_start_fresh && page.rerequest_after.is_none() {
                                    p { "This link is not accepting another request right now." }
                                }
                            }
                            if request.state == ghinvite_core::RequestState::Pending {
                                h2 { class: "text-xl font-semibold", "Awaiting review" }
                                p { class: "text-sm leading-6 text-base-content/70", "The account admins have your request. This page checks for updates every 20 seconds." }
                            }
                            if request.state == ghinvite_core::RequestState::Approved {
                                div { class: "alert alert-success",
                                    div {
                                        h2 { class: "text-xl font-semibold", "Approved" }
                                        p { if has_organization { "Your request is approved. Organization, team and repository delivery are tracked separately below. Check again for updates; accepting access happens on GitHub." } else { "Your request is approved. Repository delivery is tracked separately below. Check again for updates; accepting access happens on GitHub." } }
                                    }
                                }
                            }
                            p { if has_organization { "Approval does not guarantee delivery. Organization or team eligibility and unavailable repositories may block delivery; your request keeps its original targets and decision deadline." } else { "Approval does not guarantee delivery. Unavailable repositories may block delivery; your request keeps its original scope and decision deadline." } }
                            if original.is_none() {
                                a { class: "btn btn-outline", href: "/join", "Check again" }
                            }
                            a { class:"link",href:format!("/requests/{}",request.request_id),"View request status and refresh delivery" }
                        }
                    }
                    if let Some(view) = &organization_delivery {
                        ghinvite_ui::organization_delivery::OrganizationDelivery { view: view.clone() }
                        if let Some(request) = page.as_ref().and_then(|p|p.request.as_ref()).filter(|r|r.state == ghinvite_core::RequestState::Approved) {
                            form { method:"post", action:"/join/organization/refresh", "data-organization-refresh":"",
                                ghinvite_ui::csrf::CsrfField {}
                                input {r#type:"hidden",name:"request_id",value:request.request_id.to_string()}
                                button {r#type:"submit",class:"btn btn-outline","Refresh organization delivery"}
                                p {role:"status","aria-live":"polite","data-refresh-status":""}
                            }
                        }
                    }
                    if !delivery.is_empty() {
                        ul { class: "space-y-4", aria_label: "Repository delivery",
                            for row in &delivery {
                                ghinvite_ui::invitation::DeliveryRow { row: row.clone(), login: login.clone() }
                            }
                        }
                    }
                    if mode != FormMode::Closed {
                        form { method: "post", action: "/join?operation_id={id}", class: "space-y-4",
                            ghinvite_ui::csrf::CsrfField {}
                            input { r#type: "hidden", name: "operation_id", value: "{id}" }
                            ghinvite_ui::invitation::JustificationField {
                                value: justification.clone(), readonly: mode == FormMode::Retry,
                                max_chars: Some(ghinvite_core::admission::MAX_JUSTIFICATION_CHARS),
                                error: if mode == FormMode::Validation { ghinvite_ui::request_form::justification_error(&justification) } else { None },
                            }
                            div { class: "card-actions justify-end",
                                button { r#type: "submit", class: "btn btn-primary", if mode == FormMode::Retry { "Retry same attempt" } else if page.as_ref().and_then(|p|p.request.as_ref()).is_some_and(|r|r.state == ghinvite_core::RequestState::Lapsed) { "Request again" } else { "Submit request" } }
                            }
                        }
                    }
                    if let Some(original) = &original {
                        div { class: "flex flex-wrap gap-3",
                            a { class: "btn btn-outline", href: "{original}", "Recover original attempt / Check again" }
                            if page.as_ref().is_some_and(|page| page.can_start_fresh) {
                                a { class: "link", href: "{original}&fresh=true", "Start a fresh attempt with edited input" }
                            }
                        }
                    }
                    p { class: "text-sm leading-6 text-base-content/70", "Keep the invitation URL. Open it again to recover your latest request after signing in." }
                    }
                }
            }
        });
        (status, Html(html)).into_response()
    }
}
