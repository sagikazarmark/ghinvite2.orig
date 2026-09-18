//! Session-bound continuations persisted before ingress, including on a lost
//! acknowledgement. Current account authorization is required on every access.
use super::*;
use axum::http::StatusCode;
use axum::response::{Redirect, Response};
use base64::Engine;
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use ghinvite_core::admission::AdminLinkCommand;
use ghinvite_core::request_lifecycle::{DecideRequest, DecisionOutcome, DecisionReceipt};
use ghinvite_core::storage::projection::CreateLink;
use rand::RngCore;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum Command {
    Create(CreateLink),
    Revoke(AdminLinkCommand),
    Decision(DecideRequest),
}

impl Command {
    fn id(&self) -> String {
        match self {
            Self::Create(c) => format!("create-{}", c.link_id),
            Self::Revoke(c) => format!("revoke-{}", c.link_id),
            Self::Decision(c) => format!(
                "decision-{}-{}",
                c.link_id,
                String::from(c.operation_id.clone())
            ),
        }
    }

    fn link_id(&self) -> ghinvite_core::InvitationLinkId {
        match self {
            Self::Create(c) => c.link_id,
            Self::Revoke(c) => c.link_id,
            Self::Decision(c) => c.link_id,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Create(_) => "Create invitation link",
            Self::Revoke(_) => "Revoke invitation link",
            Self::Decision(c) => match c.action {
                ghinvite_core::request_lifecycle::DecisionAction::Approve => {
                    "Approve invitation request"
                }
                ghinvite_core::request_lifecycle::DecisionAction::Decline { .. } => {
                    "Decline invitation request"
                }
            },
        }
    }
}

fn scope(admin: &RequireConsoleAdminOf) -> crate::Result<String> {
    use sha2::{Digest, Sha256};
    let session_id = admin.tower.id().ok_or_else(|| failure("missing session"))?;
    let digest = Sha256::digest(format!(
        "ghinvite/admin-attempt/v2/{session_id}/{}/{}",
        admin.session.user_id, admin.account.account_id
    ));
    Ok(hex::encode(digest))
}

fn failure(_: impl std::fmt::Display) -> crate::WebError {
    crate::WebError::Session("Attempt recovery temporarily unavailable.".into())
}

fn seal(state: &AppState, scope: &str, command: &Command) -> crate::Result<String> {
    let mut nonce = [0; 24];
    rand::rngs::OsRng
        .try_fill_bytes(&mut nonce)
        .map_err(failure)?;
    let cipher = XChaCha20Poly1305::new((&state.config.session_secret).into());
    let aad = format!("admin-attempt/v1/{scope}/{}", command.id());
    let plaintext = serde_json::to_vec(command).map_err(failure)?;
    let encrypted = cipher
        .encrypt(
            &XNonce::try_from(nonce.as_slice()).map_err(failure)?,
            Payload {
                msg: &plaintext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(failure)?;
    let mut bytes = nonce.to_vec();
    bytes.extend(encrypted);
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn open(
    state: &AppState,
    scope: &str,
    stored: ghinvite_core::storage::admin_attempts::StoredAttempt,
) -> crate::Result<Command> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(stored.payload)
        .map_err(failure)?;
    if bytes.len() < 24 {
        return Err(failure("invalid continuation"));
    }
    let cipher = XChaCha20Poly1305::new((&state.config.session_secret).into());
    let aad = format!("admin-attempt/v1/{scope}/{}", stored.id);
    let plaintext = cipher
        .decrypt(
            &XNonce::try_from(&bytes[..24]).map_err(failure)?,
            Payload {
                msg: &bytes[24..],
                aad: aad.as_bytes(),
            },
        )
        .map_err(failure)?;
    serde_json::from_slice(&plaintext).map_err(failure)
}

pub(super) async fn load(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    id: &str,
) -> crate::Result<Option<Command>> {
    let scope = scope(admin)?;
    state
        .storage
        .get_admin_attempt(&scope, id, Utc::now().timestamp())
        .await?
        .map(|r| open(state, &scope, r))
        .transpose()
}

// An immutable record per attempt prevents request-local session snapshots from
// overwriting each other's submitted input. A collision never authorizes ingress.
async fn retain(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    command: &Command,
) -> crate::Result<Command> {
    let scope = scope(admin)?;
    let binding = match command {
        Command::Decision(c) => format!("request-{}-{}", c.link_id, c.request_id),
        _ => command.id(),
    };
    let payload = seal(state, &scope, command)?;
    let stored = state
        .storage
        .retain_admin_attempt(
            &scope,
            &command.id(),
            &binding,
            &payload,
            admin.tower.expiry_date().unix_timestamp(),
            Utc::now().timestamp(),
        )
        .await?;
    open(state, &scope, stored)
}

fn url(admin: &RequireConsoleAdminOf, command: &Command) -> String {
    format!(
        "/console/accounts/{}/attempts/{}",
        admin.account.account_login,
        command.id()
    )
}

pub(super) async fn index(State(state): State<AppState>, admin: RequireConsoleAdminOf) -> Response {
    let result = async {
        let scope = scope(&admin)?;
        let records = state
            .storage
            .list_admin_attempts(&scope, Utc::now().timestamp())
            .await?;
        records
            .into_iter()
            .map(|r| open(&state, &scope, r))
            .collect::<crate::Result<Vec<_>>>()
    }
    .await;
    match result {
        Ok(commands) => {
            let rows = commands
                .iter()
                .map(|c| ghinvite_ui::attempts::AttemptRow {
                    label: c.label().into(),
                    id: c.id(),
                    status_url: url(&admin, c),
                    detail_url: format!(
                        "/console/accounts/{}/links/{}",
                        admin.account.account_login,
                        c.link_id()
                    ),
                })
                .collect::<Vec<_>>();
            let html = render(admin.session.csrf_token.clone(), move || {
                rsx! {
                    ghinvite_ui::attempts::AttemptListPage {
                        signed_in_login: admin.session.login.clone(), account_login: admin.account.account_login.clone(), rows: rows.clone(),
                    }
                }
            });
            (
                [(axum::http::header::CACHE_CONTROL, "private, no-store")],
                Html(html),
            )
                .into_response()
        }
        Err(e) => e.into_response(),
    }
}

pub(super) async fn page(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, id)): axum::extract::Path<(String, String)>,
) -> Response {
    let command = match load(&state, &admin, &id).await {
        Ok(Some(c)) => c,
        Ok(None) => return console_not_found_response(&admin),
        Err(e) => return e.into_response(),
    };
    let result = match &command {
        Command::Decision(c) => {
            let result = match &state.request_lifecycle {
                Some(l) => l.decision_status(c.clone()).await,
                None => Err(crate::WebError::NotFound),
            };
            return match result {
                Ok(Some(receipt)) => decision_response(&admin, &command, &receipt),
                Ok(None) | Err(crate::WebError::NotFound) => unknown(&admin, &command),
                Err(e) => failed(&admin, &command, e),
            };
        }
        Command::Create(c) => match &state.admission {
            Some(a) => a
                .link_status(AdminLinkCommand {
                    link_id: c.link_id,
                    admin: c.admin.clone(),
                })
                .await
                .and_then(|link| {
                    if link.creation == *c {
                        Ok(Some("Invitation link created."))
                    } else {
                        Err(crate::WebError::Conflict)
                    }
                }),
            None => Err(crate::WebError::NotFound),
        },
        Command::Revoke(c) => match &state.admission {
            Some(a) => a.link_status(c.clone()).await.map(|link| {
                link.revoked_at
                    .map(|_| "Invitation link stopped accepting new invitation requests.")
            }),
            None => Err(crate::WebError::NotFound),
        },
    };
    match result {
        Ok(Some(message)) => render_attempt(&admin, &command, StatusCode::OK, message, false),
        Ok(None) | Err(crate::WebError::NotFound) => unknown(&admin, &command),
        Err(e) => failed(&admin, &command, e),
    }
}

pub(super) async fn retry(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    axum::extract::Path((_login, id)): axum::extract::Path<(String, String)>,
    _form: CsrfForm<EmptyForm>,
) -> Response {
    match load(&state, &admin, &id).await {
        Ok(Some(command)) => execute(&state, &admin, command).await,
        Ok(None) => console_not_found_response(&admin),
        Err(e) => e.into_response(),
    }
}

pub(super) async fn execute(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    command: Command,
) -> Response {
    let original = match retain(state, admin, &command).await {
        Ok(c) => c,
        Err(e) => return failed(admin, &command, e),
    };
    if original.id() != command.id() {
        if let (Command::Decision(old), Command::Decision(new)) = (&original, &command)
            && old.action != new.action
        {
            return render_attempt(
                admin,
                &original,
                StatusCode::CONFLICT,
                "The requested decision was not applied. A different original decision attempt is retained. Check its status before deciding what to do next.",
                false,
            );
        }
        return Redirect::to(&url(admin, &original)).into_response();
    }
    if serde_json::to_value(&original).unwrap() != serde_json::to_value(&command).unwrap() {
        return failed(admin, &original, crate::WebError::Conflict);
    }
    let result = match &command {
        Command::Decision(c) => {
            let result = match &state.request_lifecycle {
                Some(l) => l.decide(c.clone()).await,
                None => Err(crate::WebError::NotFound),
            };
            return match result {
                Ok(receipt) if receipt.outcome == DecisionOutcome::Incompatible => {
                    decision_response(admin, &command, &receipt)
                }
                Ok(receipt) => {
                    let _ = session::set_flash(
                        &admin.tower,
                        session::Flash {
                            level: session::FlashLevel::Success,
                            message: decision_message(&receipt),
                        },
                    )
                    .await;
                    Redirect::to(&format!(
                        "/console/accounts/{}/requests",
                        admin.account.account_login
                    ))
                    .into_response()
                }
                Err(e) => failed(admin, &command, e),
            };
        }
        Command::Create(c) => match &state.admission {
            Some(a) => a.create(c.clone()).await.map(|_| ()),
            None => Err(crate::WebError::NotFound),
        },
        Command::Revoke(c) => match &state.admission {
            Some(a) => a.revoke(c.clone()).await.map(|_| ()),
            None => Err(crate::WebError::NotFound),
        },
    };
    match result {
        Ok(()) => Redirect::to(&format!(
            "/console/accounts/{}/links/{}",
            admin.account.account_login,
            command.link_id()
        ))
        .into_response(),
        Err(e) => failed(admin, &command, e),
    }
}

pub(super) fn unknown(admin: &RequireConsoleAdminOf, command: &Command) -> Response {
    render_attempt(
        admin,
        command,
        StatusCode::BAD_GATEWAY,
        "Outcome unknown. The original attempt may have completed. Check its status or retry the original attempt with the retained input.",
        true,
    )
}

fn decision_message(receipt: &DecisionReceipt) -> String {
    if receipt.request.state == ghinvite_core::RequestState::Expired {
        return "Request expired. The decision deadline passed; the requested decision was not applied.".into();
    }
    if receipt.outcome == DecisionOutcome::Incompatible {
        return format!(
            "Request is {}. The requested decision was not applied.",
            receipt.request.state
        );
    }
    format!(
        "Request {}{}.{}",
        if receipt.outcome == DecisionOutcome::AlreadyCompleted {
            "already "
        } else {
            ""
        },
        receipt.request.state,
        if receipt.request.state == ghinvite_core::RequestState::Approved {
            " Unavailable repositories may block delivery. The original scope and decision deadline stay unchanged."
        } else {
            ""
        }
    )
}

fn decision_response(
    admin: &RequireConsoleAdminOf,
    command: &Command,
    receipt: &DecisionReceipt,
) -> Response {
    let status = if receipt.outcome == DecisionOutcome::Incompatible {
        StatusCode::CONFLICT
    } else {
        StatusCode::OK
    };
    render_attempt(admin, command, status, &decision_message(receipt), false)
}

pub(super) fn failed(
    admin: &RequireConsoleAdminOf,
    command: &Command,
    error: crate::WebError,
) -> Response {
    match error {
        crate::WebError::Restate(_) => unknown(admin, command),
        crate::WebError::Conflict => render_attempt(
            admin,
            command,
            StatusCode::CONFLICT,
            "Operation conflict. This identity is bound to different input. Check the original attempt status; no replacement attempt was submitted.",
            false,
        ),
        // Anything else is not about this attempt's outcome, so it renders the
        // shared failure page pointing back at the link it was acting on.
        e => e.into_response_with_recovery(
            format!(
                "/console/accounts/{}/links/{}",
                admin.account.account_login,
                command.link_id()
            ),
            "Back to link details",
        ),
    }
}

fn render_attempt(
    admin: &RequireConsoleAdminOf,
    command: &Command,
    status: StatusCode,
    message: &str,
    retry: bool,
) -> Response {
    let account = admin.account.account_login.clone();
    let login = admin.session.login.clone();
    let row = ghinvite_ui::attempts::AttemptRow {
        label: command.label().into(),
        id: command.id(),
        status_url: url(admin, command),
        detail_url: format!("/console/accounts/{account}/links/{}", command.link_id()),
    };
    let message = message.to_owned();
    let html = render(admin.session.csrf_token.clone(), move || {
        rsx! {
            ghinvite_ui::attempts::AttemptPage {
                signed_in_login: login.clone(), account_login: account.clone(), row: row.clone(), message: message.clone(), retry,
            }
        }
    });
    (
        status,
        [(axum::http::header::CACHE_CONTROL, "private, no-store")],
        Html(html),
    )
        .into_response()
}
