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
use ghinvite_core::invitation_link::REPOSITORY_SCOPE_MAX_REPOS;
use ghinvite_core::{
    Description, DescriptionError, InternalNote, InternalNoteTooLong, InvitationLinkRepo,
    Permission,
};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// The new invitation link form as the admin fills it in.
///
/// Text fields hold exactly what was typed (untrimmed); normalisation happens
/// when the validated form becomes creation data. `permission` stays a string
/// because the `<select>` binds strings; validation maps it to a
/// [`ghinvite_core::Permission`]. The numeric guardrails are already parsed:
/// `None` is a deliberately blank field (unlimited / no expiration).
#[derive(Clone, Debug, PartialEq, Form)]
#[form(crate = "::dioform_core")]
pub struct CreateLinkForm {
    pub description: String,
    pub internal_note: String,
    pub permission: String,
    pub approval_required: bool,
    pub max_uses: Option<u32>,
    pub expires_in_days: Option<u32>,
    pub repo_ids: Vec<u64>,
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
    /// Shown in the alert above the form: the summary line, followed by any
    /// form-level messages that have no control of their own.
    pub summary: Vec<String>,
    pub description: Option<String>,
    pub internal_note: Option<String>,
    pub permission: Option<String>,
    pub max_uses: Option<String>,
    pub expires_in_days: Option<String>,
    /// Section-level error for the repository checkbox group (`repo_ids`).
    pub repo_scope: Option<String>,
}

/// The new invitation link form as the view renders it: the submitted values
/// verbatim (so an admin sees exactly what they typed when correcting a
/// mistake) plus the errors to show next to each control.
///
/// Serialised into the island's props blob, so the browser starts from the
/// same values and server-side errors the page shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkFormValues {
    pub description: String,
    pub permission: String,
    pub approval_required: bool,
    pub max_uses: String,
    pub expires_in_days: String,
    pub internal_note: String,
    pub selected_repo_ids: Vec<u64>,
    pub errors: LinkFormErrors,
}

impl Default for LinkFormValues {
    fn default() -> Self {
        Self {
            description: String::new(),
            permission: "pull".into(),
            approval_required: false,
            max_uses: String::new(),
            expires_in_days: "30".into(),
            internal_note: String::new(),
            selected_repo_ids: Vec::new(),
            errors: LinkFormErrors::default(),
        }
    }
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
    #[props(default)]
    pub csrf_token: Option<String>,
    /// `action` of the form; the route that handles the POST.
    pub action: String,
    pub values: LinkFormValues,
    /// Available repositories, in the order the account makes them available.
    pub repos: Vec<RepositoryChoice>,
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
        self.description.is_some()
            || self.internal_note.is_some()
            || self.permission.is_some()
            || self.max_uses.is_some()
            || self.expires_in_days.is_some()
            || self.repo_scope.is_some()
    }

    /// The slot a model field's errors render in, if it has one.
    fn slot_mut(&mut self, field: &FieldIdentity) -> Option<&mut Option<String>> {
        let fields = CreateLinkForm::fields();
        if *field == fields.description().identity() {
            Some(&mut self.description)
        } else if *field == fields.internal_note().identity() {
            Some(&mut self.internal_note)
        } else if *field == fields.permission().identity() {
            Some(&mut self.permission)
        } else if *field == fields.max_uses().identity() {
            Some(&mut self.max_uses)
        } else if *field == fields.expires_in_days().identity() {
            Some(&mut self.expires_in_days)
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
pub const PERMISSION_UNSUPPORTED: &str =
    "Choose a supported permission level: pull, triage, push, maintain, or admin.";
pub const MAX_USES_NOT_POSITIVE: &str = "Max use must be a whole number of 1 or more.";
pub const MAX_USES_TOO_LARGE: &str = "Max use is too large. Use a smaller number, or leave it blank for unlimited invitation requests.";
pub const EXPIRES_IN_DAYS_NOT_POSITIVE: &str =
    "Expiration must be a whole number of days, 1 or more.";
pub const EXPIRES_IN_DAYS_TOO_FAR: &str =
    "Expiration is too far in the future. Use fewer days, or leave it blank for no expiration.";
pub const REPO_SCOPE_REQUIRED: &str = "Repository scope is required. Select at least one available repository for this invitation link.";
pub const REPO_SCOPE_TOO_MANY: &str = "Repository scope is limited to 100 repositories. Select fewer repositories, or create another invitation link for the rest.";

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

/// Parse the typed expiration: blank means no expiration (`None`); otherwise a
/// positive whole number of days. Whether `now` can actually be advanced that
/// far is the validator's job (it needs `now`); the parser only guards the
/// storage type.
pub fn parse_expires_in_days(raw: &str) -> Result<Option<u32>, String> {
    parse_optional_count(raw, EXPIRES_IN_DAYS_NOT_POSITIVE, EXPIRES_IN_DAYS_TOO_FAR)
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
/// - `permission`: exactly one of the supported collaborator levels.
/// - `max_uses`: blank or at least 1.
/// - `expires_in_days`: blank or at least 1, and `now` must be advanceable by
///   that many days to a representable timestamp.
/// - `repo_ids`: at least one selected id must be an available repository,
///   every selected id must still be available, and at most
///   [`REPOSITORY_SCOPE_MAX_REPOS`] may be selected.
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
    let fields = CreateLinkForm::fields();

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

    // The `<select>` only ever submits the exact lowercase level names, so
    // nothing is trimmed or case-folded: any other string — including the
    // empty string a missing POST key becomes — did not come from the form and
    // is treated as tampering, not a typo.
    core.register_sync_field_validator(fields.permission(), "permission", |value, _| {
        match Permission::from_str(value) {
            Ok(_) => vec![],
            Err(_) => vec![PERMISSION_UNSUPPORTED.to_string()],
        }
    });

    core.register_sync_field_validator(fields.max_uses(), "max_uses", |value, _| match value {
        Some(0) => vec![MAX_USES_NOT_POSITIVE.to_string()],
        _ => vec![],
    });

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

    let available = available_repos.to_vec();
    core.register_sync_field_validator(fields.repo_ids(), "repo_scope", move |selected, _| {
        let scope = repository_scope(selected, &available);
        if scope.is_empty() {
            vec![REPO_SCOPE_REQUIRED.to_string()]
        } else if let Some(message) = missing_repository_notice(selected, &available) {
            vec![message]
        } else if scope.len() > REPOSITORY_SCOPE_MAX_REPOS {
            vec![REPO_SCOPE_TOO_MANY.to_string()]
        } else {
            vec![]
        }
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
                Err("Expiration is too far in the future. Use fewer days, or leave it blank for no expiration.".to_string()),
                "expires_in_days={raw:?}"
            );
        }
    }

    // --- validators (through a real FormCore run) --------------------------

    fn valid_model() -> CreateLinkForm {
        CreateLinkForm {
            description: "AI coding workshop".into(),
            internal_note: "  Keep this note  ".into(),
            permission: "push".into(),
            approval_required: true,
            max_uses: Some(7),
            expires_in_days: Some(45),
            repo_ids: vec![10],
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
        assert!(REPO_SCOPE_TOO_MANY.contains(&REPOSITORY_SCOPE_MAX_REPOS.to_string()));
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
    fn permission_rejects_tampered_values() {
        // Unknown names, GitHub-adjacent names that are not collaborator
        // levels, casing/whitespace variants (the select submits exact
        // lowercase values, so anything else did not come from the form), and
        // blank (which is also what a missing POST key becomes).
        for raw in [
            "owner", "write", "read", "Push", "PUSH", " push", "push ", "", "  ", "push\n",
        ] {
            let model = CreateLinkForm {
                permission: raw.into(),
                ..valid_model()
            };
            assert_eq!(
                only_error_on(model, CreateLinkForm::fields().permission()),
                "Choose a supported permission level: pull, triage, push, maintain, or admin.",
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
                "Expiration is too far in the future. Use fewer days, or leave it blank for no expiration.",
                "expires_in_days={days}"
            );
        }
    }

    #[test]
    fn repo_scope_requires_at_least_one_available_repository() {
        for repo_ids in [vec![], vec![999], vec![999, 1000]] {
            let model = CreateLinkForm {
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
                Some(fields.permission().identity()),
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
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string()
                ],
                description: Some("d".to_string()),
                internal_note: Some("n".to_string()),
                permission: Some("p".to_string()),
                max_uses: Some("m".to_string()),
                expires_in_days: Some("e".to_string()),
                repo_scope: Some("r".to_string()),
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
            csrf_token: None,
            action: "/console/accounts/acme/links".to_string(),
            values: LinkFormValues {
                description: description.to_string(),
                permission: "push".to_string(),
                approval_required: true,
                max_uses: "7".to_string(),
                expires_in_days: "45".to_string(),
                internal_note: "Keep this note".to_string(),
                selected_repo_ids: vec![10],
                errors: LinkFormErrors {
                    summary: vec![SUMMARY_MESSAGE.to_string()],
                    description: Some(DESCRIPTION_REQUIRED.to_string()),
                    ..LinkFormErrors::default()
                },
            },
            repos: available_repos(),
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
}
