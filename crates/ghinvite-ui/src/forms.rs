//! Form primitives shared by server-rendered forms.
//!
//! Every mutation in ghinvite is a native `<form method="post">` rendered on the
//! server (ADR 0001). [`Field`] is the one place that knows how a labelled
//! control, its help text, and its field-level error fit together, so pages
//! never hand-write two near-identical control branches to toggle
//! `aria-invalid`.

use dioxus::prelude::*;

/// Which control a [`Field`] renders, together with the constraints that only
/// make sense for that control. Attributes shared by every kind (`placeholder`,
/// `required`, `value`) live on [`FieldProps`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// Single-line `<input type="text">`.
    Text { maxlength: Option<u32> },
    /// Multi-line `<textarea>`; the value is rendered as its text content.
    Textarea { rows: Option<u32> },
    /// `<input type="number" inputmode="numeric">`.
    Number { min: Option<i64>, max: Option<i64> },
}

#[derive(Clone, PartialEq, Props)]
pub struct FieldProps {
    /// DOM id of the control. Also prefixes the `{id}-help` and `{id}-error`
    /// ids that `aria-describedby` points at.
    pub id: String,
    /// Form field name submitted with the POST.
    pub name: String,
    /// Visible label text.
    pub label: String,
    pub kind: FieldKind,
    /// Current value, preserved when the form is re-rendered after a
    /// validation failure.
    #[props(default)]
    pub value: String,
    #[props(default)]
    pub required: bool,
    pub placeholder: Option<String>,
    /// Help text rendered as `<p id="{id}-help">` under the control.
    pub help: Option<String>,
    /// Field-level error rendered as `<p id="{id}-error">` under the help text.
    /// Its presence is what sets `aria-invalid` and the error styling.
    pub error: Option<String>,
}

/// A labelled form control with optional help text and field-level error.
///
/// Renders one `<div class="form-control">` containing the label, the control,
/// then help and error paragraphs (each only when present). `aria-invalid` and
/// `aria-describedby` are derived from which of those paragraphs exist, so the
/// control markup is written exactly once per kind. Props in, markup out; no
/// hooks.
#[component]
pub fn Field(props: FieldProps) -> Element {
    let has_error = props.error.is_some();
    let help_id = format!("{}-help", props.id);
    let error_id = format!("{}-error", props.id);
    let described_by = described_by(
        props.help.as_ref().map(|_| help_id.as_str()),
        props.error.as_ref().map(|_| error_id.as_str()),
    );

    let control = match &props.kind {
        FieldKind::Textarea { rows } => {
            let class = if has_error {
                "textarea textarea-bordered textarea-error w-full"
            } else {
                "textarea textarea-bordered w-full"
            };
            let rows = rows.map(|n| n.to_string());
            rsx! {
                textarea {
                    id: "{props.id}",
                    name: "{props.name}",
                    class: "{class}",
                    required: props.required,
                    rows: rows,
                    placeholder: props.placeholder.clone(),
                    aria_invalid: if has_error { "true" },
                    aria_describedby: described_by,
                    "{props.value}"
                }
            }
        }
        FieldKind::Text { maxlength } => input_control(
            &props,
            described_by,
            InputAttrs {
                input_type: "text",
                inputmode: None,
                maxlength: maxlength.map(|n| n.to_string()),
                min: None,
                max: None,
            },
        ),
        FieldKind::Number { min, max } => input_control(
            &props,
            described_by,
            InputAttrs {
                input_type: "number",
                inputmode: Some("numeric"),
                maxlength: None,
                min: min.map(|n| n.to_string()),
                max: max.map(|n| n.to_string()),
            },
        ),
    };

    rsx! {
        div { class: "form-control gap-2",
            label { class: "label", r#for: "{props.id}",
                span { class: "label-text font-medium", "{props.label}" }
            }
            {control}
            {props.help.as_ref().map(|help| rsx! {
                p { id: "{help_id}", class: "text-sm text-base-content/65", "{help}" }
            })}
            {props.error.as_ref().map(|message| rsx! {
                p { id: "{error_id}", class: "text-sm font-medium text-error", "{message}" }
            })}
        }
    }
}

/// The attributes that differ between the `<input>`-based kinds. Numeric
/// values are pre-formatted so SSR emits them quoted (`min="1"`), matching the
/// hand-written markup they replace.
struct InputAttrs {
    input_type: &'static str,
    inputmode: Option<&'static str>,
    maxlength: Option<String>,
    min: Option<String>,
    max: Option<String>,
}

/// The single `<input>` element used by every non-textarea kind.
fn input_control(props: &FieldProps, described_by: Option<String>, attrs: InputAttrs) -> Element {
    let class = if props.error.is_some() {
        "input input-bordered input-error w-full"
    } else {
        "input input-bordered w-full"
    };
    rsx! {
        input {
            id: "{props.id}",
            r#type: attrs.input_type,
            name: "{props.name}",
            value: "{props.value}",
            class: "{class}",
            required: props.required,
            inputmode: attrs.inputmode,
            maxlength: attrs.maxlength,
            min: attrs.min,
            max: attrs.max,
            placeholder: props.placeholder.clone(),
            aria_invalid: if props.error.is_some() { "true" },
            aria_describedby: described_by,
        }
    }
}

/// Space-separated `aria-describedby` target list, or `None` when the control
/// has nothing to point at (so the attribute is omitted entirely).
fn described_by(help_id: Option<&str>, error_id: Option<&str>) -> Option<String> {
    let ids: Vec<&str> = help_id.into_iter().chain(error_id).collect();
    if ids.is_empty() {
        None
    } else {
        Some(ids.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render<F>(component: F) -> String
    where
        F: 'static + Clone + Fn() -> Element + Send,
    {
        crate::testing::render(component)
    }

    #[test]
    fn text_field_renders_label_control_and_help_with_describedby() {
        let html = render(|| {
            rsx! {
                Field {
                    id: "description",
                    name: "description",
                    label: "Description",
                    kind: FieldKind::Text { maxlength: Some(120) },
                    value: "AI coding workshop",
                    required: true,
                    placeholder: "Purpose or audience",
                    help: "Visible only to admins.",
                }
            }
        });

        assert!(html.contains("<div class=\"form-control gap-2\">"));
        assert!(html.contains("<label class=\"label\" for=\"description\">"));
        assert!(html.contains("<span class=\"label-text font-medium\">Description</span>"));
        assert!(html.contains("id=\"description\""));
        assert!(html.contains("type=\"text\""));
        assert!(html.contains("name=\"description\""));
        assert!(html.contains("value=\"AI coding workshop\""));
        assert!(html.contains("class=\"input input-bordered w-full\""));
        assert!(!html.contains("input-error"));
        assert!(html.contains("required"));
        assert!(html.contains("maxlength=\"120\""));
        assert!(html.contains("placeholder=\"Purpose or audience\""));
        assert!(html.contains("aria-describedby=\"description-help\""));
        assert!(html.contains(
            "<p id=\"description-help\" class=\"text-sm text-base-content/65\">Visible only to admins.</p>"
        ));
        assert!(!html.contains("aria-invalid"));
        assert!(!html.contains("description-error"));
        assert!(!html.contains("inputmode"));
        assert_eq!(html.matches("<input").count(), 1);
    }

    #[test]
    fn text_field_with_error_sets_aria_invalid_and_describes_help_then_error() {
        let html = render(|| {
            rsx! {
                Field {
                    id: "description",
                    name: "description",
                    label: "Description",
                    kind: FieldKind::Text { maxlength: Some(120) },
                    value: "   ",
                    required: true,
                    help: "Visible only to admins.",
                    error: "Description is required.",
                }
            }
        });

        assert!(html.contains("aria-invalid=\"true\""));
        assert!(html.contains("aria-describedby=\"description-help description-error\""));
        assert!(html.contains("class=\"input input-bordered input-error w-full\""));
        assert!(html.contains("id=\"description-help\""));
        assert!(html.contains(
            "<p id=\"description-error\" class=\"text-sm font-medium text-error\">Description is required.</p>"
        ));
        assert!(
            html.find("id=\"description-help\"").unwrap()
                < html.find("id=\"description-error\"").unwrap()
        );
        assert_eq!(html.matches("<input").count(), 1);
    }

    #[test]
    fn field_without_help_or_error_omits_describedby_and_messages() {
        let html = render(|| {
            rsx! {
                Field {
                    id: "expires_in_days",
                    name: "expires_in_days",
                    label: "Expires in days",
                    kind: FieldKind::Number { min: Some(1), max: None },
                    value: "30",
                }
            }
        });

        assert!(!html.contains("aria-describedby"));
        assert!(!html.contains("aria-invalid"));
        assert!(!html.contains("expires_in_days-help"));
        assert!(!html.contains("expires_in_days-error"));
        assert!(!html.contains("<p"));
        assert!(!html.contains("required"));
        assert!(!html.contains("placeholder"));
    }

    #[test]
    fn field_with_error_but_no_help_describes_by_error_only() {
        let html = render(|| {
            rsx! {
                Field {
                    id: "max_uses",
                    name: "max_uses",
                    label: "Max use",
                    kind: FieldKind::Number { min: Some(1), max: None },
                    value: "0",
                    error: "Max use must be a positive whole number.",
                }
            }
        });

        assert!(html.contains("aria-invalid=\"true\""));
        assert!(html.contains("aria-describedby=\"max_uses-error\""));
        assert!(html.contains("id=\"max_uses-error\""));
        assert!(!html.contains("max_uses-help"));
    }

    #[test]
    fn number_field_renders_type_min_max_and_inputmode() {
        let html = render(|| {
            rsx! {
                Field {
                    id: "max_uses",
                    name: "max_uses",
                    label: "Max use",
                    kind: FieldKind::Number { min: Some(1), max: Some(365) },
                    value: "7",
                    placeholder: "Unlimited",
                    help: "Blank means unlimited invitation requests.",
                }
            }
        });

        assert!(html.contains("type=\"number\""));
        assert!(html.contains("inputmode=\"numeric\""));
        assert!(html.contains("min=\"1\""));
        assert!(html.contains("max=\"365\""));
        assert!(html.contains("name=\"max_uses\" value=\"7\""));
        assert!(html.contains("class=\"input input-bordered w-full\""));
        assert!(html.contains("placeholder=\"Unlimited\""));
        assert!(!html.contains("maxlength"));
        assert!(!html.contains("rows"));
    }

    #[test]
    fn textarea_field_renders_value_as_content_with_rows() {
        let html = render(|| {
            rsx! {
                Field {
                    id: "internal_note",
                    name: "internal_note",
                    label: "Internal note",
                    kind: FieldKind::Textarea { rows: Some(4) },
                    value: "Keep this note",
                    placeholder: "Why this link exists",
                    help: "Optional admin-only notes.",
                }
            }
        });

        assert!(html.contains("<textarea"));
        assert!(!html.contains("<input"));
        assert!(html.contains("id=\"internal_note\""));
        assert!(html.contains("name=\"internal_note\""));
        assert!(html.contains("class=\"textarea textarea-bordered w-full\""));
        assert!(html.contains("rows=\"4\""));
        assert!(html.contains("placeholder=\"Why this link exists\""));
        assert!(html.contains("aria-describedby=\"internal_note-help\""));
        assert!(html.contains(">Keep this note</textarea>"));
        assert!(!html.contains("value=\"Keep this note\""));
        assert!(!html.contains("type="));
    }

    #[test]
    fn textarea_field_with_error_uses_textarea_error_class() {
        let html = render(|| {
            rsx! {
                Field {
                    id: "internal_note",
                    name: "internal_note",
                    label: "Internal note",
                    kind: FieldKind::Textarea { rows: None },
                    value: "",
                    error: "Internal note is too long.",
                }
            }
        });

        assert!(html.contains("class=\"textarea textarea-bordered textarea-error w-full\""));
        assert!(html.contains("aria-invalid=\"true\""));
        assert!(html.contains("aria-describedby=\"internal_note-error\""));
        assert!(!html.contains("rows"));
    }

    #[test]
    fn field_escapes_user_supplied_values() {
        let html = render(|| {
            rsx! {
                Field {
                    id: "description",
                    name: "description",
                    label: "Description",
                    kind: FieldKind::Text { maxlength: None },
                    value: "<script>alert(1)</script>",
                }
            }
        });

        assert!(!html.contains("<script>"));
        assert!(html.contains("value=\"&#60;script&#62;alert(1)&#60;/script&#62;\""));
        assert!(!html.contains("maxlength"));
    }
}
