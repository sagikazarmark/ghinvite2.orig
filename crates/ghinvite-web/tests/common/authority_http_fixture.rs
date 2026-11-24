//! HTTP response fixture for the web's concrete link and request clients.
//!
//! This is not a Restate implementation or evidence of owner correctness. Its
//! in-memory response model supports web rendering/recovery tests only; it has
//! no durable execution, projection queue, GitHub effects or retention behavior.
//! `ghinvite-workflows/tests/canonical_link.rs` proves assembled composition.
//!
//! It answers the ingress paths the web calls for invitation links
//! (`/{LINK_SERVICE}/{link_id}/{method}`) with the same status codes the real
//! virtual object uses: 404 for an unknown link or request or a foreign
//! account, 409 for a command identity reused with different input, 400 for
//! invalid input. Like the real object, a replayed creation, decision or
//! admission attempt answers its original receipt and takes no effect again.
//! Requester attempts follow the admission rules: an inactive link or an open
//! request is a final rejection, a requester page recovers the latest attempt
//! and hides the decline reason, and a page with nothing to show is missing.
//! It records every call so tests can assert what reached the authority.
//!
//! The pure rules (admission, decisions, expiry, the revocation cascade,
//! requester views, link metadata and secret rotation) are `ghinvite-core`'s,
//! the same ones the real objects call; only dispatch, replay and bookkeeping
//! are restated here.
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
use ghinvite_core::invitation_link::{
    AccountAdmin, CreateLink, CreateLinkError, CreateLinkOutcome, GuardrailField, LinkCreated,
    LinkMetadata, LinkSnapshot, RotateSecret, RotationRefusal, SecretRotated,
};
use ghinvite_core::request_lifecycle::RequestSnapshot;
use ghinvite_core::request_lifecycle::{DecideRequest, DecisionReceipt, RequestStatus};
use ghinvite_core::{InvitationLink, InvitationLinkId, RequestId, RequestState};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The Restate virtual object that owns invitation links.
pub const LINK_SERVICE: &str = "InvitationLink";

/// A genuine later observation keeps its immutable create receipt intact.
pub fn accepted_repository_snapshot(
    mut snapshot: ghinvite_core::delivery::DeliverySnapshot,
) -> ghinvite_core::delivery::DeliverySnapshot {
    let (target_kind, target_id) =
        ghinvite_core::delivery_audit::repository_target(&snapshot.create).unwrap();
    snapshot.revision += 1;
    snapshot.settlement = Some(ghinvite_core::delivery::Settlement {
        state: ghinvite_core::InvitationState::Accepted,
        event: ghinvite_core::audit::AuditEvent {
            id: ghinvite_core::delivery_audit::repository_settlement_id(&snapshot.create).unwrap(),
            account_id: snapshot.create.command.account_id,
            occurred_at: snapshot.create.confirmed_at.unwrap() + Duration::hours(1),
            event_type: ghinvite_core::audit::EventType::InvitationAccepted,
            actor_kind: ghinvite_core::audit::ActorKind::Github,
            actor_id: None,
            target_kind,
            target_id,
            metadata: Value::Null,
            request_id: Some(snapshot.create.command.request_id.to_string()),
        },
    });
    snapshot
}

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
    lifecycle: BTreeMap<
        (String, String),
        (
            ghinvite_core::link_lifecycle::LifecycleAction,
            ghinvite_core::link_lifecycle::LifecycleCommand,
            ghinvite_core::link_lifecycle::LifecycleOutcome,
        ),
    >,
    rotations: BTreeMap<(String, String), (RotateSecret, SecretRotated)>,
    requests: BTreeMap<String, RequestSnapshot>,
    /// Scripted decisions by request and operation.
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
    repository_deliveries: BTreeMap<String, Option<ghinvite_core::delivery::DeliverySnapshot>>,
    org_progress: BTreeMap<String, ghinvite_core::request_lifecycle::OrganizationProgress>,
    organization_deliveries:
        BTreeMap<String, ghinvite_core::org_delivery::OrganizationDeliverySnapshot>,
    organization_refresh: Option<ghinvite_core::org_delivery::OrganizationRefresh>,
    repository_refresh: Option<ghinvite_core::delivery::RepositoryRefresh>,
    organization_facts: BTreeMap<String, ghinvite_core::org_eligibility::Facts>,
    requester_pages: BTreeMap<String, RequesterPage>,
    failures: BTreeMap<String, Failure>,
    created: Vec<CreateLink>,
    creation_results: BTreeMap<String, (CreateLink, CreateLinkOutcome)>,
    metadata_updates: Vec<UpdateMetadata>,
    revocations: Vec<AdminLinkCommand>,
    revoke_results: BTreeMap<
        (String, String),
        (
            ghinvite_core::revocation::RevokeCommand,
            ghinvite_core::revocation::RevokeOutcome,
        ),
    >,
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
    pub fn seed_organization_facts(
        &self,
        account_id: u64,
        facts: ghinvite_core::org_eligibility::Facts,
    ) {
        self.lock()
            .organization_facts
            .insert(format!("org:{account_id}"), facts);
    }

    pub fn seed_link(&self, link: &InvitationLink) {
        self.seed(snapshot(link));
    }

    /// Make the authority hold `request`, as if it had admitted it earlier and
    /// it had reached its state since. A pending or approved request blocks
    /// its requester's admission on the link.
    pub fn seed_request(&self, request: RequestSnapshot) {
        let mut inner = self.lock();
        let blocker = (
            request.link_id.expect("linked fixture").to_string(),
            request.requester_id,
        );
        if Rejection::open_request(request.state).is_some() {
            inner.blockers.insert(blocker, request.request_id);
        } else if inner.blockers.get(&blocker) == Some(&request.request_id) {
            inner.blockers.remove(&blocker);
        }
        inner
            .requests
            .insert(request.request_id.to_string(), request);
    }

    /// Explicit None and misbound snapshots exercise the web read boundary.
    pub fn set_repository_delivery(
        &self,
        key: String,
        snapshot: Option<ghinvite_core::delivery::DeliverySnapshot>,
    ) {
        self.lock().repository_deliveries.insert(key, snapshot);
    }

    pub fn seed_organization_delivery(
        &self,
        snapshot: ghinvite_core::org_delivery::OrganizationDeliverySnapshot,
    ) {
        self.lock()
            .organization_deliveries
            .insert(snapshot.create.command.delivery_key(), snapshot);
    }
    pub fn set_organization_refresh(
        &self,
        result: ghinvite_core::org_delivery::OrganizationRefresh,
    ) {
        self.lock().organization_refresh = Some(result);
    }
    pub fn set_organization_progress(
        &self,
        request: RequestId,
        progress: ghinvite_core::request_lifecycle::OrganizationProgress,
    ) {
        self.lock()
            .org_progress
            .insert(request.to_string(), progress);
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

    pub fn set_repository_refresh(&self, result: ghinvite_core::delivery::RepositoryRefresh) {
        self.lock().repository_refresh = Some(result);
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

    pub fn clear_requester_page(&self, link_id: InvitationLinkId) {
        self.lock().requester_pages.remove(&link_id.to_string());
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

    pub fn link(&self, id: impl Into<Option<InvitationLinkId>>) -> Option<LinkSnapshot> {
        let id = id.into()?;
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
    let targets = link
        .targets
        .clone()
        .normalized()
        .expect("valid target fixture");
    LinkSnapshot {
        metadata: None,
        link_id: link.id,
        creation: CreateLink {
            batch_source: link.batch_source,
            access_lifetime_seconds: link.access_lifetime_seconds,
            email_domains: link.email_domains.clone(),
            invitation_lifetime_seconds: link.invitation_lifetime_seconds,
            abuse_threshold_per_hour: link.abuse_threshold_per_hour,
            budget_share_pct: link.budget_share_pct,
            link_id: link.id,
            admin: AccountAdmin {
                account_id: link.account_id,
                user_id: link.created_by,
            },
            account_id: link.account_id,
            organization_installation_id: link.organization_installation_id,
            installation_id: link.installation_id,
            description: link.description.clone(),
            internal_note: link.internal_note.clone(),
            expires_at: link.expires_at,
            max_uses: link.max_uses,
            approval_required: link.approval_required,
            targets,
            secret_sha256: link.secret.sha256,
        },
        secret: link.secret.clone(),
        created_at: link.created_at,
        uses: link.uses_count.into(),
        revision: 1,
        revoked_at: link.revoked_at,
        revoked_by: link.revoked_by,
        pause: link.pause.clone(),
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
        command.request_id.to_string(),
        String::from(command.operation_id.clone()),
    )
}

async fn handle(
    State(inner): State<Arc<Mutex<Inner>>>,
    Path((service, key, method)): Path<(String, String, String)>,
    body: Bytes,
) -> Response {
    let mut inner = inner.lock().unwrap();
    let body = if matches!(
        service.as_str(),
        "RepositoryDelivery" | "OrganizationDelivery"
    ) && body.is_empty()
    {
        Some(Value::Null)
    } else {
        serde_json::from_slice::<Value>(&body).ok()
    };
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
    if service == "OrganizationEligibility" && method == "facts" {
        return json(&inner.organization_facts.get(&key));
    }
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
    if service == "RepositoryDelivery" {
        return match (method, inner.repository_deliveries.get(&key)) {
            ("status", Some(snapshot)) => json(snapshot),
            ("refresh", Some(Some(_))) => json(
                &inner
                    .repository_refresh
                    .clone()
                    .unwrap_or(ghinvite_core::delivery::RepositoryRefresh::Requested),
            ),
            _ => status(404),
        };
    }
    if service == "OrganizationDelivery" {
        return match method {
            "status" => json(&inner.organization_deliveries.get(&key)),
            "refresh" if inner.organization_deliveries.contains_key(&key) => json(
                &inner
                    .organization_refresh
                    .clone()
                    .unwrap_or(ghinvite_core::org_delivery::OrganizationRefresh::Requested),
            ),
            _ => status(404),
        };
    }
    if service != LINK_SERVICE && service != "InvitationRequest" {
        return status(404);
    }
    if service == "InvitationRequest" && method == "requester_status" {
        let Ok(query) =
            serde_json::from_value::<ghinvite_core::request_lifecycle::RequesterStatus>(body)
        else {
            return status(400);
        };
        let Some(mut request) = inner.requests.get(&key).cloned().filter(|r| {
            r.request_id == query.request_id
                && r.requester_id == query.requester_id
                && query.requester_id != 0
        }) else {
            return status(404);
        };
        let Some(link) = request
            .link_id
            .and_then(|id| inner.links.get(&id.to_string()))
            .cloned()
        else {
            return status(404);
        };
        evaluate(inner, &mut request, Utc::now());
        return json(&ghinvite_core::request_lifecycle::RequesterView::new(
            request,
            link.creation.targets,
            inner.progress.get(&key).cloned().unwrap_or_default(),
            inner.org_progress.get(&key).cloned(),
        ));
    }
    let request_method = matches!(
        method,
        "decide"
            | "decision_status"
            | "request_status"
            | "delivery_progress"
            | "organization_delivery_progress"
    );
    if request_method != (service == "InvitationRequest") {
        return status(404);
    }
    let key = if service == "InvitationRequest" {
        if body["request_id"].as_str() != Some(&key) {
            return status(404);
        }
        body["link_id"].as_str().unwrap_or_default().to_owned()
    } else {
        key
    };
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
            if !command.admin.acts_for(command.account_id) {
                return status(404);
            }
            let Ok(command) = command.canonical_input() else {
                return status(400);
            };
            if let Some((original, outcome)) = inner.creation_results.get(&key) {
                return if &command == original {
                    json(outcome)
                } else {
                    status(409)
                };
            }
            if let Some(original) = inner.creations.get(&key) {
                return if command.clone().normalized().ok().as_ref() == Some(&original.creation) {
                    json(&Ok::<_, CreateLinkError>(LinkCreated {
                        link_id,
                        secret_generation: 1,
                    }))
                } else {
                    status(409)
                };
            }
            if let Err(refusal) = command
                .validate_guardrails()
                .and_then(|_| command.validate_targets())
            {
                let outcome: CreateLinkOutcome = Err(refusal);
                inner
                    .creation_results
                    .insert(key, (command, outcome.clone()));
                return json(&outcome);
            }
            let Ok(creation) = command.normalized() else {
                return status(400);
            };
            let now = Utc::now();
            if creation.expires_at.is_some_and(|expiry| expiry <= now) {
                let outcome: CreateLinkOutcome = Err(CreateLinkError::InvalidGuardrail {
                    field: GuardrailField::ExpiresAt,
                });
                inner
                    .creation_results
                    .insert(key, (creation, outcome.clone()));
                return json(&outcome);
            }
            let link = LinkSnapshot {
                metadata: None,
                link_id,
                secret: ghinvite_core::LinkSecretState {
                    secret_generation: 1,
                    sha256: creation.secret_sha256,
                },
                creation,
                created_at: now,
                uses: 0,
                revision: 1,
                revoked_at: None,
                revoked_by: None,
                pause: None,
            };
            let outcome = Ok(LinkCreated {
                link_id,
                secret_generation: 1,
            });
            inner
                .creation_results
                .insert(key.clone(), (link.creation.clone(), outcome.clone()));
            inner.links.insert(key.clone(), link.clone());
            inner.creations.insert(key, link);
            inner.applied.push(method.into());
            json(&outcome)
        }
        "link_status" => {
            let Ok(command) = serde_json::from_value::<AdminLinkCommand>(body) else {
                return status(400);
            };
            let Some(link) = inner.links.get(&key) else {
                return status(404);
            };
            if !command.admin.acts_for(link.creation.account_id) {
                return status(404);
            }
            json(link)
        }
        "revoke" | "revoke_status" => {
            use ghinvite_core::revocation::*;
            let Ok(command) = serde_json::from_value::<RevokeCommand>(body) else {
                return status(400);
            };
            let Some(link) = inner.links.get(&key) else {
                return status(404);
            };
            if !command.admin.acts_for(link.creation.account_id) {
                return status(404);
            }
            let receipt_key = (key.clone(), String::from(command.operation_id.clone()));
            if let Some((old, result)) = inner.revoke_results.get(&receipt_key) {
                if old != &command {
                    return status(409);
                }
                return if method == "revoke_status" {
                    json(&Some(result))
                } else {
                    json(result)
                };
            }
            if method == "revoke_status" {
                return json(&None::<RevokeOutcome>);
            }
            let outcome = if link.revoked_at.is_some() {
                Err(RevokeRefusal::Revoked)
            } else if command.cascade.cancel_pending_invitations
                || command.cascade.remove_granted_access
            {
                Err(RevokeRefusal::UnsupportedCascade)
            } else {
                let now = Utc::now();
                let link = inner.links.get_mut(&key).unwrap();
                link.revoked_at = Some(now);
                link.revoked_by = Some(command.admin.user_id);
                link.revision += 1;
                let mut addressed_requests = 0;
                if command.cascade.decline_pending_requests {
                    for request in inner
                        .requests
                        .values_mut()
                        .filter(|r| r.link_id == Some(command.link_id))
                    {
                        addressed_requests += 1;
                        request
                            .cancel_for_revoked_link(now)
                            .expect("fixture revision in range");
                    }
                }
                inner.applied.push(method.into());
                inner.revocations.push(AdminLinkCommand {
                    link_id: command.link_id,
                    admin: command.admin.clone(),
                });
                Ok(RevokeAccepted {
                    addressed_access_grants: 0,
                    addressed_requests,
                    addressed_deliveries: 0,
                })
            };
            inner.revoke_results.insert(receipt_key, (command, outcome));
            json(&outcome)
        }
        "cascade_progress" => {
            use ghinvite_core::revocation::*;
            let Ok(command) = serde_json::from_value::<AdminLinkCommand>(body) else {
                return status(400);
            };
            let Some(link) = inner.links.get(&key) else {
                return status(404);
            };
            if !command.admin.acts_for(link.creation.account_id) {
                return status(404);
            }
            let accepted = inner.revoke_results.values().find_map(|(c, r)| {
                if c.link_id == command.link_id {
                    r.as_ref().ok()
                } else {
                    None
                }
            });
            json(&accepted.map(|r| CascadeProgress {
                addressed_access_grants: 0,
                done_access_grants: 0,
                addressed_requests: r.addressed_requests,
                addressed_deliveries: r.addressed_deliveries,
                done_requests: r.addressed_requests,
                done_deliveries: 0,
                waiting_on_github: 0,
                unavailable: 0,
            }))
        }
        "pause" | "resume" => {
            use ghinvite_core::link_lifecycle::{LifecycleAction, LifecycleCommand};
            let Ok(command) = serde_json::from_value::<LifecycleCommand>(body) else {
                return status(400);
            };
            let Some(link) = inner.links.get(&key) else {
                return status(404);
            };
            if command.link_id.to_string() != key
                || !command.admin.acts_for(link.creation.account_id)
            {
                return status(404);
            }
            let action = if method == "pause" {
                LifecycleAction::Pause
            } else {
                LifecycleAction::Resume
            };
            let operation = (key.clone(), String::from(command.operation_id.clone()));
            if let Some((original_action, original, outcome)) = inner.lifecycle.get(&operation) {
                return if *original_action == action && *original == command {
                    json(outcome)
                } else {
                    status(409)
                };
            }
            // This HTTP fixture shares the pure decision only. Retained owner
            // serialization and projection/audit are tested in real Restate.
            let Ok(outcome) = ghinvite_core::link_lifecycle::decide(
                link,
                action,
                command.admin.user_id,
                Utc::now(),
            ) else {
                return status(500);
            };
            if let Ok(next) = &outcome {
                inner.links.insert(key.clone(), next.clone());
                inner.applied.push(method.into());
            }
            inner
                .lifecycle
                .insert(operation, (action, command, outcome.clone()));
            json(&outcome)
        }
        "rotate_secret" => {
            let Ok(command) = serde_json::from_value::<RotateSecret>(body) else {
                return status(400);
            };
            let Some(link) = inner.links.get_mut(&key) else {
                return status(404);
            };
            if !command.admin.acts_for(link.creation.account_id) {
                return status(404);
            }
            let operation = (key.clone(), String::from(command.operation_id.clone()));
            if let Some((old, receipt)) = inner.rotations.get(&operation) {
                return if *old == command {
                    json(receipt)
                } else {
                    status(409)
                };
            }
            let receipt = match link.rotate_secret(command.new_sha256) {
                Ok(receipt) => receipt,
                Err(RotationRefusal::Revoked) => return status(409),
                Err(RotationRefusal::Exhausted) => return status(400),
            };
            inner
                .rotations
                .insert(operation, (command, receipt.clone()));
            inner.applied.push(method.into());
            json(&receipt)
        }
        "update_metadata" => {
            let Ok(command) = serde_json::from_value::<UpdateMetadata>(body) else {
                return status(400);
            };
            let Some(link) = inner.links.get_mut(&key) else {
                return status(404);
            };
            if !command.admin.acts_for(link.creation.account_id) {
                return status(404);
            }
            let Ok(metadata) =
                LinkMetadata::parse(&command.description, command.internal_note.as_deref())
            else {
                return status(400);
            };
            let changed = link.update_metadata(metadata);
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
            if !command.admin.acts_for(link.creation.account_id) {
                return status(404);
            }
            if command.action.normalize().is_err() {
                return status(400);
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
            if request.link_id != Some(link_id) {
                return status(404);
            }
            let revision = request.revision;
            let outcome = request.decide(command.admin.user_id, &command.action, Utc::now());
            if request.revision != revision {
                keep(inner, &request);
            }
            let receipt = DecisionReceipt { outcome, request };
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
            if !inner.links[&key]
                .secret
                .matches(&command.secret_sha256, command.secret_generation)
            {
                return status(404);
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
            if !link
                .secret
                .matches(&command.secret_sha256, command.secret_generation)
            {
                let receipt = AdmissionReceipt {
                    decided_at: Utc::now(),
                    result: AdmissionResult::Rejected {
                        reason: Rejection::NotAvailable,
                    },
                };
                inner
                    .admissions
                    .insert(operation, (command, receipt.clone()));
                inner.applied.push(method.into());
                return json(&receipt);
            }
            let blocker = (key.clone(), command.requester_id);
            let mut blocking = match inner.blockers.get(&blocker) {
                Some(id) => match inner.requests.get(&id.to_string()) {
                    Some(request) => Some(request.clone()),
                    None => return status(500),
                },
                None => None,
            };
            let now = Utc::now();
            // Scripted UI response control only: the real owner consumes bounded
            // scope_evidence and performs no installation network read.
            let availability = if link.inactive(now).is_none() {
                let account = link.creation.account_id;
                inner.availability.get(&account).cloned()
            } else {
                None
            };
            // An overdue blocker expires first, as the request owner's
            // eligibility verdict expires it.
            let expired = blocking
                .as_mut()
                .and_then(|request| request.expire_due(now).then(|| request.clone()));
            let reason = link
                .inactive(now)
                .map(Rejection::from)
                .or_else(|| blocking.and_then(|r| Rejection::open_request(r.state)))
                .or_else(|| command.domain_refusal(&link.creation.email_domains, now))
                .or_else(|| {
                    command
                        .scope_unavailable(&link.creation.targets, now)
                        .then_some(Rejection::ScopeUnavailable)
                })
                .or(match &availability {
                    Some(Availability::Unavailable(reason)) => Some(reason.clone()),
                    _ => None,
                });
            let (result, request) = if let Some(reason) = reason {
                (Some(AdmissionResult::Rejected { reason }), None)
            } else if matches!(availability, Some(Availability::Unknown)) {
                (None, None)
            } else {
                let approval_required = link.creation.approval_required;
                let request = command
                    .accept(&mut link, RequestId::new(), approval_required, now)
                    .expect("fixture use count in range");
                let result = AdmissionResult::Accepted {
                    request_id: request.request_id,
                    state: request.state,
                    decision_deadline: request.decision_deadline,
                };
                (Some(result), Some(request))
            };
            // Apply the after-images.
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
        "offer" => {
            let Ok(query) = serde_json::from_value::<ghinvite_core::admission::OfferQuery>(body)
            else {
                return status(400);
            };
            let Some(link) = inner.links.get(&key) else {
                return status(404);
            };
            if query.link_id != link.link_id
                || !link
                    .secret
                    .matches(&query.secret_sha256, query.secret_generation)
            {
                return status(404);
            }
            match ghinvite_core::admission::LinkOffer::current(link, Utc::now()) {
                Some(offer) => json(&offer),
                None => status(404),
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
            if !link
                .secret
                .matches(&query.secret_sha256, query.secret_generation)
            {
                return status(404);
            }
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
                evaluate(inner, &mut current, now);
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
                evaluate(inner, &mut current, now);
                Some(current)
            } else {
                None
            };
            let can_start_fresh = link.inactive(now).is_none()
                && blocker.is_none_or(|r| Rejection::open_request(r.state).is_none());
            let paused = link.inactive(now) == Some(ghinvite_core::Inactive::Paused);
            if !can_start_fresh && !paused && attempt.is_none() && request.is_none() {
                return status(404);
            }
            json(&RequesterPage {
                two_factor_required: None,
                email_domains: link.creation.email_domains.clone(),
                rerequest_after: None,
                paused,
                link_id,
                targets: link.creation.targets,
                approval_required: link.creation.approval_required,
                can_start_fresh,
                attempt,
                request: request.map(RequestSnapshot::requester_view),
            })
        }
        "organization_delivery_progress" => {
            let Ok(query) = serde_json::from_value::<RequestStatus>(body) else {
                return status(400);
            };
            let Some(request) = inner.requests.get(&query.request_id.to_string()) else {
                return status(404);
            };
            if request.link_id != Some(link_id) || request.requester_id != query.requester_id {
                return status(404);
            }
            let progress = matches!(request.state, RequestState::Approved | RequestState::Lapsed)
                .then(|| inner.org_progress.get(&query.request_id.to_string()))
                .flatten();
            json(&progress)
        }
        "delivery_progress" => {
            let Ok(query) = serde_json::from_value::<RequestStatus>(body) else {
                return status(400);
            };
            let Some(request) = inner.requests.get(&query.request_id.to_string()) else {
                return status(404);
            };
            if request.link_id != Some(link_id) || request.requester_id != query.requester_id {
                return status(404);
            }
            if !matches!(request.state, RequestState::Approved | RequestState::Lapsed) {
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
                .targets
                .repos
                .iter()
                .map(|repo| RepositoryProgress {
                    delivery_generation: 1,
                    repo_id: repo.repo_id,
                    stage: DispatchStage::Approved,
                })
                .collect();
            json(&progress)
        }
        _ => status(404),
    }
}

/// Keep a request the shared rules changed. A request that leaves pending
/// other than by approval no longer blocks its requester.
fn keep(inner: &mut Inner, request: &RequestSnapshot) {
    inner
        .requests
        .insert(request.request_id.to_string(), request.clone());
    let blocker = (
        request.link_id.expect("linked fixture").to_string(),
        request.requester_id,
    );
    if request.state != RequestState::Approved
        && inner.blockers.get(&blocker) == Some(&request.request_id)
    {
        inner.blockers.remove(&blocker);
    }
}

/// Evaluate a stored request at `now`, as every real read does, and keep the
/// result.
fn evaluate(inner: &mut Inner, request: &mut RequestSnapshot, now: DateTime<Utc>) {
    if request.expire_due(now) {
        keep(inner, request);
    }
}
