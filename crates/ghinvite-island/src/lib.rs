//! Browser island for the new invitation link form (ADR 0001).
//!
//! The server renders `ghinvite_ui::links::LinkCreateFormPage`: the plain
//! `<form>` inside `<div id="link-form-island">`, a JSON props blob, and one
//! `<script type="module">` tag. When that module loads, [`LinkFormIsland`]
//! mounts on the container and re-renders **the same `LinkCreateForm`
//! component** the server rendered — with a dioform 0.7 form behind it.
//! First-frame byte parity is covered by `tests/parity.rs`, within the numeric
//! restoration boundary below. After mounting:
//!
//! - every control is bound to the shared `CreateLinkForm` model, and the
//!   shared `register_validators` run on commit (leaving a field) through the
//!   `dioform` facade — the very same rules the server runs through
//!   `dioform-core`;
//! - the form's `onsubmit` is dioform's `progressive_submit()`: the native
//!   POST is cancelled only while a known blocker exists (a validation error
//!   or a numeric field whose text does not parse); otherwise the browser
//!   POSTs to the existing route exactly as it does without JavaScript;
//! - failed responses with non-empty errors configure
//!   `FormConfig::browser_rejection((), …)`: supplied typed values become the
//!   draft and baseline, and errors show as a prior rejected attempt without
//!   marking fields touched or starting a fake submission. Mixed client and
//!   server-only diagnostics survive together; rerenders do not replay them;
//! - parsed numeric bindings consume invalid raw input from that rejection on
//!   mount, without simulating input. Related edits clear restored field errors.
//!   A fresh core preflight retires the rejection and allows an unchanged retry
//!   of server-only errors when client/native validation passes. `ParseBlocked`
//!   returns before core preflight and retains the rejection until a later
//!   submit can reach it.
//!
//! Invalid numeric raw input is restored only with failed-response errors.
//! Valid noncanonical text such as `"007"` or `" 7 "` still formats as `"7"`
//! from the typed model; preserving that spelling or first-frame byte parity
//! for it is not promised. Chromium may display restored `"abc"` as empty,
//! while the binding retains its parse blocker. Registry controls still need
//! application-owned ARIA help/error associations for first-pass SSR.
//!
//! This crate is the only place the `dioform` facade and (on wasm32)
//! `dioxus-web` appear. The component compiles natively so the parity test
//! can render it with `dioxus_ssr`; the browser entrypoint is `main.rs`.

pub mod takeover;

use dioform::prelude::*;
use dioxus::prelude::*;
use ghinvite_ui::field::ControlHandlers;
use ghinvite_ui::link_form::{
    CreateLinkForm, LinkFormErrors, LinkFormIslandProps, LinkFormValues, SUMMARY_MESSAGE,
    parse_expires_in_days, parse_max_uses, register_validators,
};
use ghinvite_ui::links::{LinkCreateForm, LinkFormHandlers, RepositoryScopeHandlers};

/// The reactive new invitation link form.
///
/// Takes the props the server serialised into the page and renders
/// `LinkCreateForm` from dioform state: values come from the bindings, errors
/// from the visible validation errors (and the numeric parse errors) folded
/// into `LinkFormErrors` exactly the way the server folds its own, and the
/// listeners are the bindings' handlers. See the crate docs for the lifecycle.
#[component]
pub fn LinkFormIsland(props: LinkFormIslandProps) -> Element {
    use_context_provider(|| ghinvite_ui::csrf::CsrfToken(props.csrf_token.clone()));
    let fields = CreateLinkForm::fields();

    // The form: the model built from the preserved values, the shared
    // validators registered once, validation on commit (leaving a field).
    // Field ids are the shared components' (`description`, `permission`, ...),
    // not dioform's derived ones, so no id namespace is configured.
    let form = use_form_config(form_config(&props));

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
            description: Some(commit_on_focus_exit(description.into())),
            internal_note: ControlHandlers {
                oninput: Some(EventHandler::new(internal_note.oninput())),
                onchange: None,
                onblur: Some(EventHandler::new(internal_note.onblur())),
            },
            permission: Some(commit_on_focus_exit(permission.into())),
            approval_required: ControlHandlers {
                oninput: None,
                onchange: Some(EventHandler::new(approval_required.onchange())),
                onblur: Some(EventHandler::new(approval_required.onblur())),
            },
            max_uses: Some(commit_on_focus_exit(max_uses.into())),
            expires_in_days: Some(commit_on_focus_exit(expires_in_days.into())),
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

/// Configure a fresh page once; hook rerenders do not restore the response again.
fn form_config(props: &LinkFormIslandProps) -> FormConfig<CreateLinkForm> {
    let repos = props.repos.clone();
    let now = props.now;
    let mut config = FormConfig::new(initial_model(&props.values))
        .validation_mode(ValidationMode::on_commit())
        .register_core(move |core| register_validators(core, &repos, now));
    if !props.values.errors.is_empty() {
        let values = props.values.clone();
        config = config.browser_rejection((), move |_| browser_rejection(&values));
    }
    config
}

/// Preserve commit-on-blur for registry controls, including unchanged fields.
/// Called inside the one-time handlers hook so callback allocations stay stable.
fn commit_on_focus_exit<T: 'static>(binding: dioxus_field::Binding<T>) -> dioxus_field::Binding<T> {
    let writer = binding.clone();
    dioxus_field::Binding::new(
        binding.read,
        Callback::new(move |(value, origin)| writer.write(value, origin)),
        Callback::new(|()| {}),
    )
    .with_focus_exit(Callback::new(move |()| {
        binding.commit();
        binding.focus_exit();
    }))
}

/// The typed model the preserved values describe — the same mapping the
/// server's `CreateLinkSubmission::to_model` applies to the raw POST: text
/// verbatim, a numeric guardrail that fails to parse left blank (its raw text
/// is restored separately by [`browser_rejection`]).
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

/// Restore the previous browser POST without starting a managed submission.
/// Errors coexist with client validation; parsed bindings consume raw text
/// without pretending the user edited it. A fresh preflight retires the prior
/// rejection, while a mounted parse blocker retains it until corrected.
fn browser_rejection(values: &LinkFormValues) -> BrowserRejection<CreateLinkForm> {
    let fields = CreateLinkForm::fields();
    let mut rejection =
        BrowserRejection::new(SubmitErrors::new(submit_errors_from(&values.errors)));
    if parse_max_uses(&values.max_uses).is_err() {
        rejection = rejection.raw_field(fields.max_uses(), values.max_uses.clone());
    }
    if parse_expires_in_days(&values.expires_in_days).is_err() {
        rejection = rejection.raw_field(fields.expires_in_days(), values.expires_in_days.clone());
    }
    rejection
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
    use ghinvite_ui::link_form::{
        EXPIRES_IN_DAYS_NOT_POSITIVE, MAX_USES_NOT_POSITIVE, RepositoryChoice,
    };
    use std::{cell::Cell, rc::Rc};

    /// Inspect the production configuration after its parsed hooks have mounted.
    fn check_mounted_config(
        values: LinkFormValues,
        check: impl Fn(&FormHandle<CreateLinkForm>, [(String, Option<String>); 2]) + Clone + 'static,
    ) {
        let props = LinkFormIslandProps {
            csrf_token: None,
            action: "/console/accounts/acme/links".into(),
            values,
            repos: vec![RepositoryChoice {
                id: 10,
                full_name: "acme/api".into(),
            }],
            now: "2026-05-04T12:00:00Z".parse().unwrap(),
        };
        let checked = Rc::new(Cell::new(false));
        let rendered = checked.clone();
        let mut vdom = VirtualDom::new_with_props(
            move |()| {
                let form = use_form_config(form_config(&props));
                let fields = CreateLinkForm::fields();
                let max_uses =
                    use_number_with(&form, fields.max_uses(), parse_max_uses, format_count);
                let expires_in_days = use_number_with(
                    &form,
                    fields.expires_in_days(),
                    parse_expires_in_days,
                    format_count,
                );

                let state = form.state_snapshot();
                assert_eq!(state.draft().baseline(), &form.snapshot());
                assert_eq!(state.draft().current(), state.draft().baseline());
                assert!(!form.is_dirty());
                assert!(!form.is_submitting());
                assert!(
                    !form
                        .submit_availability()
                        .contains(SubmitBlocker::InFlightSubmission)
                );
                for metadata in [
                    form.field_metadata(fields.description()),
                    form.field_metadata(fields.internal_note()),
                    form.field_metadata(fields.permission()),
                    form.field_metadata(fields.approval_required()),
                    form.field_metadata(fields.max_uses()),
                    form.field_metadata(fields.expires_in_days()),
                    form.field_metadata(fields.repo_ids()),
                ] {
                    assert!(!metadata.is_touched());
                    assert!(!metadata.is_blurred());
                }
                check(
                    &form,
                    [
                        (
                            max_uses.value(),
                            max_uses
                                .parse_error()
                                .map(|error| error.message().to_string()),
                        ),
                        (
                            expires_in_days.value(),
                            expires_in_days
                                .parse_error()
                                .map(|error| error.message().to_string()),
                        ),
                    ],
                );
                rendered.set(true);
                VNode::empty()
            },
            (),
        );
        vdom.rebuild_in_place();
        assert!(checked.get(), "the form probe must render");
    }

    #[test]
    fn rejected_config_restores_clean_baseline_and_prior_attempt_after_numeric_hooks_mount() {
        let server_error = "A link with this description already exists.";
        check_mounted_config(
            LinkFormValues {
                description: "  Workshop  ".into(),
                permission: "push".into(),
                approval_required: true,
                max_uses: "abc".into(),
                expires_in_days: "00".into(),
                internal_note: "Keep this note".into(),
                selected_repo_ids: vec![10],
                errors: LinkFormErrors {
                    summary: vec![SUMMARY_MESSAGE.into()],
                    description: Some(server_error.into()),
                    max_uses: Some(MAX_USES_NOT_POSITIVE.into()),
                    expires_in_days: Some(EXPIRES_IN_DAYS_NOT_POSITIVE.into()),
                    ..LinkFormErrors::default()
                },
            },
            move |form, numbers| {
                assert_eq!(
                    form.snapshot(),
                    CreateLinkForm {
                        description: "  Workshop  ".into(),
                        internal_note: "Keep this note".into(),
                        permission: "push".into(),
                        approval_required: true,
                        max_uses: None,
                        expires_in_days: None,
                        repo_ids: vec![10],
                    }
                );
                assert_eq!(form.submit_attempt_count(), 1);
                assert_eq!(form.last_submit_status(), Some(SubmitStatus::Rejected));
                assert_eq!(
                    numbers,
                    [
                        ("abc".into(), Some(MAX_USES_NOT_POSITIVE.into())),
                        ("00".into(), Some(EXPIRES_IN_DAYS_NOT_POSITIVE.into())),
                    ]
                );
                assert_eq!(form.parse_errors().len(), 2);
                assert!(
                    form.submit_availability()
                        .contains(SubmitBlocker::ParseErrors)
                );
                assert!(
                    form.visible_validation_errors()
                        .iter()
                        .any(|error| error.error() == server_error)
                );
            },
        );
    }

    #[test]
    fn fresh_config_has_no_prior_attempt_or_parse_errors_after_numeric_hooks_mount() {
        check_mounted_config(LinkFormValues::default(), |form, numbers| {
            assert_eq!(form.submit_attempt_count(), 0);
            assert_eq!(form.last_submit_status(), None);
            assert_eq!(numbers, [(String::new(), None), ("30".into(), None)]);
            assert!(form.parse_errors().is_empty());
            assert!(form.visible_validation_errors().is_empty());
        });
    }

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
