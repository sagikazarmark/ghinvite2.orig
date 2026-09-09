//! Validation for the new invitation link form.
//!
//! [`validate`] is the deep end of the form: it accepts the raw
//! [`CreateLinkForm`] the browser posted and returns either the guardrails
//! the command facade may act on ([`ValidatedCreateLink`]) or every field
//! error at once ([`LinkFormErrors`]) so the route can re-render the form with
//! the admin's values preserved and each problem shown next to its field.
//!
//! Adding a rule is one validator function returning `Result<T, FieldError>`,
//! one slot in the match inside [`validate`], and one field on
//! [`LinkFormErrors`].

use crate::views::links::{LinkFormErrors, LinkFormValues};
use chrono::{DateTime, Utc};
use serde::Deserialize;

/// Raw POST body of the new invitation link form, exactly as submitted.
///
/// Text fields are `Option<String>` so a missing key and an empty value both
/// reach the validators unchanged; nothing is parsed or trimmed during
/// deserialization.
#[derive(Debug, Deserialize)]
pub struct CreateLinkForm {
    pub description: Option<String>,
    pub permission: String,
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
    /// exactly what they typed when correcting a mistake.
    pub fn into_view_values(self, errors: LinkFormErrors) -> LinkFormValues {
        LinkFormValues {
            description: self.description.unwrap_or_default(),
            permission: self.permission,
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
    pub approval_required: bool,
}

/// Validate a submitted new invitation link form.
///
/// Runs every rule and reports all failures together. `now` is unused until
/// the expiration rule lands but is part of the contract so callers anchor
/// time once per request.
pub fn validate(
    form: &CreateLinkForm,
    _now: DateTime<Utc>,
) -> Result<ValidatedCreateLink, LinkFormErrors> {
    let description = validate_description(form.description.as_deref());

    match description {
        Ok(description) => Ok(ValidatedCreateLink {
            description,
            internal_note: normalize_internal_note(form.internal_note.as_deref()),
            approval_required: form.approval_required.is_some(),
        }),
        Err(error) => Err(LinkFormErrors {
            summary: vec![SUMMARY.to_string()],
            description: Some(error.message),
        }),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_form() -> CreateLinkForm {
        CreateLinkForm {
            description: Some("AI coding workshop".into()),
            permission: "push".into(),
            approval_required: Some("true".into()),
            max_uses: Some("7".into()),
            expires_in_days: Some("45".into()),
            internal_note: Some("  Keep this note  ".into()),
            repo_ids: vec![10],
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-04T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn valid_form_yields_normalized_creation_data() {
        let validated = validate(&valid_form(), now()).unwrap();

        assert_eq!(
            validated,
            ValidatedCreateLink {
                description: "AI coding workshop".into(),
                internal_note: Some("Keep this note".into()),
                approval_required: true,
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

        let validated = validate(&form, now()).unwrap();

        assert_eq!(validated.internal_note, None);
        assert!(!validated.approval_required);
    }

    #[test]
    fn invalid_description_reports_field_error_with_summary() {
        let form = CreateLinkForm {
            description: Some("   ".into()),
            ..valid_form()
        };

        let errors = validate(&form, now()).unwrap_err();

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
}
