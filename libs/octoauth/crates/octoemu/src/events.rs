//! The webhook outbox: every state change GitHub would announce by webhook
//! appends an [`Event`] here, with the installations it would go to and its
//! payload. Webhook delivery (GE-04) reads it; until then it is observable at
//! `GET /__octoemu/events` and [`Emulator::events`](crate::Emulator::events).
//!
//! Emitted today (GE-03): `member` with `added` (an invitation accepted, or
//! an organization member added directly), `edited` (a collaborator's role
//! changed) and `removed`.

use crate::seed::{Repo, Seed};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
        id
    }

    pub(crate) fn events(&self) -> &[Event] {
        &self.events
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
