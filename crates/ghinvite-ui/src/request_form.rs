//! Shared native invitation-request field validation (ADR 0001).
use dioform_core::{Form, FormCore};
use dioform_derive::Form;
use ghinvite_core::admission::MAX_JUSTIFICATION_CHARS;

#[derive(Clone, Debug, PartialEq, Form)]
#[form(crate = "::dioform_core")]
pub struct RequestForm {
    pub justification: String,
}

pub fn justification_error(value: &str) -> Option<String> {
    let mut core = FormCore::new(RequestForm {
        justification: value.to_owned(),
    });
    core.register_sync_field_validator(RequestForm::fields().justification(), "length", |value, _| {
        if value.trim().chars().count() > MAX_JUSTIFICATION_CHARS {
            vec![format!("Shorten your justification to at most {MAX_JUSTIFICATION_CHARS} characters, then submit again.")]
        } else {
            vec![]
        }
    });
    core.validate_for_submit();
    core.validation_errors()
        .first()
        .map(|error| error.error().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn justification_limit_counts_characters_instead_of_utf8_bytes() {
        assert!(justification_error(&"é".repeat(500)).is_none());
        assert!(justification_error(&"é".repeat(501)).is_some());
        assert!(justification_error(&format!("  {}  ", "文".repeat(500))).is_none());
    }
}
