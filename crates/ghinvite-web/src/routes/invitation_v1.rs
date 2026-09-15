//! Native forms for recoverable authoritative attempts; SQL is not on this path.
use crate::{WebError, admission::RestateAdmission, session::Session};
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
};
use dioxus::prelude::*;
use ghinvite_core::admission::{
    AdmissionReceipt, AdmissionResult, Admit, Rejection, RequesterPage,
};

fn attempt_url(code: &str, id: &str) -> String {
    format!("/i/{code}?operation_id={id}")
}

pub async fn page(
    state: &crate::AppState,
    tower: &tower_sessions::Session,
    admission: &RestateAdmission,
    session: &Session,
    code: &str,
    operation: Option<&str>,
    fresh: bool,
) -> Response {
    let local = load_local(state, tower, session.user_id, code, operation).await;
    let page = match admission.lookup(code, session.user_id, operation).await {
        Ok(page) => page,
        Err(error) => {
            if let Some(local) = local {
                if fresh {
                    return render(
                        session,
                        code,
                        &ghinvite_core::RequestId::new().to_string(),
                        None,
                        None,
                        "This is a fresh attempt. The original attempt remains recoverable below.",
                        Some(&attempt_url(code, &String::from(local.operation_id))),
                        FormMode::Fresh,
                        StatusCode::OK,
                        vec![],
                    );
                }
                if matches!(error, WebError::NotFound | WebError::Restate(_)) {
                    return failed(session, code, &local, WebError::Restate("unknown".into()));
                }
            }
            return safe_error(error);
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
        } else {
            "Review the repositories and submit your request.".into()
        }
    });
    let mut delivery = Vec::new();
    if let Some(request) = &page.request
        && request.state == ghinvite_core::RequestState::Approved
    {
        let progress = if let Some(lifecycle) = &state.request_lifecycle {
            lifecycle
                .delivery_progress(ghinvite_core::request_lifecycle::RequestStatus {
                    link_id: page.link_id,
                    request_id: request.request_id,
                    requester_id: session.user_id,
                })
                .await
                .ok()
        } else {
            None
        };
        let receipts = state
            .storage
            .list_delivery_for_request(request.request_id)
            .await
            .ok();
        for repo in &page.repos {
            let label = ghinvite_ui::invitation::delivery_label(
                receipts
                    .as_ref()
                    .and_then(|rows| rows.iter().find(|r| r.command.repo_id == repo.repo_id))
                    .map(|r| &r.outcome),
                progress
                    .as_ref()
                    .and_then(|rows| rows.iter().find(|r| r.repo_id == repo.repo_id))
                    .map(|r| &r.stage),
                None,
            );
            delivery.push(format!("{}: {label}", repo.repo_full_name));
        }
    }
    let mode = if receipt.is_some() {
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
    admission: &RestateAdmission,
    session: &Session,
    code: &str,
    operation: &str,
    justification: Option<String>,
) -> Response {
    if ghinvite_core::Slug::from_string(code.to_owned()).is_err() {
        return WebError::NotFound.into_response();
    }
    // Validate identity and normalize input before any network call.
    let mut command = match admission.command(
        ghinvite_core::InvitationLinkId::new(),
        operation,
        session.user_id,
        justification,
    ) {
        Ok(command) => command,
        Err(error) => return safe_error(error),
    };
    // Save a separate protected record before ingress. Each attempt has its own
    // key, so concurrent request-local session snapshots cannot erase it.
    if let Err(error) = save_local(state, tower, session.user_id, code, &command).await {
        return failed(session, code, &command, error);
    }
    command.link_id = match admission.resolve(code).await {
        Ok(id) => id,
        Err(error) => return failed(session, code, &command, error),
    };
    // Persist recovery input before admission. If the response is lost, the
    // link's per-user pointer and bookmark URL both recover this attempt.
    let outcome = match admission.prepare(command.clone()).await {
        Ok(attempt) => match attempt.receipt {
            Some(receipt) => Ok(receipt),
            None => admission.admit(command.clone()).await,
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

fn failed(session: &Session, code: &str, command: &Admit, error: WebError) -> Response {
    match error {
        WebError::BadRequest(_)
        | WebError::NotFound
        | WebError::Forbidden
        | WebError::Session(_) => safe_error(error),
        _ => {
            let conflict = matches!(error, WebError::Conflict);
            let id = String::from(command.operation_id.clone());
            render(
                session,
                code,
                &id,
                command.justification.as_deref(),
                None,
                if conflict {
                    "Operation conflict. This ID is bound to different input. Recover the original attempt, or explicitly start a fresh attempt to edit your input."
                } else {
                    "Outcome unknown. Your request may have been accepted. Retry this same attempt or check its status."
                },
                Some(&attempt_url(code, &id)),
                if conflict {
                    FormMode::Closed
                } else {
                    FormMode::Retry
                },
                if conflict {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::BAD_GATEWAY
                },
                vec![],
            )
        }
    }
}

fn local_key(
    tower: &tower_sessions::Session,
    user: u64,
    code: &str,
    operation: &str,
) -> Option<tower_sessions::session::Id> {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!(
        "ghinvite/attempt/v1/{}/{user}/{code}/{operation}",
        tower.id()?
    ));
    Some(tower_sessions::session::Id(i128::from_be_bytes(
        digest[..16].try_into().unwrap(),
    )))
}
async fn save_local(
    state: &crate::AppState,
    tower: &tower_sessions::Session,
    user: u64,
    code: &str,
    command: &Admit,
) -> crate::Result<()> {
    let id = String::from(command.operation_id.clone());
    let key = local_key(tower, user, code, &id)
        .ok_or_else(|| WebError::Session("missing session".into()))?;
    let store = state.attempt_store.as_ref().unwrap();
    let failure = |_| WebError::Session("Attempt recovery temporarily unavailable.".into());
    if let Some(old) = store.load(&key).await.map_err(failure)? {
        let old: Admit = serde_json::from_value(old.data["attempt"].clone())
            .map_err(|_| WebError::Internal("invalid continuation".into()))?;
        if old.justification != command.justification
            || old.requester_id != command.requester_id
            || old.operation_id != command.operation_id
        {
            return Err(WebError::Conflict);
        }
    } else {
        let mut record = continuation(key);
        record
            .data
            .insert("attempt".into(), serde_json::to_value(command).unwrap());
        store.create(&mut record).await.map_err(failure)?;
        if record.id != key {
            return Err(WebError::Conflict);
        }
    }
    let pointer = local_key(tower, user, code, "latest").unwrap();
    let existing = store.load(&pointer).await.map_err(failure)?;
    let is_new = existing.is_none();
    let mut record = existing.unwrap_or_else(|| continuation(pointer));
    record
        .data
        .insert("operation".into(), serde_json::json!(id));
    if is_new {
        store.create(&mut record).await.map_err(failure)?;
    } else {
        store.save(&record).await.map_err(failure)?;
    }
    Ok(())
}
fn continuation(id: tower_sessions::session::Id) -> tower_sessions::session::Record {
    tower_sessions::session::Record {
        id,
        data: Default::default(),
        expiry_date: tower_sessions::cookie::time::OffsetDateTime::now_utc()
            + tower_sessions::cookie::time::Duration::minutes(30),
    }
}
async fn load_local(
    state: &crate::AppState,
    tower: &tower_sessions::Session,
    user: u64,
    code: &str,
    operation: Option<&str>,
) -> Option<Admit> {
    let store = state.attempt_store.as_ref()?;
    let id = match operation {
        Some(id) => String::from(
            ghinvite_core::admission::AdmissionOperationId::try_from(id.to_owned()).ok()?,
        ),
        None => store
            .load(&local_key(tower, user, code, "latest")?)
            .await
            .ok()??
            .data["operation"]
            .as_str()?
            .to_owned(),
    };
    let record = store
        .load(&local_key(tower, user, code, &id)?)
        .await
        .ok()??;
    serde_json::from_value(record.data["attempt"].clone()).ok()
}

#[derive(Clone, Copy, PartialEq)]
enum FormMode {
    Fresh,
    Retry,
    Closed,
}

pub(crate) fn safe_error(error: WebError) -> Response {
    match error {
        WebError::Restate(_) => (
            StatusCode::BAD_GATEWAY,
            "Status temporarily unavailable. Keep this URL and check again.",
        )
            .into_response(),
        _ => error.into_response(),
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
    delivery: Vec<String>,
) -> Response {
    let code = code.to_owned();
    let id = id.to_owned();
    let justification = justification.unwrap_or_default().to_owned();
    let page = page.cloned();
    let message = message.to_owned();
    let original = original.map(str::to_owned);
    let login = session.login.clone();
    let html = crate::views::render::render_with_csrf(session.csrf_token.clone(), move || {
        rsx! {
            ghinvite_ui::layouts::InvitationLayout {
                signed_in_login: Some(login.clone()), title: "Request repository access · ghinvite".to_owned(),
                account_login: None, active_nav: None, flash: None,
                h1 { "Request repository access" }
                p { role: "status", "{message}" }
                if let Some(page) = &page {
                    p { "Permission: {page.permission}" }
                    ul { for repo in &page.repos { li { "{repo.repo_full_name}" } } }
                    if let Some(request) = &page.request {
                        p { "Current request status: {request.state}" }
                        p { "Approval does not guarantee delivery. Unavailable repositories may block delivery; your request keeps its original scope and decision deadline." }
                    }
                }
                ul { for row in &delivery { li { "{row}" } } }
                if mode != FormMode::Closed {
                    form { method: "post", action: "/i/{code}?operation_id={id}",
                        ghinvite_ui::csrf::CsrfField {}
                        input { r#type: "hidden", name: "operation_id", value: "{id}" }
                        label { r#for: "justification", "Justification" }
                        textarea { id: "justification", name: "justification", readonly: mode == FormMode::Retry, "{justification}" }
                        button { r#type: "submit", if mode == FormMode::Retry { "Retry same attempt" } else { "Submit request" } }
                    }
                }
                if let Some(original) = &original {
                    a { href: "{original}", "Recover original attempt / Check again" }
                    a { href: "{original}&fresh=true", "Start a fresh attempt with edited input" }
                }
                p { "Keep the attempt URL to recover it after signing in again. Opening this invitation link also recovers your latest attempt." }
            }
        }
    });
    (status, Html(html)).into_response()
}
