//! Validation for the new invitation link form.
//!
//! [`validate`] is the deep end of the form: it accepts the raw
//! [`CreateLinkForm`] the browser posted plus the account's available
//! repositories, and returns either the guardrails the command facade may act
//! on ([`ValidatedCreateLink`]) or every field error at once
//! ([`LinkFormErrors`]) so the route can re-render the form with the admin's
//! values preserved and each problem shown next to its field.
//!
//! Adding a rule is one validator function returning `Result<T, FieldError>`,
//! one slot in the match inside [`validate`], and one field on
//! [`LinkFormErrors`].

use crate::views::links::{LinkFormErrors, LinkFormValues};
use chrono::{DateTime, TimeDelta, Utc};
use ghinvite_core::{InvitationLinkRepo, Permission};
use ghinvite_github::payloads::GhRepo;
use serde::Deserialize;
use std::str::FromStr;

/// Raw POST body of the new invitation link form, exactly as submitted.
///
/// Text fields are `Option<String>` so a missing key and an empty value both
/// reach the validators unchanged; nothing is parsed or trimmed during
/// deserialization.
#[derive(Debug, Deserialize)]
pub struct CreateLinkForm {
    pub description: Option<String>,
    /// `Option` so a POST that omits the key (tampering — the `<select>` always
    /// submits one) reaches `validate` and gets the inline permission error
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

impl CreateLinkForm {
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

/// Invitation-link creation data that passed every rule this module owns.
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
    /// repositories, in the order the account makes them available.
    pub repos: Vec<InvitationLinkRepo>,
}

/// Validate a submitted new invitation link form.
///
/// Runs every rule and reports all failures together. `available_repos` are
/// the repositories the installation currently exposes; only those can enter
/// the repository scope. `now` anchors the expiration timestamp; callers pass
/// the same instant they record as `created_at` so the two agree.
///
/// The error side is boxed because [`LinkFormErrors`] carries a message slot
/// per field and is only built on the (cold) failure path.
pub fn validate(
    form: &CreateLinkForm,
    available_repos: &[GhRepo],
    now: DateTime<Utc>,
) -> Result<ValidatedCreateLink, Box<LinkFormErrors>> {
    let description = validate_description(form.description.as_deref());
    let permission = validate_permission(form.permission.as_deref());
    let max_uses = validate_max_uses(form.max_uses.as_deref());
    let expires_at = validate_expires_in_days(form.expires_in_days.as_deref(), now);
    let repos = validate_repo_scope(&form.repo_ids, available_repos);

    match (description, permission, max_uses, expires_at, repos) {
        (Ok(description), Ok(permission), Ok(max_uses), Ok(expires_at), Ok(repos)) => {
            Ok(ValidatedCreateLink {
                description,
                internal_note: normalize_internal_note(form.internal_note.as_deref()),
                permission,
                approval_required: form.approval_required.is_some(),
                max_uses,
                expires_at,
                repos,
            })
        }
        (description, permission, max_uses, expires_at, repos) => Err(Box::new(LinkFormErrors {
            summary: vec![SUMMARY.to_string()],
            description: description.err().map(|e| e.message),
            permission: permission.err().map(|e| e.message),
            max_uses: max_uses.err().map(|e| e.message),
            expires_in_days: expires_at.err().map(|e| e.message),
            repo_scope: repos.err().map(|e| e.message),
        })),
    }
}

/// The one summary line shown above the form whenever any field failed.
const SUMMARY: &str = "Fix the highlighted fields before creating this invitation link.";

const DESCRIPTION_MAX_CHARS: usize = 120;

/// A field-level validation failure and the message rendered under its
/// control.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FieldError {
    message: String,
}

impl FieldError {
    fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}

fn validate_description(raw: Option<&str>) -> Result<String, FieldError> {
    let raw_description = raw.unwrap_or("");
    let description = raw_description.trim();
    if description.is_empty() {
        return Err(FieldError::new(
            "Description is required. Use short, single-line admin-only context for this invitation link.",
        ));
    }
    if raw_description.contains('\n') || raw_description.contains('\r') {
        return Err(FieldError::new("Description must be a single line."));
    }
    if description.chars().count() > DESCRIPTION_MAX_CHARS {
        return Err(FieldError::new(
            "Description must be 120 characters or fewer.",
        ));
    }
    Ok(description.to_string())
}

/// Internal note is optional and unvalidated: trimmed, and blank means none.
fn normalize_internal_note(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|note| !note.is_empty())
        .map(str::to_string)
}

/// Permission level: exactly one of the supported GitHub collaborator levels.
///
/// The `<select>` only ever submits those exact lowercase values, so nothing
/// is trimmed or case-folded here: any other string — or a missing key — did
/// not come from the form and is treated as tampering, not a typo.
fn validate_permission(raw: Option<&str>) -> Result<Permission, FieldError> {
    raw.and_then(|raw| Permission::from_str(raw).ok())
        .ok_or_else(|| {
            FieldError::new(
                "Choose a supported permission level: pull, triage, push, maintain, or admin.",
            )
        })
}

/// Max use: blank means unlimited; otherwise a positive whole number that
/// fits the `u32` the invitation link stores.
fn validate_max_uses(raw: Option<&str>) -> Result<Option<u32>, FieldError> {
    let Some(typed) = non_blank(raw) else {
        return Ok(None);
    };
    positive_whole_number(typed)
        .and_then(|count| u32::try_from(count).map_err(|_| NumberProblem::TooLarge))
        .map(Some)
        .map_err(|problem| match problem {
            NumberProblem::NotPositiveWholeNumber => {
                FieldError::new("Max use must be a whole number of 1 or more.")
            }
            NumberProblem::TooLarge => FieldError::new(
                "Max use is too large. Use a smaller number, or leave it blank for unlimited invitation requests.",
            ),
        })
}

/// Expiration: blank means no expiration; otherwise a positive whole number
/// of days that `now` can safely be advanced by. There is no product maximum;
/// the only upper bound is what chrono can represent as a timestamp.
fn validate_expires_in_days(
    raw: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, FieldError> {
    let Some(typed) = non_blank(raw) else {
        return Ok(None);
    };
    positive_whole_number(typed)
        .and_then(|days| expiration_after(now, days).ok_or(NumberProblem::TooLarge))
        .map(Some)
        .map_err(|problem| match problem {
            NumberProblem::NotPositiveWholeNumber => {
                FieldError::new("Expiration must be a whole number of days, 1 or more.")
            }
            NumberProblem::TooLarge => FieldError::new(
                "Expiration is too far in the future. Use fewer days, or leave it blank for no expiration.",
            ),
        })
}

/// `now + days`, or `None` when the day count or the resulting instant is
/// outside what chrono can represent. Every step is checked; nothing here can
/// panic on adversarial input.
fn expiration_after(now: DateTime<Utc>, days: u64) -> Option<DateTime<Utc>> {
    let days = i64::try_from(days).ok()?;
    let delta = TimeDelta::try_days(days)?;
    now.checked_add_signed(delta)
}

/// Repository scope: the selected IDs intersected with the available
/// repositories, in available order, each repository at most once.
///
/// The server is the authority on what is available, so an ID the
/// installation does not expose (stale page, tampered request) is dropped
/// without comment and can never become a scope entry. What remains must be
/// non-empty, because a repository scope is by definition a non-empty set.
fn validate_repo_scope(
    selected: &[u64],
    available: &[GhRepo],
) -> Result<Vec<InvitationLinkRepo>, FieldError> {
    let repos: Vec<InvitationLinkRepo> = available
        .iter()
        .filter(|repo| selected.contains(&repo.id))
        .map(|repo| InvitationLinkRepo {
            repo_id: repo.id,
            repo_full_name: repo.full_name.clone(),
        })
        .collect();
    if repos.is_empty() {
        return Err(FieldError::new(
            "Repository scope is required. Select at least one available repository for this invitation link.",
        ));
    }
    Ok(repos)
}

/// `Some(trimmed)` when the admin typed something; `None` for a missing key,
/// an empty value, or whitespace only.
fn non_blank(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|typed| !typed.is_empty())
}

/// Why a typed value is not a usable positive whole number. The two cases get
/// different messages: one is a typo, the other is a number the guardrail
/// cannot represent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NumberProblem {
    /// Zero, negative, decimal, signed, or not digits at all.
    NotPositiveWholeNumber,
    /// Digits only, but larger than the guardrail can represent: past the
    /// storage type for max use, past a representable timestamp for
    /// expiration.
    TooLarge,
}

/// Read a non-blank typed value as a positive whole number written in ASCII
/// digits: no sign, decimal point, exponent, or separators. Callers narrow
/// the `u64` further to their storage type.
fn positive_whole_number(typed: &str) -> Result<u64, NumberProblem> {
    if !typed.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(NumberProblem::NotPositiveWholeNumber);
    }
    match typed.parse::<u64>() {
        Ok(0) => Err(NumberProblem::NotPositiveWholeNumber),
        Ok(number) => Ok(number),
        // All digits and non-empty, so the only way to fail is overflow.
        Err(_) => Err(NumberProblem::TooLarge),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_form() -> CreateLinkForm {
        CreateLinkForm {
            description: Some("AI coding workshop".into()),
            permission: Some("push".into()),
            approval_required: Some("true".into()),
            max_uses: Some("7".into()),
            expires_in_days: Some("45".into()),
            internal_note: Some("  Keep this note  ".into()),
            repo_ids: vec![10],
        }
    }

    fn repo(id: u64, full_name: &str) -> GhRepo {
        GhRepo {
            id,
            full_name: full_name.into(),
            private: true,
        }
    }

    /// The available repositories the installation exposes, in the order
    /// GitHub returned them (which is the order the form lists them).
    fn available_repos() -> Vec<GhRepo> {
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

    #[test]
    fn valid_form_yields_normalized_creation_data() {
        let validated = validate(&valid_form(), &available_repos(), now()).unwrap();

        assert_eq!(
            validated,
            ValidatedCreateLink {
                description: "AI coding workshop".into(),
                internal_note: Some("Keep this note".into()),
                permission: ghinvite_core::Permission::Push,
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
    fn blank_internal_note_and_unchecked_approval_normalize_to_none_and_false() {
        let form = CreateLinkForm {
            internal_note: Some("   ".into()),
            approval_required: None,
            ..valid_form()
        };

        let validated = validate(&form, &available_repos(), now()).unwrap();

        assert_eq!(validated.internal_note, None);
        assert!(!validated.approval_required);
    }

    #[test]
    fn invalid_description_reports_field_error_with_summary() {
        let form = CreateLinkForm {
            description: Some("   ".into()),
            ..valid_form()
        };

        let errors = validate(&form, &available_repos(), now()).unwrap_err();

        assert_eq!(
            errors.summary,
            vec!["Fix the highlighted fields before creating this invitation link.".to_string()]
        );
        assert_eq!(
            errors.description.as_deref(),
            Some(
                "Description is required. Use short, single-line admin-only context for this invitation link."
            )
        );
    }

    #[test]
    fn into_view_values_preserves_submitted_values_verbatim() {
        let form = CreateLinkForm {
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
    fn description_validation_trims_valid_input() {
        assert_eq!(
            validate_description(Some("  AI coding workshop  ")).unwrap(),
            "AI coding workshop"
        );
    }

    #[test]
    fn description_validation_requires_value() {
        let err = validate_description(None).unwrap_err();
        assert_eq!(
            err.message,
            "Description is required. Use short, single-line admin-only context for this invitation link."
        );

        let err = validate_description(Some("   ")).unwrap_err();
        assert_eq!(
            err.message,
            "Description is required. Use short, single-line admin-only context for this invitation link."
        );
    }

    #[test]
    fn description_validation_rejects_multiline_text() {
        let err = validate_description(Some("AI\nworkshop")).unwrap_err();
        assert_eq!(err.message, "Description must be a single line.");

        let err = validate_description(Some("AI workshop\n")).unwrap_err();
        assert_eq!(err.message, "Description must be a single line.");

        let err = validate_description(Some("\rAI workshop")).unwrap_err();
        assert_eq!(err.message, "Description must be a single line.");
    }

    #[test]
    fn description_validation_rejects_over_120_characters() {
        let too_long = "x".repeat(121);
        let err = validate_description(Some(&too_long)).unwrap_err();
        assert_eq!(err.message, "Description must be 120 characters or fewer.");
    }

    fn with_max_uses(raw: Option<&str>) -> CreateLinkForm {
        CreateLinkForm {
            max_uses: raw.map(str::to_string),
            ..valid_form()
        }
    }

    fn max_uses_error(raw: &str) -> String {
        let errors = validate(&with_max_uses(Some(raw)), &available_repos(), now()).unwrap_err();
        assert_eq!(errors.description, None, "only max use should fail");
        assert_eq!(errors.permission, None, "only max use should fail");
        assert_eq!(errors.repo_scope, None, "only max use should fail");
        assert!(
            !errors.summary.is_empty(),
            "summary line accompanies field errors"
        );
        errors
            .max_uses
            .unwrap_or_else(|| panic!("max_uses={raw:?} should produce a max use error"))
    }

    #[test]
    fn max_uses_blank_or_missing_means_unlimited() {
        for raw in [None, Some(""), Some("   "), Some("\t\n")] {
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
    fn max_uses_rejects_zero_negative_decimal_and_text() {
        for raw in [
            "0", "00", "-1", "-0", "1.5", "2.0", "abc", "5x", "+5", "1e3", "1,000",
        ] {
            assert_eq!(
                max_uses_error(raw),
                "Max use must be a whole number of 1 or more.",
                "max_uses={raw:?}"
            );
        }
    }

    #[test]
    fn max_uses_rejects_values_too_large_to_store() {
        for raw in ["4294967296", "99999999999999999999999"] {
            assert_eq!(
                max_uses_error(raw),
                "Max use is too large. Use a smaller number, or leave it blank for unlimited invitation requests.",
                "max_uses={raw:?}"
            );
        }
    }

    fn with_expires_in_days(raw: Option<&str>) -> CreateLinkForm {
        CreateLinkForm {
            expires_in_days: raw.map(str::to_string),
            ..valid_form()
        }
    }

    fn expires_in_days_error(raw: &str) -> String {
        let errors =
            validate(&with_expires_in_days(Some(raw)), &available_repos(), now()).unwrap_err();
        assert_eq!(errors.description, None, "only expiration should fail");
        assert_eq!(errors.permission, None, "only expiration should fail");
        assert_eq!(errors.max_uses, None, "only expiration should fail");
        assert_eq!(errors.repo_scope, None, "only expiration should fail");
        assert!(
            !errors.summary.is_empty(),
            "summary line accompanies field errors"
        );
        errors
            .expires_in_days
            .unwrap_or_else(|| panic!("expires_in_days={raw:?} should produce an expiration error"))
    }

    #[test]
    fn expires_in_days_blank_or_missing_means_no_expiration() {
        for raw in [None, Some(""), Some("   "), Some("\t\n")] {
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
    fn expires_in_days_rejects_zero_negative_decimal_and_text() {
        for raw in [
            "0", "00", "-1", "-30", "1.5", "30.0", "abc", "30d", "+30", "1e2", "1,000",
        ] {
            assert_eq!(
                expires_in_days_error(raw),
                "Expiration must be a whole number of days, 1 or more.",
                "expires_in_days={raw:?}"
            );
        }
    }

    #[test]
    fn expires_in_days_rejects_values_that_cannot_produce_a_timestamp() {
        // Past chrono's representable calendar, past i64 days, past u64 digits.
        for raw in ["100000000", "9223372036854775808", "18446744073709551616"] {
            assert_eq!(
                expires_in_days_error(raw),
                "Expiration is too far in the future. Use fewer days, or leave it blank for no expiration.",
                "expires_in_days={raw:?}"
            );
        }
    }

    #[test]
    fn validate_reports_every_field_error_at_once() {
        let form = CreateLinkForm {
            description: Some("".into()),
            permission: Some("owner".into()),
            max_uses: Some("0".into()),
            expires_in_days: Some("abc".into()),
            repo_ids: vec![],
            ..valid_form()
        };

        let errors = validate(&form, &available_repos(), now()).unwrap_err();

        assert_eq!(
            errors.summary,
            vec!["Fix the highlighted fields before creating this invitation link.".to_string()]
        );
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

    const PERMISSION_UNSUPPORTED: &str =
        "Choose a supported permission level: pull, triage, push, maintain, or admin.";

    fn with_permission(permission: &str) -> CreateLinkForm {
        CreateLinkForm {
            permission: Some(permission.into()),
            ..valid_form()
        }
    }

    /// Only the permission level should fail; returns its message.
    fn permission_error(permission: &str) -> String {
        let errors = validate(&with_permission(permission), &available_repos(), now()).unwrap_err();
        assert_eq!(errors.description, None, "only permission should fail");
        assert_eq!(errors.max_uses, None, "only permission should fail");
        assert_eq!(errors.expires_in_days, None, "only permission should fail");
        assert_eq!(errors.repo_scope, None, "only permission should fail");
        assert!(
            !errors.summary.is_empty(),
            "summary line accompanies field errors"
        );
        errors.permission.unwrap_or_else(|| {
            panic!("permission={permission:?} should produce a permission error")
        })
    }

    #[test]
    fn permission_accepts_every_supported_level() {
        use ghinvite_core::Permission;

        for (raw, expected) in [
            ("pull", Permission::Pull),
            ("triage", Permission::Triage),
            ("push", Permission::Push),
            ("maintain", Permission::Maintain),
            ("admin", Permission::Admin),
        ] {
            let validated = validate(&with_permission(raw), &available_repos(), now()).unwrap();
            assert_eq!(validated.permission, expected, "permission={raw:?}");
        }
    }

    #[test]
    fn permission_rejects_tampered_values() {
        // Unknown names, GitHub-adjacent names that are not collaborator
        // levels, casing/whitespace variants (the select submits exact lowercase
        // values, so anything else did not come from the form), and blank.
        for raw in [
            "owner", "write", "read", "Push", "PUSH", " push", "push ", "", "  ", "push\n",
        ] {
            assert_eq!(
                permission_error(raw),
                PERMISSION_UNSUPPORTED,
                "permission={raw:?}"
            );
        }
    }

    #[test]
    fn permission_rejects_a_missing_key_as_tampering() {
        // The select always submits a value; a POST without the key is a
        // tampered submission and must get the inline error, not a bare 400.
        let form = CreateLinkForm {
            permission: None,
            ..valid_form()
        };
        let errors = validate(&form, &available_repos(), now()).unwrap_err();
        assert_eq!(errors.permission.as_deref(), Some(PERMISSION_UNSUPPORTED));
        assert_eq!(errors.description, None, "only permission should fail");
        assert_eq!(errors.repo_scope, None, "only permission should fail");
    }

    #[test]
    fn tampered_permission_is_reported_alongside_other_field_errors() {
        let form = CreateLinkForm {
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
        assert_eq!(errors.repo_scope, None);
    }

    const REPO_SCOPE_REQUIRED: &str = "Repository scope is required. Select at least one available repository for this invitation link.";

    fn with_repo_ids(repo_ids: Vec<u64>) -> CreateLinkForm {
        CreateLinkForm {
            repo_ids,
            ..valid_form()
        }
    }

    /// Only the repository scope should fail; returns its message.
    fn repo_scope_error(repo_ids: Vec<u64>) -> String {
        let errors =
            validate(&with_repo_ids(repo_ids.clone()), &available_repos(), now()).unwrap_err();
        assert_eq!(
            errors.description, None,
            "only repository scope should fail"
        );
        assert_eq!(errors.permission, None, "only repository scope should fail");
        assert_eq!(errors.max_uses, None, "only repository scope should fail");
        assert_eq!(
            errors.expires_in_days, None,
            "only repository scope should fail"
        );
        assert!(
            !errors.summary.is_empty(),
            "summary line accompanies field errors"
        );
        errors.repo_scope.unwrap_or_else(|| {
            panic!("repo_ids={repo_ids:?} should produce a repository scope error")
        })
    }

    #[test]
    fn repo_scope_rejects_empty_selection() {
        assert_eq!(repo_scope_error(vec![]), REPO_SCOPE_REQUIRED);
    }

    #[test]
    fn repo_scope_rejects_selection_of_only_unknown_repositories() {
        assert_eq!(repo_scope_error(vec![999]), REPO_SCOPE_REQUIRED);
        assert_eq!(repo_scope_error(vec![999, 1000]), REPO_SCOPE_REQUIRED);
    }

    #[test]
    fn repo_scope_rejects_any_selection_when_no_repositories_are_available() {
        let errors = validate(&with_repo_ids(vec![10]), &[], now()).unwrap_err();

        assert_eq!(errors.repo_scope.as_deref(), Some(REPO_SCOPE_REQUIRED));
    }

    #[test]
    fn repo_scope_accepts_available_repositories_with_their_full_names() {
        let validated = validate(&with_repo_ids(vec![11]), &available_repos(), now()).unwrap();

        assert_eq!(validated.repos, vec![scope_repo(11, "acme/web")]);
    }

    #[test]
    fn repo_scope_drops_unknown_repositories_and_keeps_the_available_ones() {
        let validated = validate(
            &with_repo_ids(vec![999, 10, 1000]),
            &available_repos(),
            now(),
        )
        .unwrap();

        assert_eq!(validated.repos, vec![scope_repo(10, "acme/api")]);
    }

    #[test]
    fn repo_scope_follows_available_repository_order_and_ignores_duplicates() {
        let validated = validate(
            &with_repo_ids(vec![12, 10, 12, 11, 10]),
            &available_repos(),
            now(),
        )
        .unwrap();

        assert_eq!(
            validated.repos,
            vec![
                scope_repo(10, "acme/api"),
                scope_repo(11, "acme/web"),
                scope_repo(12, "acme/docs"),
            ]
        );
    }
}
