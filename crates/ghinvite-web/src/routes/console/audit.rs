//! Audit browsing: URL normalization, bounded local enrichment, and safe summaries.
use crate::{
    middleware::auth::RequireConsoleAdminOf,
    state::AppState,
    views::audit::{AuditLogPage, AuditRow},
    views::render::render_with_csrf as render,
};
use axum::{
    extract::State,
    http::{StatusCode, Uri},
    response::{Html, IntoResponse},
};
use dioxus::prelude::*;
use ghinvite_core::{
    audit::{ActorKind, AuditEvent, EventType, TargetKind},
    storage::{AuditBoundary, AuditPosition},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    v: u8,
    event: Option<EventType>,
    time: chrono::DateTime<chrono::Utc>,
    id: ghinvite_core::AuditEventId,
}

struct Query {
    event: Option<EventType>,
    position: AuditPosition,
}

impl Query {
    fn parse(uri: &Uri) -> Self {
        let params: HashMap<_, _> =
            url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
                .into_owned()
                .collect();
        let event = params.get("event").and_then(|v| v.parse().ok());
        let decode = |token: &String| -> Option<AuditBoundary> {
            let cursor = super::cursor::decode(token, |c: &Cursor| c.time)?;
            if cursor.v != 1 || cursor.event != event {
                return None;
            }
            Some(AuditBoundary {
                occurred_at: cursor.time,
                id: cursor.id,
            })
        };
        let position = match (params.get("before"), params.get("after")) {
            (Some(token), None) => decode(token).map(AuditPosition::Before),
            (None, Some(token)) => decode(token).map(AuditPosition::After),
            _ => None,
        }
        .unwrap_or_default();
        Self { event, position }
    }

    fn href(&self, base: &str, position: AuditPosition) -> String {
        let mut params = url::form_urlencoded::Serializer::new(String::new());
        if let Some(event) = self.event {
            params.append_pair("event", event.as_str());
        }
        if let Some(b) = position.boundary() {
            let cursor = Cursor {
                v: 1,
                event: self.event,
                time: b.occurred_at,
                id: b.id,
            };
            let token = super::cursor::encode(&cursor);
            params.append_pair(
                if matches!(position, AuditPosition::Before(_)) {
                    "before"
                } else {
                    "after"
                },
                &token,
            );
        }
        let params = params.finish();
        if params.is_empty() {
            base.to_owned()
        } else {
            format!("{base}?{params}")
        }
    }
}

pub(super) async fn page(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    uri: Uri,
) -> impl IntoResponse {
    let query = Query::parse(&uri);
    let base = format!("/console/accounts/{}/audit", admin.account.account_login);
    let mut older_href = None;
    let mut newer_href = None;
    let (status, rows) = match state
        .storage
        .list_audit_events(admin.account.account_id, query.event, query.position)
        .await
    {
        Ok(page) => {
            if page.has_older
                && let Some(last) = page.events.last()
            {
                older_href = Some(query.href(&base, AuditPosition::Before(last.into())));
            }
            if page.has_newer
                && let Some(first) = page.events.first()
            {
                newer_href = Some(query.href(&base, AuditPosition::After(first.into())));
            }
            let mut users = HashMap::new();
            let mut links = HashMap::new();
            let mut rows = Vec::with_capacity(page.events.len());
            for event in page.events {
                let actor = match (event.actor_kind, event.actor_id) {
                    (ActorKind::System, _) => "System".into(),
                    (ActorKind::Github, _) => "GitHub".into(),
                    (ActorKind::User, None) => "Unknown GitHub user".into(),
                    (ActorKind::User, Some(id)) => {
                        if let std::collections::hash_map::Entry::Vacant(entry) = users.entry(id) {
                            entry.insert(
                                state
                                    .storage
                                    .get_user(id)
                                    .await
                                    .ok()
                                    .flatten()
                                    .map(|u| u.login),
                            );
                        }
                        match &users[&id] {
                            Some(login) => format!("@{login} · GitHub user ID {id}"),
                            None => format!("GitHub user ID {id}"),
                        }
                    }
                };
                let kind = match event.target_kind {
                    TargetKind::Installation => "Installation",
                    TargetKind::InvitationLink => "Invitation link",
                    TargetKind::InvitationRequest => "Invitation request",
                    TargetKind::GithubInvitation => "GitHub invitation (internal ID)",
                };
                let mut resource_href = None;
                if event.target_kind == TargetKind::InvitationLink
                    && let Ok(id) = event.target_id.parse::<ghinvite_core::InvitationLinkId>()
                {
                    if let std::collections::hash_map::Entry::Vacant(entry) = links.entry(id) {
                        entry.insert(
                            state
                                .storage
                                .invitation_link_belongs_to_account(admin.account.account_id, id)
                                .await
                                .unwrap_or(false),
                        );
                    }
                    if links[&id] {
                        resource_href = Some(format!(
                            "/console/accounts/{}/links/{id}",
                            admin.account.account_login
                        ));
                    }
                }
                let unavailable =
                    if event.target_kind == TargetKind::InvitationLink && resource_href.is_none() {
                        " · Unavailable"
                    } else {
                        ""
                    };
                rows.push(AuditRow {
                    time: event.occurred_at,
                    event: event.event_type,
                    actor,
                    resource: format!("{kind} {}{unavailable}", event.target_id),
                    resource_href,
                    details: details(&event),
                });
            }
            (StatusCode::OK, Some(rows))
        }
        Err(_) => {
            tracing::warn!("failed to load audit history");
            (StatusCode::INTERNAL_SERVER_ERROR, None)
        }
    };
    let latest_href = query.href(&base, AuditPosition::Latest);
    let retry_href = query.href(&base, query.position);
    let html = render(admin.session.csrf_token.clone(), move || {
        rsx! {
            AuditLogPage {
                signed_in_login: Some(admin.session.login.clone()), account_login: admin.account.account_login.clone(),
                event: query.event, in_range: query.position != AuditPosition::Latest, rows: rows.clone(),
                latest_href: latest_href.clone(), retry_href: retry_href.clone(), older_href: older_href.clone(), newer_href: newer_href.clone(),
            }
        }
    });
    (status, Html(html))
}

fn details(event: &AuditEvent) -> String {
    let m = &event.metadata;
    let mut parts = Vec::new();
    match event.event_type {
        EventType::InvitationLinkMetadataUpdated => {
            if let Some(fields) = m.get("changed_fields").and_then(|v| v.as_array()) {
                for (key, label) in [
                    ("description", "Description"),
                    ("internal_note", "Internal note"),
                ] {
                    if fields.iter().any(|v| v.as_str() == Some(key)) {
                        parts.push(label.into());
                    }
                }
            }
        }
        EventType::RequestApproved
            if m.get("reason").and_then(|v| v.as_str()) == Some("auto_approve") =>
        {
            parts.push("Auto-approved".into())
        }
        EventType::InvitationAccepted
            if m.get("reason").and_then(|v| v.as_str()) == Some("already_collaborator") =>
        {
            parts.push("Already a collaborator".into())
        }
        EventType::InvitationAccepted | EventType::InvitationCancelled
            if m.get("reconciled").and_then(|v| v.as_bool()) == Some(true) =>
        {
            parts.push("Observed during reconciliation".into())
        }
        EventType::InvitationLinkCreated => {
            if let Some(permission) = m
                .get("permission")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse::<ghinvite_core::Permission>().ok())
            {
                parts.push(format!("Permission level: {permission}"));
            }
            if let Some(required) = m.get("approval_required").and_then(|v| v.as_bool()) {
                parts.push(
                    if required {
                        "Account-admin approval required"
                    } else {
                        "Auto-approval"
                    }
                    .into(),
                );
            }
            match m.get("max_uses") {
                Some(serde_json::Value::Null) => parts.push("Max use: unlimited".into()),
                Some(value) => {
                    if let Some(n) = value.as_u64().filter(|n| *n > 0 && *n <= u32::MAX as u64) {
                        parts.push(format!("Max use: {n}"));
                    }
                }
                None => {}
            }
            if let Some(n) = m.get("repo_count").and_then(|v| v.as_u64()) {
                parts.push(format!("Repositories: {n}"));
            }
        }
        EventType::InstallationReposChanged => {
            match m.get("selected_repos_kind").and_then(|v| v.as_str()) {
                Some("all") => parts.push("All available repositories".into()),
                Some("subset") => parts.push("Selected repositories".into()),
                _ => {}
            }
        }
        _ => {}
    }
    if parts.is_empty() {
        "—".into()
    } else {
        parts.join("; ")
    }
}
