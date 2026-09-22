//! An in-process stand-in for the Restate invitation link authority.
//!
//! It answers the ingress paths the web calls for invitation links
//! (`/{LINK_SERVICE}/{link_id}/{method}`) and codes
//! (`/{CODE_SERVICE}/{code}/resolve`) with the same status codes the real
//! virtual object uses: 404 for an unknown link or request or a foreign
//! account, 409 for a command identity reused with different input, 400 for
//! invalid input. Like the real object, a replayed creation, decision or
//! admission attempt answers its original receipt and takes no effect again.
//! Requester attempts follow the admission rules: an inactive link or an open
//! request is a final rejection, a requester page recovers the latest attempt
//! and hides the decline reason, and a page with nothing to show is missing.
//! It records every call so tests can assert what reached the authority.
//!
//! Tests script the failures web recovery depends on: a failure answered
//! before the command applied ([`FakeLinkAuthority::fail`],
//! [`FakeLinkAuthority::fail_once`]), and a command that applied but whose
//! acknowledgement was lost ([`FakeLinkAuthority::lose_acknowledgements`]).
//! Answers that depend on what the fake does not model are set explicitly:
//! installation eligibility ([`FakeLinkAuthority::set_availability`]) and
//! delivery progress ([`FakeLinkAuthority::set_delivery_progress`]).
//!
//! The server runs on the test's tokio runtime, so it outlives this handle and
//! keeps serving long-running browser fixtures.
#![allow(dead_code)]

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Utc};
use ghinvite_core::admission::{
    AdminLinkCommand, AdmissionOperationId, AdmissionReceipt, AdmissionResult, Admit, Attempt,
    AttemptQuery, Rejection, RequesterPage, UpdateMetadata,
};
use ghinvite_core::delivery::{DispatchStage, RepositoryProgress};
use ghinvite_core::request_lifecycle::{
    DecideRequest, DecisionAction, DecisionOutcome, DecisionReceipt, RequestStatus,
    TerminalDecision,
};
use ghinvite_core::storage::projection::{
    AccountAdmin, CreateLink, LinkMetadata, LinkSnapshot, RequestSnapshot,
};
use ghinvite_core::{InvitationLink, InvitationLinkId, RequestId, RequestState};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The Restate virtual object that owns invitation links.
pub const LINK_SERVICE: &str = "InvitationLink";
/// The Restate virtual object that resolves invitation codes to links.
pub const CODE_SERVICE: &str = "InvitationCode";

/// How long an admitted request awaits review, as the real object sets it.
const PENDING_LIFETIME: Duration = Duration::days(7);

/// How a scripted method fails.
#[derive(Clone, Copy)]
enum Failure {
    /// Answer `status` without applying the command.
    Before { status: u16, once: bool },
    /// Apply the command, then answer 503 as if the acknowledgement was lost.
    LostAcknowledgement,
}

/// What installation eligibility answers when admission consults it for an
/// account's active link.
#[derive(Clone, Debug)]
pub enum Availability {
    Available,
    /// Admission rejects the attempt for this reason.
    Unavailable(Rejection),
    /// Eligibility could not be confirmed: admission answers 503 and retains
    /// nothing, so the same attempt can retry.
    Unknown,
}

#[derive(Default)]
struct Inner {
    links: BTreeMap<String, LinkSnapshot>,
    /// Each link's original creation receipt, which a replayed `create`
    /// answers even after later mutations.
    creations: BTreeMap<String, LinkSnapshot>,
    requests: BTreeMap<String, RequestSnapshot>,
    /// Retained decisions by link and operation.
    decisions: BTreeMap<(String, String), (DecideRequest, DecisionReceipt)>,
    /// Prepared attempts by link and operation.
    attempts: BTreeMap<(String, String), Admit>,
    /// Retained admission receipts by link and operation.
    admissions: BTreeMap<(String, String), (Admit, AdmissionReceipt)>,
    /// Each requester's latest attempt, by link and requester.
    latest: BTreeMap<(String, u64), AdmissionOperationId>,
    /// The pending or approved request that blocks a requester's admission,
    /// by link and requester.
    blockers: BTreeMap<(String, u64), RequestId>,
    /// Eligibility by account; available unless set.
    availability: BTreeMap<u64, Availability>,
    /// Scripted delivery progress by request.
    progress: BTreeMap<String, Vec<RepositoryProgress>>,
    requester_pages: BTreeMap<String, RequesterPage>,
    failures: BTreeMap<String, Failure>,
    created: Vec<CreateLink>,
    metadata_updates: Vec<UpdateMetadata>,
    revocations: Vec<AdminLinkCommand>,
    calls: Vec<(String, Value)>,
    applied: Vec<String>,
}

#[derive(Clone)]
pub struct FakeLinkAuthority {
    inner: Arc<Mutex<Inner>>,
    url: String,
}

impl FakeLinkAuthority {
    pub async fn start() -> Self {
        let inner = Arc::new(Mutex::new(Inner::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = axum::Router::new()
            .route("/{service}/{key}/{method}", axum::routing::post(handle))
            .with_state(inner.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { inner, url }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn client(&self) -> Arc<ghinvite_web::RestateClient> {
        Arc::new(ghinvite_web::RestateClient::new(self.url.clone()).unwrap())
    }

    /// Make the authority hold `snapshot`, as if it had been created earlier.
    pub fn seed(&self, snapshot: LinkSnapshot) {
        let mut inner = self.lock();
        let key = snapshot.link_id.to_string();
        inner.creations.insert(key.clone(), snapshot.clone());
        inner.links.insert(key, snapshot);
    }

    /// Seed the authoritative counterpart of a projected link.
    pub fn seed_link(&self, link: &InvitationLink) {
        self.seed(snapshot(link));
    }

    /// Make the authority hold `request`, as if it had admitted it earlier and
    /// it had reached its state since. A pending or approved request blocks
    /// its requester's admission on the link.
    pub fn seed_request(&self, request: RequestSnapshot) {
        let mut inner = self.lock();
        let blocker = (request.link_id.to_string(), request.requester_id);
        if blocks(&request) {
            inner.blockers.insert(blocker, request.request_id);
        } else if inner.blockers.get(&blocker) == Some(&request.request_id) {
            inner.blockers.remove(&blocker);
        }
        inner
            .requests
            .insert(request.request_id.to_string(), request);
    }

    /// Make the authority retain `receipt` for `command`'s operation, as if it
    /// had admitted or rejected that attempt earlier. It becomes the
    /// requester's latest attempt; seed an accepted request separately.
    pub fn seed_admission(&self, command: Admit, receipt: AdmissionReceipt) {
        let mut inner = self.lock();
        let link = command.link_id.to_string();
        inner.latest.insert(
            (link.clone(), command.requester_id),
            command.operation_id.clone(),
        );
        let key = (link, String::from(command.operation_id.clone()));
        inner.admissions.insert(key, (command, receipt));
    }

    /// Answer admission for `account_id`'s active links as installation
    /// eligibility would. Accounts are available unless set.
    pub fn set_availability(&self, account_id: u64, availability: Availability) {
        self.lock().availability.insert(account_id, availability);
    }

    /// Answer `delivery_progress` for the approved `request` with `progress`
    /// instead of every repository awaiting dispatch. The fake keeps no
    /// dispatch plans, so this is how a test places repositories in later
    /// stages.
    pub fn set_delivery_progress(&self, request: RequestId, progress: Vec<RepositoryProgress>) {
        self.lock().progress.insert(request.to_string(), progress);
    }

    /// Make the authority retain `receipt` for `command`'s operation, as if it
    /// had decided that command earlier.
    pub fn seed_decision(&self, command: DecideRequest, receipt: DecisionReceipt) {
        let key = decision_key(&command);
        self.lock().decisions.insert(key, (command, receipt));
    }

    /// Answer `requester_page` for the page's link with `page` instead of the
    /// page the authority would derive from its attempts and requests.
    pub fn set_requester_page(&self, page: RequesterPage) {
        self.lock()
            .requester_pages
            .insert(page.link_id.to_string(), page);
    }

    /// Answer every later call of `method` with `status`, without applying it.
    pub fn fail(&self, method: &str, status: u16) {
        self.script(
            method,
            Failure::Before {
                status,
                once: false,
            },
        );
    }

    /// Answer the next call of `method` with `status`, without applying it.
    pub fn fail_once(&self, method: &str, status: u16) {
        self.script(method, Failure::Before { status, once: true });
    }

    /// Apply every later call of `method` that takes effect, then answer 503
    /// as if the acknowledgement was lost. Replays and reads take no effect,
    /// so they are still answered.
    pub fn lose_acknowledgements(&self, method: &str) {
        self.script(method, Failure::LostAcknowledgement);
    }

    /// Answer `method` normally again.
    pub fn recover(&self, method: &str) {
        self.lock().failures.remove(method);
    }

    pub fn link(&self, id: InvitationLinkId) -> Option<LinkSnapshot> {
        self.lock().links.get(&id.to_string()).cloned()
    }

    pub fn request(&self, id: RequestId) -> Option<RequestSnapshot> {
        self.lock().requests.get(&id.to_string()).cloned()
    }

    /// Every request the authority holds, seeded or admitted.
    pub fn requests(&self) -> Vec<RequestSnapshot> {
        self.lock().requests.values().cloned().collect()
    }

    /// Every `create` command answered, including replays.
    pub fn created(&self) -> Vec<CreateLink> {
        self.lock().created.clone()
    }

    pub fn metadata_updates(&self) -> Vec<UpdateMetadata> {
        self.lock().metadata_updates.clone()
    }

    pub fn revocations(&self) -> Vec<AdminLinkCommand> {
        self.lock().revocations.clone()
    }

    /// The method name of every call received, in order.
    pub fn calls(&self) -> Vec<String> {
        self.lock()
            .calls
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }

    /// The input of every call of `method` received, in order, including
    /// calls that failed.
    pub fn received<T: DeserializeOwned>(&self, method: &str) -> Vec<T> {
        self.lock()
            .calls
            .iter()
            .filter(|(called, _)| called == method)
            .map(|(_, body)| serde_json::from_value(body.clone()).unwrap())
            .collect()
    }

    /// The method name of every call that took effect, in order.
    pub fn applied(&self) -> Vec<String> {
        self.lock().applied.clone()
    }

    fn script(&self, method: &str, failure: Failure) {
        self.lock().failures.insert(method.into(), failure);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }
}

/// The authoritative snapshot of a projected link, as if the authority had
/// created it with the link's current guardrails.
pub fn snapshot(link: &InvitationLink) -> LinkSnapshot {
    let mut repos = link.repos.clone();
    repos.sort_by_key(|repo| repo.repo_id);
    LinkSnapshot {
        metadata: None,
        link_id: link.id,
        creation: CreateLink {
            link_id: link.id,
            admin: AccountAdmin {
                account_id: link.account_id,
                user_id: link.created_by,
            },
            account_id: link.account_id,
            installation_id: link.installation_id,
            description: link.description.clone(),
            internal_note: link.internal_note.clone(),
            expires_at: link.expires_at,
            max_uses: link.max_uses,
            permission: link.permission,
            approval_required: link.approval_required,
            repos,
        },
        invitation_code: link.slug.as_str().into(),
        created_at: link.created_at,
        uses: link.uses_count.into(),
        revision: 1,
        revoked_at: link.revoked_at,
        revoked_by: link.revoked_by,
    }
}

/// The real handlers' terminal errors, with their messages. Any other status
/// carries a diagnostic the web must not disclose.
fn status(code: u16) -> Response {
    let message = match code {
        400 => "invalid command",
        404 => "not found",
        409 => "operation conflict",
        _ => "fixture: private upstream diagnostic",
    };
    (StatusCode::from_u16(code).unwrap(), message).into_response()
}

fn json<T: serde::Serialize>(value: &T) -> Response {
    axum::Json(serde_json::to_value(value).unwrap()).into_response()
}

fn decision_key(command: &DecideRequest) -> (String, String) {
    (
        command.link_id.to_string(),
        String::from(command.operation_id.clone()),
    )
}

/// The admin acts for the link's account, as the real `validate_admin` checks.
fn admits(admin: &AccountAdmin, link: &LinkSnapshot) -> bool {
    admin.account_id == link.creation.account_id && admin.account_id != 0 && admin.user_id != 0
}

async fn handle(
    State(inner): State<Arc<Mutex<Inner>>>,
    Path((service, key, method)): Path<(String, String, String)>,
    body: Bytes,
) -> Response {
    let mut inner = inner.lock().unwrap();
    let body = serde_json::from_slice::<Value>(&body).ok();
    inner
        .calls
        .push((method.clone(), body.clone().unwrap_or(Value::Null)));
    let lose_acknowledgement = match inner.failures.get(&method).copied() {
        Some(Failure::Before { status: code, once }) => {
            if once {
                inner.failures.remove(&method);
            }
            return status(code);
        }
        Some(Failure::LostAcknowledgement) => true,
        None => false,
    };
    let Some(body) = body else {
        return status(400);
    };
    let applied = inner.applied.len();
    let response = answer(&mut inner, &service, key, &method, body);
    if lose_acknowledgement && inner.applied.len() > applied {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "fixture: applied, acknowledgement lost",
        )
            .into_response();
    }
    response
}

fn answer(inner: &mut Inner, service: &str, key: String, method: &str, body: Value) -> Response {
    if service == CODE_SERVICE && method == "resolve" {
        return match inner.links.values().find(|l| l.invitation_code == key) {
            Some(link) => json(&link.link_id),
            None => status(404),
        };
    }
    if service != LINK_SERVICE {
        return status(404);
    }
    let Ok(link_id) = key.parse::<InvitationLinkId>() else {
        return status(404);
    };
    // Every command names its link; a different key is not that link.
    if body.get("link_id").and_then(|id| id.as_str()) != Some(key.as_str()) {
        return status(404);
    }
    match method {
        "create" => {
            let Ok(command) = serde_json::from_value::<CreateLink>(body) else {
                return status(400);
            };
            inner.created.push(command.clone());
            let creation = match normalize_creation(command) {
                Ok(creation) => creation,
                Err(code) => return status(code),
            };
            if let Some(link) = inner.links.get(&key) {
                return if link.creation == creation {
                    json(&inner.creations[&key])
                } else {
                    status(409)
                };
            }
            let now = Utc::now();
            if creation.expires_at.is_some_and(|expiry| expiry <= now) {
                return status(400);
            }
            let link = LinkSnapshot {
                metadata: None,
                link_id,
                creation,
                invitation_code: key[key.len() - 16..].into(),
                created_at: now,
                uses: 0,
                revision: 1,
                revoked_at: None,
                revoked_by: None,
            };
            inner.links.insert(key.clone(), link.clone());
            inner.creations.insert(key, link.clone());
            inner.applied.push(method.into());
            json(&link)
        }
        "link_status" | "revoke" => {
            let Ok(command) = serde_json::from_value::<AdminLinkCommand>(body) else {
                return status(400);
            };
            let Some(link) = inner.links.get_mut(&key) else {
                return status(404);
            };
            if !admits(&command.admin, link) {
                return status(404);
            }
            let revoke = method == "revoke" && link.revoked_at.is_none();
            if revoke {
                link.revoked_at = Some(Utc::now());
                link.revoked_by = Some(command.admin.user_id);
                link.revision += 1;
            }
            let link = link.clone();
            if revoke {
                inner.applied.push(method.into());
            }
            if method == "revoke" {
                inner.revocations.push(command);
            }
            json(&link)
        }
        "update_metadata" => {
            let Ok(command) = serde_json::from_value::<UpdateMetadata>(body) else {
                return status(400);
            };
            let Some(link) = inner.links.get_mut(&key) else {
                return status(404);
            };
            if !admits(&command.admin, link) {
                return status(404);
            }
            let Ok(metadata) =
                LinkMetadata::parse(&command.description, command.internal_note.as_deref())
            else {
                return status(400);
            };
            // Saving the current details again changes nothing.
            let changed = link.description() != metadata.description
                || link.internal_note() != metadata.internal_note.as_deref();
            if changed {
                link.metadata = Some(metadata);
                link.revision += 1;
            }
            let link = link.clone();
            if changed {
                inner.applied.push(method.into());
            }
            inner.metadata_updates.push(command);
            json(&link)
        }
        "decide" | "decision_status" => {
            let Ok(mut command) = serde_json::from_value::<DecideRequest>(body) else {
                return status(400);
            };
            let Some(link) = inner.links.get(&key) else {
                return status(404);
            };
            if !admits(&command.admin, link) {
                return status(404);
            }
            if let DecisionAction::Decline { reason } = &mut command.action {
                *reason = reason
                    .take()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty());
                if reason.as_ref().is_some_and(|s| s.len() > 16_384) {
                    return status(400);
                }
            }
            let retained = inner.decisions.get(&decision_key(&command));
            if method == "decision_status" {
                return match retained {
                    Some((old, receipt)) if *old == command => json(&Some(receipt)),
                    Some(_) => status(409),
                    None => json(&None::<DecisionReceipt>),
                };
            }
            if let Some((old, receipt)) = retained {
                return if *old == command {
                    json(receipt)
                } else {
                    status(409)
                };
            }
            let Some(mut request) = inner.requests.get(&command.request_id.to_string()).cloned()
            else {
                return status(404);
            };
            if request.link_id != link_id {
                return status(404);
            }
            let was_pending = request.state == RequestState::Pending;
            evaluate(inner, &mut request, Some(&command), Utc::now());
            let receipt = DecisionReceipt {
                outcome: outcome(was_pending, &command.action, &request),
                request,
            };
            inner
                .decisions
                .insert(decision_key(&command), (command, receipt.clone()));
            inner.applied.push(method.into());
            json(&receipt)
        }
        "prepare_attempt" => {
            let Ok(mut command) = serde_json::from_value::<Admit>(body) else {
                return status(400);
            };
            if command.normalize().is_err() || command.requester_id == 0 {
                return status(400);
            }
            if !inner.links.contains_key(&key) {
                return status(404);
            }
            let operation = (key.clone(), String::from(command.operation_id.clone()));
            if let Some((old, receipt)) = inner.admissions.get(&operation) {
                return if *old == command {
                    json(&Attempt {
                        input: command,
                        receipt: Some(receipt.clone()),
                    })
                } else {
                    status(409)
                };
            }
            match inner.attempts.get(&operation) {
                Some(old) if *old != command => return status(409),
                Some(_) => {}
                None => {
                    inner.attempts.insert(operation, command.clone());
                    inner
                        .latest
                        .insert((key, command.requester_id), command.operation_id.clone());
                    inner.applied.push(method.into());
                }
            }
            json(&Attempt {
                input: command,
                receipt: None,
            })
        }
        "admit" => {
            let Ok(mut command) = serde_json::from_value::<Admit>(body) else {
                return status(400);
            };
            if command.requester_id == 0 {
                return status(404);
            }
            if command.normalize().is_err() {
                return status(400);
            }
            let operation = (key.clone(), String::from(command.operation_id.clone()));
            if let Some((old, receipt)) = inner.admissions.get(&operation) {
                return if *old == command {
                    json(receipt)
                } else {
                    status(409)
                };
            }
            if inner
                .attempts
                .get(&operation)
                .is_some_and(|prepared| *prepared != command)
            {
                return status(409);
            }
            let Some(mut link) = inner.links.get(&key).cloned() else {
                return status(404);
            };
            let blocker = (key.clone(), command.requester_id);
            let mut blocking = match inner.blockers.get(&blocker) {
                Some(id) => match inner.requests.get(&id.to_string()) {
                    Some(request) => Some(request.clone()),
                    None => return status(500),
                },
                None => None,
            };
            let now = Utc::now();
            // Only an active link consults installation eligibility.
            let availability = if link.inactive(now).is_none() {
                let account = link.creation.account_id;
                inner.availability.get(&account).cloned()
            } else {
                None
            };
            // The real `decide_admission`: an overdue blocker expires first.
            let expired = blocking
                .as_mut()
                .and_then(|request| transition(request, None, now).then(|| request.clone()));
            let reason = local_rejection(&link, blocking.as_ref(), now).or(match &availability {
                Some(Availability::Unavailable(reason)) => Some(reason.clone()),
                _ => None,
            });
            let (result, request) = if let Some(reason) = reason {
                (Some(AdmissionResult::Rejected { reason }), None)
            } else if matches!(availability, Some(Availability::Unknown)) {
                (None, None)
            } else {
                let request = accept(&mut link, &command, now);
                let result = AdmissionResult::Accepted {
                    request_id: request.request_id,
                    state: request.state,
                    decision_deadline: request.decision_deadline,
                };
                (Some(result), Some(request))
            };
            // Apply the after-images, as `apply_admission` does.
            if request.is_none() && expired.is_some() {
                link.revision += 1;
            }
            if request.is_some() || expired.is_some() {
                inner.links.insert(key.clone(), link);
            }
            if let Some(expired) = &expired {
                inner
                    .requests
                    .insert(expired.request_id.to_string(), expired.clone());
                inner.blockers.remove(&blocker);
            }
            if let Some(request) = request {
                inner.blockers.insert(blocker, request.request_id);
                inner
                    .requests
                    .insert(request.request_id.to_string(), request);
            }
            inner
                .latest
                .insert((key, command.requester_id), command.operation_id.clone());
            if result.is_some() || expired.is_some() {
                inner.applied.push(method.into());
            }
            match result {
                Some(result) => {
                    let receipt = AdmissionReceipt {
                        decided_at: now,
                        result,
                    };
                    inner
                        .admissions
                        .insert(operation, (command, receipt.clone()));
                    json(&receipt)
                }
                None => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Repository availability could not be confirmed. Retry the same attempt.",
                )
                    .into_response(),
            }
        }
        "requester_page" => {
            let Ok(query) = serde_json::from_value::<AttemptQuery>(body) else {
                return status(400);
            };
            if query.requester_id == 0 {
                return status(404);
            }
            let Some(link) = inner.links.get(&key).cloned() else {
                return status(404);
            };
            if let Some(page) = inner.requester_pages.get(&key) {
                return json(page);
            }
            let explicit = query.operation_id.is_some();
            let operation_id = query.operation_id.or_else(|| {
                inner
                    .latest
                    .get(&(key.clone(), query.requester_id))
                    .cloned()
            });
            let mut attempt = None;
            if let Some(id) = operation_id {
                let operation = (key.clone(), String::from(id));
                attempt = match inner.admissions.get(&operation) {
                    Some((input, receipt)) => Some(Attempt {
                        input: input.clone(),
                        receipt: Some(receipt.clone()),
                    }),
                    None => inner.attempts.get(&operation).map(|input| Attempt {
                        input: input.clone(),
                        receipt: None,
                    }),
                };
                if attempt
                    .as_ref()
                    .is_some_and(|a| a.input.requester_id != query.requester_id)
                    || (explicit && attempt.is_none())
                {
                    return status(404);
                }
            }
            let now = Utc::now();
            let blocker_id = inner
                .blockers
                .get(&(key.clone(), query.requester_id))
                .copied();
            let request_id = match attempt
                .as_ref()
                .and_then(|a| a.receipt.as_ref())
                .map(|r| &r.result)
            {
                Some(AdmissionResult::Accepted { request_id, .. }) => Some(*request_id),
                _ => blocker_id,
            };
            let mut request = None;
            if let Some(id) = request_id {
                let Some(mut current) = inner.requests.get(&id.to_string()).cloned() else {
                    return status(404);
                };
                if current.requester_id != query.requester_id {
                    return status(404);
                }
                evaluate(inner, &mut current, None, now);
                request = Some(current);
            }
            // An old receipt can name a terminal request while a newer
            // request blocks admission.
            let blocker = if blocker_id == request_id {
                request.clone()
            } else if let Some(id) = blocker_id {
                let Some(mut current) = inner.requests.get(&id.to_string()).cloned() else {
                    return status(404);
                };
                evaluate(inner, &mut current, None, now);
                Some(current)
            } else {
                None
            };
            let can_start_fresh = local_rejection(&link, blocker.as_ref(), now).is_none();
            if !can_start_fresh && attempt.is_none() && request.is_none() {
                return status(404);
            }
            json(&RequesterPage {
                link_id,
                invitation_code: link.invitation_code,
                repos: link.creation.repos,
                permission: link.creation.permission,
                approval_required: link.creation.approval_required,
                can_start_fresh,
                attempt,
                request: request.map(requester_view),
            })
        }
        "delivery_progress" => {
            let Ok(query) = serde_json::from_value::<RequestStatus>(body) else {
                return status(400);
            };
            let Some(request) = inner.requests.get(&query.request_id.to_string()) else {
                return status(404);
            };
            if request.link_id != link_id || request.requester_id != query.requester_id {
                return status(404);
            }
            if request.state != RequestState::Approved {
                return json(&Vec::<RepositoryProgress>::new());
            }
            if let Some(progress) = inner.progress.get(&query.request_id.to_string()) {
                return json(progress);
            }
            let Some(link) = inner.links.get(&key) else {
                return status(404);
            };
            // Without a dispatch plan, every repository awaits dispatch.
            let progress: Vec<_> = link
                .creation
                .repos
                .iter()
                .map(|repo| RepositoryProgress {
                    repo_id: repo.repo_id,
                    stage: DispatchStage::Approved,
                })
                .collect();
            json(&progress)
        }
        _ => status(404),
    }
}

/// The real `normalize_creation`: the input the creation identity binds.
fn normalize_creation(command: CreateLink) -> Result<CreateLink, u16> {
    if command.admin.account_id != command.account_id
        || command.account_id == 0
        || command.admin.user_id == 0
    {
        return Err(404);
    }
    command.normalized().map_err(|_| 400)
}

/// The real `InvitationLink::transition`: evaluate a stored request at `now`
/// and keep the result. A request that leaves pending other than by approval
/// no longer blocks its requester.
fn evaluate(
    inner: &mut Inner,
    request: &mut RequestSnapshot,
    command: Option<&DecideRequest>,
    now: DateTime<Utc>,
) {
    if !transition(request, command, now) {
        return;
    }
    inner
        .requests
        .insert(request.request_id.to_string(), request.clone());
    let blocker = (request.link_id.to_string(), request.requester_id);
    if request.state != RequestState::Approved
        && inner.blockers.get(&blocker) == Some(&request.request_id)
    {
        inner.blockers.remove(&blocker);
    }
}

/// The real `transition_request`: a pending request past its deadline
/// expires, otherwise an admin decision applies. A request that already left
/// pending, or a read without a decision, is unchanged. Answers whether the
/// request changed.
fn transition(
    request: &mut RequestSnapshot,
    command: Option<&DecideRequest>,
    now: DateTime<Utc>,
) -> bool {
    if request.state != RequestState::Pending {
        return false;
    }
    let deadline = request.decision_deadline.expect("pending deadline");
    let (state, decided_by, effective_at, decline_reason) = if now >= deadline {
        (RequestState::Expired, None, deadline, None)
    } else if let Some(command) = command {
        match &command.action {
            DecisionAction::Approve => (
                RequestState::Approved,
                Some(command.admin.user_id),
                now,
                None,
            ),
            DecisionAction::Decline { reason } => (
                RequestState::Declined,
                Some(command.admin.user_id),
                now,
                reason.clone(),
            ),
        }
    } else {
        return false;
    };
    request.state = state;
    request.revision += 1;
    request.decision = Some(TerminalDecision {
        decision_id: format!("request.{state}/{}", request.request_id),
        decided_by,
        effective_at,
        evaluated_at: now,
        decline_reason,
    });
    true
}

/// The real `local_rejection`: an inactive link, then one pending or
/// approved request per requester.
fn local_rejection(
    link: &LinkSnapshot,
    request: Option<&RequestSnapshot>,
    now: DateTime<Utc>,
) -> Option<Rejection> {
    if let Some(inactive) = link.inactive(now) {
        Some(inactive.into())
    } else if request.is_some_and(blocks) {
        Some(Rejection::ExistingRequest)
    } else {
        None
    }
}

/// The real `accept`: consume one use of `link` and create the admitted
/// request, pending until its deadline or approved at once when the link
/// needs no approval.
fn accept(link: &mut LinkSnapshot, command: &Admit, now: DateTime<Utc>) -> RequestSnapshot {
    let request_id = RequestId::new();
    let approval_required = link.creation.approval_required;
    let state = if approval_required {
        RequestState::Pending
    } else {
        RequestState::Approved
    };
    link.uses += 1;
    link.revision += 1;
    RequestSnapshot {
        request_id,
        link_id: link.link_id,
        account_id: link.creation.account_id,
        requester_id: command.requester_id,
        justification: command.justification.clone(),
        state,
        admitted_at: now,
        decision_deadline: approval_required.then_some(now + PENDING_LIFETIME),
        revision: 1,
        decision: (!approval_required).then(|| TerminalDecision {
            decision_id: format!("request.approved/{request_id}"),
            decided_by: None,
            effective_at: now,
            evaluated_at: now,
            decline_reason: None,
        }),
    }
}

/// The real `requester_view`: the requester never sees the decline reason.
fn requester_view(mut request: RequestSnapshot) -> RequestSnapshot {
    if let Some(decision) = &mut request.decision {
        decision.decline_reason = None;
    }
    request
}

/// The real `decision_outcome`.
fn outcome(
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

/// A pending or approved request blocks its requester's admission.
fn blocks(request: &RequestSnapshot) -> bool {
    matches!(
        request.state,
        RequestState::Pending | RequestState::Approved
    )
}
