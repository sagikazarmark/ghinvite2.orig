//! An in-process stand-in for the Restate invitation link authority.
//!
//! It answers the ingress paths the web calls for invitation links
//! (`/{LINK_SERVICE}/{link_id}/{method}`) and codes
//! (`/{CODE_SERVICE}/{code}/resolve`) with the same status codes the real
//! virtual object uses: 404 for an unknown link or a foreign account, 409 for a
//! creation identity reused with different input, 400 for invalid input. It
//! records every command so tests can assert what reached the authority.
//!
//! The server runs on the test's tokio runtime, so it outlives this handle and
//! keeps serving long-running browser fixtures.
#![allow(dead_code)]

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use ghinvite_core::admission::{AdminLinkCommand, RequesterPage, UpdateMetadata};
use ghinvite_core::storage::projection::{CreateLink, LinkMetadata, LinkSnapshot};
use ghinvite_core::{InvitationLink, InvitationLinkId};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The Restate virtual object that owns invitation links.
pub const LINK_SERVICE: &str = "InvitationLink";
/// The Restate virtual object that resolves invitation codes to links.
pub const CODE_SERVICE: &str = "InvitationCode";

#[derive(Default)]
struct Inner {
    links: BTreeMap<String, LinkSnapshot>,
    requester_pages: BTreeMap<String, RequesterPage>,
    failures: BTreeMap<String, u16>,
    created: Vec<CreateLink>,
    metadata_updates: Vec<UpdateMetadata>,
    revocations: Vec<AdminLinkCommand>,
    calls: Vec<String>,
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
        self.lock()
            .links
            .insert(snapshot.link_id.to_string(), snapshot);
    }

    /// Seed the authoritative counterpart of a projected link.
    pub fn seed_link(&self, link: &InvitationLink) {
        self.seed(snapshot(link));
    }

    /// Answer `requester_page` for the page's link with `page` instead of the
    /// default fresh-form page derived from the seeded link.
    pub fn set_requester_page(&self, page: RequesterPage) {
        self.lock()
            .requester_pages
            .insert(page.link_id.to_string(), page);
    }

    /// Answer every later call of `method` with `status`.
    pub fn fail(&self, method: &str, status: u16) {
        self.lock().failures.insert(method.into(), status);
    }

    pub fn recover(&self, method: &str) {
        self.lock().failures.remove(method);
    }

    pub fn link(&self, id: InvitationLinkId) -> Option<LinkSnapshot> {
        self.lock().links.get(&id.to_string()).cloned()
    }

    /// Every `create` command received, including replays.
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
        self.lock().calls.clone()
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
            version: 1,
            link_id: link.id,
            admin: ghinvite_core::storage::projection::AccountAdmin {
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

fn status(code: u16) -> Response {
    StatusCode::from_u16(code).unwrap().into_response()
}

fn json<T: serde::Serialize>(value: &T) -> Response {
    axum::Json(serde_json::to_value(value).unwrap()).into_response()
}

async fn handle(
    State(inner): State<Arc<Mutex<Inner>>>,
    Path((service, key, method)): Path<(String, String, String)>,
    body: Bytes,
) -> Response {
    let mut inner = inner.lock().unwrap();
    inner.calls.push(method.clone());
    if let Some(code) = inner.failures.get(&method) {
        return status(*code);
    }
    let parse = |body: &[u8]| serde_json::from_slice::<serde_json::Value>(body).ok();
    let Some(body) = parse(&body) else {
        return status(400);
    };
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
    match method.as_str() {
        "create" => {
            let Ok(command) = serde_json::from_value::<CreateLink>(body) else {
                return status(400);
            };
            inner.created.push(command.clone());
            if let Some(link) = inner.links.get(&key) {
                return if link.creation == command {
                    json(link)
                } else {
                    status(409)
                };
            }
            let now = Utc::now();
            if command.description.trim().is_empty()
                || command.repos.is_empty()
                || command.expires_at.is_some_and(|expiry| expiry <= now)
            {
                return status(400);
            }
            let link = LinkSnapshot {
                metadata: None,
                link_id,
                creation: command,
                invitation_code: key[key.len() - 16..].into(),
                created_at: now,
                uses: 0,
                revision: 1,
                revoked_at: None,
                revoked_by: None,
            };
            inner.links.insert(key, link.clone());
            json(&link)
        }
        "link_status" | "revoke" => {
            let Ok(command) = serde_json::from_value::<AdminLinkCommand>(body) else {
                return status(400);
            };
            let Some(link) = inner.links.get_mut(&key) else {
                return status(404);
            };
            if command.admin.account_id != link.creation.account_id {
                return status(404);
            }
            if method == "revoke" && link.revoked_at.is_none() {
                link.revoked_at = Some(Utc::now());
                link.revoked_by = Some(command.admin.user_id);
                link.revision += 1;
            }
            let link = link.clone();
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
            if command.admin.account_id != link.creation.account_id {
                return status(404);
            }
            let description = command.description.trim().to_owned();
            let internal_note = command
                .internal_note
                .as_deref()
                .map(str::trim)
                .filter(|note| !note.is_empty())
                .map(str::to_owned);
            if description.is_empty()
                || description.chars().count() > 120
                || description.contains(['\r', '\n'])
            {
                return status(400);
            }
            link.metadata = Some(LinkMetadata {
                description,
                internal_note,
            });
            link.revision += 1;
            let link = link.clone();
            inner.metadata_updates.push(command);
            json(&link)
        }
        "requester_page" => {
            if let Some(page) = inner.requester_pages.get(&key) {
                return json(page);
            }
            let Some(link) = inner.links.get(&key) else {
                return status(404);
            };
            json(&RequesterPage {
                link_id,
                invitation_code: link.invitation_code.clone(),
                repos: link.creation.repos.clone(),
                permission: link.creation.permission,
                approval_required: link.creation.approval_required,
                can_start_fresh: link.revoked_at.is_none(),
                attempt: None,
                request: None,
            })
        }
        _ => status(404),
    }
}
