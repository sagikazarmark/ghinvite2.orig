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
//! - values carrying errors, or numeric text the typed model cannot hold,
//!   configure `FormConfig::browser_rejection((), …)`: supplied typed values
//!   become the draft and baseline, and errors show as a prior rejected attempt
//!   without marking fields touched or starting a fake submission. Mixed client
//!   and server-only diagnostics survive together; rerenders do not replay them;
//! - parsed numeric bindings consume invalid raw input from that rejection on
//!   mount, without simulating input. Related edits clear restored field errors.
//!   A fresh core preflight retires the rejection and allows an unchanged retry
//!   of server-only errors when client/native validation passes. `ParseBlocked`
//!   returns before core preflight and retains the rejection until a later
//!   submit can reach it;
//! - the repository picker's buttons (Search, Previous/Next page) stay native
//!   POSTs that only re-render the form on the server, so they skip that
//!   preflight ([`PickerSubmit`]); the island never filters repositories
//!   itself, and its props carry only the page the server listed.
//!
//! Raw restoration is not gated on the server having rejected anything: the
//! takeover adopts what the admin typed before the island mounted, so a `0` on
//! a form the server never saw has to survive too, and the typed model would
//! otherwise blank it into "unlimited". Its parse error is then visible from
//! the first frame rather than on leaving the field.
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
    parse_abuse_threshold_per_hour, parse_budget_share_pct, parse_expires_in_days,
    parse_invitation_lifetime_hours, parse_max_uses, register_target_validators,
};
use ghinvite_ui::links::{LinkCreateForm, LinkFormHandlers, RepositoryScopeHandlers};
use std::cell::Cell;
use std::rc::Rc;

/// Which native POSTs the island lets through without its submit preflight.
///
/// The repository picker's buttons submit the create form so the server can
/// re-render it with another page of repositories; a blocker elsewhere in the
/// form (an empty description, say) must not stop the admin from searching.
/// The form's default button is a search too, but Enter only searches from
/// the repository filter: pressed in another field it does nothing, rather
/// than reloading the page the admin is filling in.
///
/// Clicks fire before the submit they cause, so the click handlers arm the
/// pass and the submit handler spends it.
#[derive(Clone, Default)]
pub struct PickerSubmit {
    helper: Rc<Cell<bool>>,
    enter_in_query: Rc<Cell<bool>>,
}

impl PickerSubmit {
    /// A picker button was clicked: its submit goes through natively.
    pub fn helper_clicked(&self) {
        self.helper.set(true);
    }

    /// A key went down in the repository filter.
    pub fn query_key(&self, enter: bool) {
        self.enter_in_query.set(enter);
    }

    /// The default button was clicked by Enter's implicit submission. Returns
    /// whether that submission may go ahead: only when Enter was pressed in
    /// the repository filter, which then searches like its button.
    pub fn default_clicked(&self) -> bool {
        let search = self.enter_in_query.replace(false);
        if search {
            self.helper.set(true);
        }
        search
    }

    /// Spend the pass for the submission now under way, if a picker button
    /// caused it.
    pub fn take(&self) -> bool {
        self.enter_in_query.set(false);
        self.helper.replace(false)
    }
}

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

    let email_domains = form.text(fields.email_domains());
    let description = form.text(fields.description());
    let internal_note = form.textarea(fields.internal_note());
    let permission = form.select(fields.permission());
    let approval_required = form.checkbox(fields.approval_required());
    let expiration_timestamp = form.text(fields.expiration_timestamp());
    let access_lifetime_hours = form.text(fields.access_lifetime_hours());
    let invitation_lifetime_seconds = form.text(fields.invitation_lifetime_seconds());
    let never_expires = form.checkbox(fields.never_expires());
    let include_organization = form.checkbox(fields.include_organization());
    let team_ids = use_multi_select(&form, fields.team_ids());
    let max_uses = use_number_with(&form, fields.max_uses(), parse_max_uses, format_count);
    let expires_in_days = use_number_with(
        &form,
        fields.expires_in_days(),
        parse_expires_in_days,
        format_count,
    );
    let invitation_lifetime_hours = use_number_with(
        &form,
        fields.invitation_lifetime_hours(),
        parse_invitation_lifetime_hours,
        format_count,
    );
    let abuse_threshold_per_hour = use_number_with(
        &form,
        fields.abuse_threshold_per_hour(),
        parse_abuse_threshold_per_hour,
        format_count,
    );
    let budget_share_pct = use_number_with(
        &form,
        fields.budget_share_pct(),
        parse_budget_share_pct,
        format_count,
    );
    let repo_ids = use_multi_select(&form, fields.repo_ids());
    let repo_grants = use_multi_select(&form, fields.repo_grants());
    let submit = form.progressive_submit();
    let browser = form.browser_submit(props.action.clone());

    // The listeners, created once: `EventHandler`s live in this scope until
    // it unmounts, so minting them per render would grow without bound. The
    // bindings they capture are handles onto the one form, so the first
    // render's clones stay valid.
    let handlers = {
        let submit = submit.clone();
        let email_domains = email_domains.clone();
        let description = description.clone();
        let internal_note = internal_note.clone();
        let permission = permission.clone();
        let approval_required = approval_required.clone();
        let expiration_timestamp = expiration_timestamp.clone();
        let access_lifetime_hours = access_lifetime_hours.clone();
        let invitation_lifetime_seconds = invitation_lifetime_seconds.clone();
        let never_expires = never_expires.clone();
        let include_organization = include_organization.clone();
        let team_ids = team_ids.clone();
        let max_uses = max_uses.clone();
        let expires_in_days = expires_in_days.clone();
        let invitation_lifetime_hours = invitation_lifetime_hours.clone();
        let abuse_threshold_per_hour = abuse_threshold_per_hour.clone();
        let budget_share_pct = budget_share_pct.clone();
        let repo_ids = repo_ids.clone();
        let repo_grants = repo_grants.clone();
        let offered_repos = props.repos.clone();
        let helper_permission = permission.clone();
        let picker = PickerSubmit::default();
        use_hook(move || LinkFormHandlers {
            onsubmit: Some(EventHandler::new({
                let picker = picker.clone();
                move |event: FormEvent| {
                    // A picker button only asks the server for another page.
                    if picker.take() {
                        return;
                    }
                    // Allowed → the browser POSTs natively; Blocked → dioform
                    // has already cancelled the event and made the errors
                    // visible.
                    let _ = submit.on_submit(event);
                }
            })),
            picker_submit: Some(EventHandler::new({
                let picker = picker.clone();
                move |_event: MouseEvent| picker.helper_clicked()
            })),
            default_submit: Some(EventHandler::new({
                let picker = picker.clone();
                move |event: MouseEvent| {
                    if !picker.default_clicked() {
                        event.prevent_default();
                    }
                }
            })),
            picker_query_keydown: Some(EventHandler::new(move |event: KeyboardEvent| {
                picker.query_key(event.key() == Key::Enter);
            })),
            email_domains: Some(commit_on_focus_exit(email_domains.into())),
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
            expiration_timestamp: Some(commit_on_focus_exit(expiration_timestamp.into())),
            access_lifetime_hours: Some(commit_on_focus_exit(access_lifetime_hours.into())),
            invitation_lifetime_seconds: Some(commit_on_focus_exit(
                invitation_lifetime_seconds.into(),
            )),
            never_expires: ControlHandlers {
                oninput: None,
                onchange: Some(EventHandler::new(never_expires.onchange())),
                onblur: Some(EventHandler::new(never_expires.onblur())),
            },
            include_organization: ControlHandlers {
                oninput: None,
                onchange: Some(EventHandler::new(include_organization.onchange())),
                onblur: Some(EventHandler::new(include_organization.onblur())),
            },
            teams: RepositoryScopeHandlers {
                onchange: Some(EventHandler::new({
                    let ids = team_ids.clone();
                    move |(id, checked)| ids.on_change(id, checked)
                })),
                onblur: Some(EventHandler::new(move |_| {
                    team_ids.on_commit();
                    team_ids.on_focus_exit();
                })),
            },
            max_uses: Some(commit_on_focus_exit(max_uses.into())),
            expires_in_days: Some(commit_on_focus_exit(expires_in_days.into())),
            invitation_lifetime_hours: Some(commit_on_focus_exit(invitation_lifetime_hours.into())),
            abuse_threshold_per_hour: Some(commit_on_focus_exit(abuse_threshold_per_hour.into())),
            budget_share_pct: Some(commit_on_focus_exit(budget_share_pct.into())),
            repo_grant: Some(EventHandler::new({
                let repo_grants = repo_grants.clone();
                move |(id, value): (u64, String)| {
                    for old in repo_grants.value() {
                        if old
                            .split_once(':')
                            .is_some_and(|(key, _)| key.parse::<u64>() == Ok(id))
                        {
                            repo_grants.on_change(old, false);
                        }
                    }
                    repo_grants.on_change(value, true);
                    repo_grants.on_commit();
                }
            })),
            repo_grant_blur: Some(EventHandler::new({
                let repo_grants = repo_grants.clone();
                move |_event: FocusEvent| {
                    repo_grants.on_commit();
                    repo_grants.on_focus_exit();
                }
            })),
            set_all: Some(EventHandler::new(move |event: MouseEvent| {
                event.prevent_default();
                let permission = helper_permission.value();
                for old in repo_grants.value() {
                    repo_grants.on_change(old, false);
                }
                for repo in &offered_repos {
                    repo_grants.on_change(format!("{}:{permission}", repo.id), true);
                }
                repo_grants.on_commit();
            })),
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
    if let Some(parse) = invitation_lifetime_hours.parse_error() {
        errors.attach(
            Some(&fields.invitation_lifetime_hours().identity()),
            parse.message().to_string(),
        );
    }
    if let Some(parse) = abuse_threshold_per_hour.parse_error() {
        errors.attach(
            Some(&fields.abuse_threshold_per_hour().identity()),
            parse.message().to_string(),
        );
    }
    if let Some(parse) = budget_share_pct.parse_error() {
        errors.attach(
            Some(&fields.budget_share_pct().identity()),
            parse.message().to_string(),
        );
    }
    for error in form.visible_validation_errors() {
        errors.attach(error.field_identity().as_ref(), error.error().clone());
    }
    let values = LinkFormValues {
        email_domains: email_domains.value(),
        description: description.value(),
        permission: permission.value(),
        approval_required: approval_required.checked(),
        expiration_timestamp: expiration_timestamp.value(),
        access_lifetime_hours: access_lifetime_hours.value(),
        invitation_lifetime_seconds: invitation_lifetime_seconds.value(),
        never_expires: never_expires.checked(),
        include_organization: include_organization.checked(),
        selected_team_ids: team_ids.value(),
        // Raw text while it fails to parse, the formatted value otherwise.
        max_uses: max_uses.value(),
        expires_in_days: expires_in_days.value(),
        invitation_lifetime_hours: invitation_lifetime_hours.value(),
        abuse_threshold_per_hour: abuse_threshold_per_hour.value(),
        budget_share_pct: budget_share_pct.value(),
        internal_note: internal_note.value(),
        selected_repo_ids: repo_ids.value(),
        repo_grants: repo_grants.value(),
        errors,
    };

    rsx! {
        LinkCreateForm {
            access_removal_enabled: props.access_removal_enabled,
            action: browser.action().to_string(),
            form: values,
            repos: props.repos.clone(),
            picker: props.picker.clone(),
            organization: props.organization.clone(),
            eligibility: props.eligibility.clone(),
            handlers: handlers,
        }
    }
}

/// Configure a fresh page once; hook rerenders do not restore the response again.
fn form_config(props: &LinkFormIslandProps) -> FormConfig<CreateLinkForm> {
    let repos = props.repos.clone();
    let organization = props.organization.clone();
    let now = props.now;
    let mut config = FormConfig::new(props.values.to_model().0)
        .validation_mode(ValidationMode::on_commit())
        .register_core(move |core| register_target_validators(core, &repos, &organization, now));
    if !props.values.errors.is_empty() || has_raw_numeric(&props.values) {
        let values = props.values.clone();
        config = config.browser_rejection((), move |_| browser_rejection(&values));
    }
    config
}

/// A numeric guardrail whose text the typed model cannot hold.
///
/// `LinkFormValues::to_model` maps an unparseable guardrail to `None`, so without
/// restoration the first frame renders it blank — turning "0" into unlimited
/// and losing the blocker that should stop the POST. That matters even with no
/// server errors at all, because the takeover adopts what the admin typed
/// before the island mounted: they may have entered `0` on a form the server
/// never rejected.
fn has_raw_numeric(values: &LinkFormValues) -> bool {
    parse_max_uses(&values.max_uses).is_err()
        || parse_expires_in_days(&values.expires_in_days).is_err()
        || parse_invitation_lifetime_hours(&values.invitation_lifetime_hours).is_err()
        || parse_abuse_threshold_per_hour(&values.abuse_threshold_per_hour).is_err()
        || parse_budget_share_pct(&values.budget_share_pct).is_err()
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
    if parse_invitation_lifetime_hours(&values.invitation_lifetime_hours).is_err() {
        rejection = rejection.raw_field(
            fields.invitation_lifetime_hours(),
            values.invitation_lifetime_hours.clone(),
        );
    }
    if parse_abuse_threshold_per_hour(&values.abuse_threshold_per_hour).is_err() {
        rejection = rejection.raw_field(
            fields.abuse_threshold_per_hour(),
            values.abuse_threshold_per_hour.clone(),
        );
    }
    if parse_budget_share_pct(&values.budget_share_pct).is_err() {
        rejection = rejection.raw_field(fields.budget_share_pct(), values.budget_share_pct.clone());
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
    if let Some(message) = &errors.internal_note {
        submit_errors.push(SubmitError::field(fields.internal_note(), message.clone()));
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
    if let Some(message) = &errors.repo_grants {
        submit_errors.push(SubmitError::field(fields.repo_grants(), message.clone()));
    }
    if let Some(message) = &errors.invitation_lifetime_hours {
        submit_errors.push(SubmitError::field(
            fields.invitation_lifetime_hours(),
            message.clone(),
        ));
    }
    if let Some(message) = &errors.abuse_threshold_per_hour {
        submit_errors.push(SubmitError::field(
            fields.abuse_threshold_per_hour(),
            message.clone(),
        ));
    }
    if let Some(message) = &errors.budget_share_pct {
        submit_errors.push(SubmitError::field(
            fields.budget_share_pct(),
            message.clone(),
        ));
    }
    if let Some(message) = &errors.email_domains {
        submit_errors.push(SubmitError::field(fields.email_domains(), message.clone()));
    }
    if let Some(message) = &errors.expiration_timestamp {
        submit_errors.push(SubmitError::field(
            fields.expiration_timestamp(),
            message.clone(),
        ));
    }
    if let Some(message) = &errors.access_lifetime_hours {
        submit_errors.push(SubmitError::field(
            fields.access_lifetime_hours(),
            message.clone(),
        ));
    }
    if let Some(message) = &errors.invitation_lifetime_seconds {
        submit_errors.push(SubmitError::field(
            fields.invitation_lifetime_seconds(),
            message.clone(),
        ));
    }
    if let Some(message) = &errors.organization {
        submit_errors.push(SubmitError::field(
            fields.include_organization(),
            message.clone(),
        ));
    }
    if let Some(message) = &errors.teams {
        submit_errors.push(SubmitError::field(fields.team_ids(), message.clone()));
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
            access_removal_enabled: false,
            eligibility: Default::default(),
            organization: Default::default(),
            csrf_token: None,
            action: "/console/accounts/acme/links".into(),
            values,
            repos: vec![RepositoryChoice {
                id: 10,
                full_name: "acme/api".into(),
            }],
            picker: Default::default(),
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
                access_lifetime_hours: String::new(),
                email_domains: String::new(),
                expiration_timestamp: String::new(),
                invitation_lifetime_seconds: String::new(),
                include_organization: false,
                selected_team_ids: vec![],
                invitation_lifetime_hours: "168".into(),
                abuse_threshold_per_hour: String::new(),
                budget_share_pct: "50".into(),
                never_expires: false,
                description: "  Workshop  ".into(),
                permission: "push".into(),
                approval_required: true,
                max_uses: "abc".into(),
                expires_in_days: "00".into(),
                internal_note: "Keep this note".into(),
                selected_repo_ids: vec![10],
                repo_grants: vec!["10:push".into()],
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
                        description: "  Workshop  ".into(),
                        internal_note: "Keep this note".into(),
                        permission: "push".into(),
                        approval_required: true,
                        max_uses: None,
                        expires_in_days: None,
                        repo_ids: vec![10],
                        repo_grants: vec!["10:push".into()],
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

    /// An admin who types `0` before the island mounts must not have it blanked
    /// into "unlimited". The typed model cannot hold that text, so the raw
    /// restoration has to run even though the server never rejected anything.
    #[test]
    fn raw_numeric_text_survives_mounting_without_any_server_errors() {
        check_mounted_config(
            LinkFormValues {
                description: "Workshop".into(),
                selected_repo_ids: vec![10],
                repo_grants: vec!["10:push".into()],
                max_uses: "0".into(),
                ..LinkFormValues::default()
            },
            |form, numbers| {
                assert_eq!(
                    numbers,
                    [
                        ("0".into(), Some(MAX_USES_NOT_POSITIVE.into())),
                        ("14".into(), None),
                    ]
                );
                assert!(
                    form.submit_availability()
                        .contains(SubmitBlocker::ParseErrors)
                );
                // No server said anything, so nothing is restored as a message.
                assert!(form.visible_validation_errors().is_empty());
            },
        );
    }

    #[test]
    fn fresh_config_has_no_prior_attempt_or_parse_errors_after_numeric_hooks_mount() {
        check_mounted_config(LinkFormValues::default(), |form, numbers| {
            assert_eq!(form.submit_attempt_count(), 0);
            assert_eq!(form.last_submit_status(), None);
            assert_eq!(numbers, [(String::new(), None), ("14".into(), None)]);
            assert!(form.parse_errors().is_empty());
            assert!(form.visible_validation_errors().is_empty());
        });
    }

    #[test]
    fn picker_buttons_pass_one_submission_and_enter_searches_only_from_the_filter() {
        let picker = PickerSubmit::default();
        // An ordinary submit runs the preflight.
        assert!(!picker.take());

        // Search, Previous or Next: that one submission goes through.
        picker.helper_clicked();
        assert!(picker.take());
        assert!(!picker.take(), "the pass is spent");

        // Enter in the filter clicks the default button, which searches.
        picker.query_key(true);
        assert!(picker.default_clicked());
        assert!(picker.take());

        // Enter anywhere else is not a search, and not a submission.
        assert!(!picker.default_clicked());
        assert!(!picker.take());

        // Typing in the filter and then pressing Enter elsewhere is not one.
        picker.query_key(true);
        picker.query_key(false);
        assert!(!picker.default_clicked());

        // An armed filter key never outlives the submission that followed.
        picker.query_key(true);
        assert!(!picker.take());
        assert!(!picker.default_clicked());
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
            access_lifetime_hours: None,
            email_domains: None,
            expiration_timestamp: None,
            invitation_lifetime_seconds: None,
            organization: None,
            teams: None,
            invitation_lifetime_hours: None,
            abuse_threshold_per_hour: None,
            budget_share_pct: None,
            internal_note: None,
            summary: vec![SUMMARY_MESSAGE.to_string(), "whole form".to_string()],
            description: Some("d".to_string()),
            permission: None,
            max_uses: Some("m".to_string()),
            expires_in_days: None,
            repo_scope: Some("r".to_string()),
            repo_grants: None,
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

    /// Every field of the model, as the island addresses it when it attaches a
    /// message.
    fn every_field() -> [dioform::advanced::FieldIdentity; 8] {
        let fields = CreateLinkForm::fields();
        [
            fields.description().identity(),
            fields.internal_note().identity(),
            fields.permission().identity(),
            fields.approval_required().identity(),
            fields.max_uses().identity(),
            fields.expires_in_days().identity(),
            fields.repo_ids().identity(),
            fields.repo_grants().identity(),
        ]
    }

    /// The island's round trip: the server's errors become dioform submit
    /// errors on mount, and dioform's errors are folded back through
    /// `LinkFormErrors::attach` on every render.
    fn round_trip(errors: &LinkFormErrors) -> LinkFormErrors {
        let mut folded = LinkFormErrors::default();
        for error in submit_errors_from(errors) {
            let field = error.target().as_field().cloned();
            folded.attach(field.as_ref(), error.into_error());
        }
        folded
    }

    /// `submit_errors_from` and `LinkFormErrors::attach` are inverses written
    /// out by hand on either side of the crate boundary. A message attached to
    /// any field must come back in the slot it left, and no two fields may
    /// share a slot — or a server error would render beside the wrong control
    /// on the island's first frame.
    #[test]
    fn every_field_message_round_trips_into_its_own_slot() {
        let mut slotted: Vec<LinkFormErrors> = Vec::new();
        for field in every_field() {
            // The same message for every field, so two fields landing in one
            // slot produce equal errors.
            let mut errors = LinkFormErrors::default();
            errors.attach(Some(&field), "message".into());

            assert_eq!(round_trip(&errors), errors, "{field:?}");
            // The approval checkbox has no slot, so its message is form-level;
            // every other field fills exactly one slot.
            if errors.summary == [SUMMARY_MESSAGE] {
                assert!(
                    !slotted.contains(&errors),
                    "{field:?} shares a slot: {errors:?}"
                );
                slotted.push(errors);
            } else {
                assert_eq!(
                    field,
                    CreateLinkForm::fields().approval_required().identity()
                );
            }
        }
        assert_eq!(slotted.len(), 7, "seven fields carry a message slot");
    }

    /// Every slot at once, spelled out without `..` so a seventh slot fails to
    /// compile here until it is given a round trip.
    #[test]
    fn every_slot_filled_at_once_round_trips() {
        let errors = LinkFormErrors {
            access_lifetime_hours: None,
            email_domains: None,
            expiration_timestamp: None,
            invitation_lifetime_seconds: None,
            organization: None,
            teams: None,
            invitation_lifetime_hours: None,
            abuse_threshold_per_hour: None,
            budget_share_pct: None,
            summary: vec![SUMMARY_MESSAGE.to_string(), "whole form".to_string()],
            description: Some("d".to_string()),
            internal_note: Some("n".to_string()),
            permission: Some("p".to_string()),
            max_uses: Some("m".to_string()),
            expires_in_days: Some("e".to_string()),
            repo_scope: Some("r".to_string()),
            repo_grants: Some("g".to_string()),
        };
        assert_eq!(round_trip(&errors), errors);
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
