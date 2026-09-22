//! The new invitation link form on the server.
//!
//! A thin adapter over the shared model in [`crate::views::link_form`]: the
//! raw POST body ([`CreateLinkSubmission`]) is parsed into the typed
//! `CreateLinkForm`, the shared validators run through `dioform-core`, and the
//! route gets back either the guardrails the command facade may act on
//! ([`ValidatedCreateLink`]) or every field error at once (`LinkFormErrors`)
//! so it can re-render the form with the admin's values preserved and each
//! problem shown next to its field.
//!
//! No rule lives here. Adding one is a validator in `register_validators`
//! (and, for a typed text field, a parser) in the shared module.

use crate::views::link_form::{
    self, CreateLinkForm, LinkFormErrors, RepositoryChoice, register_validators,
};
use crate::views::links::LinkFormValues;
use chrono::{DateTime, Utc};
use dioform_core::FormCore;
use ghinvite_core::storage::projection::{AccountAdmin, CreateLink};
use ghinvite_core::{
    Description, InternalNote, InvitationLinkId, InvitationLinkRepo, Permission, RepositoryScope,
};
use serde::Deserialize;
use std::str::FromStr;

/// Raw POST body of the new invitation link form, exactly as submitted.
///
/// Text fields are `Option<String>` so a missing key and an empty value both
/// reach [`Self::to_model`] unchanged; nothing is parsed or trimmed during
/// deserialization.
#[derive(Clone, Debug, Deserialize)]
pub struct CreateLinkSubmission {
    #[serde(default)]
    pub reload_repos: bool,
    pub description: Option<String>,
    /// `Option` so a POST that omits the key (tampering — the `<select>` always
    /// submits one) reaches validation and gets the inline permission error
    /// instead of failing deserialization with a bare 400.
    pub permission: Option<String>,
    #[serde(default)]
    pub approval_required: Option<String>,
    pub max_uses: Option<String>,
    pub expires_in_days: Option<String>,
    pub internal_note: Option<String>,
    #[serde(default)]
    pub repo_ids: Vec<u64>,
}

impl CreateLinkSubmission {
    /// Parse the submission into the shared typed model.
    ///
    /// A missing key becomes the empty string, which is also what an empty
    /// control submits; from there the mapping is the shared
    /// [`LinkFormValues::to_model`], the same one the island mounts with.
    pub fn to_model(&self) -> (CreateLinkForm, LinkFormErrors) {
        self.clone()
            .into_view_values(LinkFormErrors::default())
            .to_model()
    }

    /// The submitted values as the form view model with `errors` attached.
    ///
    /// Values are preserved verbatim (untrimmed, unparsed) so an admin sees
    /// exactly what they typed when correcting a mistake. Submitted repository
    /// IDs and the permission value are carried as-is: the view only renders a
    /// checkbox per available repository and an option per supported
    /// permission level, so an unknown or tampered value can never appear
    /// checked or selected (the select simply falls back to its first option,
    /// with the permission error shown beneath it).
    pub fn into_view_values(self, errors: LinkFormErrors) -> LinkFormValues {
        LinkFormValues {
            description: self.description.unwrap_or_default(),
            permission: self.permission.unwrap_or_default(),
            approval_required: self.approval_required.is_some(),
            max_uses: self.max_uses.unwrap_or_default(),
            expires_in_days: self.expires_in_days.unwrap_or_default(),
            internal_note: self.internal_note.unwrap_or_default(),
            selected_repo_ids: self.repo_ids,
            errors,
        }
    }
}

/// Invitation-link creation data that passed every rule of the form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedCreateLink {
    pub description: String,
    pub internal_note: Option<String>,
    /// The permission level, one of the supported GitHub collaborator levels.
    pub permission: Permission,
    pub approval_required: bool,
    /// `None` is an intentional "unlimited" (the field was left blank).
    pub max_uses: Option<u32>,
    /// `None` is an intentional "no expiration" (the field was left blank).
    /// Otherwise `now` plus the submitted whole number of days.
    pub expires_at: Option<DateTime<Utc>>,
    /// The repository scope: a non-empty subset of the available
    /// repositories, ordered by repository ID.
    pub repos: Vec<InvitationLinkRepo>,
}

impl ValidatedCreateLink {
    /// The creation command carrying this data under a creation identity:
    /// the allocated link, the asserting admin, and the account's
    /// installation. A retried submission replays when its command equals the
    /// retained one.
    pub fn into_command(
        self,
        link_id: InvitationLinkId,
        admin: AccountAdmin,
        account_id: u64,
        installation_id: u64,
    ) -> CreateLink {
        CreateLink {
            link_id,
            admin,
            account_id,
            installation_id,
            description: self.description,
            internal_note: self.internal_note,
            expires_at: self.expires_at,
            max_uses: self.max_uses,
            permission: self.permission,
            approval_required: self.approval_required,
            repos: self.repos,
        }
    }
}

/// Validate a submitted new invitation link form.
///
/// Parses the submission, runs the shared validators through a `FormCore`,
/// and reports every failure together. `available_repos` are the repositories
/// the installation currently exposes, in the shape the form offered them;
/// only those can enter the repository scope. `now` anchors the expiration
/// timestamp; callers pass the same instant they record as `created_at` so
/// the two agree.
///
/// Everything here is synchronous: the caller loads the repositories first,
/// and the `FormCore` (which holds `Rc`) is created and dropped inside this
/// call, never held across an `.await`.
///
/// The error side is boxed because `LinkFormErrors` carries a message slot
/// per field and is only built on the (cold) failure path.
pub fn validate(
    form: &CreateLinkSubmission,
    available_repos: &[RepositoryChoice],
    now: DateTime<Utc>,
) -> Result<ValidatedCreateLink, Box<LinkFormErrors>> {
    let (model, mut errors) = form.to_model();

    let mut core = FormCore::new(model.clone());
    register_validators(&mut core, available_repos, now);
    core.validate_for_submit();
    for error in core.validation_errors() {
        errors.attach(error.field_identity().as_ref(), error.error().clone());
    }

    if !errors.is_empty() {
        return Err(Box::new(errors));
    }

    // Every rule passed. These are the same derivations the validators
    // consulted, so they cannot fail now; should the two ever drift, fail
    // closed (re-render) rather than create a link with unvalidated
    // guardrails.
    let permission = Permission::from_str(&model.permission).ok();
    let expires_at = match model.expires_in_days {
        None => Some(None),
        Some(days) => link_form::expiration_after(now, days).map(Some),
    };
    let repos = RepositoryScope::parse(link_form::repository_scope(
        &model.repo_ids,
        available_repos,
    ));
    match (
        Description::parse(&model.description),
        InternalNote::parse(&model.internal_note),
        permission,
        expires_at,
        repos,
    ) {
        (Ok(description), Ok(internal_note), Some(permission), Some(expires_at), Ok(repos)) => {
            Ok(ValidatedCreateLink {
                description: description.into(),
                internal_note: internal_note.map(String::from),
                permission,
                approval_required: model.approval_required,
                max_uses: model.max_uses,
                expires_at,
                repos: repos.into(),
            })
        }
        _ => Err(Box::new(errors)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_form() -> CreateLinkSubmission {
        CreateLinkSubmission {
            reload_repos: false,
            description: Some("AI coding workshop".into()),
            permission: Some("push".into()),
            approval_required: Some("true".into()),
            max_uses: Some("7".into()),
            expires_in_days: Some("45".into()),
            internal_note: Some("  Keep this note  ".into()),
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

    fn scope_repo(repo_id: u64, full_name: &str) -> InvitationLinkRepo {
        InvitationLinkRepo {
            repo_id,
            repo_full_name: full_name.into(),
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-04T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    const SUMMARY: &str = "Fix the highlighted fields before creating this invitation link.";
    const PERMISSION_UNSUPPORTED: &str =
        "Choose a supported permission level: pull, triage, push, maintain, or admin.";
    const REPO_SCOPE_REQUIRED: &str = "Repository scope is required. Select at least one available repository for this invitation link.";

    /// Exactly one field failed, alongside the summary line; returns its slot.
    fn single_field_errors(errors: &LinkFormErrors) -> &LinkFormErrors {
        assert_eq!(errors.summary, vec![SUMMARY.to_string()]);
        let filled = [
            &errors.description,
            &errors.internal_note,
            &errors.permission,
            &errors.max_uses,
            &errors.expires_in_days,
            &errors.repo_scope,
        ]
        .iter()
        .filter(|slot| slot.is_some())
        .count();
        assert_eq!(filled, 1, "exactly one field should fail: {errors:?}");
        errors
    }

    #[test]
    fn valid_form_yields_normalized_creation_data() {
        let validated = validate(&valid_form(), &available_repos(), now()).unwrap();

        assert_eq!(
            validated,
            ValidatedCreateLink {
                description: "AI coding workshop".into(),
                internal_note: Some("Keep this note".into()),
                permission: Permission::Push,
                approval_required: true,
                max_uses: Some(7),
                expires_at: Some(
                    DateTime::parse_from_rfc3339("2026-06-18T12:00:00Z")
                        .unwrap()
                        .with_timezone(&Utc)
                ),
                repos: vec![scope_repo(10, "acme/api")],
            }
        );
    }

    #[test]
    fn description_is_trimmed_and_blank_note_and_unchecked_approval_normalize() {
        let form = CreateLinkSubmission {
            description: Some("  AI coding workshop  ".into()),
            internal_note: Some("   ".into()),
            approval_required: None,
            ..valid_form()
        };

        let validated = validate(&form, &available_repos(), now()).unwrap();

        assert_eq!(validated.description, "AI coding workshop");
        assert_eq!(validated.internal_note, None);
        assert!(!validated.approval_required);
    }

    #[test]
    fn missing_keys_are_blank_fields_not_deserialization_failures() {
        // Everything but the repository selection omitted: description is
        // required and permission is tampered; max use / expiration / note
        // are legitimately blank.
        let form = CreateLinkSubmission {
            reload_repos: false,
            description: None,
            permission: None,
            approval_required: None,
            max_uses: None,
            expires_in_days: None,
            internal_note: None,
            repo_ids: vec![10],
        };

        let errors = validate(&form, &available_repos(), now()).unwrap_err();

        assert_eq!(errors.summary, vec![SUMMARY.to_string()]);
        assert_eq!(
            errors.description.as_deref(),
            Some(
                "Description is required. Use short, single-line admin-only context for this invitation link."
            )
        );
        assert_eq!(errors.permission.as_deref(), Some(PERMISSION_UNSUPPORTED));
        assert_eq!(errors.max_uses, None);
        assert_eq!(errors.expires_in_days, None);
        assert_eq!(errors.repo_scope, None);
    }

    #[test]
    fn into_view_values_preserves_submitted_values_verbatim() {
        let form = CreateLinkSubmission {
            description: Some("  AI coding workshop  ".into()),
            max_uses: Some(" abc ".into()),
            expires_in_days: None,
            ..valid_form()
        };

        let values = form.into_view_values(LinkFormErrors::default());

        assert_eq!(values.description, "  AI coding workshop  ");
        assert_eq!(values.permission, "push");
        assert!(values.approval_required);
        assert_eq!(values.max_uses, " abc ");
        assert_eq!(values.expires_in_days, "");
        assert_eq!(values.internal_note, "  Keep this note  ");
        assert_eq!(values.selected_repo_ids, vec![10]);
        assert_eq!(values.errors, LinkFormErrors::default());
    }

    #[test]
    fn to_model_carries_text_verbatim_and_parses_the_numeric_guardrails() {
        let (model, errors) = valid_form().to_model();

        assert!(errors.is_empty());
        assert_eq!(
            model,
            CreateLinkForm {
                description: "AI coding workshop".into(),
                internal_note: "  Keep this note  ".into(),
                permission: "push".into(),
                approval_required: true,
                max_uses: Some(7),
                expires_in_days: Some(45),
                repo_ids: vec![10],
            }
        );
    }

    #[test]
    fn to_model_reports_unparsable_guardrails_against_their_field_and_leaves_them_blank() {
        let form = CreateLinkSubmission {
            max_uses: Some("abc".into()),
            expires_in_days: Some("18446744073709551616".into()),
            ..valid_form()
        };

        let (model, errors) = form.to_model();

        assert_eq!(model.max_uses, None);
        assert_eq!(model.expires_in_days, None);
        assert_eq!(
            errors.max_uses.as_deref(),
            Some("Max use must be a whole number of 1 or more.")
        );
        assert_eq!(
            errors.expires_in_days.as_deref(),
            Some(
                "Expiration is too far in the future. Use fewer days, or leave it blank for no expiration."
            )
        );
        assert_eq!(errors.summary, vec![SUMMARY.to_string()]);
    }

    fn with_max_uses(raw: Option<&str>) -> CreateLinkSubmission {
        CreateLinkSubmission {
            max_uses: raw.map(str::to_string),
            ..valid_form()
        }
    }

    #[test]
    fn max_uses_blank_or_missing_means_unlimited() {
        for raw in [None, Some(""), Some("   ")] {
            let validated = validate(&with_max_uses(raw), &available_repos(), now()).unwrap();
            assert_eq!(validated.max_uses, None, "max_uses={raw:?}");
        }
    }

    #[test]
    fn max_uses_accepts_positive_whole_numbers_and_trims() {
        let max_uses = |raw: &str| {
            validate(&with_max_uses(Some(raw)), &available_repos(), now())
                .unwrap()
                .max_uses
        };

        assert_eq!(max_uses("1"), Some(1));
        assert_eq!(max_uses(" 25 "), Some(25));
        assert_eq!(max_uses("4294967295"), Some(u32::MAX));
    }

    #[test]
    fn max_uses_parse_failures_are_reported_once_in_the_max_use_slot() {
        for (raw, message) in [
            ("0", "Max use must be a whole number of 1 or more."),
            ("-1", "Max use must be a whole number of 1 or more."),
            ("1.5", "Max use must be a whole number of 1 or more."),
            ("abc", "Max use must be a whole number of 1 or more."),
            (
                "4294967296",
                "Max use is too large. Use a smaller number, or leave it blank for unlimited invitation requests.",
            ),
        ] {
            let errors =
                validate(&with_max_uses(Some(raw)), &available_repos(), now()).unwrap_err();
            assert_eq!(
                single_field_errors(&errors).max_uses.as_deref(),
                Some(message),
                "max_uses={raw:?}"
            );
        }
    }

    fn with_expires_in_days(raw: Option<&str>) -> CreateLinkSubmission {
        CreateLinkSubmission {
            expires_in_days: raw.map(str::to_string),
            ..valid_form()
        }
    }

    #[test]
    fn expires_in_days_blank_or_missing_means_no_expiration() {
        for raw in [None, Some(""), Some("   ")] {
            let validated =
                validate(&with_expires_in_days(raw), &available_repos(), now()).unwrap();
            assert_eq!(validated.expires_at, None, "expires_in_days={raw:?}");
        }
    }

    #[test]
    fn expires_in_days_accepts_positive_whole_numbers_anchored_at_now() {
        let expires_at = |raw: &str| {
            validate(&with_expires_in_days(Some(raw)), &available_repos(), now())
                .unwrap()
                .expires_at
                .unwrap()
        };

        assert_eq!(expires_at("1").to_rfc3339(), "2026-05-05T12:00:00+00:00");
        assert_eq!(expires_at(" 45 ").to_rfc3339(), "2026-06-18T12:00:00+00:00");
        assert_eq!(
            expires_at("36500").to_rfc3339(),
            "2126-04-10T12:00:00+00:00"
        );
    }

    #[test]
    fn expires_in_days_failures_from_parser_and_validator_share_the_expiration_slot() {
        for (raw, message) in [
            // Parser: not a positive whole number.
            ("0", "Expiration must be a whole number of days, 1 or more."),
            (
                "abc",
                "Expiration must be a whole number of days, 1 or more.",
            ),
            // Validator: parses, but `now` cannot be advanced that far.
            (
                "100000000",
                "Expiration is too far in the future. Use fewer days, or leave it blank for no expiration.",
            ),
            // Parser: past the storage type.
            (
                "18446744073709551616",
                "Expiration is too far in the future. Use fewer days, or leave it blank for no expiration.",
            ),
        ] {
            let errors =
                validate(&with_expires_in_days(Some(raw)), &available_repos(), now()).unwrap_err();
            assert_eq!(
                single_field_errors(&errors).expires_in_days.as_deref(),
                Some(message),
                "expires_in_days={raw:?}"
            );
        }
    }

    #[test]
    fn permission_accepts_every_supported_level() {
        for (raw, expected) in [
            ("pull", Permission::Pull),
            ("triage", Permission::Triage),
            ("push", Permission::Push),
            ("maintain", Permission::Maintain),
            ("admin", Permission::Admin),
        ] {
            let form = CreateLinkSubmission {
                permission: Some(raw.into()),
                ..valid_form()
            };
            let validated = validate(&form, &available_repos(), now()).unwrap();
            assert_eq!(validated.permission, expected, "permission={raw:?}");
        }
    }

    #[test]
    fn permission_rejects_tampered_values_and_a_missing_key() {
        for permission in [Some("owner"), Some("Push"), Some(""), None] {
            let form = CreateLinkSubmission {
                permission: permission.map(str::to_string),
                ..valid_form()
            };
            let errors = validate(&form, &available_repos(), now()).unwrap_err();
            assert_eq!(
                single_field_errors(&errors).permission.as_deref(),
                Some(PERMISSION_UNSUPPORTED),
                "permission={permission:?}"
            );
        }
    }

    #[test]
    fn tampered_permission_is_reported_alongside_other_field_errors() {
        let form = CreateLinkSubmission {
            permission: Some("owner".into()),
            max_uses: Some("0".into()),
            ..valid_form()
        };

        let errors = validate(&form, &available_repos(), now()).unwrap_err();

        assert_eq!(errors.permission.as_deref(), Some(PERMISSION_UNSUPPORTED));
        assert_eq!(
            errors.max_uses.as_deref(),
            Some("Max use must be a whole number of 1 or more.")
        );
        assert_eq!(errors.description, None);
        assert_eq!(errors.expires_in_days, None);
        assert_eq!(errors.repo_scope, None);
    }

    #[test]
    fn validate_reports_every_field_error_at_once() {
        let form = CreateLinkSubmission {
            description: Some("".into()),
            permission: Some("owner".into()),
            max_uses: Some("0".into()),
            expires_in_days: Some("abc".into()),
            repo_ids: vec![],
            ..valid_form()
        };

        let errors = validate(&form, &available_repos(), now()).unwrap_err();

        assert_eq!(errors.summary, vec![SUMMARY.to_string()]);
        assert!(errors.description.is_some());
        assert_eq!(errors.permission.as_deref(), Some(PERMISSION_UNSUPPORTED));
        assert_eq!(
            errors.max_uses.as_deref(),
            Some("Max use must be a whole number of 1 or more.")
        );
        assert_eq!(
            errors.expires_in_days.as_deref(),
            Some("Expiration must be a whole number of days, 1 or more.")
        );
        assert_eq!(errors.repo_scope.as_deref(), Some(REPO_SCOPE_REQUIRED));
    }

    fn with_repo_ids(repo_ids: Vec<u64>) -> CreateLinkSubmission {
        CreateLinkSubmission {
            repo_ids,
            ..valid_form()
        }
    }

    #[test]
    fn repo_scope_rejects_empty_or_all_unknown_selections() {
        for repo_ids in [vec![], vec![999], vec![999, 1000]] {
            let errors =
                validate(&with_repo_ids(repo_ids.clone()), &available_repos(), now()).unwrap_err();
            assert_eq!(
                single_field_errors(&errors).repo_scope.as_deref(),
                Some(REPO_SCOPE_REQUIRED),
                "repo_ids={repo_ids:?}"
            );
        }
    }

    #[test]
    fn repo_scope_rejects_any_selection_when_no_repositories_are_available() {
        let errors = validate(&with_repo_ids(vec![10]), &[], now()).unwrap_err();

        assert_eq!(
            single_field_errors(&errors).repo_scope.as_deref(),
            Some(REPO_SCOPE_REQUIRED)
        );
    }

    #[test]
    fn repo_scope_is_ordered_by_repository_id_and_ignores_duplicates() {
        let mut available = available_repos();
        available.reverse();
        let validated =
            validate(&with_repo_ids(vec![12, 10, 12, 11, 10]), &available, now()).unwrap();

        assert_eq!(
            validated.repos,
            vec![
                scope_repo(10, "acme/api"),
                scope_repo(11, "acme/web"),
                scope_repo(12, "acme/docs"),
            ]
        );
    }

    #[test]
    fn repo_scope_over_100_repositories_is_a_field_error() {
        let available: Vec<_> = (1..=101)
            .map(|id| repo(id, &format!("acme/repo-{id}")))
            .collect();

        let validated = validate(&with_repo_ids((1..=100).collect()), &available, now()).unwrap();
        assert_eq!(validated.repos.len(), 100);

        let errors = validate(&with_repo_ids((1..=101).collect()), &available, now()).unwrap_err();
        assert_eq!(
            single_field_errors(&errors).repo_scope.as_deref(),
            Some(link_form::REPO_SCOPE_TOO_MANY)
        );
    }

    #[test]
    fn a_scope_core_refuses_is_a_field_error_not_a_silent_rerender() {
        // Only GitHub supplies these: a full name that is not `owner/name`,
        // and one repository listed twice.
        let malformed = vec![repo(10, "acme")];
        let listed_twice = vec![repo(10, "acme/api"), repo(10, "acme/api")];

        for available in [malformed, listed_twice] {
            let errors = validate(&with_repo_ids(vec![10]), &available, now()).unwrap_err();
            assert_eq!(
                single_field_errors(&errors).repo_scope.as_deref(),
                Some(link_form::REPO_SCOPE_UNUSABLE),
                "available={available:?}"
            );
        }
    }

    #[test]
    fn internal_note_over_16384_bytes_is_a_field_error() {
        let with_note = |note: String| CreateLinkSubmission {
            internal_note: Some(note),
            ..valid_form()
        };

        let validated = validate(
            &with_note(format!(" {} ", "x".repeat(16_384))),
            &available_repos(),
            now(),
        )
        .unwrap();
        assert_eq!(validated.internal_note, Some("x".repeat(16_384)));

        let errors =
            validate(&with_note("x".repeat(16_385)), &available_repos(), now()).unwrap_err();
        assert_eq!(
            single_field_errors(&errors).internal_note.as_deref(),
            Some(link_form::INTERNAL_NOTE_TOO_LONG)
        );
    }
}
