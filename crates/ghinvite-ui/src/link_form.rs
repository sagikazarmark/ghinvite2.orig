//! The new invitation link form's shared model.
//!
//! One typed [`CreateLinkForm`] (dioform `#[derive(Form)]`), one set of
//! validators registered against its typed field paths, and the pure parsers
//! that turn typed text into the model's numeric guardrails. Per ADR 0001 the
//! server runs all of this through `dioform-core` inside the axum handler, and
//! the browser island reuses the very same registrations through the `dioform`
//! facade — so this module knows nothing about HTTP, Dioxus, or the browser.
//!
//! Rendered `name` attributes come from [`CreateLinkForm::fields`] so the HTML
//! and the model cannot drift.

use chrono::{DateTime, TimeDelta, Utc};
use dioform_core::{FieldIdentity, Form, FormCore};
use dioform_derive::Form;
// The `Props` derive expands to paths under `dioxus_core`, which `dioxus`'s
// prelude normally brings in; this module only needs the derive itself.
use dioxus::{dioxus_core, prelude::Props};
use ghinvite_core::{
    Description, DescriptionError, InternalNote, InternalNoteTooLong, InvitationLinkRepo,
    Permission, RepositoryScope, RepositoryScopeError,
};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// The new invitation link form as the admin fills it in.
///
/// Text fields hold exactly what was typed (untrimmed); normalisation happens
/// when the validated form becomes creation data. `permission` stays a string
/// because the set-all helper binds strings. Only the explicit `repo_grants`
/// values determine each selected target’s permission. The numeric guardrails are already parsed:
/// `None` is a blank field (unlimited uses / default expiration).
/// Only `never_expires` opts out of expiration.
#[derive(Clone, Debug, PartialEq, Form)]
#[form(crate = "::dioform_core")]
pub struct CreateLinkForm {
    pub access_lifetime_hours: String,
    pub email_domains: String,
    pub description: String,
    pub internal_note: String,
    pub permission: String,
    pub approval_required: bool,
    pub include_organization: bool,
    pub team_ids: Vec<u64>,
    pub max_uses: Option<u32>,
    pub expires_in_days: Option<u32>,
    pub invitation_lifetime_hours: Option<u32>,
    pub abuse_threshold_per_hour: Option<u32>,
    pub budget_share_pct: Option<u32>,
    pub expiration_timestamp: String,
    pub invitation_lifetime_seconds: String,
    pub never_expires: bool,
    pub repo_ids: Vec<u64>,
    /// Explicit per-repository values: `<id>:<permission>`.
    pub repo_grants: Vec<String>,
}

/// One of the account's available repositories, offered as a repository-scope
/// option on the new invitation link form.
///
/// This is the form's own shape, not the GitHub API payload: the web route
/// maps the installation's repository list into it at the boundary, so this
/// crate (and any browser build of it) never depends on the GitHub client.
/// Serialised into the island's props blob.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryChoice {
    /// GitHub repository id, submitted as the `repo_ids` checkbox value.
    pub id: u64,
    /// `owner/name`, shown as the checkbox label.
    pub full_name: String,
}

/// Repositories the picker lists per page. GitHub lists installation
/// repositories 100 to a page, so an unfiltered picker page never needs more
/// than one listing page beyond the ones before it.
pub const REPOSITORY_PICKER_PAGE_SIZE: usize = 50;
/// Name matches a delegated Repository Admin is shown, without paging (P1
/// §6.2: search-first, at most 20 candidates, no permission reads).
pub const DELEGATED_SEARCH_LIMIT: usize = 20;
/// Longest repository filter the picker keeps, in characters.
pub const REPOSITORY_QUERY_MAX_CHARS: usize = 100;
/// POST keys of the picker controls. They only steer which repositories the
/// re-rendered form lists; none of them is part of [`CreateLinkForm`].
pub const REPO_QUERY_FIELD: &str = "repo_query";
pub const REPO_PAGE_FIELD: &str = "repo_page";
pub const REPO_SEARCH_FIELD: &str = "repo_search";
pub const REPO_GOTO_FIELD: &str = "repo_goto";

/// The create form's repository picker: which slice of the installation's
/// repositories the repository scope group lists.
///
/// The server filters the installation's repository list by `query` and
/// renders one page of the matches, so neither the markup nor the island's
/// props grow with the installation. Selected repositories outside that page
/// are listed too, ahead of it (`pinned` of them), so a selection survives
/// searching and paging. Searching and paging are native POSTs to the create
/// route that only re-render the form ([`REPO_SEARCH_FIELD`],
/// [`REPO_GOTO_FIELD`]); they never create anything, and the island does not
/// filter on its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryPicker {
    /// The filter as the admin typed it; matched case-insensitively against
    /// `owner/name`.
    pub query: String,
    /// The 1-based page shown.
    pub page: usize,
    /// Pages of matches; at least 1.
    pub pages: usize,
    /// Repositories matching `query` across every page.
    pub matches: usize,
    /// The first `pinned` repositories offered are selections from outside
    /// this page.
    pub pinned: usize,
    /// A delegated Repository Admin must search before anything is listed.
    pub search_required: bool,
    /// More repositories match than a delegated search shows.
    pub truncated: bool,
}

impl Default for RepositoryPicker {
    fn default() -> Self {
        Self {
            query: String::new(),
            page: 1,
            pages: 1,
            matches: 0,
            pinned: 0,
            search_required: false,
            truncated: false,
        }
    }
}

impl RepositoryPicker {
    /// `raw` as the filter the picker keeps: line breaks removed (a search
    /// input cannot hold them) and at most [`REPOSITORY_QUERY_MAX_CHARS`].
    pub fn normalize_query(raw: &str) -> String {
        raw.chars()
            .filter(|c| *c != '\r' && *c != '\n')
            .take(REPOSITORY_QUERY_MAX_CHARS)
            .collect()
    }

    /// Whether `full_name` matches `query` (blank matches everything).
    pub fn matches_query(query: &str, full_name: &str) -> bool {
        full_name
            .to_lowercase()
            .contains(&query.trim().to_lowercase())
    }
}

/// Verified choices supplied by the server; these never come from POST fields.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrganizationOffer {
    #[default]
    NotApplicable,
    MissingApp,
    Disabled,
    Unavailable,
    Available(OrganizationChoice),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizationChoice {
    pub id: u64,
    pub login: String,
    pub teams: Vec<TeamChoice>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamChoice {
    pub id: u64,
    pub slug: String,
    pub name: String,
    /// Positive upstream evidence only; false does not guarantee write eligibility.
    pub externally_managed: bool,
}

/// The form's validation failures, one message slot per control the view can
/// attach it to, plus the summary shown above the form.
///
/// Built by [`LinkFormErrors::attach`]ing messages keyed by the model's
/// [`FieldIdentity`], so the mapping from a dioform error to a slot is by
/// identity, not position. The typed fields stay public because the view reads
/// them directly (and tests build them literally). Serialised into the
/// island's props blob so the browser starts from the server's errors.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LinkFormErrors {
    #[serde(default)]
    pub access_lifetime_hours: Option<String>,
    #[serde(default)]
    pub email_domains: Option<String>,
    /// Shown in the alert above the form: the summary line, followed by any
    /// form-level messages that have no control of their own.
    pub summary: Vec<String>,
    pub description: Option<String>,
    pub internal_note: Option<String>,
    pub permission: Option<String>,
    pub max_uses: Option<String>,
    pub expires_in_days: Option<String>,
    pub invitation_lifetime_hours: Option<String>,
    pub abuse_threshold_per_hour: Option<String>,
    pub budget_share_pct: Option<String>,
    /// Section-level error for the repository checkbox group (`repo_ids`).
    pub repo_scope: Option<String>,
    #[serde(default)]
    pub organization: Option<String>,
    #[serde(default)]
    pub expiration_timestamp: Option<String>,
    #[serde(default)]
    pub invitation_lifetime_seconds: Option<String>,
    #[serde(default)]
    pub teams: Option<String>,
    pub repo_grants: Option<String>,
}

/// The new invitation link form as the view renders it: the submitted values
/// verbatim (so an admin sees exactly what they typed when correcting a
/// mistake) plus the errors to show next to each control.
///
/// Serialised into the island's props blob, so the browser starts from the
/// same values and server-side errors the page shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkFormValues {
    #[serde(default)]
    pub access_lifetime_hours: String,
    #[serde(default)]
    pub email_domains: String,
    pub description: String,
    pub permission: String,
    pub approval_required: bool,
    #[serde(default)]
    pub include_organization: bool,
    #[serde(default)]
    pub selected_team_ids: Vec<u64>,
    pub max_uses: String,
    pub expires_in_days: String,
    pub invitation_lifetime_hours: String,
    pub abuse_threshold_per_hour: String,
    pub budget_share_pct: String,
    #[serde(default)]
    pub expiration_timestamp: String,
    #[serde(default)]
    pub invitation_lifetime_seconds: String,
    #[serde(default)]
    pub never_expires: bool,
    pub internal_note: String,
    pub selected_repo_ids: Vec<u64>,
    pub repo_grants: Vec<String>,
    pub errors: LinkFormErrors,
}

impl Default for LinkFormValues {
    fn default() -> Self {
        Self {
            access_lifetime_hours: String::new(),
            email_domains: String::new(),
            description: String::new(),
            permission: "pull".into(),
            approval_required: false,
            include_organization: false,
            selected_team_ids: vec![],
            max_uses: String::new(),
            expires_in_days: "14".into(),
            invitation_lifetime_hours: "168".into(),
            abuse_threshold_per_hour: String::new(),
            budget_share_pct: "50".into(),
            expiration_timestamp: String::new(),
            invitation_lifetime_seconds: String::new(),
            never_expires: false,
            internal_note: String::new(),
            selected_repo_ids: Vec::new(),
            repo_grants: Vec::new(),
            errors: LinkFormErrors::default(),
        }
    }
}

impl LinkFormValues {
    /// The typed model these values describe, applied by the server to the raw
    /// POST and by the island to the values it mounts with.
    ///
    /// Text fields are carried verbatim; the numeric guardrails go through
    /// [`parse_max_uses`] and [`parse_expires_in_days`]. A guardrail that fails
    /// to parse is left blank in the model and its message attached to the
    /// returned errors, keyed by field identity, so the validators do not
    /// report the same field twice. `self.errors` is not consulted.
    pub fn to_model(&self) -> (CreateLinkForm, LinkFormErrors) {
        let fields = CreateLinkForm::fields();
        let mut errors = LinkFormErrors::default();

        // A guardrail parses to its value, or to blank plus a message on its
        // field.
        let mut parsed = |field: FieldIdentity, parsed: Result<Option<u32>, String>| {
            parsed.unwrap_or_else(|message| {
                errors.attach(Some(&field), message);
                None
            })
        };
        let max_uses = parsed(fields.max_uses().identity(), parse_max_uses(&self.max_uses));
        let expires_in_days = parsed(
            fields.expires_in_days().identity(),
            parse_expires_in_days(&self.expires_in_days),
        );

        let invitation_lifetime_hours = parsed(
            fields.invitation_lifetime_hours().identity(),
            parse_invitation_lifetime_hours(&self.invitation_lifetime_hours),
        );
        let abuse_threshold_per_hour = parsed(
            fields.abuse_threshold_per_hour().identity(),
            parse_abuse_threshold_per_hour(&self.abuse_threshold_per_hour),
        );

        let budget_share_pct = parsed(
            fields.budget_share_pct().identity(),
            parse_budget_share_pct(&self.budget_share_pct),
        );
        let model = CreateLinkForm {
            access_lifetime_hours: self.access_lifetime_hours.clone(),
            email_domains: self.email_domains.clone(),
            description: self.description.clone(),
            internal_note: self.internal_note.clone(),
            permission: self.permission.clone(),
            approval_required: self.approval_required,
            include_organization: self.include_organization,
            team_ids: self.selected_team_ids.clone(),
            max_uses,
            expires_in_days,
            invitation_lifetime_hours,
            abuse_threshold_per_hour,
            budget_share_pct,
            expiration_timestamp: self.expiration_timestamp.clone(),
            invitation_lifetime_seconds: self.invitation_lifetime_seconds.clone(),
            never_expires: self.never_expires,
            repo_ids: self.selected_repo_ids.clone(),
            repo_grants: self.repo_grants.clone(),
        };
        (model, errors)
    }
}

/// Replace exactly one repository grant after an explicit edit (or set-all).
pub fn set_repo_grant(values: &mut Vec<String>, id: u64, grant: &str) {
    values.retain(|value| {
        value
            .split_once(':')
            .is_none_or(|(key, _)| key.parse::<u64>() != Ok(id))
    });
    values.push(format!("{id}:{grant}"));
}

/// Raw per-repository selection, kept even when it cannot be rendered as an option.
pub fn raw_repo_grant(values: &[String], id: u64) -> Option<&str> {
    values.iter().find_map(|value| {
        value
            .split_once(':')
            .filter(|(key, _)| key.parse::<u64>() == Ok(id))
            .map(|(_, grant)| grant)
    })
}

/// Derive grants only for selected repositories. Extra dropdowns do not grant access.
/// A selected repository must have one explicit, canonical, supported grant.
pub fn repository_targets(
    selected: &[u64],
    grants: &[String],
    available: &[RepositoryChoice],
) -> Result<ghinvite_core::targets::Targets, &'static str> {
    let scope = RepositoryScope::parse(repository_scope(selected, available))
        .map_err(|_| REPO_SCOPE_UNUSABLE)?;
    let mut repos = Vec::new();
    for repo in scope.as_slice() {
        let matches: Vec<_> = grants
            .iter()
            .filter_map(|value| value.split_once(':'))
            .filter(|(id, _)| id.parse::<u64>() == Ok(repo.repo_id))
            .collect();
        let [(id, raw)] = matches.as_slice() else {
            return Err(REPO_GRANT_REQUIRED);
        };
        if *id != repo.repo_id.to_string() {
            return Err(REPO_GRANT_REQUIRED);
        }
        let permission = Permission::from_str(raw).map_err(|_| REPO_GRANT_REQUIRED)?;
        repos.push(ghinvite_core::targets::RepoTarget {
            repo_id: repo.repo_id,
            repo_full_name: repo.repo_full_name.clone(),
            grant: permission.into(),
        });
    }
    Ok(ghinvite_core::targets::Targets {
        repos,
        ..Default::default()
    })
}

pub const EXACT_EXPIRATION_INVALID: &str = "Enter an exact future RFC 3339 expiration, or clear it to use days. Clear Never expires when setting an exact expiration.";
pub const EXACT_LIFETIME_INVALID: &str = "Exact invitation lifetime must be a whole number of 3600–604800 seconds, or blank to use hours.";
pub fn exact_expiration(raw: &str) -> Result<Option<DateTime<Utc>>, &'static str> {
    if raw.is_empty() {
        return Ok(None);
    }
    DateTime::parse_from_rfc3339(raw)
        .map(|date| Some(date.with_timezone(&Utc)))
        .map_err(|_| EXACT_EXPIRATION_INVALID)
}
pub fn exact_lifetime(raw: &str) -> Result<Option<u32>, &'static str> {
    if raw.is_empty() {
        return Ok(None);
    }
    if !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(EXACT_LIFETIME_INVALID);
    }
    raw.parse::<u32>()
        .ok()
        .filter(|seconds| (3600..=604800).contains(seconds))
        .map(Some)
        .ok_or(EXACT_LIFETIME_INVALID)
}

pub const ACCESS_LIFETIME_INVALID: &str =
    "Enter positive whole hours (up to 1193046), exact seconds with an s suffix, or leave blank.";
/// A finite access lifetime starts only when a newly created invitation is accepted.
pub fn access_lifetime_seconds(raw: &str) -> Result<Option<u32>, &'static str> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let seconds = if let Some(seconds) = raw.strip_suffix('s') {
        seconds.parse::<u32>().ok().filter(|seconds| *seconds > 0)
    } else {
        raw.parse::<u32>()
            .ok()
            .filter(|hours| *hours > 0)
            .and_then(|hours| hours.checked_mul(3600))
    };
    seconds.map(Some).ok_or(ACCESS_LIFETIME_INVALID)
}

pub const ORGANIZATION_UNAVAILABLE: &str = "Organization membership is unavailable. An active organization app and organization owner access are required.";
pub const ORGANIZATION_MAX_USES: &str =
    "Set a maximum number of uses for an automatically approved organization invitation.";
pub const TEAM_SELECTION_INVALID: &str =
    "Choose available teams in this organization. Externally managed teams cannot be selected.";

fn team_targets(
    selected_org: bool,
    selected: &[u64],
    offer: &OrganizationOffer,
) -> Result<Vec<ghinvite_core::targets::TeamTarget>, &'static str> {
    if selected.is_empty() {
        return Ok(vec![]);
    }
    if !selected_org || selected.len() > 100 {
        return Err(TEAM_SELECTION_INVALID);
    }
    let OrganizationOffer::Available(org) = offer else {
        return Err(TEAM_SELECTION_INVALID);
    };
    let mut ids = std::collections::BTreeSet::new();
    selected
        .iter()
        .map(|id| {
            if !ids.insert(*id) {
                return Err(TEAM_SELECTION_INVALID);
            }
            let team = org
                .teams
                .iter()
                .find(|team| team.id == *id && !team.externally_managed)
                .ok_or(TEAM_SELECTION_INVALID)?;
            Ok(ghinvite_core::targets::TeamTarget {
                team_id: team.id,
                team_slug: team.slug.clone(),
                grant: ghinvite_core::targets::MemberGrant::Member,
            })
        })
        .collect()
}

pub fn selected_targets(
    model: &CreateLinkForm,
    repos: &[RepositoryChoice],
    offer: &OrganizationOffer,
) -> Result<ghinvite_core::targets::Targets, &'static str> {
    let mut targets = if model.repo_ids.is_empty() && model.include_organization {
        ghinvite_core::targets::Targets::default()
    } else {
        repository_targets(&model.repo_ids, &model.repo_grants, repos)?
    };
    if model.include_organization {
        let OrganizationOffer::Available(org) = offer else {
            return Err(ORGANIZATION_UNAVAILABLE);
        };
        targets.organization = Some(ghinvite_core::targets::OrganizationTarget {
            org_id: org.id,
            org_login: org.login.clone(),
            grant: ghinvite_core::targets::MemberGrant::Member,
        });
    }
    targets.teams = team_targets(model.include_organization, &model.team_ids, offer)?;
    targets.normalized().map_err(|_| TEAM_SELECTION_INVALID)
}

// --- island props -----------------------------------------------------------

/// `id` of the container the island mounts on; it wraps the server-rendered
/// `<form>` (see `links::LinkCreateFormPage`).
pub const LINK_FORM_ISLAND_ROOT_ID: &str = "link-form-island";
/// `id` of the `<script type="application/json">` block holding the
/// serialised [`LinkFormIslandProps`].
pub const LINK_FORM_ISLAND_PROPS_ID: &str = "link-form-props";
/// `src` of the island's module script. A stable name: the island build
/// writes a small loader here that imports the hashed `dx` bundle, so the
/// server never needs to know the hash. When the file is absent the browser
/// gets a 404 for the module and the plain form keeps working.
pub const LINK_FORM_ISLAND_MODULE_SRC: &str = "/assets/ghinvite-island.js";

/// Everything the island needs to re-render the form the server rendered:
/// the same inputs `LinkCreateForm` received plus the server's clock.
///
/// The server serialises this into the props blob ([`Self::script_json`]);
/// the island deserialises it on mount. `now` is the instant the server
/// validated against, so the island passes it to [`register_validators`]
/// instead of reading a clock in wasm — both sides judge expiration from the
/// same anchor.
///
/// Also a Dioxus `Props` struct, so the island component
/// (`ghinvite_island::LinkFormIsland`) takes it directly: what the server
/// serialised is exactly what the browser component receives.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Props)]
pub struct LinkFormIslandProps {
    #[serde(default)]
    #[props(default)]
    pub access_removal_enabled: bool,
    // Require explicit forwarding at the browser entry point, including not-applicable or unknown facts.
    #[serde(default)]
    pub eligibility: crate::organization_eligibility::Eligibility,
    #[props(default)]
    pub csrf_token: Option<String>,
    /// `action` of the form; the route that handles the POST.
    pub action: String,
    pub values: LinkFormValues,
    /// The repositories the form offers: the picker's pinned selections, then
    /// its page, each in the order the account makes them available.
    pub repos: Vec<RepositoryChoice>,
    #[serde(default)]
    #[props(default)]
    pub picker: RepositoryPicker,
    #[serde(default)]
    #[props(default)]
    pub organization: OrganizationOffer,
    /// The server's instant, RFC 3339.
    pub now: DateTime<Utc>,
}

impl LinkFormIslandProps {
    /// The props as the body of a `<script type="application/json">` block.
    ///
    /// JSON is not HTML: the HTML parser ends a script element at the first
    /// `</script` regardless of what the JSON quoting says, so a description
    /// containing `</script>` would break out of the data block. Every `<` is
    /// written as the JSON escape `\u003c`, which `JSON.parse` reads back as
    /// `<` and the HTML parser never sees as a tag.
    pub fn script_json(&self) -> String {
        json_for_script_block(self)
    }
}

/// Serialise `value` for embedding inside a `<script type="application/json">`
/// element: standard JSON with every `<` escaped as `\u003c` so the text can
/// never contain `</script` (or a comment opener `<!--`) for the HTML parser.
pub fn json_for_script_block<T: Serialize + ?Sized>(value: &T) -> String {
    // Serialising a struct made of strings, bools, integers, vectors, options
    // and a chrono timestamp cannot fail: there are no map keys to reject and
    // no I/O. `expect` documents that this is structurally infallible.
    serde_json::to_string(value)
        .expect("island props serialise to JSON")
        .replace('<', "\\u003c")
}

impl LinkFormErrors {
    /// Attach one message: to the slot for `field`, or to the summary for a
    /// form-level error (`None`) or a field without a slot of its own. The
    /// summary line is added the first time anything is attached. A field
    /// shows one message, so the first attached message per field wins.
    pub fn attach(&mut self, field: Option<&FieldIdentity>, message: String) {
        if self.summary.is_empty() {
            self.summary.push(SUMMARY_MESSAGE.to_string());
        }
        match field.and_then(|field| self.slot_mut(field)) {
            Some(slot) => {
                if slot.is_none() {
                    *slot = Some(message);
                }
            }
            None => self.summary.push(message),
        }
    }

    /// Retire the message attached to `field`: it described a value that is
    /// gone, because the admin changed that control (the browser island's
    /// takeover does this for edits made before it mounted). The inverse of
    /// [`Self::attach`], so the summary line goes with the last field message —
    /// an alert reading "fix the highlighted fields" must never survive the
    /// last highlighted field. Form-level messages keep both.
    pub fn retire(&mut self, field: &FieldIdentity) {
        if let Some(slot) = self.slot_mut(field) {
            *slot = None;
        }
        if !self.has_field_message() && self.summary == [SUMMARY_MESSAGE] {
            self.summary.clear();
        }
    }

    /// No message has been attached.
    pub fn is_empty(&self) -> bool {
        self.summary.is_empty() && !self.has_field_message()
    }

    /// Some control carries a message of its own.
    fn has_field_message(&self) -> bool {
        self.access_lifetime_hours.is_some()
            || self.email_domains.is_some()
            || self.description.is_some()
            || self.internal_note.is_some()
            || self.permission.is_some()
            || self.max_uses.is_some()
            || self.expires_in_days.is_some()
            || self.invitation_lifetime_hours.is_some()
            || self.abuse_threshold_per_hour.is_some()
            || self.budget_share_pct.is_some()
            || self.expiration_timestamp.is_some()
            || self.invitation_lifetime_seconds.is_some()
            || self.organization.is_some()
            || self.teams.is_some()
            || self.repo_scope.is_some()
            || self.repo_grants.is_some()
    }

    /// The slot a model field's errors render in, if it has one.
    fn slot_mut(&mut self, field: &FieldIdentity) -> Option<&mut Option<String>> {
        let fields = CreateLinkForm::fields();
        if *field == fields.access_lifetime_hours().identity() {
            Some(&mut self.access_lifetime_hours)
        } else if *field == fields.email_domains().identity() {
            Some(&mut self.email_domains)
        } else if *field == fields.description().identity() {
            Some(&mut self.description)
        } else if *field == fields.internal_note().identity() {
            Some(&mut self.internal_note)
        } else if *field == fields.permission().identity() {
            Some(&mut self.permission)
        } else if *field == fields.max_uses().identity() {
            Some(&mut self.max_uses)
        } else if *field == fields.expires_in_days().identity() {
            Some(&mut self.expires_in_days)
        } else if *field == fields.invitation_lifetime_hours().identity() {
            Some(&mut self.invitation_lifetime_hours)
        } else if *field == fields.abuse_threshold_per_hour().identity() {
            Some(&mut self.abuse_threshold_per_hour)
        } else if *field == fields.budget_share_pct().identity() {
            Some(&mut self.budget_share_pct)
        } else if *field == fields.expiration_timestamp().identity() {
            Some(&mut self.expiration_timestamp)
        } else if *field == fields.invitation_lifetime_seconds().identity() {
            Some(&mut self.invitation_lifetime_seconds)
        } else if *field == fields.include_organization().identity() {
            Some(&mut self.organization)
        } else if *field == fields.team_ids().identity() {
            Some(&mut self.teams)
        } else if *field == fields.repo_grants().identity() {
            Some(&mut self.repo_grants)
        } else if *field == fields.repo_ids().identity() {
            Some(&mut self.repo_scope)
        } else {
            None
        }
    }
}

// --- messages ---------------------------------------------------------------
//
// Every user-facing message this form can produce, in one place so the server,
// the island, and the tests agree byte for byte. Copy follows CONTEXT.md.

/// The one summary line shown above the form whenever any field failed.
pub const SUMMARY_MESSAGE: &str =
    "Fix the highlighted fields before creating this invitation link.";
pub const DESCRIPTION_REQUIRED: &str =
    "Description is required. Use short, single-line admin-only context for this invitation link.";
pub const DESCRIPTION_SINGLE_LINE: &str = "Description must be a single line.";
pub const DESCRIPTION_TOO_LONG: &str = "Description must be 120 characters or fewer.";
pub const INTERNAL_NOTE_TOO_LONG: &str = "Internal note must be 16384 UTF-8 bytes or fewer.";
pub const REPO_GRANT_REQUIRED: &str =
    "Choose exactly one supported permission for every selected repository.";
pub const PERMISSION_UNSUPPORTED: &str =
    "Choose a supported permission level: pull, triage, push, maintain, or admin.";
pub const INVITATION_LIFETIME_INVALID: &str =
    "Invitation lifetime must be a whole number of hours from 1 to 168 (7 days).";
pub const ABUSE_THRESHOLD_INVALID: &str = "Hourly abuse threshold must be a whole number from 1 to 4294967295, or blank for the automatic threshold.";
pub const MAX_USES_NOT_POSITIVE: &str = "Max use must be a whole number of 1 or more.";
pub const MAX_USES_TOO_LARGE: &str = "Max use is too large. Use a smaller number, or leave it blank for unlimited invitation requests.";
pub const EXPIRES_IN_DAYS_NOT_POSITIVE: &str =
    "Expiration must be a whole number of days, 1 or more.";
pub const EXPIRES_IN_DAYS_TOO_FAR: &str =
    "Expiration is too far in the future. Use fewer days, or select Never expires.";
pub const REPO_SCOPE_REQUIRED: &str = "Repository scope is required. Select at least one available repository for this invitation link.";
pub const REPO_SCOPE_TOO_MANY: &str = "Repository scope is limited to 100 repositories. Select fewer repositories, or create another invitation link for the rest.";
pub const REPO_SCOPE_UNUSABLE: &str = "GitHub returned repository details that cannot be used in a repository scope. Reload the repositories, then try again.";
/// Form-level: every rule passed, yet the checked values could not be
/// derived. Nothing the admin typed is at fault, so no field is highlighted.
pub const NO_GUARDRAIL: &str = "Automatically approved links need an expiration, a maximum use limit, or an email-domain restriction.";
pub const CREATION_VALUES_UNCHECKED: &str = "The invitation link could not be created from these values. Try again, and report the problem if it keeps happening.";

// --- parsers ----------------------------------------------------------------
//
// dioform separates parsing (typed text → model value) from validation (rules
// over the model). These parsers are what the server applies to the raw POST
// and what the island hands to `use_number_with`, so the two agree on which
// strings become which `Option<u32>` — and on the message when they do not.

/// Parse the typed max use: blank means unlimited (`None`); otherwise a
/// positive whole number that fits the `u32` the invitation link stores.
pub fn parse_max_uses(raw: &str) -> Result<Option<u32>, String> {
    parse_optional_count(raw, MAX_USES_NOT_POSITIVE, MAX_USES_TOO_LARGE)
}

/// Parse the typed expiration: blank means the default duration (`None`); otherwise a
/// positive whole number of days. Whether `now` can actually be advanced that
/// far is the validator's job (it needs `now`); the parser only guards the
/// storage type.
pub fn parse_expires_in_days(raw: &str) -> Result<Option<u32>, String> {
    parse_optional_count(raw, EXPIRES_IN_DAYS_NOT_POSITIVE, EXPIRES_IN_DAYS_TOO_FAR)
}

/// Blank uses GitHub's seven-day lifetime; shorter lifetimes request own expiry.
pub fn parse_invitation_lifetime_hours(raw: &str) -> Result<Option<u32>, String> {
    let value = parse_optional_count(
        raw,
        INVITATION_LIFETIME_INVALID,
        INVITATION_LIFETIME_INVALID,
    )?;
    if value.is_some_and(|hours| hours > 168) {
        return Err(INVITATION_LIFETIME_INVALID.into());
    }
    Ok(value)
}

/// Blank retains the automatic threshold derived from the link's maximum uses.
pub fn parse_abuse_threshold_per_hour(raw: &str) -> Result<Option<u32>, String> {
    parse_optional_count(raw, ABUSE_THRESHOLD_INVALID, ABUSE_THRESHOLD_INVALID)
}

pub const BUDGET_SHARE_INVALID: &str = "Enter a whole percentage from 1 to 100.";
/// Blank uses the immutable 50% default.
pub fn parse_email_domains(
    raw: &str,
) -> Result<Vec<ghinvite_core::email_domain::DomainRule>, String> {
    const INVALID: &str = "Enter at most 32 valid domains, separated by commas or new lines. Use *.example.com for subdomains only.";
    if raw.chars().count() > 4096 {
        return Err(INVALID.into());
    }
    let mut rules = Vec::new();
    for value in raw
        .split([',', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        rules.push(
            ghinvite_core::email_domain::DomainRule::parse(value)
                .map_err(|_| INVALID.to_owned())?,
        );
    }
    rules.sort();
    rules.dedup();
    if rules.len() > ghinvite_core::email_domain::MAX_DOMAIN_RULES {
        return Err(INVALID.into());
    }
    Ok(rules)
}

pub fn parse_budget_share_pct(raw: &str) -> Result<Option<u32>, String> {
    let value = parse_optional_count(raw, BUDGET_SHARE_INVALID, BUDGET_SHARE_INVALID)?;
    if value.is_some_and(|pct| pct > 100) {
        return Err(BUDGET_SHARE_INVALID.into());
    }
    Ok(value)
}

/// Read a trimmed typed value as an optional positive whole number written in
/// ASCII digits: no sign, decimal point, exponent, or separators. The two
/// failure messages are a typo (`not_positive`) versus a number the guardrail
/// cannot represent (`too_large`).
fn parse_optional_count(
    raw: &str,
    not_positive: &str,
    too_large: &str,
) -> Result<Option<u32>, String> {
    let typed = raw.trim();
    if typed.is_empty() {
        return Ok(None);
    }
    if !typed.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(not_positive.to_string());
    }
    match typed.parse::<u32>() {
        Ok(0) => Err(not_positive.to_string()),
        Ok(count) => Ok(Some(count)),
        // All digits and non-empty, so the only way to fail is overflow.
        Err(_) => Err(too_large.to_string()),
    }
}

// --- validators -------------------------------------------------------------

/// Register every rule of the new invitation link form on `core`, once.
///
/// This is the single definition of the form's guardrails. The server calls it
/// on a fresh `FormCore` before `validate_for_submit`; the browser island
/// passes the same call to `FormConfig::register_core`. Both inputs are
/// snapshots the caller already holds: `available_repos` is the repository
/// list the form offered (the server is the authority on what is available),
/// and `now` anchors the expiration timestamp so it agrees with the instant
/// recorded as `created_at`. `now` is injected rather than read here so the
/// rules are pure and the browser can supply its own clock.
///
/// Rules, one sync field validator each, in form order:
/// - `description`: a [`Description`].
/// - `internal_note`: blank or an [`InternalNote`].
/// - `repo_grants`: one canonical supported permission for every selected repository;
///   the optional set-all helper never determines a creation grant.
/// - `max_uses`: blank or at least 1.
/// - `expires_in_days`: blank (14 days) or at least 1, and `now` must be advanceable by
///   that many days to a representable timestamp.
/// - `repo_ids`: at least one selected id must be an available repository,
///   every selected id must still be available, and the available
///   repositories they select must form a [`RepositoryScope`].
///
/// The `>= 1` checks on the numeric guardrails are deliberately duplicated
/// from the parsers: the parsers reject `0` at the text boundary (so the
/// island can show the error while typing and the raw POST never yields
/// `Some(0)`), and the validators reject it on the model so the typed form's
/// invariant holds however the model was built.
pub fn register_validators(
    core: &mut FormCore<CreateLinkForm, String>,
    available_repos: &[RepositoryChoice],
    now: DateTime<Utc>,
) {
    register_target_validators(
        core,
        available_repos,
        &OrganizationOffer::NotApplicable,
        now,
    );
}

pub fn register_target_validators(
    core: &mut FormCore<CreateLinkForm, String>,
    available_repos: &[RepositoryChoice],
    organization: &OrganizationOffer,
    now: DateTime<Utc>,
) {
    let fields = CreateLinkForm::fields();

    core.register_sync_field_validator(
        fields.access_lifetime_hours(),
        "access_lifetime",
        |raw, _| {
            access_lifetime_seconds(raw)
                .err()
                .map(str::to_owned)
                .into_iter()
                .collect()
        },
    );
    core.register_sync_field_validator(
        fields.expiration_timestamp(),
        "exact_expiration",
        move |raw, context| match exact_expiration(raw) {
            Ok(Some(date)) if date <= now || context.form().never_expires => {
                vec![EXACT_EXPIRATION_INVALID.into()]
            }
            Err(error) => vec![error.into()],
            _ => vec![],
        },
    );
    core.register_sync_field_validator(
        fields.invitation_lifetime_seconds(),
        "exact_lifetime",
        |raw, _| {
            exact_lifetime(raw)
                .err()
                .map(str::to_owned)
                .into_iter()
                .collect()
        },
    );

    core.register_sync_form_validator("NoGuardrail", |context| {
        let model = context.form();
        if !model.approval_required
            && model.max_uses.is_none()
            && model.never_expires
            && parse_email_domains(&model.email_domains).is_ok_and(|domains| domains.is_empty())
        {
            vec![dioform_core::FormValidationError::form(
                NO_GUARDRAIL.to_string(),
            )]
        } else {
            vec![]
        }
    });

    core.register_sync_field_validator(
        fields.budget_share_pct(),
        "budget_share_pct",
        |value, _| {
            if value.is_some_and(|pct| !(1..=100).contains(&pct)) {
                vec![BUDGET_SHARE_INVALID.to_string()]
            } else {
                vec![]
            }
        },
    );
    core.register_sync_field_validator(fields.email_domains(), "email_domains", |value, _| {
        parse_email_domains(value).err().into_iter().collect()
    });
    core.register_sync_field_validator(fields.description(), "description", |value, _| {
        description_problem(value)
            .map(str::to_string)
            .into_iter()
            .collect()
    });

    core.register_sync_field_validator(fields.internal_note(), "internal_note", |value, _| {
        internal_note_problem(value)
            .map(str::to_string)
            .into_iter()
            .collect()
    });

    core.register_sync_field_validator(
        fields.max_uses(),
        "max_uses",
        |value, context| match value {
            None if context.form().include_organization && !context.form().approval_required => {
                vec![ORGANIZATION_MAX_USES.into()]
            }
            Some(0) => vec![MAX_USES_NOT_POSITIVE.to_string()],
            _ => vec![],
        },
    );

    core.register_sync_field_validator(
        fields.expires_in_days(),
        "expires_in_days",
        move |value, _| match value {
            None => vec![],
            Some(0) => vec![EXPIRES_IN_DAYS_NOT_POSITIVE.to_string()],
            Some(days) if expiration_after(now, *days).is_none() => {
                vec![EXPIRES_IN_DAYS_TOO_FAR.to_string()]
            }
            Some(_) => vec![],
        },
    );

    core.register_sync_field_validator(
        fields.invitation_lifetime_hours(),
        "invitation_lifetime_hours",
        |value, _| {
            if value.is_some_and(|hours| !(1..=168).contains(&hours)) {
                vec![INVITATION_LIFETIME_INVALID.into()]
            } else {
                vec![]
            }
        },
    );
    core.register_sync_field_validator(
        fields.abuse_threshold_per_hour(),
        "abuse_threshold_per_hour",
        |value, _| {
            if *value == Some(0) {
                vec![ABUSE_THRESHOLD_INVALID.into()]
            } else {
                vec![]
            }
        },
    );

    let grant_repos = available_repos.to_vec();
    core.register_sync_field_validator(
        fields.repo_grants(),
        "repo_grants",
        move |grants, context| {
            let selected = &context.form().repo_ids;
            // Scope failures have their own diagnostic. Never derive a partial scope.
            if missing_repository_notice(selected, &grant_repos).is_some()
                || RepositoryScope::parse(repository_scope(selected, &grant_repos)).is_err()
            {
                return vec![];
            }
            repository_targets(selected, grants, &grant_repos)
                .err()
                .map(str::to_owned)
                .into_iter()
                .collect()
        },
    );

    let available = available_repos.to_vec();
    core.register_sync_field_validator(
        fields.repo_ids(),
        "repo_scope",
        move |selected, context| {
            if selected.is_empty() && context.form().include_organization {
                return vec![];
            }
            // A selection naming repositories that went away is reported before
            // core's rules, which only see the ones still available.
            if let Some(message) = missing_repository_notice(selected, &available)
                && !repository_scope(selected, &available).is_empty()
            {
                return vec![message];
            }
            match RepositoryScope::parse(repository_scope(selected, &available)) {
                Ok(_) => vec![],
                Err(RepositoryScopeError::Empty) => vec![REPO_SCOPE_REQUIRED.to_string()],
                Err(RepositoryScopeError::TooMany) => vec![REPO_SCOPE_TOO_MANY.to_string()],
                Err(RepositoryScopeError::Duplicate | RepositoryScopeError::InvalidRepository) => {
                    vec![REPO_SCOPE_UNUSABLE.to_string()]
                }
            }
        },
    );
    let offer = organization.clone();
    core.register_sync_field_validator(
        fields.include_organization(),
        "organization",
        move |selected, _| {
            if *selected && !matches!(offer, OrganizationOffer::Available(_)) {
                return vec![ORGANIZATION_UNAVAILABLE.into()];
            }
            vec![]
        },
    );
    let offer = organization.clone();
    core.register_sync_field_validator(fields.team_ids(), "teams", move |selected, context| {
        team_targets(context.form().include_organization, selected, &offer)
            .err()
            .map(str::to_owned)
            .into_iter()
            .collect()
    });
}

/// Why a typed description is not a usable one, if it is not.
fn description_problem(raw: &str) -> Option<&'static str> {
    Description::parse(raw).err().map(description_message)
}

/// The field-level message for a description the admin must correct.
pub(crate) fn description_message(error: DescriptionError) -> &'static str {
    match error {
        DescriptionError::Required => DESCRIPTION_REQUIRED,
        DescriptionError::MultiLine => DESCRIPTION_SINGLE_LINE,
        DescriptionError::TooLong => DESCRIPTION_TOO_LONG,
    }
}

/// Why a typed internal note is not a usable one, if it is not.
fn internal_note_problem(raw: &str) -> Option<&'static str> {
    InternalNote::parse(raw)
        .err()
        .map(|InternalNoteTooLong| INTERNAL_NOTE_TOO_LONG)
}

// --- shared derivations -----------------------------------------------------
pub fn missing_repository_notice(
    selected: &[u64],
    available: &[RepositoryChoice],
) -> Option<String> {
    let missing: Vec<_> = selected
        .iter()
        .filter(|id| !available.iter().any(|repo| repo.id == **id))
        .map(u64::to_string)
        .collect();
    (!missing.is_empty()).then(|| format!("Selected repositories are no longer available: {}. Review the remaining scope before creating the invitation link.", missing.join(", ")))
}

//
// The two rules whose outcome is also a value the server needs (an expiration
// timestamp, the repository scope) are written as derivations. The validators
// above only ask "did it produce something?"; the server calls the same
// functions to obtain the value, so the two can never disagree.

/// `now + days`, or `None` when the resulting instant is outside what chrono
/// can represent. There is no product maximum for expiration; the only upper
/// bound is a representable timestamp. Every step is checked; nothing here
/// can panic on adversarial input.
pub fn expiration_after(now: DateTime<Utc>, days: u32) -> Option<DateTime<Utc>> {
    let delta = TimeDelta::try_days(i64::from(days))?;
    now.checked_add_signed(delta)
}

/// The repository scope a selection resolves to: the selected ids intersected
/// with the available repositories, in available order, each repository at
/// most once.
///
/// The server is the authority on what is available, so an id the
/// installation does not expose (stale page, tampered request) is dropped
/// without comment and can never become a scope entry. A repository scope is
/// by definition non-empty; an empty result is the validator's failure case.
pub fn repository_scope(
    selected: &[u64],
    available: &[RepositoryChoice],
) -> Vec<InvitationLinkRepo> {
    available
        .iter()
        .filter(|repo| selected.contains(&repo.id))
        .map(|repo| InvitationLinkRepo {
            repo_id: repo.id,
            repo_full_name: repo.full_name.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioform_core::FieldPath;

    #[test]
    fn domain_rules_bound_unique_normalized_entries_and_raw_input() {
        assert_eq!(
            parse_email_domains("EXAMPLE.COM.,example.com, *.bücher.example")
                .unwrap()
                .len(),
            2
        );
        let duplicates = std::iter::repeat_n("example.com", 40)
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(parse_email_domains(&duplicates).unwrap().len(), 1);
        let rules = (0..33)
            .map(|n| format!("example{n}.com"))
            .collect::<Vec<_>>();
        assert!(parse_email_domains(&rules[..32].join(",")).is_ok());
        assert!(parse_email_domains(&rules.join(",")).is_err());
        assert!(parse_email_domains(&"x".repeat(4097)).is_err());
        assert!(parse_email_domains("https://example.com").is_err());
    }
    #[test]
    fn field_names_are_the_post_keys_the_route_accepts() {
        // The HTTP contract the route tests exercise; the form's `name`
        // attributes are rendered from these, so they must not drift.
        let fields = CreateLinkForm::fields();

        assert_eq!(fields.description().field_name(), "description");
        assert_eq!(fields.internal_note().field_name(), "internal_note");
        assert_eq!(fields.permission().field_name(), "permission");
        assert_eq!(fields.approval_required().field_name(), "approval_required");
        assert_eq!(fields.max_uses().field_name(), "max_uses");
        assert_eq!(fields.expires_in_days().field_name(), "expires_in_days");
        assert_eq!(fields.repo_ids().field_name(), "repo_ids");
    }

    #[test]
    fn to_model_carries_text_verbatim_and_blanks_an_unparsable_guardrail() {
        let values = LinkFormValues {
            access_lifetime_hours: String::new(),
            email_domains: String::new(),
            expiration_timestamp: String::new(),
            invitation_lifetime_seconds: String::new(),
            include_organization: false,
            selected_team_ids: vec![],
            invitation_lifetime_hours: "168".into(),
            abuse_threshold_per_hour: String::new(),
            budget_share_pct: "50".into(),
            never_expires: false,
            description: "  AI  ".into(),
            permission: "owner".into(),
            approval_required: true,
            max_uses: "abc".into(),
            expires_in_days: " 45 ".into(),
            internal_note: "note".into(),
            selected_repo_ids: vec![999, 10],
            repo_grants: vec!["999:push".into(), "10:push".into()],
            errors: LinkFormErrors {
                description: Some("ignored".into()),
                ..LinkFormErrors::default()
            },
        };

        let (model, errors) = values.to_model();

        assert_eq!(
            model,
            CreateLinkForm {
                access_lifetime_hours: String::new(),
                email_domains: String::new(),
                expiration_timestamp: String::new(),
                invitation_lifetime_seconds: String::new(),
                include_organization: false,
                team_ids: vec![],
                invitation_lifetime_hours: Some(168),
                abuse_threshold_per_hour: None,
                budget_share_pct: Some(50),
                never_expires: false,
                description: "  AI  ".into(),
                internal_note: "note".into(),
                permission: "owner".into(),
                approval_required: true,
                max_uses: None,
                expires_in_days: Some(45),
                repo_ids: vec![999, 10],
                repo_grants: vec!["999:push".into(), "10:push".into()],
            }
        );
        assert_eq!(
            errors,
            LinkFormErrors {
                summary: vec![SUMMARY_MESSAGE.to_string()],
                max_uses: Some(MAX_USES_NOT_POSITIVE.to_string()),
                ..LinkFormErrors::default()
            }
        );
    }

    // --- parsers -----------------------------------------------------------

    #[test]
    fn parse_max_uses_blank_means_unlimited() {
        for raw in ["", "   ", "\t\n"] {
            assert_eq!(parse_max_uses(raw), Ok(None), "max_uses={raw:?}");
        }
    }

    #[test]
    fn parse_max_uses_accepts_positive_whole_numbers_and_trims() {
        assert_eq!(parse_max_uses("1"), Ok(Some(1)));
        assert_eq!(parse_max_uses(" 25 "), Ok(Some(25)));
        assert_eq!(parse_max_uses("05"), Ok(Some(5)));
        assert_eq!(parse_max_uses("4294967295"), Ok(Some(u32::MAX)));
    }

    #[test]
    fn parse_max_uses_rejects_zero_negative_decimal_and_text() {
        for raw in [
            "0", "00", "-1", "-0", "1.5", "2.0", "abc", "5x", "+5", "1e3", "1,000", " 5 x",
        ] {
            assert_eq!(
                parse_max_uses(raw),
                Err("Max use must be a whole number of 1 or more.".to_string()),
                "max_uses={raw:?}"
            );
        }
    }

    #[test]
    fn parse_max_uses_rejects_values_too_large_to_store() {
        for raw in ["4294967296", "99999999999999999999999"] {
            assert_eq!(
                parse_max_uses(raw),
                Err("Max use is too large. Use a smaller number, or leave it blank for unlimited invitation requests.".to_string()),
                "max_uses={raw:?}"
            );
        }
    }

    #[test]
    fn parse_expires_in_days_blank_means_no_expiration() {
        for raw in ["", "   ", "\t\n"] {
            assert_eq!(
                parse_expires_in_days(raw),
                Ok(None),
                "expires_in_days={raw:?}"
            );
        }
    }

    #[test]
    fn parse_expires_in_days_accepts_positive_whole_numbers_and_trims() {
        assert_eq!(parse_expires_in_days("1"), Ok(Some(1)));
        assert_eq!(parse_expires_in_days(" 45 "), Ok(Some(45)));
        assert_eq!(parse_expires_in_days("36500"), Ok(Some(36500)));
        // Parses fine; whether `now` can be advanced that far is the
        // validator's call, not the parser's.
        assert_eq!(parse_expires_in_days("100000000"), Ok(Some(100_000_000)));
    }

    #[test]
    fn parse_expires_in_days_rejects_zero_negative_decimal_and_text() {
        for raw in [
            "0", "00", "-1", "-30", "1.5", "30.0", "abc", "30d", "+30", "1e2", "1,000",
        ] {
            assert_eq!(
                parse_expires_in_days(raw),
                Err("Expiration must be a whole number of days, 1 or more.".to_string()),
                "expires_in_days={raw:?}"
            );
        }
    }

    #[test]
    fn parse_expires_in_days_rejects_digit_strings_past_the_storage_type() {
        for raw in ["4294967296", "9223372036854775808", "18446744073709551616"] {
            assert_eq!(
                parse_expires_in_days(raw),
                Err(
                    "Expiration is too far in the future. Use fewer days, or select Never expires."
                        .to_string()
                ),
                "expires_in_days={raw:?}"
            );
        }
    }

    // --- validators (through a real FormCore run) --------------------------

    fn valid_model() -> CreateLinkForm {
        CreateLinkForm {
            access_lifetime_hours: String::new(),
            email_domains: String::new(),
            expiration_timestamp: String::new(),
            invitation_lifetime_seconds: String::new(),
            include_organization: false,
            team_ids: vec![],
            invitation_lifetime_hours: Some(168),
            abuse_threshold_per_hour: None,
            budget_share_pct: None,
            never_expires: false,
            description: "AI coding workshop".into(),
            internal_note: "  Keep this note  ".into(),
            permission: "push".into(),
            approval_required: true,
            max_uses: Some(7),
            expires_in_days: Some(45),
            repo_ids: vec![10],
            repo_grants: vec!["10:push".into()],
        }
    }

    fn repo(id: u64, full_name: &str) -> RepositoryChoice {
        RepositoryChoice {
            id,
            full_name: full_name.into(),
        }
    }

    /// The available repositories the installation exposes, in the order
    /// GitHub returned them (which is the order the form lists them).
    fn available_repos() -> Vec<RepositoryChoice> {
        vec![
            repo(10, "acme/api"),
            repo(11, "acme/web"),
            repo(12, "acme/docs"),
        ]
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-04T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// Run a submit validation with the registered rules and flatten the
    /// errors to `(field, message)`; a `None` field is a form-level error.
    fn submit_errors(
        model: CreateLinkForm,
        available: &[RepositoryChoice],
    ) -> Vec<(Option<FieldIdentity>, String)> {
        let mut core = FormCore::new(model);
        register_validators(&mut core, available, now());
        let valid = core.validate_for_submit();
        let errors: Vec<_> = core
            .validation_errors()
            .iter()
            .map(|error| (error.field_identity(), error.error().clone()))
            .collect();
        assert_eq!(
            valid,
            errors.is_empty(),
            "validate_for_submit and validation_errors disagree"
        );
        errors
    }

    /// Exactly one field failed, and it is `path`; returns its message.
    fn only_error_on<Value>(
        model: CreateLinkForm,
        path: FieldPath<CreateLinkForm, Value>,
    ) -> String {
        let errors = submit_errors(model, &available_repos());
        match errors.as_slice() {
            [(Some(field), message)] if *field == path.identity() => message.clone(),
            other => panic!(
                "expected exactly one error on {:?}, got {other:?}",
                path.field_name()
            ),
        }
    }

    #[test]
    fn automatic_approval_requires_at_least_one_guardrail() {
        let mut model = CreateLinkForm {
            never_expires: true,
            approval_required: false,
            max_uses: None,
            expires_in_days: None,
            ..valid_model()
        };
        assert_eq!(
            submit_errors(model.clone(), &available_repos()),
            vec![(None, NO_GUARDRAIL.to_string())]
        );
        model.approval_required = true;
        assert!(submit_errors(model.clone(), &available_repos()).is_empty());
        model.approval_required = false;
        model.never_expires = false;
        model.expires_in_days = Some(14);
        assert!(submit_errors(model.clone(), &available_repos()).is_empty());
        model.never_expires = true;
        model.expires_in_days = None;
        model.max_uses = Some(1);
        assert!(submit_errors(model, &available_repos()).is_empty());
    }

    #[test]
    fn valid_model_passes_every_rule() {
        assert_eq!(submit_errors(valid_model(), &available_repos()), vec![]);
    }

    #[test]
    fn description_is_required() {
        for raw in ["", "   ", "\t\n"] {
            let model = CreateLinkForm {
                description: raw.into(),
                ..valid_model()
            };
            assert_eq!(
                only_error_on(model, CreateLinkForm::fields().description()),
                "Description is required. Use short, single-line admin-only context for this invitation link.",
                "description={raw:?}"
            );
        }
    }

    #[test]
    fn description_must_be_a_single_line() {
        for raw in ["AI\nworkshop", "AI workshop\n", "\rAI workshop"] {
            let model = CreateLinkForm {
                description: raw.into(),
                ..valid_model()
            };
            assert_eq!(
                only_error_on(model, CreateLinkForm::fields().description()),
                "Description must be a single line.",
                "description={raw:?}"
            );
        }
    }

    #[test]
    fn description_is_limited_to_120_characters_after_trimming() {
        let too_long = CreateLinkForm {
            description: "x".repeat(121),
            ..valid_model()
        };
        assert_eq!(
            only_error_on(too_long, CreateLinkForm::fields().description()),
            "Description must be 120 characters or fewer."
        );

        let padded_limit = CreateLinkForm {
            description: format!("  {}  ", "é".repeat(120)),
            ..valid_model()
        };
        assert_eq!(submit_errors(padded_limit, &available_repos()), vec![]);
    }

    #[test]
    fn internal_note_is_limited_to_16384_bytes_after_trimming() {
        let padded_limit = CreateLinkForm {
            internal_note: format!("  {}\n", "x".repeat(16_384)),
            ..valid_model()
        };
        assert_eq!(submit_errors(padded_limit, &available_repos()), vec![]);

        let too_long = CreateLinkForm {
            internal_note: "x".repeat(16_385),
            ..valid_model()
        };
        assert_eq!(
            only_error_on(too_long, CreateLinkForm::fields().internal_note()),
            "Internal note must be 16384 UTF-8 bytes or fewer."
        );
    }

    #[test]
    fn limit_messages_name_the_core_limits() {
        use ghinvite_core::invitation_link::{DESCRIPTION_MAX_CHARS, INTERNAL_NOTE_MAX_BYTES};
        assert!(DESCRIPTION_TOO_LONG.contains(&DESCRIPTION_MAX_CHARS.to_string()));
        assert!(INTERNAL_NOTE_TOO_LONG.contains(&INTERNAL_NOTE_MAX_BYTES.to_string()));
        assert!(
            REPO_SCOPE_TOO_MANY
                .contains(&ghinvite_core::invitation_link::REPOSITORY_SCOPE_MAX_REPOS.to_string())
        );
    }

    #[test]
    fn permission_accepts_every_supported_level() {
        for raw in ["pull", "triage", "push", "maintain", "admin"] {
            let model = CreateLinkForm {
                permission: raw.into(),
                ..valid_model()
            };
            assert_eq!(
                submit_errors(model, &available_repos()),
                vec![],
                "permission={raw:?}"
            );
        }
    }

    #[test]
    fn repository_grant_rejects_tampered_values() {
        // Unknown names, GitHub-adjacent names that are not collaborator
        // levels, casing/whitespace variants (the select submits exact
        // lowercase values, so anything else did not come from the form), and
        // blank (which is also what a missing POST key becomes).
        for raw in [
            "owner", "write", "read", "Push", "PUSH", " push", "push ", "", "  ", "push\n",
        ] {
            let model = CreateLinkForm {
                repo_grants: vec![format!("10:{raw}")],
                ..valid_model()
            };
            assert_eq!(
                only_error_on(model, CreateLinkForm::fields().repo_grants()),
                REPO_GRANT_REQUIRED,
                "permission={raw:?}"
            );
        }
    }

    #[test]
    fn max_uses_blank_or_positive_passes() {
        for max_uses in [None, Some(1), Some(25), Some(u32::MAX)] {
            let model = CreateLinkForm {
                max_uses,
                ..valid_model()
            };
            assert_eq!(
                submit_errors(model, &available_repos()),
                vec![],
                "max_uses={max_uses:?}"
            );
        }
    }

    #[test]
    fn max_uses_zero_is_rejected_by_the_model_rule_too() {
        // The parser never produces `Some(0)`, but the model rule holds
        // regardless of how the model was built.

        let model = CreateLinkForm {
            max_uses: Some(0),
            ..valid_model()
        };
        assert_eq!(
            only_error_on(model, CreateLinkForm::fields().max_uses()),
            "Max use must be a whole number of 1 or more."
        );
    }

    #[test]
    fn expires_in_days_blank_or_safe_passes() {
        for expires_in_days in [None, Some(1), Some(45), Some(36500)] {
            let model = CreateLinkForm {
                expires_in_days,
                ..valid_model()
            };
            assert_eq!(
                submit_errors(model, &available_repos()),
                vec![],
                "expires_in_days={expires_in_days:?}"
            );
        }
    }

    #[test]
    fn expires_in_days_zero_is_rejected_by_the_model_rule_too() {
        let model = CreateLinkForm {
            expires_in_days: Some(0),
            ..valid_model()
        };
        assert_eq!(
            only_error_on(model, CreateLinkForm::fields().expires_in_days()),
            "Expiration must be a whole number of days, 1 or more."
        );
    }

    #[test]
    fn expires_in_days_rejects_values_that_cannot_produce_a_timestamp() {
        // Past chrono's representable calendar, and the largest the storage
        // type allows.
        for days in [100_000_000, u32::MAX] {
            let model = CreateLinkForm {
                expires_in_days: Some(days),
                ..valid_model()
            };
            assert_eq!(
                only_error_on(model, CreateLinkForm::fields().expires_in_days()),
                "Expiration is too far in the future. Use fewer days, or select Never expires.",
                "expires_in_days={days}"
            );
        }
    }

    #[test]
    fn repo_scope_requires_at_least_one_available_repository() {
        for repo_ids in [vec![], vec![999], vec![999, 1000]] {
            let model = CreateLinkForm {
                repo_grants: vec!["10:pull".into(), "11:pull".into(), "12:pull".into()],
                repo_ids: repo_ids.clone(),
                ..valid_model()
            };
            assert_eq!(
                only_error_on(model, CreateLinkForm::fields().repo_ids()),
                "Repository scope is required. Select at least one available repository for this invitation link.",
                "repo_ids={repo_ids:?}"
            );
        }
    }

    #[test]
    fn repo_scope_rejects_any_selection_when_no_repositories_are_available() {
        let errors = submit_errors(valid_model(), &[]);

        assert_eq!(
            errors,
            vec![(
                Some(CreateLinkForm::fields().repo_ids().identity()),
                "Repository scope is required. Select at least one available repository for this invitation link.".to_string()
            )]
        );
    }

    #[test]
    fn repo_scope_accepts_available_repositories_and_duplicates() {
        for repo_ids in [vec![11], vec![12, 10, 12, 11, 10]] {
            let model = CreateLinkForm {
                repo_grants: vec!["10:pull".into(), "11:pull".into(), "12:pull".into()],
                repo_ids: repo_ids.clone(),
                ..valid_model()
            };
            assert_eq!(
                submit_errors(model, &available_repos()),
                vec![],
                "repo_ids={repo_ids:?}"
            );
        }
    }

    #[test]
    fn repo_scope_is_limited_to_100_repositories() {
        let many: Vec<_> = (1..=101)
            .map(|id| repo(id, &format!("acme/repo-{id}")))
            .collect();
        let selecting = |count: u64| CreateLinkForm {
            repo_ids: (1..=count).collect(),
            repo_grants: (1..=count).map(|id| format!("{id}:pull")).collect(),
            ..valid_model()
        };

        assert_eq!(submit_errors(selecting(100), &many), vec![]);
        assert_eq!(
            submit_errors(selecting(101), &many),
            vec![(
                Some(CreateLinkForm::fields().repo_ids().identity()),
                "Repository scope is limited to 100 repositories. Select fewer repositories, or create another invitation link for the rest.".to_string()
            )]
        );
    }

    #[test]
    fn every_rule_can_fail_at_once_and_each_error_is_on_its_own_field() {
        let model = CreateLinkForm {
            description: "".into(),
            internal_note: "x".repeat(16_385),
            permission: "owner".into(),
            max_uses: Some(0),
            expires_in_days: Some(0),
            repo_ids: vec![],
            repo_grants: vec![],
            ..valid_model()
        };

        let errors = submit_errors(model, &available_repos());
        let fields = CreateLinkForm::fields();

        let failed: Vec<Option<FieldIdentity>> =
            errors.iter().map(|(field, _)| field.clone()).collect();
        assert_eq!(
            failed,
            vec![
                Some(fields.description().identity()),
                Some(fields.internal_note().identity()),
                Some(fields.max_uses().identity()),
                Some(fields.expires_in_days().identity()),
                Some(fields.repo_ids().identity()),
            ],
            "one error per failing field, in registration order, no form-level errors"
        );
    }

    // --- shared derivations -----------------------------------------------

    #[test]
    fn expiration_after_advances_now_by_whole_days_or_reports_none() {
        assert_eq!(
            expiration_after(now(), 45).unwrap().to_rfc3339(),
            "2026-06-18T12:00:00+00:00"
        );
        assert_eq!(
            expiration_after(now(), 36500).unwrap().to_rfc3339(),
            "2126-04-10T12:00:00+00:00"
        );
        assert_eq!(expiration_after(now(), 100_000_000), None);
        assert_eq!(expiration_after(now(), u32::MAX), None);
    }

    #[test]
    fn repository_scope_follows_available_order_drops_unknown_and_ignores_duplicates() {
        let scope = |ids: &[u64]| {
            repository_scope(ids, &available_repos())
                .into_iter()
                .map(|repo| (repo.repo_id, repo.repo_full_name))
                .collect::<Vec<_>>()
        };

        assert_eq!(scope(&[11]), vec![(11, "acme/web".to_string())]);
        assert_eq!(scope(&[999, 10, 1000]), vec![(10, "acme/api".to_string())]);
        assert_eq!(
            scope(&[12, 10, 12, 11, 10]),
            vec![
                (10, "acme/api".to_string()),
                (11, "acme/web".to_string()),
                (12, "acme/docs".to_string()),
            ]
        );
        assert_eq!(scope(&[]), vec![]);
        assert_eq!(repository_scope(&[10], &[]), vec![]);
    }

    // --- LinkFormErrors ----------------------------------------------------

    #[test]
    fn attaching_a_field_error_fills_its_slot_and_adds_the_summary_once() {
        let fields = CreateLinkForm::fields();
        let mut errors = LinkFormErrors::default();
        assert!(errors.is_empty());

        errors.attach(Some(&fields.description().identity()), "d".to_string());
        errors.attach(Some(&fields.internal_note().identity()), "n".to_string());
        errors.attach(Some(&fields.permission().identity()), "p".to_string());
        errors.attach(Some(&fields.max_uses().identity()), "m".to_string());
        errors.attach(Some(&fields.expires_in_days().identity()), "e".to_string());
        errors.attach(Some(&fields.repo_ids().identity()), "r".to_string());

        assert_eq!(
            errors,
            LinkFormErrors {
                access_lifetime_hours: None,
                email_domains: None,
                expiration_timestamp: None,
                invitation_lifetime_seconds: None,
                organization: None,
                teams: None,
                invitation_lifetime_hours: None,
                abuse_threshold_per_hour: None,
                budget_share_pct: None,
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string()
                ],
                description: Some("d".to_string()),
                internal_note: Some("n".to_string()),
                permission: Some("p".to_string()),
                max_uses: Some("m".to_string()),
                expires_in_days: Some("e".to_string()),
                repo_scope: Some("r".to_string()),
                repo_grants: None,
            }
        );
        assert!(!errors.is_empty());
    }

    #[test]
    fn first_message_per_field_wins() {
        let field = CreateLinkForm::fields().max_uses().identity();
        let mut errors = LinkFormErrors::default();

        errors.attach(Some(&field), "first".to_string());
        errors.attach(Some(&field), "second".to_string());

        assert_eq!(errors.max_uses.as_deref(), Some("first"));
        assert_eq!(errors.summary.len(), 1);
    }

    #[test]
    fn form_level_and_unmapped_field_errors_land_in_the_summary() {
        let mut errors = LinkFormErrors::default();

        errors.attach(None, "whole form".to_string());
        errors.attach(
            Some(&CreateLinkForm::fields().approval_required().identity()),
            "approval".to_string(),
        );

        assert_eq!(
            errors.summary,
            vec![
                "Fix the highlighted fields before creating this invitation link.".to_string(),
                "whole form".to_string(),
                "approval".to_string(),
            ]
        );
        assert_eq!(errors.description, None);
        assert_eq!(errors.repo_scope, None);
    }

    #[test]
    fn retiring_the_last_field_message_takes_the_summary_line_with_it() {
        let fields = CreateLinkForm::fields();
        let mut errors = LinkFormErrors::default();
        errors.attach(Some(&fields.description().identity()), "d".to_string());
        errors.attach(Some(&fields.max_uses().identity()), "m".to_string());

        errors.retire(&fields.description().identity());

        assert_eq!(errors.description, None);
        assert_eq!(errors.max_uses.as_deref(), Some("m"));
        assert_eq!(errors.summary, vec![SUMMARY_MESSAGE.to_string()]);

        errors.retire(&fields.max_uses().identity());

        assert!(errors.is_empty());
    }

    #[test]
    fn retiring_every_field_message_keeps_a_form_level_one_and_its_summary_line() {
        let fields = CreateLinkForm::fields();
        let mut errors = LinkFormErrors::default();
        errors.attach(Some(&fields.description().identity()), "d".to_string());
        errors.attach(None, "whole form".to_string());

        errors.retire(&fields.description().identity());

        assert_eq!(
            errors.summary,
            vec![SUMMARY_MESSAGE.to_string(), "whole form".to_string()]
        );
        assert!(!errors.is_empty());
    }

    #[test]
    fn retiring_a_field_without_a_slot_or_a_message_changes_nothing() {
        let fields = CreateLinkForm::fields();
        let mut errors = LinkFormErrors::default();
        errors.attach(Some(&fields.max_uses().identity()), "m".to_string());
        let before = errors.clone();

        errors.retire(&fields.approval_required().identity());
        errors.retire(&fields.description().identity());

        assert_eq!(errors, before);
    }

    // --- island props ------------------------------------------------------

    fn island_props(description: &str) -> LinkFormIslandProps {
        LinkFormIslandProps {
            access_removal_enabled: false,
            eligibility: Default::default(),
            organization: Default::default(),
            csrf_token: None,
            action: "/console/accounts/acme/links".to_string(),
            values: LinkFormValues {
                access_lifetime_hours: String::new(),
                email_domains: String::new(),
                expiration_timestamp: String::new(),
                invitation_lifetime_seconds: String::new(),
                include_organization: false,
                selected_team_ids: vec![],
                invitation_lifetime_hours: "168".into(),
                abuse_threshold_per_hour: String::new(),
                budget_share_pct: "50".into(),
                never_expires: false,
                description: description.to_string(),
                permission: "push".to_string(),
                approval_required: true,
                max_uses: "7".to_string(),
                expires_in_days: "45".to_string(),
                internal_note: "Keep this note".to_string(),
                selected_repo_ids: vec![10],
                repo_grants: vec!["10:push".into()],
                errors: LinkFormErrors {
                    summary: vec![SUMMARY_MESSAGE.to_string()],
                    description: Some(DESCRIPTION_REQUIRED.to_string()),
                    ..LinkFormErrors::default()
                },
            },
            repos: available_repos(),
            picker: RepositoryPicker {
                query: "acme".into(),
                page: 2,
                pages: 3,
                matches: 120,
                pinned: 1,
                ..RepositoryPicker::default()
            },
            now: now(),
        }
    }

    #[test]
    fn island_props_json_carries_action_values_errors_repos_and_now() {
        let json = island_props("AI coding workshop").script_json();

        assert!(json.contains("\"action\":\"/console/accounts/acme/links\""));
        assert!(json.contains("\"description\":\"AI coding workshop\""));
        assert!(json.contains("\"permission\":\"push\""));
        assert!(json.contains("\"approval_required\":true"));
        assert!(json.contains("\"max_uses\":\"7\""));
        assert!(json.contains("\"expires_in_days\":\"45\""));
        assert!(json.contains("\"internal_note\":\"Keep this note\""));
        assert!(json.contains("\"selected_repo_ids\":[10]"));
        assert!(json.contains(
            "\"summary\":[\"Fix the highlighted fields before creating this invitation link.\"]"
        ));
        assert!(json.contains("\"repo_scope\":null"));
        assert!(json.contains("{\"id\":10,\"full_name\":\"acme/api\"}"));
        assert!(json.contains("{\"id\":12,\"full_name\":\"acme/docs\"}"));
        assert!(json.contains("\"now\":\"2026-05-04T12:00:00Z\""));
        assert!(json.contains("\"picker\":{\"query\":\"acme\",\"page\":2,\"pages\":3,"));
    }

    #[test]
    fn island_props_without_a_picker_list_one_unfiltered_page() {
        // A blob written before the picker existed still mounts.
        let mut json: serde_json::Value =
            serde_json::from_str(&island_props("Workshop").script_json()).unwrap();
        json.as_object_mut().unwrap().remove("picker");
        let props: LinkFormIslandProps = serde_json::from_value(json).unwrap();
        assert_eq!(props.picker, RepositoryPicker::default());
        assert_eq!((props.picker.page, props.picker.pages), (1, 1));
    }

    #[test]
    fn picker_query_is_one_bounded_line_matched_case_insensitively() {
        assert_eq!(RepositoryPicker::normalize_query("api\r\nweb"), "apiweb");
        assert_eq!(
            RepositoryPicker::normalize_query(&"é".repeat(150))
                .chars()
                .count(),
            REPOSITORY_QUERY_MAX_CHARS
        );
        assert!(RepositoryPicker::matches_query("", "acme/api"));
        assert!(RepositoryPicker::matches_query("  ACME/A ", "acme/api"));
        assert!(RepositoryPicker::matches_query("pi", "Acme/API"));
        assert!(!RepositoryPicker::matches_query("web", "acme/api"));
    }

    #[test]
    fn island_props_json_round_trips() {
        let props = island_props("AI coding workshop");

        let back: LinkFormIslandProps = serde_json::from_str(&props.script_json()).unwrap();

        assert_eq!(back, props);
    }

    #[test]
    fn island_props_json_cannot_close_the_script_block() {
        // A description is admin-typed text that ends up inside a <script>
        // element; the HTML parser would end the element at `</script` no
        // matter how JSON quotes it.
        let props = island_props("</script><script>alert(1)</script><!--");

        let json = props.script_json();

        assert!(!json.contains('<'), "raw '<' in script block: {json}");
        assert!(json.contains("\\u003c/script>\\u003cscript>alert(1)\\u003c/script>\\u003c!--"));
        // Still the same value once parsed as JSON.
        let back: LinkFormIslandProps = serde_json::from_str(&json).unwrap();
        assert_eq!(back, props);
    }

    #[test]
    fn json_for_script_block_escapes_every_less_than_sign() {
        assert_eq!(json_for_script_block("a<b<c"), "\"a\\u003cb\\u003cc\"");
        assert_eq!(
            json_for_script_block(&vec!["<", ">"]),
            "[\"\\u003c\",\">\"]"
        );
        assert_eq!(json_for_script_block(&7u32), "7");
    }
    #[test]
    fn budget_share_parser_and_model_enforce_bounds() {
        for (raw, expected) in [
            ("", None),
            ("1", Some(1)),
            ("50", Some(50)),
            ("100", Some(100)),
        ] {
            assert_eq!(parse_budget_share_pct(raw).unwrap(), expected);
        }
        for raw in ["0", "101", "-1", "1.5", "9999999999999"] {
            assert!(parse_budget_share_pct(raw).is_err());
        }
        for pct in [0, 101, u32::MAX] {
            let model = CreateLinkForm {
                budget_share_pct: Some(pct),
                ..valid_model()
            };
            assert_eq!(
                only_error_on(model, CreateLinkForm::fields().budget_share_pct()),
                BUDGET_SHARE_INVALID
            );
        }
    }

    #[test]
    fn typed_lifetime_and_threshold_cannot_bypass_text_validation() {
        let model = CreateLinkForm {
            invitation_lifetime_hours: Some(169),
            ..valid_model()
        };
        assert_eq!(
            only_error_on(model, CreateLinkForm::fields().invitation_lifetime_hours()),
            INVITATION_LIFETIME_INVALID
        );

        let model = CreateLinkForm {
            abuse_threshold_per_hour: Some(0),
            ..valid_model()
        };
        assert_eq!(
            only_error_on(model, CreateLinkForm::fields().abuse_threshold_per_hour()),
            ABUSE_THRESHOLD_INVALID
        );
    }
}
