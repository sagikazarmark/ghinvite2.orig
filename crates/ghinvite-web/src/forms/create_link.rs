//! The new invitation link form on the server.
//!
//! A thin adapter over the shared model in [`ghinvite_ui::link_form`]: the
//! raw POST body ([`CreateLinkSubmission`]) is parsed into the typed
//! `CreateLinkForm`, the shared validators run through `dioform-core`, and the
//! route gets back either the guardrails the command facade may act on
//! ([`ValidatedCreateLink`]) or every field error at once (`LinkFormErrors`)
//! so it can re-render the form with the admin's values preserved and each
//! problem shown next to its field.
//!
//! No rule lives here. Adding one is a validator in `register_validators`
//! (and, for a typed text field, a parser) in the shared module.

use chrono::{DateTime, Utc};
use dioform_core::{Form, FormCore};
use ghinvite_core::invitation_link::{AccountAdmin, CreateLink};
use ghinvite_core::{Description, InternalNote, InvitationLinkId};
use ghinvite_ui::link_form::{self, CreateLinkForm, LinkFormErrors, RepositoryChoice};
use ghinvite_ui::links::LinkFormValues;
use serde::Deserialize;
use std::num::NonZeroU32;

/// Raw POST body of the new invitation link form, exactly as submitted.
///
/// Text fields are `Option<String>` so a missing key and an empty value both
/// reach [`Self::to_model`] unchanged; nothing is parsed or trimmed during
/// deserialization.
#[derive(Clone, Debug, Deserialize)]
pub struct CreateLinkSubmission {
    #[serde(default)]
    pub access_lifetime_hours: String,
    pub email_domains: Option<String>,
    #[serde(default)]
    pub reload_repos: bool,
    #[serde(default)]
    pub apply_all: bool,
    #[serde(default)]
    pub include_organization: Option<String>,
    #[serde(default)]
    pub team_ids: Vec<u64>,
    pub description: Option<String>,
    /// Optional set-all helper; creation authority comes only from `repo_grants`.
    pub permission: Option<String>,
    #[serde(default)]
    pub approval_required: Option<String>,
    pub max_uses: Option<String>,
    pub expires_in_days: Option<String>,
    pub expiration_timestamp: Option<String>,
    pub invitation_lifetime_seconds: Option<String>,
    pub invitation_lifetime_hours: Option<String>,
    pub abuse_threshold_per_hour: Option<String>,
    pub budget_share_pct: Option<String>,
    #[serde(default)]
    pub never_expires: Option<String>,
    pub internal_note: Option<String>,
    #[serde(default)]
    pub repo_ids: Vec<u64>,
    #[serde(default)]
    pub repo_grants: Vec<String>,
}

impl CreateLinkSubmission {
    /// Parse the submission into the shared typed model.
    ///
    /// Omitted or blank expiration uses 14 days; only the explicit
    /// never checkbox removes expiration. Other missing text keys become empty strings. The mapping is the shared
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
            access_lifetime_hours: self.access_lifetime_hours,
            email_domains: self.email_domains.unwrap_or_default(),
            description: self.description.unwrap_or_default(),
            permission: self.permission.unwrap_or_default(),
            approval_required: self.approval_required.is_some(),
            include_organization: self.include_organization.as_deref() == Some("true"),
            selected_team_ids: self.team_ids,
            max_uses: self.max_uses.unwrap_or_default(),
            expires_in_days: self.expires_in_days.unwrap_or_else(|| "14".into()),
            expiration_timestamp: self.expiration_timestamp.unwrap_or_default(),
            invitation_lifetime_seconds: self.invitation_lifetime_seconds.unwrap_or_default(),
            invitation_lifetime_hours: self
                .invitation_lifetime_hours
                .unwrap_or_else(|| "168".into()),
            abuse_threshold_per_hour: self.abuse_threshold_per_hour.unwrap_or_default(),
            budget_share_pct: self.budget_share_pct.unwrap_or_else(|| "50".into()),
            never_expires: self.never_expires.as_deref() == Some("true"),
            internal_note: self.internal_note.unwrap_or_default(),
            selected_repo_ids: self.repo_ids,
            repo_grants: self.repo_grants,
            errors,
        }
    }
}

/// Invitation-link creation data that passed every rule of the form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedCreateLink {
    pub access_lifetime_seconds: Option<u32>,
    pub email_domains: Vec<ghinvite_core::email_domain::DomainRule>,
    pub description: Description,
    /// `None` when the internal note was left blank.
    pub internal_note: Option<InternalNote>,
    pub targets: ghinvite_core::targets::Targets,
    pub approval_required: bool,
    /// `None` is an intentional "unlimited" (the field was left blank).
    pub max_uses: Option<NonZeroU32>,
    /// `None` is intentional no expiration (the Never expires box was checked).
    /// Otherwise `now` plus the submitted whole number of days.
    pub expires_at: Option<DateTime<Utc>>,
    pub invitation_lifetime_seconds: u32,
    pub abuse_threshold_per_hour: Option<u32>,
    pub budget_share_pct: u8,
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
        secret_sha256: ghinvite_core::SecretHash,
    ) -> CreateLink {
        CreateLink {
            batch_source: None,
            access_lifetime_seconds: self.access_lifetime_seconds,
            email_domains: self.email_domains,
            link_id,
            admin,
            account_id,
            organization_installation_id: None,
            installation_id,
            description: self.description.into(),
            internal_note: self.internal_note.map(String::from),
            expires_at: self.expires_at,
            max_uses: self.max_uses.map(NonZeroU32::get),
            invitation_lifetime_seconds: self.invitation_lifetime_seconds,
            abuse_threshold_per_hour: self.abuse_threshold_per_hour,
            budget_share_pct: self.budget_share_pct,
            approval_required: self.approval_required,
            targets: self.targets,
            secret_sha256,
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
#[cfg(test)]
pub fn validate(
    form: &CreateLinkSubmission,
    available_repos: &[RepositoryChoice],
    now: DateTime<Utc>,
) -> Result<ValidatedCreateLink, Box<LinkFormErrors>> {
    validate_selection(
        form,
        available_repos,
        &link_form::OrganizationOffer::NotApplicable,
        now,
    )
}
pub fn validate_selection(
    form: &CreateLinkSubmission,
    available_repos: &[RepositoryChoice],
    organization: &link_form::OrganizationOffer,
    now: DateTime<Utc>,
) -> Result<ValidatedCreateLink, Box<LinkFormErrors>> {
    let (model, mut errors) = form.to_model();
    if form
        .include_organization
        .as_deref()
        .is_some_and(|value| value != "true")
    {
        errors.attach(
            Some(&CreateLinkForm::fields().include_organization().identity()),
            link_form::ORGANIZATION_UNAVAILABLE.into(),
        );
    }

    let mut core = FormCore::new(model.clone());
    link_form::register_target_validators(&mut core, available_repos, organization, now);
    core.validate_for_submit();
    for error in core.validation_errors() {
        errors.attach(error.field_identity().as_ref(), error.error().clone());
    }

    finish(&model, errors, available_repos, organization, now)
}

/// The creation data of a model the validators reported `errors` for.
///
/// When every rule passed, the values come from the same derivations the
/// validators consulted, so they cannot fail. Should the two ever drift, fail
/// closed — re-render rather than create a link with unvalidated guardrails —
/// and say so, rather than show the form again with nothing to correct.
fn finish(
    model: &CreateLinkForm,
    errors: LinkFormErrors,
    available_repos: &[RepositoryChoice],
    organization: &link_form::OrganizationOffer,
    now: DateTime<Utc>,
) -> Result<ValidatedCreateLink, Box<LinkFormErrors>> {
    if !errors.is_empty() {
        return Err(Box::new(errors));
    }

    let max_uses = match model.max_uses {
        None => Some(None),
        Some(max) => NonZeroU32::new(max).map(Some),
    };
    let expires_at = if !model.expiration_timestamp.is_empty() {
        link_form::exact_expiration(&model.expiration_timestamp)
            .ok()
            .filter(|date| date.is_some_and(|date| date > now) && !model.never_expires)
    } else if model.never_expires {
        Some(None)
    } else {
        link_form::expiration_after(now, model.expires_in_days.unwrap_or(14)).map(Some)
    };
    let invitation_lifetime_seconds = if !model.invitation_lifetime_seconds.is_empty() {
        link_form::exact_lifetime(&model.invitation_lifetime_seconds)
            .ok()
            .flatten()
    } else {
        model
            .invitation_lifetime_hours
            .unwrap_or(168)
            .checked_mul(3600)
            .filter(|seconds| (3600..=604800).contains(seconds))
    };
    let abuse_threshold = model.abuse_threshold_per_hour.filter(|value| *value > 0);
    if invitation_lifetime_seconds.is_none()
        || abuse_threshold != model.abuse_threshold_per_hour
        || !(1..=100).contains(&model.budget_share_pct.unwrap_or(50))
    {
        return Err(Box::new(LinkFormErrors {
            summary: vec![link_form::CREATION_VALUES_UNCHECKED.into()],
            ..errors
        }));
    }
    let targets = link_form::selected_targets(model, available_repos, organization);
    match (
        Description::parse(&model.description),
        InternalNote::parse(&model.internal_note),
        max_uses,
        expires_at,
        targets,
    ) {
        (Ok(description), Ok(internal_note), Some(max_uses), Some(expires_at), Ok(targets)) => {
            Ok(ValidatedCreateLink {
                access_lifetime_seconds: link_form::access_lifetime_seconds(
                    &model.access_lifetime_hours,
                )
                .expect("validated access lifetime"),
                email_domains: link_form::parse_email_domains(&model.email_domains)
                    .expect("validated email domains"),
                description,
                internal_note,
                targets,
                approval_required: model.approval_required,
                max_uses,
                expires_at,
                invitation_lifetime_seconds: invitation_lifetime_seconds.expect("checked above"),
                abuse_threshold_per_hour: abuse_threshold,
                budget_share_pct: model.budget_share_pct.unwrap_or(50) as u8,
            })
        }
        _ => {
            tracing::error!("create link form: validated values failed their derivation");
            // Form-level only: with no field highlighted, the "fix the
            // highlighted fields" line would mislead.
            Err(Box::new(LinkFormErrors {
                summary: vec![link_form::CREATION_VALUES_UNCHECKED.to_string()],
                ..errors
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghinvite_core::{InvitationLinkRepo, Permission};

    fn valid_form() -> CreateLinkSubmission {
        CreateLinkSubmission {
            access_lifetime_hours: String::new(),
            email_domains: None,
            expiration_timestamp: None,
            invitation_lifetime_seconds: None,
            include_organization: None,
            team_ids: vec![],
            invitation_lifetime_hours: None,
            abuse_threshold_per_hour: None,
            budget_share_pct: None,
            never_expires: None,
            reload_repos: false,
            apply_all: false,
            description: Some("AI coding workshop".into()),
            permission: Some("push".into()),
            approval_required: Some("true".into()),
            max_uses: Some("7".into()),
            expires_in_days: Some("45".into()),
            internal_note: Some("  Keep this note  ".into()),
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
            &errors.invitation_lifetime_hours,
            &errors.abuse_threshold_per_hour,
            &errors.repo_scope,
            &errors.repo_grants,
        ]
        .iter()
        .filter(|slot| slot.is_some())
        .count();
        assert_eq!(filled, 1, "exactly one field should fail: {errors:?}");
        errors
    }

    #[test]
    fn creation_command_uses_per_target_grants_and_explicit_secret_hash() {
        let validated = validate(&valid_form(), &available_repos(), now()).unwrap();
        let hash = ghinvite_core::SecretHash::from_bytes([42; 32]);
        let command = validated.into_command(
            InvitationLinkId::new(),
            AccountAdmin {
                account_id: 9,
                user_id: 7,
            },
            9,
            1,
            hash,
        );
        assert_eq!(command.secret_sha256, hash);
        assert_eq!(command.targets.repos.len(), 1);
        assert_eq!(command.targets.repos[0].repo_id, 10);
        assert_eq!(
            command.targets.repos[0].grant,
            ghinvite_core::targets::RepoRole::Write
        );
        assert!(command.targets.organization.is_none());
        assert!(command.targets.teams.is_empty());
    }

    #[test]
    fn budget_share_boundary_and_command_preserve_immutable_policy() {
        for (raw, expected) in [
            (None, 50),
            (Some(""), 50),
            (Some("1"), 1),
            (Some("100"), 100),
        ] {
            let mut form = valid_form();
            form.budget_share_pct = raw.map(str::to_owned);
            let validated = validate(&form, &available_repos(), now()).unwrap();
            assert_eq!(validated.budget_share_pct, expected);
            let command = validated.into_command(
                InvitationLinkId::new(),
                AccountAdmin {
                    account_id: 1,
                    user_id: 2,
                },
                1,
                3,
                ghinvite_core::SecretHash::from_bytes([1; 32]),
            );
            assert_eq!(command.budget_share_pct, expected);
        }
        for raw in ["0", "101", "256", "-1", "1.5", "999999999999999"] {
            let mut form = valid_form();
            form.budget_share_pct = Some(raw.into());
            let errors = validate(&form, &available_repos(), now()).unwrap_err();
            assert!(errors.budget_share_pct.is_some(), "{raw}");
        }
    }

    #[test]
    fn valid_form_yields_normalized_creation_data() {
        let validated = validate(&valid_form(), &available_repos(), now()).unwrap();

        assert_eq!(
            validated,
            ValidatedCreateLink {
                access_lifetime_seconds: None,
                email_domains: vec![],
                invitation_lifetime_seconds: 604800,
                abuse_threshold_per_hour: None,
                budget_share_pct: 50,
                description: Description::parse("AI coding workshop").unwrap(),
                internal_note: InternalNote::parse("Keep this note").unwrap(),
                targets: link_form::repository_targets(
                    &[10],
                    &["10:push".into()],
                    &available_repos()
                )
                .unwrap(),
                approval_required: true,
                max_uses: NonZeroU32::new(7),
                expires_at: Some(
                    DateTime::parse_from_rfc3339("2026-06-18T12:00:00Z")
                        .unwrap()
                        .with_timezone(&Utc)
                ),
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

        assert_eq!(validated.description.as_str(), "AI coding workshop");
        assert_eq!(validated.internal_note, None);
        assert!(!validated.approval_required);
    }

    #[test]
    fn missing_keys_are_blank_fields_not_deserialization_failures() {
        // Everything but the repository selection omitted: description is
        // required and permission is tampered; max use / expiration / note
        // are legitimately blank.
        let form = CreateLinkSubmission {
            access_lifetime_hours: String::new(),
            email_domains: None,
            expiration_timestamp: None,
            invitation_lifetime_seconds: None,
            include_organization: None,
            team_ids: vec![],
            invitation_lifetime_hours: None,
            abuse_threshold_per_hour: None,
            budget_share_pct: None,
            never_expires: None,
            reload_repos: false,
            apply_all: false,
            description: None,
            permission: None,
            approval_required: None,
            max_uses: None,
            expires_in_days: None,
            internal_note: None,
            repo_ids: vec![10],
            repo_grants: vec!["10:push".into()],
        };

        let errors = validate(&form, &available_repos(), now()).unwrap_err();

        assert_eq!(errors.summary, vec![SUMMARY.to_string()]);
        assert_eq!(
            errors.description.as_deref(),
            Some(
                "Description is required. Use short, single-line admin-only context for this invitation link."
            )
        );
        assert!(errors.permission.is_none());
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
        assert_eq!(values.expires_in_days, "14");
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
                description: "AI coding workshop".into(),
                internal_note: "  Keep this note  ".into(),
                permission: "push".into(),
                approval_required: true,
                max_uses: Some(7),
                expires_in_days: Some(45),
                repo_ids: vec![10],
                repo_grants: vec!["10:push".into()],
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
            Some("Expiration is too far in the future. Use fewer days, or select Never expires.")
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
                .map(NonZeroU32::get)
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
    fn blank_expiration_without_checkbox_defaults_to_fourteen_days() {
        for raw in [Some(""), Some("   ")] {
            let validated =
                validate(&with_expires_in_days(raw), &available_repos(), now()).unwrap();
            assert_eq!(
                validated.expires_at,
                Some(now() + chrono::Duration::days(14)),
                "expires_in_days={raw:?}"
            );
        }
    }

    #[test]
    fn omitted_expiration_defaults_to_fourteen_days_but_explicit_never_requires_a_guardrail() {
        let mut form = CreateLinkSubmission {
            approval_required: None,
            max_uses: None,
            expires_in_days: None,
            ..valid_form()
        };
        assert_eq!(
            validate(&form, &available_repos(), now())
                .unwrap()
                .expires_at,
            Some(now() + chrono::Duration::days(14))
        );
        form.expires_in_days = Some("".into());
        assert_eq!(
            validate(&form, &available_repos(), now())
                .unwrap()
                .expires_at,
            Some(now() + chrono::Duration::days(14))
        );
        form.never_expires = Some("true".into());
        let errors = validate(&form, &available_repos(), now()).unwrap_err();
        assert!(
            errors
                .summary
                .iter()
                .any(|message| message == link_form::NO_GUARDRAIL)
        );
        form.approval_required = Some("true".into());
        assert!(
            validate(&form, &available_repos(), now())
                .unwrap()
                .expires_at
                .is_none()
        );
    }

    #[test]
    fn only_the_explicit_never_checkbox_removes_expiration() {
        for submitted in [None, Some("false"), Some(""), Some("true")] {
            let mut form = valid_form();
            form.never_expires = submitted.map(str::to_owned);
            let validated = validate(&form, &available_repos(), now()).unwrap();
            assert_eq!(validated.expires_at.is_none(), submitted == Some("true"));
            let values = form.into_view_values(Default::default());
            assert_eq!(values.never_expires, submitted == Some("true"));
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
                "Expiration is too far in the future. Use fewer days, or select Never expires.",
            ),
            // Parser: past the storage type.
            (
                "18446744073709551616",
                "Expiration is too far in the future. Use fewer days, or select Never expires.",
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
    fn helper_permission_does_not_override_explicit_target_grants() {
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
            assert_eq!(
                validated.targets.repos[0].grant,
                ghinvite_core::targets::RepoRole::Write,
                "helper permission={raw:?}, {expected:?}"
            );
        }
    }

    #[test]
    fn selected_grants_reject_tampered_values_and_a_missing_key() {
        for permission in [Some("owner"), Some("Push"), Some(""), None] {
            let form = CreateLinkSubmission {
                repo_grants: permission
                    .map(|raw| vec![format!("10:{raw}")])
                    .unwrap_or_default(),
                ..valid_form()
            };
            let errors = validate(&form, &available_repos(), now()).unwrap_err();
            assert_eq!(
                single_field_errors(&errors).repo_grants.as_deref(),
                Some(link_form::REPO_GRANT_REQUIRED),
                "permission={permission:?}"
            );
        }
    }

    #[test]
    fn tampered_target_grant_is_reported_alongside_other_field_errors() {
        let form = CreateLinkSubmission {
            repo_grants: vec!["10:owner".into()],
            max_uses: Some("0".into()),
            ..valid_form()
        };

        let errors = validate(&form, &available_repos(), now()).unwrap_err();

        assert_eq!(
            errors.repo_grants.as_deref(),
            Some(link_form::REPO_GRANT_REQUIRED)
        );
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
            repo_grants: vec![],
            ..valid_form()
        };

        let errors = validate(&form, &available_repos(), now()).unwrap_err();

        assert_eq!(errors.summary, vec![SUMMARY.to_string()]);
        assert!(errors.description.is_some());
        assert!(errors.permission.is_none());
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
            repo_grants: repo_ids
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .map(|id| format!("{id}:push"))
                .collect(),
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
            validated
                .targets
                .repos
                .iter()
                .map(|repo| scope_repo(repo.repo_id, &repo.repo_full_name))
                .collect::<Vec<_>>(),
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
        assert_eq!(validated.targets.repos.len(), 100);

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
        assert_eq!(
            validated.internal_note.as_ref().map(InternalNote::as_str),
            Some("x".repeat(16_384).as_str())
        );

        let errors =
            validate(&with_note("x".repeat(16_385)), &available_repos(), now()).unwrap_err();
        assert_eq!(
            single_field_errors(&errors).internal_note.as_deref(),
            Some(link_form::INTERNAL_NOTE_TOO_LONG)
        );
    }

    #[test]
    fn values_the_validators_passed_but_the_derivations_refuse_fail_visibly() {
        // Unreachable through `validate` while the derivations match the
        // validators; this model skips them to stand in for a drift.
        let (mut model, _) = valid_form().to_model();
        model.max_uses = Some(0);

        let errors = finish(
            &model,
            LinkFormErrors::default(),
            &available_repos(),
            &link_form::OrganizationOffer::NotApplicable,
            now(),
        )
        .unwrap_err();

        assert_eq!(
            *errors,
            LinkFormErrors {
                summary: vec![link_form::CREATION_VALUES_UNCHECKED.to_string()],
                ..LinkFormErrors::default()
            }
        );
    }
    #[test]
    fn heterogeneous_grants_are_authoritative_and_helper_is_optional() {
        let mut form = valid_form();
        form.repo_ids = vec![10, 11];
        form.repo_grants = vec!["10:pull".into(), "11:maintain".into(), "12:admin".into()];
        for helper in [None, Some("owner".into()), Some("push".into())] {
            form.permission = helper;
            let command = validate(&form, &available_repos(), now())
                .unwrap()
                .into_command(
                    InvitationLinkId::new(),
                    AccountAdmin {
                        account_id: 9,
                        user_id: 7,
                    },
                    9,
                    1,
                    ghinvite_core::SecretHash::from_bytes([4; 32]),
                );
            assert_eq!(
                command
                    .targets
                    .repos
                    .iter()
                    .map(|repo| (repo.repo_id, repo.grant))
                    .collect::<Vec<_>>(),
                vec![
                    (10, ghinvite_core::targets::RepoRole::Read),
                    (11, ghinvite_core::targets::RepoRole::Maintain)
                ]
            );
        }
    }

    #[test]
    fn missing_duplicate_and_invalid_selected_grants_refuse_without_repair() {
        for grants in [
            vec![],
            vec!["10:owner"],
            vec!["10:pull", "10:push"],
            vec!["010:pull"],
            vec!["10:"],
        ] {
            let mut form = valid_form();
            form.repo_grants = grants.iter().map(|grant| (*grant).to_string()).collect();
            let errors = validate(&form, &available_repos(), now()).unwrap_err();
            assert_eq!(
                errors.repo_grants.as_deref(),
                Some(link_form::REPO_GRANT_REQUIRED)
            );
            let values = form.clone().into_view_values(*errors);
            assert_eq!(values.repo_grants, form.repo_grants);
            assert_eq!(values.to_model().0.repo_grants, form.repo_grants);
        }
    }
    #[test]
    fn lifetime_and_abuse_threshold_are_validated_and_bound_into_creation() {
        for (raw, seconds) in [
            (None, 604800),
            (Some(""), 604800),
            (Some("1"), 3600),
            (Some("168"), 604800),
        ] {
            let mut form = valid_form();
            form.invitation_lifetime_hours = raw.map(str::to_owned);
            form.abuse_threshold_per_hour = Some("23".into());
            let validated = validate(&form, &available_repos(), now()).unwrap();
            let command = validated.into_command(
                InvitationLinkId::new(),
                AccountAdmin {
                    account_id: 9,
                    user_id: 7,
                },
                9,
                1,
                ghinvite_core::SecretHash::from_bytes([4; 32]),
            );
            assert_eq!(command.invitation_lifetime_seconds, seconds);
            assert_eq!(command.abuse_threshold_per_hour, Some(23));
        }
        let mut form = valid_form();
        form.abuse_threshold_per_hour = Some(" ".into());
        assert_eq!(
            validate(&form, &available_repos(), now())
                .unwrap()
                .abuse_threshold_per_hour,
            None
        );
        for raw in ["0", "169", "-1", "1.5", "1e2", "4294967296"] {
            form.invitation_lifetime_hours = Some(raw.into());
            let errors = validate(&form, &available_repos(), now()).unwrap_err();
            assert_eq!(
                errors.invitation_lifetime_hours.as_deref(),
                Some(link_form::INVITATION_LIFETIME_INVALID)
            );
            assert_eq!(
                form.clone()
                    .into_view_values(*errors)
                    .invitation_lifetime_hours,
                raw
            );
        }
        form.invitation_lifetime_hours = None;
        for raw in ["0", "-1", "1.5", "4294967296"] {
            form.abuse_threshold_per_hour = Some(raw.into());
            let errors = validate(&form, &available_repos(), now()).unwrap_err();
            assert_eq!(
                errors.abuse_threshold_per_hour.as_deref(),
                Some(link_form::ABUSE_THRESHOLD_INVALID)
            );
            assert_eq!(
                form.clone()
                    .into_view_values(*errors)
                    .abuse_threshold_per_hour,
                raw
            );
        }
        form.abuse_threshold_per_hour = Some(u32::MAX.to_string());
        assert_eq!(
            validate(&form, &available_repos(), now())
                .unwrap()
                .abuse_threshold_per_hour,
            Some(u32::MAX)
        );
    }
    #[test]
    fn email_domains_are_normalized_and_can_supply_the_only_admission_guardrail() {
        let mut form = valid_form();
        form.email_domains = Some("EXAMPLE.COM., example.com, *.BÜCHER.example".into());
        form.approval_required = None;
        form.never_expires = Some("true".into());
        form.expires_in_days = None;
        form.max_uses = None;
        let validated = validate(&form, &available_repos(), now()).unwrap();
        assert_eq!(
            validated
                .email_domains
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["example.com", "*.xn--bcher-kva.example"]
        );
        assert_eq!(
            form.clone()
                .into_view_values(Default::default())
                .email_domains,
            "EXAMPLE.COM., example.com, *.BÜCHER.example"
        );
        form.email_domains = Some("https://example.com".into());
        assert!(
            validate(&form, &available_repos(), now())
                .unwrap_err()
                .email_domains
                .is_some()
        );
        form.email_domains = None;
        assert!(validate(&form, &available_repos(), now()).is_err());
    }
}

#[cfg(test)]
mod organization_selection_tests {
    use super::*;
    use ghinvite_ui::link_form::{OrganizationChoice, OrganizationOffer, TeamChoice};
    fn offer() -> OrganizationOffer {
        OrganizationOffer::Available(OrganizationChoice {
            id: 100,
            login: "acme".into(),
            teams: vec![
                TeamChoice {
                    id: 7,
                    slug: "learners".into(),
                    name: "Learners".into(),
                    externally_managed: false,
                },
                TeamChoice {
                    id: 8,
                    slug: "managed".into(),
                    name: "Managed".into(),
                    externally_managed: true,
                },
            ],
        })
    }
    fn input() -> CreateLinkSubmission {
        serde_json::from_value(serde_json::json!({"description":"Workshop","include_organization":"true","team_ids":[7],"approval_required":"true","max_uses":"5"})).unwrap()
    }
    #[test]
    fn organization_only_and_mixed_targets_have_fixed_member_and_independent_repo_grants() {
        let now = Utc::now();
        let org = validate_selection(&input(), &[], &offer(), now).unwrap();
        assert!(org.targets.repos.is_empty());
        assert_eq!(org.targets.organization.unwrap().org_id, 100);
        assert_eq!(
            org.targets.teams[0].grant,
            ghinvite_core::targets::MemberGrant::Member
        );
        let mut form = input();
        form.repo_ids = vec![10, 11];
        form.repo_grants = vec!["10:pull".into(), "11:maintain".into()];
        let repos = vec![
            RepositoryChoice {
                id: 10,
                full_name: "acme/docs".into(),
            },
            RepositoryChoice {
                id: 11,
                full_name: "acme/code".into(),
            },
        ];
        let mixed = validate_selection(&form, &repos, &offer(), now).unwrap();
        assert_eq!(
            mixed.targets.repos[0].grant,
            ghinvite_core::targets::RepoRole::Read
        );
        assert_eq!(
            mixed.targets.repos[1].grant,
            ghinvite_core::targets::RepoRole::Maintain
        );
        assert_eq!(mixed.targets.teams.len(), 1);
    }
    #[test]
    fn teams_require_selected_org_and_current_selectable_numeric_identity() {
        for ids in [vec![999], vec![8], vec![7, 7]] {
            let mut form = input();
            form.team_ids = ids;
            assert!(
                validate_selection(&form, &[], &offer(), Utc::now())
                    .unwrap_err()
                    .teams
                    .is_some()
            );
        }
        let mut form = input();
        form.include_organization = None;
        assert!(
            validate_selection(&form, &[], &offer(), Utc::now())
                .unwrap_err()
                .teams
                .is_some()
        );
        assert!(
            validate_selection(&input(), &[], &OrganizationOffer::MissingApp, Utc::now())
                .unwrap_err()
                .organization
                .is_some()
        );
    }
    #[test]
    fn auto_approved_org_requires_explicit_positive_max_uses() {
        let mut form = input();
        form.approval_required = None;
        form.max_uses = None;
        assert!(
            validate_selection(&form, &[], &offer(), Utc::now())
                .unwrap_err()
                .max_uses
                .is_some()
        );
        form.max_uses = Some("1".into());
        assert!(validate_selection(&form, &[], &offer(), Utc::now()).is_ok());
    }
}

#[cfg(test)]
mod exact_duplicate_guardrails_tests {
    use super::*;
    #[test]
    fn exact_values_preserve_subhour_lifetime_and_timestamp_precision() {
        let now = DateTime::parse_from_rfc3339("2026-05-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let form:CreateLinkSubmission=serde_json::from_value(serde_json::json!({"description":"Duplicate","repo_ids":[1],"repo_grants":["1:pull"],"expiration_timestamp":"2027-04-05T12:34:56.123456789+02:00","invitation_lifetime_seconds":"3601"})).unwrap();
        let validated = validate(
            &form,
            &[RepositoryChoice {
                id: 1,
                full_name: "acme/one".into(),
            }],
            now,
        )
        .unwrap();
        assert_eq!(validated.invitation_lifetime_seconds, 3601);
        assert_eq!(
            validated.expires_at.unwrap(),
            DateTime::parse_from_rfc3339("2027-04-05T12:34:56.123456789+02:00").unwrap()
        );
    }
    #[test]
    fn expired_duplicate_requires_explicit_review_and_never_cannot_hide_exact_expiry() {
        let mut form:CreateLinkSubmission=serde_json::from_value(serde_json::json!({"description":"Duplicate","repo_ids":[1],"repo_grants":["1:pull"],"expiration_timestamp":"2020-01-01T00:00:00Z","invitation_lifetime_seconds":"3599"})).unwrap();
        let repos = [RepositoryChoice {
            id: 1,
            full_name: "acme/one".into(),
        }];
        let errors = validate(&form, &repos, Utc::now()).unwrap_err();
        assert!(errors.expiration_timestamp.is_some());
        assert!(errors.invitation_lifetime_seconds.is_some());
        form.expiration_timestamp = Some("2099-01-01T00:00:00Z".into());
        form.invitation_lifetime_seconds = Some("604800".into());
        form.never_expires = Some("true".into());
        form.approval_required = Some("true".into());
        assert!(
            validate(&form, &repos, Utc::now())
                .unwrap_err()
                .expiration_timestamp
                .is_some()
        );
        form.expiration_timestamp = None;
        assert!(
            validate(&form, &repos, Utc::now())
                .unwrap()
                .expires_at
                .is_none()
        );
    }
    #[test]
    fn access_lifetime_is_optional_bounded_and_preserved_in_the_immutable_command() {
        let repos = [RepositoryChoice {
            id: 1,
            full_name: "acme/one".into(),
        }];
        let form = || {
            serde_json::from_value::<CreateLinkSubmission>(serde_json::json!({"description":"Finite access","repo_ids":[1],"repo_grants":["1:pull"]})).unwrap()
        };
        for (raw, expected) in [
            ("", None),
            ("24", Some(86400)),
            ("3601s", Some(3601)),
            ("4294967295s", Some(u32::MAX)),
        ] {
            let mut form = form();
            form.access_lifetime_hours = raw.into();
            let validated = validate(&form, &repos, Utc::now()).unwrap();
            assert_eq!(validated.access_lifetime_seconds, expected);
            assert_eq!(
                form.clone()
                    .into_view_values(Default::default())
                    .access_lifetime_hours,
                raw
            );
        }
        for raw in ["0", "0s", "-1", "1.5", "1193047", "4294967296s", "never"] {
            let mut form = form();
            form.access_lifetime_hours = raw.into();
            let errors = validate(&form, &repos, Utc::now()).unwrap_err();
            assert_eq!(
                errors.access_lifetime_hours.as_deref(),
                Some(link_form::ACCESS_LIFETIME_INVALID)
            );
            assert_eq!(form.into_view_values(*errors).access_lifetime_hours, raw);
        }
    }
}
