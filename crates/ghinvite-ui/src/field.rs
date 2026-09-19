//! The `Field` form primitive shared by server-rendered forms.
//!
//! Every mutation in ghinvite is a native `<form method="post">` rendered on the
//! server (ADR 0001). [`Field`] is the one place that knows how a labelled
//! control, its help text, and its field-level error fit together, so pages
//! never hand-write two near-identical control branches to toggle
//! `aria-invalid`. (Form *models* and their validators live in modules such
//! as [`crate::link_form`]; this module is markup only.)
//!
//! Controls that `Field` does not render itself (a `<select>`, a checkbox
//! group) reuse the same building blocks — [`described_by`], [`help_text`],
//! [`error_text`], [`listeners`] — so every control follows one id scheme:
//! `{id}`, `{id}-help`, `{id}-error`.
//!
//! The optional `oninput` / `onchange` / `onblur` props exist for the browser
//! island, which renders these same components reactively. Server-side
//! rendering ignores listeners entirely, so with or without handlers the SSR
//! markup is byte-identical.

use dioxus::prelude::*;
use dioxus_field::{Binding, FieldContext, FieldMetaValues};

use crate::components::field::{Field as RegistryField, FieldAppearance, FieldError, FieldLabel};
use crate::components::input::Input;

/// Which control a [`Field`] renders, together with the constraints that only
/// make sense for that control. Attributes shared by every kind (`placeholder`,
/// `required`, `value`) live on [`FieldProps`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// Single-line `<input type="text">`.
    Text { maxlength: Option<u32> },
    /// Native registry input; initially used only by link creation's description.
    RegistryText { maxlength: Option<u32> },
    /// Registry input backed by a raw-text parsed binding in the island.
    RegistryNumber { min: Option<i64>, max: Option<i64> },
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
    /// Field-level error under the help text. Native controls render a paragraph;
    /// registry controls populate an always-mounted live region through metadata.
    pub error: Option<String>,
    /// Fired as the value changes (island only; ignored by SSR).
    pub oninput: Option<EventHandler<FormEvent>>,
    /// Fired when the value is committed (island only; ignored by SSR).
    pub onchange: Option<EventHandler<FormEvent>>,
    /// Fired when focus leaves the control (island only; ignored by SSR).
    pub onblur: Option<EventHandler<FocusEvent>>,
    /// Registry control binding. The server supplies only the preserved value.
    pub binding: Option<Binding<String>>,
}

/// The three listeners a reactive caller may attach to one control, bundled
/// so a form component can take one optional set per control instead of
/// three props each. Every handler is optional; the server renders with
/// [`ControlHandlers::default`] (no listeners), and since SSR never emits
/// listener attributes the markup is identical either way.
#[derive(Clone, Default, PartialEq)]
pub struct ControlHandlers {
    /// Fired as the value changes.
    pub oninput: Option<EventHandler<FormEvent>>,
    /// Fired when the value is committed (a `<select>` or checkbox change).
    pub onchange: Option<EventHandler<FormEvent>>,
    /// Fired when focus leaves the control.
    pub onblur: Option<EventHandler<FocusEvent>>,
}

impl ControlHandlers {
    /// The listeners as spreadable attributes (see [`listeners`]).
    pub(crate) fn attributes(&self) -> Vec<Attribute> {
        listeners(self.oninput, self.onchange, self.onblur)
    }
}

/// A labelled form control with optional help text and field-level error.
///
/// Renders the label, control, help and error in that order. Native controls
/// derive ARIA from their conditional paragraphs. Registry controls delegate to
/// a separate hook-backed Field context with an always-mounted error region.
#[component]
pub fn Field(props: FieldProps) -> Element {
    if matches!(
        props.kind,
        FieldKind::RegistryText { .. } | FieldKind::RegistryNumber { .. }
    ) {
        return rsx! { RegistryInputField { field: props } };
    }
    let has_error = props.error.is_some();
    let help_id = format!("{}-help", props.id);
    let error_id = format!("{}-error", props.id);
    let described_by = described_by(
        props.help.as_ref().map(|_| help_id.as_str()),
        props.error.as_ref().map(|_| error_id.as_str()),
    );
    let listeners = listeners(props.oninput, props.onchange, props.onblur);

    let control = match &props.kind {
        FieldKind::RegistryText { .. } | FieldKind::RegistryNumber { .. } => unreachable!(),
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
                    ..listeners,
                    "{props.value}"
                }
            }
        }
        FieldKind::Text { maxlength } => input_control(
            &props,
            described_by,
            listeners,
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
            listeners,
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
            label { class: "label text-base-content", r#for: "{props.id}",
                span { class: "label-text font-medium", "{props.label}" }
            }
            {control}
            {help_text(&help_id, props.help.as_deref())}
            {error_text(&error_id, props.error.as_deref())}
        }
    }
}

/// Keep registry hooks in their own scope, separate from the native controls.
#[component]
fn RegistryInputField(field: FieldProps) -> Element {
    let props = field;
    let (input_type, inputmode, maxlength, min, max) = match props.kind {
        FieldKind::RegistryText { maxlength } => ("text", None, maxlength, None, None),
        FieldKind::RegistryNumber { min, max } => ("number", Some("numeric"), None, min, max),
        _ => unreachable!(),
    };
    let value = use_memo(use_reactive(&props.value, |value| value));
    let help_id = format!("{}-help", props.id);
    let error_id = format!("{}-error", props.id);
    let label_id = format!("{}-label", props.id);
    let described_by = described_by(
        props.help.as_ref().map(|_| help_id.as_str()),
        props.error.as_ref().map(|_| error_id.as_str()),
    );
    let metadata = FieldMetaValues {
        id: Some(props.id.into()),
        name: Some(props.name.into()),
        required: props.required,
        errors: props
            .error
            .iter()
            .map(|error| error.as_str().into())
            .collect(),
        ..Default::default()
    };
    let bound = props.binding.is_some();
    let context = props
        .binding
        .map_or_else(FieldContext::empty, FieldContext::new)
        .with_meta_values(metadata);
    rsx! {
        RegistryField { context, appearance: FieldAppearance::None, class: "form-control gap-2",
            FieldLabel { id: label_id,
                span { class: "label-text font-medium", "{props.label}" }
            }
            Input {
                // The island reads directly from the binding; only SSR needs
                // the preserved-value override without a form producer.
                value: if bound { None } else { Some(value.into()) },
                r#type: input_type,
                inputmode,
                class: "input-bordered w-full",
                maxlength: maxlength.map(|limit| limit.to_string()),
                min: min.map(|limit| limit.to_string()),
                max: max.map(|limit| limit.to_string()),
                placeholder: props.placeholder,
                // Later sibling registrations cannot supply first-pass SSR
                // associations. Keep these explicit until upstream supports it.
                aria_describedby: described_by,
                aria_errormessage: props.error.as_ref().map(|_| error_id.clone()),
            }
            {help_text(&help_id, props.help.as_deref())}
            FieldError { id: error_id, class: "text-sm font-medium" }
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
fn input_control(
    props: &FieldProps,
    described_by: Option<String>,
    listeners: Vec<Attribute>,
    attrs: InputAttrs,
) -> Element {
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
            ..listeners,
        }
    }
}

/// The event listeners a reactive caller asked for, as spreadable attributes.
/// A `None` handler adds nothing, so a control without handlers carries no
/// listener at all (rather than a no-op one).
pub(crate) fn listeners(
    oninput: Option<EventHandler<FormEvent>>,
    onchange: Option<EventHandler<FormEvent>>,
    onblur: Option<EventHandler<FocusEvent>>,
) -> Vec<Attribute> {
    let mut attrs = Vec::new();
    if let Some(handler) = oninput {
        attrs.push(dioxus_elements::events::oninput(handler));
    }
    if let Some(handler) = onchange {
        attrs.push(dioxus_elements::events::onchange(handler));
    }
    if let Some(handler) = onblur {
        attrs.push(dioxus_elements::events::onblur(handler));
    }
    attrs
}

/// Space-separated `aria-describedby` target list, or `None` when the control
/// has nothing to point at (so the attribute is omitted entirely).
pub(crate) fn described_by(help_id: Option<&str>, error_id: Option<&str>) -> Option<String> {
    let ids: Vec<&str> = help_id.into_iter().chain(error_id).collect();
    if ids.is_empty() {
        None
    } else {
        Some(ids.join(" "))
    }
}

/// The help paragraph under a control: `<p id="{id}-help">`, or nothing.
pub(crate) fn help_text(id: &str, text: Option<&str>) -> Element {
    rsx! {
        {text.map(|text| rsx! {
            p { id: "{id}", class: "text-sm text-base-content/65", "{text}" }
        })}
    }
}

/// The field-level error paragraph under a control: `<p id="{id}-error">`,
/// or nothing. The `id` is what the control's `aria-describedby` points at.
pub(crate) fn error_text(id: &str, message: Option<&str>) -> Element {
    rsx! {
        {message.map(|message| rsx! {
            p { id: "{id}", class: "text-sm font-medium text-error", "{message}" }
        })}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_number_metadata_is_complete_after_one_rebuild() {
        for name in ["max_uses", "expires_in_days"] {
            for (raw, error) in [
                ("7", None),
                ("", None),
                ("0", Some("Must be positive.")),
                ("abc", Some("Must be positive.")),
            ] {
                for (min, max) in [(None, None), (Some(1), None), (Some(1), Some(365))] {
                    for help in [None, Some("Optional guardrail.")] {
                        let mut vdom = VirtualDom::new_with_props(
                            move || {
                                rsx! {
                                    Field {
                                        id: "guardrail",
                                        name,
                                        label: "Guardrail",
                                        kind: FieldKind::RegistryNumber { min, max },
                                        value: raw,
                                        help: help.map(str::to_string),
                                        error: error.map(str::to_string),
                                    }
                                }
                            },
                            (),
                        );
                        vdom.rebuild_in_place();
                        let html = dioxus_ssr::render(&vdom);
                        let input = html
                            .split_once("<input ")
                            .unwrap()
                            .1
                            .split_once('>')
                            .unwrap()
                            .0;
                        for attribute in [
                            "id=\"guardrail\"".to_string(),
                            format!("name=\"{name}\""),
                            format!("value=\"{raw}\""),
                            "type=\"number\"".to_string(),
                            "inputmode=\"numeric\"".to_string(),
                            "aria-labelledby=\"guardrail-label\"".to_string(),
                            format!("aria-invalid=\"{}\"", error.is_some()),
                        ] {
                            assert!(input.contains(&attribute), "missing {attribute}: {html}");
                        }
                        for (attribute, value) in [("min", min), ("max", max)] {
                            if let Some(value) = value {
                                assert!(input.contains(&format!(" {attribute}=\"{value}\"")));
                            } else {
                                assert!(!input.contains(&format!(" {attribute}=")));
                            }
                        }
                        assert!(!input.contains("maxlength="));
                        assert!(!input.contains(" required="));
                        assert_eq!(input.contains("input-error"), error.is_some());
                        let descriptions = [
                            help.map(|_| "guardrail-help"),
                            error.map(|_| "guardrail-error"),
                        ]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" ");
                        if descriptions.is_empty() {
                            assert!(!input.contains("aria-describedby="));
                        } else {
                            assert!(
                                input.contains(&format!("aria-describedby=\"{descriptions}\""))
                            );
                        }
                        assert_eq!(
                            input.contains("aria-errormessage=\"guardrail-error\""),
                            error.is_some()
                        );
                        let label = html
                            .split_once("<label ")
                            .unwrap()
                            .1
                            .split_once('>')
                            .unwrap()
                            .0;
                        assert!(label.contains("id=\"guardrail-label\""));
                        assert!(label.contains("for=\"guardrail\""));
                        assert!(html.contains(">Guardrail</span>"));
                        let region = html.split_once("id=\"guardrail-error\"").unwrap().1;
                        let (attributes, content) = region.split_once('>').unwrap();
                        assert!(attributes.contains("aria-live=\"polite\""));
                        assert!(!attributes.contains("hidden"));
                        if let Some(error) = error {
                            assert!(content.starts_with(&format!("<div>{error}</div></div>")));
                        } else {
                            assert!(content.starts_with("</div>"));
                        }
                        for id in ["guardrail", "guardrail-label", "guardrail-error"] {
                            assert_eq!(html.matches(&format!(" id=\"{id}\"")).count(), 1);
                        }
                        assert_eq!(
                            html.matches(" id=\"guardrail-help\"").count(),
                            usize::from(help.is_some())
                        );
                        assert_eq!(html.matches("<input ").count(), 1);
                    }
                }
            }
        }
    }

    #[test]
    fn registry_text_metadata_is_complete_after_one_rebuild() {
        for help in [None, Some("Visible only to admins.")] {
            for error in [None, Some("Description is required.")] {
                for required in [false, true] {
                    let mut vdom = VirtualDom::new_with_props(
                        move || {
                            rsx! {
                                Field {
                                    id: "description",
                                    name: "admin_description",
                                    label: "Description",
                                    kind: FieldKind::RegistryText { maxlength: Some(120) },
                                    value: "Preserved value",
                                    required,
                                    help: help.map(str::to_string),
                                    error: error.map(str::to_string),
                                }
                            }
                        },
                        (),
                    );
                    // No settling render: later sibling registration must not
                    // be needed for the server's initial associations.
                    vdom.rebuild_in_place();
                    let html = dioxus_ssr::render(&vdom);
                    let input = html.split_once("<input").unwrap().1;
                    let input = input.split('>').next().unwrap();
                    let label = html.split_once("<label").unwrap().1;
                    let label = label.split('>').next().unwrap();
                    assert!(input.contains("id=\"description\""), "{html}");
                    assert!(input.contains("name=\"admin_description\""));
                    assert!(input.contains("value=\"Preserved value\""));
                    assert!(input.contains("maxlength=\"120\""));
                    assert_eq!(input.contains(" required="), required);
                    assert!(label.contains("id=\"description-label\""));
                    assert!(label.contains("for=\"description\""));
                    assert!(html.contains(">Description</span>"));
                    assert!(input.contains("aria-labelledby=\"description-label\""));
                    assert!(!input.contains("aria-label="));
                    assert!(input.contains(&format!("aria-invalid=\"{}\"", error.is_some())));
                    assert_eq!(input.contains("input-error"), error.is_some());
                    let expected_description = match (help, error) {
                        (Some(_), Some(_)) => Some("description-help description-error"),
                        (Some(_), None) => Some("description-help"),
                        (None, Some(_)) => Some("description-error"),
                        (None, None) => None,
                    };
                    if let Some(ids) = expected_description {
                        assert!(input.contains(&format!("aria-describedby=\"{ids}\"")));
                    } else {
                        assert!(!input.contains("aria-describedby="));
                    }
                    assert_eq!(
                        input.contains("aria-errormessage=\"description-error\""),
                        error.is_some()
                    );
                    if let Some(help) = help {
                        assert!(html.contains(&format!("<p id=\"description-help\" class=\"text-sm text-base-content/65\">{help}</p>")));
                    } else {
                        assert!(!html.contains("description-help"));
                    }
                    let region = html.split_once("id=\"description-error\"").unwrap().1;
                    let (attributes, content) = region.split_once('>').unwrap();
                    assert!(attributes.contains("aria-live=\"polite\""));
                    assert!(attributes.contains("text-error"));
                    assert!(!attributes.contains("hidden"));
                    if let Some(error) = error {
                        assert!(content.starts_with(&format!("<div>{error}</div></div>")));
                    } else {
                        assert!(content.starts_with("</div>"));
                    }
                    for id in ["description", "description-label", "description-error"] {
                        assert_eq!(html.matches(&format!(" id=\"{id}\"")).count(), 1);
                    }
                    assert_eq!(html.matches("<input").count(), 1);
                }
            }
        }
    }

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
        assert!(html.contains("<label class=\"label text-base-content\" for=\"description\">"));
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
    fn field_with_handlers_renders_the_same_markup_as_without() {
        // The island attaches behaviour through these props; on the server
        // nothing changes (SSR without `pre_render` emits no listeners).
        let plain = render(|| {
            rsx! {
                Field {
                    id: "description",
                    name: "description",
                    label: "Description",
                    kind: FieldKind::Text { maxlength: Some(120) },
                    value: "AI coding workshop",
                    required: true,
                    help: "Visible only to admins.",
                    error: "Description is required.",
                }
            }
        });
        let wired = render(|| {
            rsx! {
                Field {
                    id: "description",
                    name: "description",
                    label: "Description",
                    kind: FieldKind::Text { maxlength: Some(120) },
                    value: "AI coding workshop",
                    required: true,
                    help: "Visible only to admins.",
                    error: "Description is required.",
                    oninput: move |_event: FormEvent| {},
                    onchange: move |_event: FormEvent| {},
                    onblur: move |_event: FocusEvent| {},
                }
            }
        });

        assert_eq!(wired, plain);
        assert!(!wired.contains("oninput"));
        assert!(!wired.contains("onchange"));
        assert!(!wired.contains("onblur"));
    }

    #[test]
    fn textarea_field_with_handlers_renders_the_same_markup_as_without() {
        let plain = render(|| {
            rsx! {
                Field {
                    id: "internal_note",
                    name: "internal_note",
                    label: "Internal note",
                    kind: FieldKind::Textarea { rows: None },
                    value: "Keep this note",
                    help: "Optional admin-only notes.",
                }
            }
        });
        let wired = render(|| {
            rsx! {
                Field {
                    id: "internal_note",
                    name: "internal_note",
                    label: "Internal note",
                    kind: FieldKind::Textarea { rows: None },
                    value: "Keep this note",
                    help: "Optional admin-only notes.",
                    oninput: move |_event: FormEvent| {},
                    onblur: move |_event: FocusEvent| {},
                }
            }
        });

        assert_eq!(wired, plain);
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
