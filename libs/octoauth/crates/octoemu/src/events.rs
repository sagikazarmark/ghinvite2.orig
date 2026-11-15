//! The webhook outbox: every state change GitHub would announce by webhook
//! appends an [`Event`] here, with the installations it goes to and its
//! payload. Webhook delivery (`crate::webhooks`, GE-04) turns each target
//! into a delivery; the outbox itself is observable at
//! `GET /__octoemu/events` and [`Emulator::events`](crate::Emulator::events).
//!
//! Emitted today: `member` with `added` (an invitation accepted, or an
//! organization member added directly), `edited` (a collaborator's role
//! changed) and `removed` (GE-03); `installation` with `created`, `deleted`,
//! `suspend`, `unsuspend` and `new_permissions_accepted`, and
//! `installation_repositories` with `added` and `removed` (GE-04).
//!
//! **Emitting an event** (for later tasks): build the payload without
//! `installation` (deliveries add `{"id", "node_id"}` unless the payload
//! carries a full one, as the installation events do), compute the targets
//! (`repository_targets` for a repository's events; an organization's
//! events need an `organization_targets` and `organization` in the
//! payload), and `state.events.push(…)`. The push wakes the dispatcher;
//! nothing else is needed for delivery.

use crate::collaboration::Actor;
use crate::seed::{Installation, Repo, Seed, UserKind};
use crate::state::State;
use crate::views::View;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Notify;

/// One announced change, before delivery.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Sequential from 1 per emulator instance.
    pub id: u64,
    /// When it happened, on the virtual clock.
    pub at: DateTime<Utc>,
    /// The `X-GitHub-Event` name, such as `member`.
    pub event: String,
    /// The payload's `action`, such as `added`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// The installations it goes to, computed when it happened: those of
    /// Apps subscribed to the event that can access the repository and are
    /// not suspended.
    pub targets: Vec<Target>,
    /// The payload, without `installation` (each delivery adds its
    /// target's `{"id", "node_id"}`).
    pub payload: Value,
}

/// An installation an [`Event`] is delivered to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// The App's ID (`X-GitHub-Hook-Installation-Target-ID`).
    pub app_id: u64,
    /// The installation's ID.
    pub installation_id: u64,
}

/// Every event of one instance, oldest first.
#[derive(Debug, Default)]
pub(crate) struct Outbox {
    events: Vec<Event>,
    /// Notified on every push: the delivery dispatcher waits on it.
    wake: Arc<Notify>,
}

impl Outbox {
    /// Appends an event; returns its ID.
    pub(crate) fn push(
        &mut self,
        at: DateTime<Utc>,
        event: &str,
        action: Option<&str>,
        targets: Vec<Target>,
        payload: Value,
    ) -> u64 {
        let id = self.events.len() as u64 + 1;
        self.events.push(Event {
            id,
            at,
            event: event.into(),
            action: action.map(Into::into),
            targets,
            payload,
        });
        self.wake.notify_one();
        id
    }

    pub(crate) fn events(&self) -> &[Event] {
        &self.events
    }

    /// What the delivery dispatcher waits on.
    pub(crate) fn waker(&self) -> Arc<Notify> {
        self.wake.clone()
    }
}

/// The installations a repository-scoped `event` goes to: every
/// installation that can access `repo`, is subscribed to `event` (its own
/// events, else its App's) and is not suspended.
pub(crate) fn repository_targets(world: &Seed, repo: &Repo, event: &str) -> Vec<Target> {
    let mut targets: Vec<Target> = world
        .installations
        .iter()
        .filter(|installation| installation.suspended_at.is_none())
        .filter(|installation| {
            let app_events = world
                .apps
                .iter()
                .find(|app| app.id == installation.app_id)
                .map(|app| &app.events);
            installation
                .events
                .as_ref()
                .or(app_events)
                .is_some_and(|events| events.iter().any(|e| e == event))
        })
        .filter(|installation| {
            crate::apps::installation_repos(world, installation)
                .iter()
                .any(|r| r.id == repo.id)
        })
        .map(|installation| Target {
            app_id: installation.app_id,
            installation_id: installation.id,
        })
        .collect();
    targets.sort_by_key(|t| t.installation_id);
    targets
}

/// Who an installation change is attributed to when nobody is named: the
/// account itself for a user account; for an organization, its first
/// active owner, else the organization (behaviour
/// `installation-event-sender`).
pub(crate) fn default_sender(world: &Seed, account: &str) -> Actor {
    let is_user = world
        .users
        .iter()
        .any(|u| u.login.eq_ignore_ascii_case(account) && u.kind == UserKind::User);
    if is_user {
        return Actor::User(account.to_owned());
    }
    world
        .memberships
        .iter()
        .find(|m| {
            m.org.eq_ignore_ascii_case(account)
                && m.role == crate::seed::OrgRole::Admin
                && m.state == crate::seed::MembershipState::Active
        })
        .map_or_else(
            || Actor::User(account.to_owned()),
            |m| Actor::User(m.user.clone()),
        )
}

fn view<'a>(state: &'a State, origin: &'a str) -> View<'a> {
    View {
        world: &state.world,
        origin,
        start: state.clock.start(),
        now: state.clock.now(),
    }
}

fn sender(view: &View<'_>, actor: &Actor) -> Value {
    match actor {
        Actor::App { app_id } => view.app_bot(*app_id),
        Actor::User(login) => view.account(login).unwrap_or_default(),
    }
}

/// An installation's own event: to its App whatever the App subscribes to
/// (documented: "All GitHub Apps receive this event by default") and
/// whether or not it is suspended (behaviour
/// `installation-events-while-suspended`).
fn installation_target(installation: &Installation) -> Vec<Target> {
    vec![Target {
        app_id: installation.app_id,
        installation_id: installation.id,
    }]
}

/// Appends `installation.<action>` for `installation` (as it is now; call
/// before removing it for `deleted`). The payload carries the full
/// installation, the repositories it can access, `requester: null` and
/// `sender` (documented shape).
pub(crate) fn emit_installation(
    state: &mut State,
    origin: &str,
    action: &str,
    installation: &Installation,
    actor: &Actor,
) {
    let view = view(state, origin);
    let repositories: Vec<Value> = crate::apps::installation_repos(&state.world, installation)
        .into_iter()
        .map(|repo| view.repo_ref(repo))
        .collect();
    let payload = json!({
        "action": action,
        "installation": view.installation(installation),
        "repositories": repositories,
        "requester": null,
        "sender": sender(&view, actor),
    });
    let now = state.clock.now();
    state.events.push(
        now,
        "installation",
        Some(action),
        installation_target(installation),
        payload,
    );
}

/// Appends `installation_repositories.<action>` (`added` or `removed`)
/// with `repository_selection`, `repositories_added` and
/// `repositories_removed` (both always present) as documented.
pub(crate) fn emit_installation_repositories(
    state: &mut State,
    origin: &str,
    action: &str,
    installation: &Installation,
    added: &[Repo],
    removed: &[Repo],
    actor: &Actor,
) {
    let view = view(state, origin);
    let refs = |repos: &[Repo]| repos.iter().map(|r| view.repo_ref(r)).collect::<Vec<_>>();
    let payload = json!({
        "action": action,
        "installation": view.installation(installation),
        "repository_selection": installation.repository_selection,
        "repositories_added": refs(added),
        "repositories_removed": refs(removed),
        "requester": null,
        "sender": sender(&view, actor),
    });
    let now = state.clock.now();
    state.events.push(
        now,
        "installation_repositories",
        Some(action),
        installation_target(installation),
        payload,
    );
}
