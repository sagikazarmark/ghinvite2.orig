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

fn attempt_url(code: &str, id: &str) -> String {
    format!("/i/{code}?operation_id={id}")
}

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
/// is implied by the continuation scope; the link is resolved later.
#[derive(serde::Serialize, serde::Deserialize)]
struct AttemptInput {
    operation_id: AdmissionOperationId,
    justification: Option<String>,
}

impl From<&Admit> for AttemptInput {
    fn from(command: &Admit) -> Self {
        Self {
            operation_id: command.operation_id.clone(),
            justification: command.justification.clone(),
        }
    }
}

/// Parse and normalize submitted input before any network call. The link is
/// resolved later, so `link_id` is a placeholder.
fn admit_command(
    operation_id: &str,
    requester_id: u64,
    justification: Option<String>,
) -> Result<Admit, InvalidInput> {
    let operation_id = AdmissionOperationId::try_from(operation_id.to_owned())
        .map_err(|_| InvalidInput::OperationId)?;
    let mut command = Admit {
        link_id: ghinvite_core::InvitationLinkId::new(),
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
    code: &str,
    operation: Option<&str>,
    fresh: bool,
) -> Response {
    let authority = &state.link_authority;
    let operation_id = match operation
        .map(|id| AdmissionOperationId::try_from(id.to_owned()))
        .transpose()
    {
        Ok(id) => id,
        Err(_) => return safe_error(code, WebError::BadRequest("Invalid operation ID.".into())),
    };
    let local = load_local(state, tower, session, code, operation).await;
    let page = match authority
        .requester_page(code, session.user_id, operation_id)
        .await
    {
        Ok(page) => page,
        Err(error) => {
            if let Some(local) = local {
                // The exact attempt may never have reached ingress. Recover
                // current eligibility independently, without submitting it.
                if matches!(error, AuthorityError::Missing)
                    && let Ok(summary) = authority.requester_page(code, session.user_id, None).await
                {
                    let original = attempt_url(code, &String::from(local.operation_id.clone()));
                    let fresh = fresh && summary.can_start_fresh;
                    let id = if fresh {
                        ghinvite_core::RequestId::new().to_string()
                    } else {
                        String::from(local.operation_id.clone())
                    };
                    return render(
                        session,
                        code,
                        &id,
                        if fresh {
                            None
                        } else {
                            local.justification.as_deref()
                        },
                        Some(&summary),
                        if fresh {
                            "This is a fresh attempt. The original attempt remains recoverable below."
                        } else {
                            "Outcome unknown. Retry this same attempt to confirm whether your request was accepted."
                        },
                        Some(&original),
                        if fresh {
                            FormMode::Fresh
                        } else {
                            FormMode::Retry
                        },
                        StatusCode::OK,
                        vec![],
                    );
                }
                if matches!(error, AuthorityError::Missing | AuthorityError::Unknown(_)) {
                    return unknown(session, code, &local);
                }
            }
            return page_error(session, code, error);
        }
    };
    if operation.is_none()
        && !fresh
        && let Some(local) = local
    {
        let local_id = String::from(local.operation_id.clone());
        if page.attempt.is_none() {
            return Redirect::to(&attempt_url(code, &local_id)).into_response();
        }
    }
    if !page.can_start_fresh && page.attempt.is_none() && page.request.is_none() {
        return super::invitation_not_found_response(session);
    }
    let fresh = fresh && page.can_start_fresh;
    let original = page
        .attempt
        .as_ref()
        .map(|a| attempt_url(code, &String::from(a.input.operation_id.clone())));
    let (id, justification, receipt) = match &page.attempt {
        Some(attempt) if !fresh => (
            String::from(attempt.input.operation_id.clone()),
            attempt.input.justification.clone(),
            attempt.receipt.clone(),
        ),
        _ => (ghinvite_core::RequestId::new().to_string(), None, None),
    };
    let message = receipt.as_ref().map(receipt_copy).unwrap_or_else(|| {
        if page.attempt.is_some() && !fresh {
            "Outcome unknown. Retry this same attempt to confirm whether your request was accepted."
                .into()
        } else if fresh {
            "This is a fresh attempt. The original attempt remains recoverable below.".into()
        } else if page.request.is_some() {
            "Your existing request is shown below.".into()
        } else {
            "Review the repositories and submit your request.".into()
        }
    });
    let mut delivery = Vec::new();
    if let Some(request) = &page.request
        && request.state == ghinvite_core::RequestState::Approved
    {
        let progress = state
            .link_authority
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
        let invitations = state
            .storage
            .list_github_invitations_for_request(request.request_id)
            .await
            .ok();
        for repo in &page.repos {
            let row = ghinvite_ui::invitation::delivery_presentation(
                repo,
                receipts.as_deref().unwrap_or_default(),
                progress.as_deref().unwrap_or_default(),
                invitations.as_deref().unwrap_or_default(),
                receipts.is_none() || invitations.is_none() || progress.is_none(),
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
    render(
        session,
        code,
        &id,
        justification.as_deref(),
        Some(&page),
        &message,
        original.as_deref(),
        mode,
        StatusCode::OK,
        delivery,
    )
}

pub async fn submit(
    state: &crate::AppState,
    tower: &tower_sessions::Session,
    session: &Session,
    code: &str,
    operation: &str,
    justification: Option<String>,
) -> Response {
    let authority = &state.link_authority;
    if ghinvite_core::Slug::from_string(code.to_owned()).is_err() {
        return WebError::NotFound.into_response();
    }
    // Validate identity and normalize input before any network call.
    let invalid_justification = ghinvite_ui::request_form::justification_error(
        justification.as_deref().unwrap_or_default(),
    )
    .is_some();
    let mut command = match admit_command(operation, session.user_id, justification.clone()) {
        Ok(command) => command,
        Err(InvalidInput::Justification) if invalid_justification => {
            let page = match authority.requester_page(code, session.user_id, None).await {
                Ok(page) => page,
                Err(error) => return page_error(session, code, error),
            };
            if !page.can_start_fresh {
                return self::page(state, tower, session, code, None, false).await;
            }
            return render(
                session,
                code,
                operation,
                justification.as_deref(),
                Some(&page),
                "Your request has not been submitted. Correct the justification below.",
                None,
                FormMode::Validation,
                StatusCode::BAD_REQUEST,
                vec![],
            );
        }
        Err(invalid) => return safe_error(code, invalid.into()),
    };
    // Retain the input before ingress: if the request never reaches the
    // authority, this is all that recovers the attempt.
    let input = AttemptInput::from(&command);
    let id = String::from(input.operation_id.clone());
    let retained = async {
        AttemptContinuations::new(state)
            .retain(&scope(tower, session, code)?, &id, None, &input)
            .await
    };
    match retained.await {
        Ok(Retention::Retained) => {}
        Ok(Retention::Conflict(_) | Retention::Bound(_)) => return conflict(session, code, &input),
        Err(error) => return safe_error(code, error),
    }
    command.link_id = match authority.resolve(code).await {
        Ok(id) => id,
        Err(error) => return failed(session, code, &command, error),
    };
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
        Ok(receipt) => {
            let id = String::from(command.operation_id.clone());
            match receipt.result {
                AdmissionResult::Accepted { .. } => {
                    Redirect::to(&attempt_url(code, &id)).into_response()
                }
                AdmissionResult::Rejected { .. } => render(
                    session,
                    code,
                    &id,
                    command.justification.as_deref(),
                    None,
                    &receipt_copy(&receipt),
                    Some(&attempt_url(code, &id)),
                    FormMode::Closed,
                    StatusCode::CONFLICT,
                    vec![],
                ),
            }
        }
        Err(error) => failed(session, code, &command, error),
    }
}

/// Render what the authority's answer means for this attempt.
fn failed(session: &Session, code: &str, command: &Admit, error: AuthorityError) -> Response {
    match error {
        AuthorityError::Invalid | AuthorityError::Missing => safe_error(code, error.into()),
        AuthorityError::Conflict => conflict(session, code, &command.into()),
        AuthorityError::Unknown(_) => unknown(session, code, &command.into()),
    }
}

/// The attempt's operation ID is bound to different input.
fn conflict(session: &Session, code: &str, input: &AttemptInput) -> Response {
    let id = String::from(input.operation_id.clone());
    render(
        session,
        code,
        &id,
        input.justification.as_deref(),
        None,
        "Operation conflict. This ID is bound to different input. Recover the original attempt to check its result and whether a fresh attempt is available.",
        Some(&attempt_url(code, &id)),
        FormMode::Closed,
        StatusCode::CONFLICT,
        vec![],
    )
}

/// Whether the attempt was admitted is unknown; offer only the same attempt.
fn unknown(session: &Session, code: &str, input: &AttemptInput) -> Response {
    let id = String::from(input.operation_id.clone());
    render(
        session,
        code,
        &id,
        input.justification.as_deref(),
        None,
        "Outcome unknown. Your request may have been accepted. Retry this same attempt or check its status.",
        Some(&attempt_url(code, &id)),
        FormMode::Retry,
        StatusCode::BAD_GATEWAY,
        vec![],
    )
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
) -> Option<AttemptInput> {
    let scope = scope(tower, session, code).ok()?;
    let continuations = AttemptContinuations::new(state);
    match operation {
        Some(id) => {
            let id = String::from(AdmissionOperationId::try_from(id.to_owned()).ok()?);
            continuations.load(&scope, &id).await.ok()?
        }
        None => continuations.latest(&scope).await.ok()?,
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
pub(crate) fn safe_error(code: &str, error: WebError) -> Response {
    error.into_response_with_recovery(format!("/i/{code}"), "Back to the invitation link")
}

fn page_error(session: &Session, code: &str, error: AuthorityError) -> Response {
    match error {
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
                Rejection::Revoked => "the invitation link was revoked",
                Rejection::Expired => "the invitation link expired",
                Rejection::Exhausted => "the invitation link has reached its max use",
                Rejection::ExistingRequest => "you already have a pending or approved request",
                Rejection::InstallationUnavailable => "the GitHub installation is unavailable",
                Rejection::RepositoryUnavailable =>
                    "one or more repositories in this invitation link are unavailable",
            }
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn render(
    session: &Session,
    code: &str,
    id: &str,
    justification: Option<&str>,
    page: Option<&RequesterPage>,
    message: &str,
    original: Option<&str>,
    mode: FormMode,
    status: StatusCode,
    delivery: Vec<ghinvite_ui::invitation::DeliveryPresentation>,
) -> Response {
    let code = code.to_owned();
    let id = id.to_owned();
    let justification = justification.unwrap_or_default().to_owned();
    let page = page.cloned();
    let message = message.to_owned();
    let original = original.map(str::to_owned);
    let login = session.login.clone();
    let refresh_seconds = page
        .as_ref()
        .and_then(|p| p.request.as_ref())
        .filter(|r| r.state == ghinvite_core::RequestState::Pending)
        .map(|_| ghinvite_ui::invitation::PENDING_REFRESH_SECONDS);
    let html = crate::views::render::render_with_csrf(session.csrf_token.clone(), move || {
        rsx! {
            ghinvite_ui::layouts::InvitationLayout {
                signed_in_login: Some(login.clone()), title: "Request repository access · ghinvite".to_owned(),
                account_login: None, active_nav: None, flash: None,
                refresh_seconds,
                div { class: "space-y-5",
                h1 { class: "text-2xl font-semibold tracking-tight", "Request repository access" }
                ghinvite_ui::invitation::IdentityConfirmation {
                    login: login.clone(),
                    return_to: format!("/i/{code}"),
                }
                if let Some(page) = &page {
                    ghinvite_ui::invitation::AccessSummary {
                        permission: page.permission, repos: page.repos.clone(), approval_required: page.approval_required,
                    }
                }
                p { class: "alert alert-info", role: "status", "{message}" }
                if let Some(page) = &page {
                    if let Some(request) = &page.request {
                        p { "Current request status: {request.state}" }
                        if request.state == ghinvite_core::RequestState::Pending {
                            h2 { class: "text-xl font-semibold", "Awaiting review" }
                            p { class: "text-sm leading-6 text-base-content/70", "The account admins have your request. This page checks for updates every 20 seconds." }
                        }
                        if request.state == ghinvite_core::RequestState::Approved {
                            div { class: "alert alert-success",
                                div {
                                    h2 { class: "text-xl font-semibold", "Approved" }
                                    p { "Your request is approved. Repository delivery is tracked separately below. Check again for updates; accepting access happens on GitHub." }
                                }
                            }
                        }
                        p { "Approval does not guarantee delivery. Unavailable repositories may block delivery; your request keeps its original scope and decision deadline." }
                        if original.is_none() {
                            a { class: "btn btn-outline", href: "/i/{code}", "Check again" }
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
                    form { method: "post", action: "/i/{code}?operation_id={id}", class: "space-y-4",
                        ghinvite_ui::csrf::CsrfField {}
                        input { r#type: "hidden", name: "operation_id", value: "{id}" }
                        ghinvite_ui::invitation::JustificationField {
                            value: justification.clone(), readonly: mode == FormMode::Retry,
                            max_bytes: Some(ghinvite_core::admission::MAX_JUSTIFICATION_BYTES),
                            error: if mode == FormMode::Validation { ghinvite_ui::request_form::justification_error(&justification) } else { None },
                        }
                        div { class: "card-actions justify-end",
                            button { r#type: "submit", class: "btn btn-primary", if mode == FormMode::Retry { "Retry same attempt" } else { "Submit request" } }
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
                p { class: "text-sm leading-6 text-base-content/70", "Keep the attempt URL to recover it after signing in again. Opening this invitation link also recovers your latest attempt." }
                }
            }
        }
    });
    (status, Html(html)).into_response()
}
