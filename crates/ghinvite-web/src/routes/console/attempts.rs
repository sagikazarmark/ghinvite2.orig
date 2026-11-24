//! Session-bound continuations persisted before ingress, including on a lost
//! acknowledgement. Current account authorization is required on every access.
use super::*;
use crate::attempt_continuations::{AttemptContinuations, Retention, Scope};
use axum::http::StatusCode;
use axum::response::{Redirect, Response};
use ghinvite_core::admission::AdminLinkCommand;
use ghinvite_core::invitation_link::{CreateLink, CreateLinkError};
use ghinvite_core::request_lifecycle::{DecideRequest, DecisionOutcome, DecisionReceipt};
use serde::{Deserialize, Serialize};

const CREATED: &str = "Invitation link created. The URL is shown only once. If you did not save it, rotate the invitation URL from link details.";
const REVOKED: &str = "Invitation link stopped accepting new invitation requests.";
/// The authority refused the creation's input; nothing was created.
pub(super) const CREATE_REJECTED: &str =
    "The invitation link could not be created with these values. Review them and try again.";
const REVOKE_REJECTED: &str = "This invitation link could not be stopped. Reload it and try again.";
const ROTATION_REVOKED: &str =
    "This invitation link is revoked, so its URL was not rotated. Nothing was changed.";

/// A definitive creation rejection. A typed business refusal binds the old
/// creation identity, so corrected input must get a fresh one. Malformed input
/// (400) applies nothing and does not bind the identity.
pub(super) struct CreateRejected {
    pub refusal: Option<CreateLinkError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum Command {
    Create(Box<CreateLink>),
    Revoke(ghinvite_core::revocation::RevokeCommand),
    Rotate(ghinvite_core::invitation_link::RotateSecret),
    Decision(DecideRequest),
    Lifecycle {
        action: ghinvite_core::link_lifecycle::LifecycleAction,
        command: ghinvite_core::link_lifecycle::LifecycleCommand,
    },
}

/// The attempt identity of the creation of `link_id`. The creation form
/// allocates the link id, so a resubmitted form finds its own attempt.
fn create_id(link_id: ghinvite_core::InvitationLinkId) -> String {
    format!("create-{link_id}")
}

/// What recovering a creation found under its identity.
pub(super) enum CreateRecovery {
    /// No creation is retained for the link: submit this one fresh.
    Fresh,
    /// A creation was retained and this is the page for it: the original's
    /// outcome, or a conflict when the resubmitted input differs.
    Answered(Response),
    /// The retained creation was replayed and definitively rejected.
    Rejected(CreateRejected),
}

/// Recover the creation of `link_id` retained in this session, before
/// current eligibility is read. Compare the resubmission against the retained
/// scope and creation anchor; only identical business input replays the original.
pub(super) async fn recover_create(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    link_id: ghinvite_core::InvitationLinkId,
    form: &create_link_form::CreateLinkSubmission,
    anchor: chrono::DateTime<Utc>,
) -> crate::Result<CreateRecovery> {
    let Some(Command::Create(original)) = load(state, admin, &create_id(link_id)).await? else {
        return Ok(CreateRecovery::Fresh);
    };
    let repos = original
        .targets
        .repos
        .iter()
        .map(|r| RepositoryChoice {
            id: r.repo_id,
            full_name: r.repo_full_name.clone(),
        })
        .collect::<Vec<_>>();
    let organization = super::organization_from_targets(&original.targets);
    let matches = create_link_form::validate_selection(form, &repos, &organization, anchor)
        .is_ok_and(|v| {
            let mut command = v.into_command(
                original.link_id,
                original.admin.clone(),
                original.account_id,
                original.installation_id,
                original.secret_sha256,
            );
            command.organization_installation_id = original.organization_installation_id;
            command == *original
        });
    let command = Command::Create(original);
    if !matches {
        return Ok(CreateRecovery::Answered(conflict(admin, &command)));
    }
    Ok(match submit(state, admin, command).await {
        Ok(response) => CreateRecovery::Answered(response),
        Err(rejected) => CreateRecovery::Rejected(rejected),
    })
}

/// The page for a creation of `link_id` still retained in this session, whose
/// outcome the authority cannot yet show.
pub(super) async fn pending_create(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    link_id: ghinvite_core::InvitationLinkId,
) -> crate::Result<Option<Response>> {
    Ok(load(state, admin, &create_id(link_id))
        .await?
        .map(|command| unknown(admin, &command)))
}

pub(super) async fn recover_rotation(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    link_id: ghinvite_core::InvitationLinkId,
    operation_id: &ghinvite_core::admission::AdmissionOperationId,
) -> Option<Response> {
    let id = format!("rotate-{link_id}-{}", String::from(operation_id.clone()));
    match load(state, admin, &id).await {
        Ok(Some(command)) => Some(execute(state, admin, command).await),
        Ok(None) => None,
        Err(error) => Some(error.into_response()),
    }
}

impl Command {
    fn id(&self) -> String {
        match self {
            Self::Create(c) => create_id(c.link_id),
            Self::Lifecycle { command, .. } => format!(
                "lifecycle-{}-{}",
                command.link_id,
                String::from(command.operation_id.clone())
            ),
            Self::Rotate(c) => format!(
                "rotate-{}-{}",
                c.link_id,
                String::from(c.operation_id.clone())
            ),
            Self::Revoke(c) => format!(
                "revoke-{}-{}",
                c.link_id,
                String::from(c.operation_id.clone())
            ),
            Self::Decision(c) => format!(
                "decision-{}-{}",
                c.request_id,
                String::from(c.operation_id.clone())
            ),
        }
    }

    fn link_id(&self) -> ghinvite_core::InvitationLinkId {
        match self {
            Self::Create(c) => c.link_id,
            Self::Lifecycle { command, .. } => command.link_id,
            Self::Revoke(c) => c.link_id,
            Self::Rotate(c) => c.link_id,
            Self::Decision(c) => c.link_id,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Create(_) => "Create invitation link",
            Self::Lifecycle { action, .. } => match action {
                ghinvite_core::link_lifecycle::LifecycleAction::Pause => "Pause invitation link",
                ghinvite_core::link_lifecycle::LifecycleAction::Resume => "Resume invitation link",
            },
            Self::Revoke(_) => "Revoke invitation link",
            Self::Rotate(_) => "Rotate invitation URL",
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

fn scope(admin: &RequireConsoleAdminOf) -> crate::Result<Scope> {
    Scope::console(
        &admin.tower,
        admin.session.user_id,
        admin.account.account_id,
    )
}

async fn load(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    id: &str,
) -> crate::Result<Option<Command>> {
    // Recovery URLs carry the creation identity too. Canonicalize before
    // looking up its browser continuation, just as the form and VO routing do.
    let canonical;
    let id = if let Some(link) = id.strip_prefix("create-") {
        let link: ghinvite_core::InvitationLinkId =
            link.parse().map_err(|_| crate::WebError::NotFound)?;
        canonical = create_id(link);
        canonical.as_str()
    } else {
        id
    };
    let command = AttemptContinuations::new(state)
        .load::<Command>(&scope(admin)?, id)
        .await?;
    if let Some(command) = &command {
        authorize_command(state, admin, command, false).await?;
    }
    Ok(command)
}

async fn authorize_command(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    command: &Command,
    mutation: bool,
) -> crate::Result<()> {
    let targets = match command {
        Command::Create(c) => {
            if c.account_id != admin.account.account_id || c.admin.user_id != admin.session.user_id
            {
                return Err(crate::WebError::Forbidden);
            }
            c.targets.clone()
        }
        _ => {
            authoritative_link(state, admin, command.link_id())
                .await
                .map_err(crate::WebError::from)?
                .targets
        }
    };
    let access = if mutation {
        admin.authority.can_mutate_targets(&targets).await
    } else {
        admin.authority.can_read_link(&targets).await
    };
    match access {
        Ok(crate::authority::Access::Allowed) => Ok(()),
        Ok(crate::authority::Access::Denied) => Err(crate::WebError::Forbidden),
        Ok(crate::authority::Access::Pending) | Err(_) => Err(crate::WebError::ConsoleAuthority),
    }
}

// An immutable record per attempt prevents request-local session snapshots from
// overwriting each other's submitted input. A collision never authorizes ingress.
// A decision is also bound to its request, so only one original decision
// attempt per request is retained.
async fn retain(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    command: &Command,
) -> crate::Result<Retention<Command>> {
    let binding = match command {
        Command::Decision(c) => Some(format!("request-{}-{}", c.link_id, c.request_id)),
        _ => None,
    };
    AttemptContinuations::new(state)
        .retain(&scope(admin)?, &command.id(), binding.as_deref(), command)
        .await
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
        AttemptContinuations::new(&state)
            .list::<Command>(&scope(&admin)?)
            .await
    }
    .await;
    match result {
        Ok(commands) => {
            let mut rows = Vec::new();
            for c in &commands {
                match authorize_command(&state, &admin, c, false).await {
                    Ok(()) => rows.push(ghinvite_ui::attempts::AttemptRow {
                        label: c.label().into(),
                        id: c.id(),
                        status_url: url(&admin, c),
                        detail_url: format!(
                            "/console/accounts/{}/links/{}",
                            admin.account.account_login,
                            c.link_id()
                        ),
                    }),
                    Err(crate::WebError::Forbidden | crate::WebError::NotFound) => {}
                    Err(error) => return error.into_response(),
                }
            }
            let html = render(admin.render_context(), move || {
                rsx! {
                    ghinvite_ui::attempts::AttemptListPage {
                        signed_in_login: admin.session.login.clone(), account_login: admin.account.account_login.clone(), rows: rows.clone(),
                    }
                }
            });
            Html(html).into_response()
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
            let result = state.request_authority.decision_status(c.clone()).await;
            return match result {
                Ok(Some(receipt)) => decision_response(&admin, &command, &receipt),
                Ok(None) | Err(AuthorityError::Missing) => unknown(&admin, &command),
                Err(e) => failed(&admin, &command, e),
            };
        }
        Command::Create(c) => state
            .link_authority
            .link_status(AdminLinkCommand {
                link_id: c.link_id,
                admin: c.admin.clone(),
            })
            .await
            .and_then(|link| {
                if link.creation == **c {
                    Ok(Some(CREATED))
                } else {
                    Err(AuthorityError::Conflict)
                }
            }),
        Command::Lifecycle { .. } => {
            return render_attempt(
                &admin,
                &command,
                StatusCode::OK,
                "Retry this retained operation to confirm its outcome. The link's current state may include later operations.",
                true,
            );
        }
        Command::Rotate(_) => {
            return render_attempt(
                &admin,
                &command,
                StatusCode::OK,
                "Check link details for the current generation. Retry this retained rotation to confirm its outcome. Its secret cannot be recovered; rotate again if the URL was lost.",
                true,
            );
        }
        Command::Revoke(c) => {
            return match state.link_authority.revoke_status(c.clone()).await {
                Ok(Some(outcome)) => revocation_response(&admin, &command, &outcome),
                Ok(None) => unknown(&admin, &command),
                Err(e) => failed(&admin, &command, e),
            };
        }
    };
    match result {
        Ok(Some(message)) => render_attempt(&admin, &command, StatusCode::OK, message, false),
        Ok(None) | Err(AuthorityError::Missing) => unknown(&admin, &command),
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

/// Execute an attempt with no submitted form to return to (a retry from the
/// recovery page, a revocation or a decision). A rejected creation starts over
/// from a fresh creation form, since its retained input was released.
pub(super) async fn execute(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    command: Command,
) -> Response {
    match submit(state, admin, command).await {
        Ok(response) => response,
        Err(_) => {
            flash(admin, session::FlashLevel::Error, CREATE_REJECTED).await;
            Redirect::to(&format!(
                "/console/accounts/{}/links/new",
                admin.account.account_login
            ))
            .into_response()
        }
    }
}

async fn flash(admin: &RequireConsoleAdminOf, level: session::FlashLevel, message: &str) {
    let _ = session::set_flash(
        &admin.tower,
        session::Flash {
            level,
            message: message.into(),
        },
    )
    .await;
}

/// A definitive rejection applied nothing, so its continuation must neither
/// read as an unknown outcome nor bind the identity to the rejected input.
async fn release(state: &AppState, admin: &RequireConsoleAdminOf, command: &Command) {
    let released = async {
        AttemptContinuations::new(state)
            .release(&scope(admin)?, &command.id())
            .await
    }
    .await;
    if released.is_err() {
        // Retained input then keeps binding the identity; resubmitting changed
        // values reports a conflict rather than applying anything.
        tracing::warn!("rejected admin attempt could not be released");
    }
}

/// Execute an attempt. Only a creation the authority rejected is returned to
/// the caller, which may hold the submitted form to re-render.
pub(super) async fn submit(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    command: Command,
) -> Result<Response, CreateRejected> {
    submit_with_secret(state, admin, command, None).await
}

/// The plaintext exists only in this request. Continuations and ingress carry
/// its hash; only a confirmed operation plus durable routing permits display.
pub(super) async fn submit_with_secret(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    command: Command,
    secret: Option<crate::link_secret::LinkSecret>,
) -> Result<Response, CreateRejected> {
    if let Err(error) = authorize_command(state, admin, &command, true).await {
        return Ok(error.into_response());
    }
    match retain(state, admin, &command).await {
        Ok(Retention::Retained) => {}
        Ok(Retention::Bound(original)) => {
            if let (Command::Decision(old), Command::Decision(new)) = (&original, &command)
                && old.action != new.action
            {
                return Ok(render_attempt(
                    admin,
                    &original,
                    StatusCode::CONFLICT,
                    "The requested decision was not applied. A different original decision attempt is retained. Check its status before deciding what to do next.",
                    false,
                ));
            }
            return Ok(Redirect::to(&url(admin, &original)).into_response());
        }
        Ok(Retention::Conflict(original)) => return Ok(conflict(admin, &original)),
        Err(e) => return Ok(back_to_link(admin, &command, e)),
    }
    let (result, completed) = match &command {
        Command::Lifecycle {
            action,
            command: input,
        } => {
            use ghinvite_core::link_lifecycle::{LifecycleAction, LifecycleRefusal};
            let result = match action {
                LifecycleAction::Pause => state.link_authority.pause(input.clone()).await,
                LifecycleAction::Resume => state.link_authority.resume(input.clone()).await,
            };
            let message = match result {
                Ok(Ok(_)) => match action {
                    LifecycleAction::Pause => {
                        "Invitation link paused. Existing requests and invitations continue."
                    }
                    LifecycleAction::Resume => "Invitation link resumed.",
                },
                Ok(Err(refusal)) => {
                    let message = match refusal {
                        LifecycleRefusal::Revoked => {
                            "This link is revoked and cannot be paused or resumed."
                        }
                        LifecycleRefusal::AlreadyPaused => {
                            "This invitation link is already paused."
                        }
                        LifecycleRefusal::NotPaused => "This invitation link is not paused.",
                        LifecycleRefusal::ExpiredOrExhausted => {
                            "This link cannot resume because it has expired or reached its maximum uses."
                        }
                    };
                    return Ok(render_attempt(
                        admin,
                        &command,
                        StatusCode::OK,
                        message,
                        false,
                    ));
                }
                Err(error) => return Ok(failed(admin, &command, error)),
            };
            (Ok(()), message)
        }
        Command::Decision(c) => {
            let result = state.request_authority.decide(c.clone()).await;
            return Ok(match result {
                Ok(receipt) if receipt.outcome == DecisionOutcome::NotAuthorized => {
                    crate::WebError::Forbidden.into_response()
                }
                Ok(receipt)
                    if matches!(
                        receipt.outcome,
                        DecisionOutcome::Incompatible | DecisionOutcome::NotPending
                    ) =>
                {
                    decision_response(admin, &command, &receipt)
                }
                Ok(receipt) => {
                    flash(
                        admin,
                        session::FlashLevel::Success,
                        &decision_message(&receipt),
                    )
                    .await;
                    Redirect::to(&format!(
                        "/console/accounts/{}/requests",
                        admin.account.account_login
                    ))
                    .into_response()
                }
                Err(e) => failed(admin, &command, e),
            });
        }
        Command::Create(c) => {
            let result = match state.link_authority.create((**c).clone()).await {
                Ok(Ok(created)) => Ok(created),
                Ok(Err(refusal)) => {
                    release(state, admin, &command).await;
                    return Err(CreateRejected {
                        refusal: Some(refusal),
                    });
                }
                Err(error) => Err(error),
            };
            if let (Ok(created), Some(secret)) = (&result, secret) {
                if created.link_id != c.link_id
                    || created.secret_generation != 1
                    || !c.secret_sha256.constant_time_eq(&secret.hash())
                {
                    return Ok(render_attempt(
                        admin,
                        &command,
                        StatusCode::CONFLICT,
                        "This creation has already been superseded. Open link details to rotate the URL.",
                        false,
                    ));
                }
                return Ok(register_and_show(
                    state,
                    admin,
                    &command,
                    secret,
                    created.secret_generation,
                    Utc::now(),
                )
                .await);
            }
            (result.map(|_| ()), CREATED)
        }
        Command::Rotate(c) => {
            let result = state.link_authority.rotate_secret(c.clone()).await;
            // A stale tab rotating a revoked link: the authority changed
            // nothing and bound no identity, so nothing is left to recover.
            if let Err(AuthorityError::Revoked) = result {
                release(state, admin, &command).await;
                flash(admin, session::FlashLevel::Error, ROTATION_REVOKED).await;
                return Ok(Redirect::to(&format!(
                    "/console/accounts/{}/links/{}",
                    admin.account.account_login, c.link_id
                ))
                .into_response());
            }
            if let (Ok(rotated), Some(secret)) = (&result, secret) {
                return Ok(register_and_show(
                    state,
                    admin,
                    &command,
                    secret,
                    rotated.secret_generation,
                    Utc::now(),
                )
                .await);
            }
            (
                result.map(|_| ()),
                "Rotation confirmed. The URL cannot be recovered. Rotate again if you did not save it.",
            )
        }
        Command::Revoke(c) => {
            return Ok(match state.link_authority.revoke(c.clone()).await {
                Ok(Ok(accepted)) => {
                    flash(admin,session::FlashLevel::Success,&format!("{REVOKED} Cascade accepted for {} requests and {} deliveries. Check link details for progress.",accepted.addressed_requests,accepted.addressed_deliveries)).await;
                    Redirect::to(&format!(
                        "/console/accounts/{}/links/{}",
                        admin.account.account_login, c.link_id
                    ))
                    .into_response()
                }
                Ok(outcome) => revocation_response(admin, &command, &outcome),
                Err(AuthorityError::Invalid) => {
                    release(state, admin, &command).await;
                    flash(admin, session::FlashLevel::Error, REVOKE_REJECTED).await;
                    Redirect::to(&format!(
                        "/console/accounts/{}/links/{}",
                        admin.account.account_login, c.link_id
                    ))
                    .into_response()
                }
                Err(error) => failed(admin, &command, error),
            });
        }
    };
    let detail = format!(
        "/console/accounts/{}/links/{}",
        admin.account.account_login,
        command.link_id()
    );
    match result {
        // Every completing execution flashes once: a first submission, a
        // retried recovered attempt, and an identical resubmission alike.
        Ok(()) => {
            flash(admin, session::FlashLevel::Success, completed).await;
            Ok(Redirect::to(&detail).into_response())
        }
        // Invalid input for the authority (for a creation, typically an
        // expiry already in the past); the authority applied nothing.
        Err(AuthorityError::Invalid) => {
            release(state, admin, &command).await;
            match command {
                Command::Create(_) => Err(CreateRejected { refusal: None }),
                _ => {
                    flash(admin, session::FlashLevel::Error, REVOKE_REJECTED).await;
                    Ok(Redirect::to(&detail).into_response())
                }
            }
        }
        Err(e) => Ok(failed(admin, &command, e)),
    }
}

async fn register_and_show(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    command: &Command,
    secret: crate::link_secret::LinkSecret,
    generation: u32,
    created_at: chrono::DateTime<Utc>,
) -> Response {
    let binding = ghinvite_core::storage::link_secrets::LinkSecret {
        secret_sha256: secret.hash(),
        link_id: command.link_id(),
        secret_generation: generation,
        created_at,
        retired_at: None,
    };
    if state.storage.register_link_secret(&binding).await.is_err() {
        return render_attempt(
            admin,
            command,
            StatusCode::SERVICE_UNAVAILABLE,
            "The link operation completed, but the URL is not ready. Open link details and rotate the invitation URL.",
            false,
        );
    }
    let encoded = secret.show_once();
    let invitation_url = zeroize::Zeroizing::new(format!(
        "{}/i#{}",
        state.config.base_url.trim_end_matches('/'),
        encoded.as_str()
    ));
    let detail = format!(
        "/console/accounts/{}/links/{}",
        admin.account.account_login,
        command.link_id()
    );
    // Dioxus escapes the URL as text and attribute content. The response is the
    // only plaintext copy delivered to the user, never a flash/session record.
    let login = admin.session.login.clone();
    let html = render(admin.render_context(), move || {
        let invitation_url = invitation_url.as_str();
        rsx! {
            ghinvite_ui::layouts::InvitationLayout {
                signed_in_login: Some(login.clone()), title: "Save invitation URL · ghinvite".to_owned(),
                account_login: None, active_nav: None, flash: None,
                h1 { "Save this invitation URL" }
                p { "This URL is shown once. Anyone with it can open the invitation. If it is lost, rotate it from link details." }
                p { a { href: "{invitation_url}", "{invitation_url}" } }
                p { a { href: "{detail}", "Back to link details" } }
            }
        }
    });
    Html(html).into_response()
}

fn unknown(admin: &RequireConsoleAdminOf, command: &Command) -> Response {
    render_attempt(
        admin,
        command,
        StatusCode::BAD_GATEWAY,
        "Outcome unknown. The original attempt may have completed. Check its status or retry the original attempt with the retained input.",
        true,
    )
}

fn decision_message(receipt: &DecisionReceipt) -> String {
    if receipt.request.state == ghinvite_core::RequestState::Cancelled {
        return "Request cancelled because its invitation link was revoked. The requested decision was not applied.".into();
    }
    if receipt.request.state == ghinvite_core::RequestState::Expired {
        return "Request expired. The decision deadline passed; the requested decision was not applied.".into();
    }
    if matches!(
        receipt.outcome,
        DecisionOutcome::Incompatible | DecisionOutcome::NotPending
    ) {
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
    if receipt.outcome == DecisionOutcome::NotAuthorized {
        return crate::WebError::Forbidden.into_response();
    }
    let status = if matches!(
        receipt.outcome,
        DecisionOutcome::Incompatible | DecisionOutcome::NotPending
    ) {
        StatusCode::CONFLICT
    } else {
        StatusCode::OK
    };
    render_attempt(admin, command, status, &decision_message(receipt), false)
}

/// Render what the authority's answer means for this attempt.
fn failed(admin: &RequireConsoleAdminOf, command: &Command, error: AuthorityError) -> Response {
    match error {
        AuthorityError::Unknown(_) => unknown(admin, command),
        AuthorityError::Conflict => conflict(admin, command),
        // Anything else is not about this attempt's outcome.
        e => back_to_link(admin, command, e.into()),
    }
}

/// The attempt's identity is bound to input other than what was submitted.
fn conflict(admin: &RequireConsoleAdminOf, command: &Command) -> Response {
    render_attempt(
        admin,
        command,
        StatusCode::CONFLICT,
        "Operation conflict. This identity is bound to different input. Check the original attempt status; no replacement attempt was submitted.",
        false,
    )
}

/// Render the shared failure page pointing back at the link the attempt was
/// acting on.
fn back_to_link(
    admin: &RequireConsoleAdminOf,
    command: &Command,
    error: crate::WebError,
) -> Response {
    error.into_response_with_recovery(
        format!(
            "/console/accounts/{}/links/{}",
            admin.account.account_login,
            command.link_id()
        ),
        "Back to link details",
    )
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
    let html = render(admin.render_context(), move || {
        rsx! {
            ghinvite_ui::attempts::AttemptPage {
                signed_in_login: login.clone(), account_login: account.clone(), row: row.clone(), message: message.clone(), retry,
            }
        }
    });
    (status, Html(html)).into_response()
}

fn revocation_response(
    admin: &RequireConsoleAdminOf,
    command: &Command,
    outcome: &ghinvite_core::revocation::RevokeOutcome,
) -> Response {
    use ghinvite_core::revocation::RevokeRefusal;
    let (status,message)=match outcome {
        Ok(accepted)=>(StatusCode::OK,format!("{REVOKED} Cascade accepted for {} requests and {} deliveries. Check link details for progress; acceptance does not confirm GitHub cancellation.",accepted.addressed_requests,accepted.addressed_deliveries)),
        Err(RevokeRefusal::Revoked)=>(StatusCode::CONFLICT,"This link was already revoked by a different operation. This operation did not change it.".into()),
        Err(RevokeRefusal::UnsupportedCascade)=>(StatusCode::BAD_REQUEST,"These cascade options are not available. This operation did not revoke the link.".into()),
    };
    render_attempt(admin, command, status, &message, false)
}
