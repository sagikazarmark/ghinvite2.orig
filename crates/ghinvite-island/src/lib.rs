//! Browser island for the new invitation link form (ADR 0001).
//!
//! The server renders `ghinvite_ui::links::LinkCreateFormPage`: the plain
//! `<form>` inside `<div id="link-form-island">`, a JSON props blob, and one
//! `<script type="module">` tag. When that module loads, [`LinkFormIsland`]
//! mounts on the container and re-renders **the same `LinkCreateForm`
//! component** the server rendered — with a dioform form behind it — so the
//! first frame is byte-identical to the server's HTML (`tests/parity.rs`) and
//! the admin sees no change. After that:
//!
//! - every control is bound to the shared `CreateLinkForm` model, and the
//!   shared `register_validators` run on commit (leaving a field) through the
//!   `dioform` facade — the very same rules the server runs through
//!   `dioform-core`;
//! - the form's `onsubmit` is dioform's `progressive_submit()`: the native
//!   POST is cancelled only while a known blocker exists (a validation error
//!   or a numeric field whose text does not parse); otherwise the browser
//!   POSTs to the existing route exactly as it does without JavaScript;
//! - the server's errors from a failed POST are seeded once on mount through
//!   the public submission lifecycle, so they show immediately and clear when
//!   the field is edited (dioform's stale-submit-error rule).
//!
//! This crate is the only place the `dioform` facade and (on wasm32)
//! `dioxus-web` appear. The component compiles natively so the parity test
//! can render it with `dioxus_ssr`; the browser entrypoint is `main.rs`.

#![forbid(unsafe_code)]

use dioform::advanced::SubmitAttempt;
use dioform::prelude::*;
use dioxus::prelude::*;
use ghinvite_ui::field::ControlHandlers;
use ghinvite_ui::link_form::{
    CreateLinkForm, LinkFormErrors, LinkFormIslandProps, LinkFormValues, SUMMARY_MESSAGE,
    parse_expires_in_days, parse_max_uses, register_validators,
};
use ghinvite_ui::links::{LinkCreateForm, LinkFormHandlers, RepositoryScopeHandlers};

/// A parsed numeric binding of the form: `Option<u32>` in the model, the
/// admin's raw text in the input.
type NumberBinding = ParsedTextBinding<CreateLinkForm, Option<u32>>;

/// The reactive new invitation link form.
///
/// Takes the props the server serialised into the page and renders
/// `LinkCreateForm` from dioform state: values come from the bindings, errors
/// from the visible validation errors (and the numeric parse errors) folded
/// into `LinkFormErrors` exactly the way the server folds its own, and the
/// listeners are the bindings' handlers. See the crate docs for the lifecycle.
#[component]
pub fn LinkFormIsland(props: LinkFormIslandProps) -> Element {
    let fields = CreateLinkForm::fields();

    // The form: the model built from the preserved values, the shared
    // validators registered once, validation on commit (leaving a field).
    // Field ids are the shared components' (`description`, `permission`, ...),
    // not dioform's derived ones, so no id namespace is configured.
    let repos = props.repos.clone();
    let now = props.now;
    let form = use_form_config(
        FormConfig::new(initial_model(&props.values))
            .validation_mode(ValidationMode::on_commit())
            .register_core(move |core| register_validators(core, &repos, now)),
    );

    let description = form.text(fields.description());
    let internal_note = form.textarea(fields.internal_note());
    let permission = form.select(fields.permission());
    let approval_required = form.checkbox(fields.approval_required());
    let max_uses = use_number_with(&form, fields.max_uses(), parse_max_uses, format_count);
    let expires_in_days = use_number_with(
        &form,
        fields.expires_in_days(),
        parse_expires_in_days,
        format_count,
    );
    let repo_ids = use_multi_select(&form, fields.repo_ids());
    let submit = form.progressive_submit();
    let browser = form.browser_submit(props.action.clone());

    // Once, on mount, before anything below reads state: seed the server's
    // errors and the raw numeric text that did not parse.
    {
        let form = form.clone();
        let values = props.values.clone();
        let max_uses = max_uses.clone();
        let expires_in_days = expires_in_days.clone();
        use_hook(move || seed_server_state(&form, &values, &max_uses, &expires_in_days));
    }

    // The listeners, created once: `EventHandler`s live in this scope until
    // it unmounts, so minting them per render would grow without bound. The
    // bindings they capture are handles onto the one form, so the first
    // render's clones stay valid.
    let handlers = {
        let submit = submit.clone();
        let description = description.clone();
        let internal_note = internal_note.clone();
        let permission = permission.clone();
        let approval_required = approval_required.clone();
        let max_uses = max_uses.clone();
        let expires_in_days = expires_in_days.clone();
        let repo_ids = repo_ids.clone();
        use_hook(move || LinkFormHandlers {
            onsubmit: Some(EventHandler::new(move |event: FormEvent| {
                // Allowed → the browser POSTs natively; Blocked → dioform has
                // already cancelled the event and made the errors visible.
                let _ = submit.on_submit(event);
            })),
            description: ControlHandlers {
                oninput: Some(EventHandler::new(description.oninput())),
                onchange: None,
                onblur: Some(EventHandler::new(description.onblur())),
            },
            internal_note: ControlHandlers {
                oninput: Some(EventHandler::new(internal_note.oninput())),
                onchange: None,
                onblur: Some(EventHandler::new(internal_note.onblur())),
            },
            permission: ControlHandlers {
                oninput: None,
                onchange: Some(EventHandler::new(permission.onchange())),
                onblur: Some(EventHandler::new(permission.onblur())),
            },
            approval_required: ControlHandlers {
                oninput: None,
                onchange: Some(EventHandler::new(approval_required.onchange())),
                onblur: Some(EventHandler::new(approval_required.onblur())),
            },
            max_uses: ControlHandlers {
                oninput: Some(EventHandler::new(max_uses.oninput())),
                onchange: None,
                onblur: Some(EventHandler::new(max_uses.onblur())),
            },
            expires_in_days: ControlHandlers {
                oninput: Some(EventHandler::new(expires_in_days.oninput())),
                onchange: None,
                onblur: Some(EventHandler::new(expires_in_days.onblur())),
            },
            repo_scope: RepositoryScopeHandlers {
                onchange: Some(EventHandler::new({
                    let repo_ids = repo_ids.clone();
                    move |(id, checked): (u64, bool)| repo_ids.on_change(id, checked)
                })),
                onblur: Some(EventHandler::new({
                    let repo_ids = repo_ids.clone();
                    move |_event: FocusEvent| {
                        repo_ids.on_commit();
                        repo_ids.on_focus_exit();
                    }
                })),
            },
        })
    };

    // The view model, read from the form. These reads subscribe this scope to
    // the form's selectors, so the island re-renders on every relevant change.
    let mut errors = LinkFormErrors::default();
    // Parse errors first, then validation errors — the order the server
    // attaches them in (`to_model`, then `validate_for_submit`); a field shows
    // its first message.
    if let Some(parse) = max_uses.parse_error() {
        errors.attach(
            Some(&fields.max_uses().identity()),
            parse.message().to_string(),
        );
    }
    if let Some(parse) = expires_in_days.parse_error() {
        errors.attach(
            Some(&fields.expires_in_days().identity()),
            parse.message().to_string(),
        );
    }
    for error in form.visible_validation_errors() {
        errors.attach(error.field_identity().as_ref(), error.error().clone());
    }
    let values = LinkFormValues {
        description: description.value(),
        permission: permission.value(),
        approval_required: approval_required.checked(),
        // Raw text while it fails to parse, the formatted value otherwise.
        max_uses: max_uses.value(),
        expires_in_days: expires_in_days.value(),
        internal_note: internal_note.value(),
        selected_repo_ids: repo_ids.value(),
        errors,
    };

    rsx! {
        LinkCreateForm {
            action: browser.action().to_string(),
            form: values,
            repos: props.repos.clone(),
            handlers: handlers,
        }
    }
}

/// The typed model the preserved values describe — the same mapping the
/// server's `CreateLinkSubmission::to_model` applies to the raw POST: text
/// verbatim, a numeric guardrail that fails to parse left blank (its raw text
/// is re-applied to the binding by [`seed_server_state`]).
fn initial_model(values: &LinkFormValues) -> CreateLinkForm {
    CreateLinkForm {
        description: values.description.clone(),
        internal_note: values.internal_note.clone(),
        permission: values.permission.clone(),
        approval_required: values.approval_required,
        max_uses: parse_max_uses(&values.max_uses).unwrap_or(None),
        expires_in_days: parse_expires_in_days(&values.expires_in_days).unwrap_or(None),
        repo_ids: values.selected_repo_ids.clone(),
    }
}

/// How a parsed guardrail is written back into its input: the number, or an
/// empty field for "blank" (`None`).
fn format_count(value: &Option<u32>) -> String {
    value.map(|count| count.to_string()).unwrap_or_default()
}

/// Reproduce, in the mounted form, the state the server rendered.
///
/// Server errors go through the public submission lifecycle:
/// `begin_submission` runs the shared validators first. If they reject the
/// preserved values (`Blocked`) the submit attempt has made their errors
/// visible — and since they are the same rules the server ran, they are the
/// server's errors. If they pass (`Started`), the server knew something the
/// browser cannot (a duplicate description, a suspended installation, ...):
/// its messages are recorded as submit errors on the fields the server
/// attached them to, and summary-only messages as form-level errors. Either
/// way the errors clear as the admin edits the field, like any dioform error.
///
/// Raw numeric text that does not parse ("abc", "0") cannot live in the
/// typed model, so it is re-applied to the parsed binding as if typed: the
/// input shows the text and the same parse error the server reported, and
/// submission stays blocked until it is corrected. (Chromium sanitises such
/// text out of a `type="number"` input, so there the field shows empty with
/// the error; the server remains the authority.) This happens *after*
/// `begin_submission`, which would otherwise be blocked by the parse error
/// before running any validator.
fn seed_server_state(
    form: &FormHandle<CreateLinkForm>,
    values: &LinkFormValues,
    max_uses: &NumberBinding,
    expires_in_days: &NumberBinding,
) {
    if !values.errors.is_empty()
        && let SubmitAttempt::Started(snapshot) = form.begin_submission()
    {
        let errors = SubmitErrors::new(submit_errors_from(&values.errors));
        if errors.is_empty() {
            // Nothing to attach (a summary with only the generic line);
            // release the in-flight submission so it cannot block a real one.
            form.finish_submission();
        } else {
            form.finish_submission_with_errors(snapshot, errors);
        }
    }

    if parse_max_uses(&values.max_uses).is_err() {
        max_uses.on_input(values.max_uses.clone());
    }
    if parse_expires_in_days(&values.expires_in_days).is_err() {
        expires_in_days.on_input(values.expires_in_days.clone());
    }
}

/// The server's `LinkFormErrors` as dioform submit errors: each field slot on
/// its field path, and every summary entry other than the generic summary
/// line as a form-level error (the generic line is re-derived by
/// `LinkFormErrors::attach` when the errors are read back).
fn submit_errors_from(errors: &LinkFormErrors) -> Vec<SubmitError<CreateLinkForm, String>> {
    let fields = CreateLinkForm::fields();
    let mut submit_errors = Vec::new();
    if let Some(message) = &errors.description {
        submit_errors.push(SubmitError::field(fields.description(), message.clone()));
    }
    if let Some(message) = &errors.permission {
        submit_errors.push(SubmitError::field(fields.permission(), message.clone()));
    }
    if let Some(message) = &errors.max_uses {
        submit_errors.push(SubmitError::field(fields.max_uses(), message.clone()));
    }
    if let Some(message) = &errors.expires_in_days {
        submit_errors.push(SubmitError::field(
            fields.expires_in_days(),
            message.clone(),
        ));
    }
    if let Some(message) = &errors.repo_scope {
        submit_errors.push(SubmitError::field(fields.repo_ids(), message.clone()));
    }
    for message in errors.summary.iter().filter(|m| *m != SUMMARY_MESSAGE) {
        submit_errors.push(SubmitError::form(message.clone()));
    }
    submit_errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_model_mirrors_the_servers_to_model() {
        let model = initial_model(&LinkFormValues {
            description: "  AI  ".into(),
            permission: "owner".into(),
            approval_required: true,
            max_uses: "abc".into(),
            expires_in_days: " 45 ".into(),
            internal_note: "note".into(),
            selected_repo_ids: vec![999, 10],
            errors: LinkFormErrors::default(),
        });

        assert_eq!(
            model,
            CreateLinkForm {
                description: "  AI  ".into(),
                internal_note: "note".into(),
                permission: "owner".into(),
                approval_required: true,
                max_uses: None,
                expires_in_days: Some(45),
                repo_ids: vec![999, 10],
            }
        );
    }

    #[test]
    fn format_count_writes_blank_for_none() {
        assert_eq!(format_count(&None), "");
        assert_eq!(format_count(&Some(7)), "7");
    }

    #[test]
    fn submit_errors_map_slots_to_fields_and_extra_summary_lines_to_the_form() {
        let fields = CreateLinkForm::fields();
        let errors = LinkFormErrors {
            summary: vec![SUMMARY_MESSAGE.to_string(), "whole form".to_string()],
            description: Some("d".to_string()),
            permission: None,
            max_uses: Some("m".to_string()),
            expires_in_days: None,
            repo_scope: Some("r".to_string()),
        };

        let mapped = submit_errors_from(&errors);

        let targets: Vec<(Option<dioform::advanced::FieldIdentity>, String)> = mapped
            .into_iter()
            .map(|error| {
                let field = error.target().as_field().cloned();
                (field, error.into_error())
            })
            .collect();
        assert_eq!(
            targets,
            vec![
                (Some(fields.description().identity()), "d".to_string()),
                (Some(fields.max_uses().identity()), "m".to_string()),
                (Some(fields.repo_ids().identity()), "r".to_string()),
                (None, "whole form".to_string()),
            ]
        );
    }

    #[test]
    fn generic_summary_line_alone_maps_to_nothing() {
        let errors = LinkFormErrors {
            summary: vec![SUMMARY_MESSAGE.to_string()],
            ..LinkFormErrors::default()
        };
        assert!(submit_errors_from(&errors).is_empty());
    }
}
