//! Shared native invitation-request field validation (ADR 0001).
use dioform_core::{Form, FormCore};
use dioform_derive::Form;
use ghinvite_core::admission::MAX_JUSTIFICATION_BYTES;

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
        if value.trim().len() > MAX_JUSTIFICATION_BYTES {
            vec![format!("Shorten your justification to at most {MAX_JUSTIFICATION_BYTES} UTF-8 bytes, then submit again.")]
        } else {
            vec![]
        }
    });
    core.validate_for_submit();
    core.validation_errors()
        .first()
        .map(|error| error.error().clone())
}
