//! Invitation-link views: create form + detail page.
//!
//! The new invitation link form is split so the browser island (ADR 0001) can
//! render exactly what the server rendered by calling the same components:
//! [`LinkCreateFormPage`] is the Console page, [`LinkCreateForm`] is just the
//! `<form>` inside it, and [`PermissionSelect`] / [`RepositoryScopeGroup`] are
//! the two controls that [`crate::field::Field`] does not cover. All of them
//! are props-in, markup-out; the optional event-handler props
//! ([`LinkFormHandlers`]) are for the island and change nothing in
//! server-rendered HTML.
//!
//! The page also emits the island's mount points: the form is wrapped in
//! `<div id="link-form-island">`, followed by a `<script type="application/json"
//! id="link-form-props">` data block (a serialised
//! [`crate::link_form::LinkFormIslandProps`]) and one `<script type="module" src>`
//! tag. Neither script is inline executable code, so both are clean under the
//! `script-src 'self'` Content-Security-Policy; if the module is missing (no
//! bundle built), the browser 404s it and the plain form keeps working.

use crate::components::alert::{Alert, AlertColor};
use crate::components::button::{Button, ButtonColor};
use crate::components::field::{Field as RegistryField, FieldAppearance, FieldError, FieldLabel};
use crate::components::native_select::{NativeSelect, NativeSelectOption};
use crate::field::{ControlHandlers, Field, FieldKind, described_by, error_text, help_text};
use crate::flash::Flash;
use crate::layouts::ConsoleLayout;
use crate::link_form::{
    CreateLinkForm, LINK_FORM_ISLAND_MODULE_SRC, LINK_FORM_ISLAND_PROPS_ID,
    LINK_FORM_ISLAND_ROOT_ID, LinkFormIslandProps, RepositoryChoice,
};
use chrono::{DateTime, Utc};
use dioform_core::Form;
use dioxus::prelude::*;
use ghinvite_core::InvitationLink;

pub use crate::link_form::{LinkFormErrors, LinkFormValues};

/// Only echoes the caller's submission while account authority is unknown.
/// No account facts or repository names may be rendered here.
#[component]
pub fn AccessVerificationRetryPage(
    signed_in_login: Option<String>,
    action: String,
    values: LinkFormValues,
) -> Element {
    let fields = CreateLinkForm::fields();
    rsx! {
        crate::layouts::HomeLayout {
            signed_in_login,
            title: "Access verification unavailable · ghinvite",
            account_login: None,
            active_nav: None,
            flash: None,
            children: rsx! {
                section { class: "mx-auto max-w-3xl space-y-4 py-8",
                    h1 { class: "text-xl font-semibold", "Access verification is temporarily unavailable" }
                    p { "Your submitted values are preserved. Retry to verify access and reload repositories before creating the invitation link." }
                    form { method: "post", action,
                        crate::csrf::CsrfField {}
                        input { r#type: "hidden", name: "reload_repos", value: "true" }
                        input { r#type: "hidden", name: fields.description().field_name(), value: values.description.clone() }
                        input { r#type: "hidden", name: fields.internal_note().field_name(), value: values.internal_note.clone() }
                        input { r#type: "hidden", name: fields.permission().field_name(), value: values.permission.clone() }
                        input { r#type: "hidden", name: fields.max_uses().field_name(), value: values.max_uses.clone() }
                        input { r#type: "hidden", name: fields.expires_in_days().field_name(), value: values.expires_in_days.clone() }
                        input { r#type: "hidden", name: fields.invitation_lifetime_hours().field_name(), value: values.invitation_lifetime_hours.clone() }
                        input { r#type: "hidden", name: fields.access_lifetime_hours().field_name(), value: values.access_lifetime_hours.clone() }
                        input { r#type: "hidden", name: fields.abuse_threshold_per_hour().field_name(), value: values.abuse_threshold_per_hour.clone() }
                        input { r#type: "hidden", name: fields.budget_share_pct().field_name(), value: values.budget_share_pct.clone() }
                        input { r#type: "hidden", name: fields.email_domains().field_name(), value: values.email_domains.clone() }
                        input { r#type: "hidden", name: "expiration_timestamp", value: values.expiration_timestamp.clone() }
                        input { r#type: "hidden", name: "invitation_lifetime_seconds", value: values.invitation_lifetime_seconds.clone() }
                        if values.never_expires {
                            input { r#type: "hidden", name: fields.never_expires().field_name(), value: "true" }
                        }
                        if values.approval_required {
                            input { r#type: "hidden", name: fields.approval_required().field_name(), value: "true" }
                        }
                        if values.include_organization { input { r#type: "hidden", name: "include_organization", value: "true" } }
                        for id in &values.selected_team_ids { input { r#type: "hidden", name: "team_ids", value: id.to_string() } }
                        for grant in values.repo_grants.iter() {
                            input { r#type: "hidden", name: "repo_grants", value: "{grant}" }
                        }
                        for id in values.selected_repo_ids.iter() {
                            input { r#type: "hidden", name: fields.repo_ids().field_name(), value: "{id}" }
                        }
                        p { "Description: {values.description}" }
                        button { r#type: "submit", class: "btn btn-primary", "Retry access verification" }
                    }
                    a { class: "btn btn-ghost", href: "/", "Go home" }
                }
            },
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct LinkCreateFormPageProps {
    #[props(default)]
    pub access_removal_enabled: bool,
    #[props(default)]
    pub eligibility: crate::organization_eligibility::Eligibility,
    #[props(default)]
    pub search_first: bool,
    /// `action` of the form: the route that handles the POST, carrying the
    /// creation identity.
    pub action: String,
    pub signed_in_login: Option<String>,
    pub flash: Option<Flash>,
    pub account_login: String,
    /// Available repositories, in the order the account makes them available.
    pub repos: Vec<RepositoryChoice>,
    #[props(default)]
    pub organization: crate::link_form::OrganizationOffer,
    #[props(default)]
    pub repository_error: Option<String>,
    pub form: LinkFormValues,
    /// The server's instant: what the route validated (or will validate)
    /// against. Handed to the island through the props blob so browser-side
    /// validation judges expiration from the same anchor.
    pub now: DateTime<Utc>,
}

/// The Console page for creating an invitation link: header, the form inside
/// the island root, the island's props blob and module script.
#[component]
pub fn LinkCreateFormPage(mut props: LinkCreateFormPageProps) -> Element {
    if props.form.repo_grants.is_empty()
        && props.form.selected_repo_ids.is_empty()
        && props.form.errors.is_empty()
    {
        props.form.repo_grants = props
            .repos
            .iter()
            .map(|repo| format!("{}:pull", repo.id))
            .collect();
    }
    let login = props.account_login.clone();
    let action = props.action.clone();
    let island_props = LinkFormIslandProps {
        access_removal_enabled: props.access_removal_enabled,
        eligibility: props.eligibility.clone(),
        csrf_token: try_consume_context::<crate::csrf::CsrfToken>()
            .unwrap_or_default()
            .0,
        action: action.clone(),
        values: props.form.clone(),
        repos: props.repos.clone(),
        organization: props.organization.clone(),
        now: props.now,
    }
    .script_json();

    rsx! {
        ConsoleLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "New invitation link · {login}",
            account_login: Some(props.account_login.clone()),
            active_nav: Some("links".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6 flex flex-col gap-2",
                    p { class: "text-sm font-medium text-primary", "Invitation links" }
                    h1 { class: "text-2xl font-semibold tracking-tight", "New invitation link" }
                    p { class: "max-w-2xl text-sm leading-6 text-base-content/70",
                        "Create a controlled invitation link that lets GitHub users request repository access to selected repositories."
                    }
                }
                if props.search_first {
                    form { method: "get", action: "/console/accounts/{login}/links/new", class: "mb-6 flex items-end gap-3",
                        div {
                            label { r#for: "repository-search", class: "block text-sm font-medium", "Find repositories" }
                            input { id: "repository-search", name: "q", r#type: "search", maxlength: "100", class: "input input-bordered", placeholder: "Repository name" }
                        }
                        button { r#type: "submit", class: "btn btn-secondary", "Search" }
                    }
                    p { class: "mb-4 text-sm", "Search shows up to 20 repositories. Access is checked for your selection before the invitation link is created." }
                }
                // The island root: `dioxus-web` mounts here and re-renders
                // `LinkCreateForm` from the props blob below.
                div { id: LINK_FORM_ISLAND_ROOT_ID,
                    LinkCreateForm {
                        access_removal_enabled: props.access_removal_enabled,
                        action: action.clone(),
                        form: props.form.clone(),
                        repos: props.repos.clone(),
                        repository_error: props.repository_error.clone(),
                        organization: props.organization.clone(),
                        eligibility: props.eligibility.clone(),
                    }
                }
                // Data, not code: a JSON script block is inert for the browser
                // and for the CSP. `dangerous_inner_html` writes it verbatim,
                // which is why `script_json` escapes `<`.
                if props.repository_error.is_none() {
                script { r#type: "application/json", id: LINK_FORM_ISLAND_PROPS_ID, dangerous_inner_html: "{island_props}" }
                // Module scripts are deferred wherever they sit, so this can
                // live next to the island it drives instead of in the shared
                // layout head; it runs after the whole document is parsed.
                script { r#type: "module", src: LINK_FORM_ISLAND_MODULE_SRC }
                }
            },
        }
    }
}

/// The five collaborator permission levels, in the order the select lists
/// them. These are the exact strings the `<select>` submits and
/// [`ghinvite_core::Permission`] parses.
pub const PERMISSION_LEVELS: [&str; 5] = ["pull", "triage", "push", "maintain", "admin"];

/// The listeners the browser island attaches to [`LinkCreateForm`], one
/// bundle per control plus the form's `onsubmit`. All optional: the server
/// renders with [`LinkFormHandlers::default`] and, because SSR never emits
/// listener attributes, the markup is byte-identical with or without them
/// (tested). This is what lets the island render the very same component the
/// server rendered instead of a copy of its layout.
#[derive(Clone, Default, PartialEq)]
pub struct LinkFormHandlers {
    pub access_lifetime_hours: Option<dioxus_field::Binding<String>>,
    pub email_domains: Option<dioxus_field::Binding<String>>,
    /// Fired when the form is submitted; the island's progressive submit
    /// preflight, which cancels the native POST only on a known blocker.
    pub onsubmit: Option<EventHandler<FormEvent>>,
    pub description: Option<dioxus_field::Binding<String>>,
    pub internal_note: ControlHandlers,
    pub permission: Option<dioxus_field::Binding<Option<String>>>,
    pub approval_required: ControlHandlers,
    pub never_expires: ControlHandlers,
    pub expiration_timestamp: Option<dioxus_field::Binding<String>>,
    pub invitation_lifetime_seconds: Option<dioxus_field::Binding<String>>,
    pub include_organization: ControlHandlers,
    pub teams: RepositoryScopeHandlers,
    pub max_uses: Option<dioxus_field::Binding<String>>,
    pub expires_in_days: Option<dioxus_field::Binding<String>>,
    pub invitation_lifetime_hours: Option<dioxus_field::Binding<String>>,
    pub abuse_threshold_per_hour: Option<dioxus_field::Binding<String>>,
    pub budget_share_pct: Option<dioxus_field::Binding<String>>,
    pub repo_scope: RepositoryScopeHandlers,
    pub repo_grant: Option<EventHandler<(u64, String)>>,
    pub set_all: Option<EventHandler<MouseEvent>>,
    pub repo_grant_blur: Option<EventHandler<FocusEvent>>,
}

/// Listeners for the repository checkbox group ([`RepositoryScopeGroup`]).
#[derive(Clone, Default, PartialEq)]
pub struct RepositoryScopeHandlers {
    /// Fired with `(repository id, checked)` when a checkbox changes.
    pub onchange: Option<EventHandler<(u64, bool)>>,
    /// Fired when focus leaves any checkbox.
    pub onblur: Option<EventHandler<FocusEvent>>,
}

/// The `<form>` of the new invitation link page: summary alert, the four
/// sections, and the submit button. Rendered by [`LinkCreateFormPage`] on the
/// server and again by the island in the browser, from the same inputs.
///
/// Rendered `name`s come from the shared model so the POST keys the server
/// parses and the controls the browser submits cannot drift apart.
#[component]
pub fn LinkCreateForm(
    #[props(default)] access_removal_enabled: bool,
    /// `action` of the form; the route that handles the POST.
    action: String,
    form: LinkFormValues,
    /// Available repositories, in the order the account makes them available.
    repos: Vec<RepositoryChoice>,
    #[props(default)] repository_error: Option<String>,
    #[props(default)] organization: crate::link_form::OrganizationOffer,
    #[props(default)] eligibility: crate::organization_eligibility::Eligibility,
    /// Listeners for the island; the server passes none.
    #[props(default)]
    handlers: LinkFormHandlers,
) -> Element {
    let fields = CreateLinkForm::fields();
    let targets = crate::link_form::selected_targets(&form.to_model().0, &repos, &organization)
        .unwrap_or_default();
    let action_path = action.split('?').next().unwrap_or(&action);
    let repository_authority = format!("{action_path}/new/selection");
    let settings_href = format!(
        "{}/settings",
        action_path.strip_suffix("/links").unwrap_or("/console")
    );
    let mut form_listeners: Vec<Attribute> = Vec::new();
    if let Some(handler) = handlers.onsubmit {
        form_listeners.push(dioxus_elements::events::onsubmit(handler));
    }
    let approval_listeners = handlers.approval_required.attributes();
    let never_listeners = handlers.never_expires.attributes();
    let organization_listeners = handlers.include_organization.attributes();

    rsx! {
        form { method: "post", action: "{action}", "data-repository-authority": "{repository_authority}", class: "max-w-3xl space-y-5", ..form_listeners,
            crate::csrf::CsrfField {}
            crate::organization_eligibility::Panel { facts: eligibility, targets }
            {if form.errors.summary.is_empty() {
                rsx! {}
            } else {
                rsx! {
                    Alert { id: "link-form-errors", color: AlertColor::Error, class: "items-start", role: "alert", aria_live: "polite",
                        div {
                            h2 { class: "font-semibold", dangerous_inner_html: "We couldn't create this invitation link" }
                            ul { class: "mt-1 list-disc space-y-1 pl-5 text-sm",
                                {form.errors.summary.iter().map(|message| rsx! { li { "{message}" } })}
                            }
                        }
                    }
                }
            }}
            section { class: "mac-panel",
                div { class: "space-y-4 p-4",
                    div {
                        h2 { class: "text-base font-semibold", "Link details" }
                        p { class: "mt-1 text-sm text-base-content/65", "Name the admin purpose for this invitation link and keep optional notes separate." }
                    }
                    Field {
                        id: "description",
                        name: fields.description().field_name().to_string(),
                        label: "Description",
                        kind: FieldKind::RegistryText { maxlength: Some(120) },
                        value: form.description.clone(),
                        required: true,
                        placeholder: "AI coding workshop",
                        help: "Visible only to admins. Use a short purpose or audience for this invitation link.",
                        error: form.errors.description.clone(),
                        binding: handlers.description,
                    }
                    Field {
                        id: "internal_note",
                        name: fields.internal_note().field_name().to_string(),
                        label: "Internal note",
                        kind: FieldKind::Textarea { rows: None },
                        value: form.internal_note.clone(),
                        placeholder: "Why this link exists",
                        help: "Optional admin-only notes. Not visible in the invitation request flow.",
                        error: form.errors.internal_note.clone(),
                        oninput: handlers.internal_note.oninput,
                        onchange: handlers.internal_note.onchange,
                        onblur: handlers.internal_note.onblur,
                    }
                }
            }
            section { class: "mac-panel",
                div { class: "space-y-4 p-4",
                    div {
                        h2 { class: "text-base font-semibold", "Access configuration" }
                        p { class: "mt-1 text-sm text-base-content/65", "Choose the GitHub permission level and repositories included in this invitation link." }
                    }
                    if !matches!(organization, crate::link_form::OrganizationOffer::Available(_)) {
                        if form.include_organization { input { r#type: "hidden", name: "include_organization", value: "true" } }
                        for id in &form.selected_team_ids { input { r#type: "hidden", name: "team_ids", value: id.to_string() } }
                        if let Some(error) = &form.errors.organization { p { class: "text-error", role: "alert", "{error}" } }
                        if let Some(error) = &form.errors.teams { p { class: "text-error", role: "alert", "{error}" } }
                    }
                    match &organization {
                        crate::link_form::OrganizationOffer::NotApplicable => rsx! {},
                        crate::link_form::OrganizationOffer::MissingApp => rsx! {
                            aside { class: "alert", role: "status",
                                span { "Organization membership and teams require the organization app." }
                                a { href: "/install/org", class: "link", "Install the org app" }
                            }
                        },
                        crate::link_form::OrganizationOffer::Disabled => rsx! {
                            p { class: "alert", role: "status", "Organization invitations are not enabled on this deployment. Repository invitations remain available." }
                        },
                        crate::link_form::OrganizationOffer::Unavailable => rsx! {
                            p { class: "alert alert-warning", role: "status", "Organization access could not be verified. Retry before selecting organization membership or teams." }
                        },
                        crate::link_form::OrganizationOffer::Available(org) => rsx! {
                            div { class: "space-y-3",
                                label { class: "flex items-center gap-3",
                                    input { r#type: "checkbox", name: fields.include_organization().field_name().to_string(), value: "true", checked: form.include_organization, class: "checkbox", ..organization_listeners }
                                    span { "Join {org.login} as a Member" }
                                }
                                p { class: "text-sm text-base-content/65", "Organization role is fixed to Member. Team roles are also Member." }
                                fieldset { class: "space-y-2", aria_describedby: "team-selection-help",
                                    legend { class: "font-medium", "Teams (optional)" }
                                    p { id: "team-selection-help", class: "text-sm", "Teams require organization membership above. Known externally managed teams cannot be invited here." }
                                    {org.teams.iter().map(|team| {
                                        let id = team.id;
                                        let mut listeners: Vec<Attribute> = Vec::new();
                                        if let Some(handler) = handlers.teams.onchange {
                                            listeners.push(dioxus_elements::events::onchange(move |event: FormEvent| handler.call((id, event.checked()))));
                                        }
                                        if let Some(handler) = handlers.teams.onblur {
                                            listeners.push(dioxus_elements::events::onblur(handler));
                                        }
                                        rsx! { label { class: "flex items-center gap-3",
                                            input { r#type: "checkbox", name: fields.team_ids().field_name().to_string(), value: id.to_string(), checked: form.selected_team_ids.contains(&id), disabled: team.externally_managed, class: "checkbox", ..listeners }
                                            span { "{team.name}" }
                                            if team.externally_managed { span { class: "text-sm", "Managed by your identity provider" } }
                                        } }
                                    })}
                                    for id in form.selected_team_ids.iter().filter(|id| !org.teams.iter().any(|team| team.id==**id)) {
                                        label { class: "flex items-center gap-3 text-warning",
                                            input { r#type: "checkbox", name: fields.team_ids().field_name().to_string(), value: id.to_string(), checked: true, class: "checkbox", onchange: { let id=*id; move |event: FormEvent| { if let Some(handler)=handlers.teams.onchange { handler.call((id,event.checked())); } } } }
                                            span { "Unavailable team {id}. Clear this selection or retry when it is available." }
                                        }
                                    }
                                    if org.teams.is_empty() { p { "No teams are currently available." } }
                                    if let Some(error) = &form.errors.teams { p { class: "text-error", role: "alert", "{error}" } }
                                }
                                // Keep validation below the choices: blur must not move the next click target.
                                if let Some(error) = &form.errors.organization { p { class: "text-error", role: "alert", "{error}" } }
                                aside { id: "organization-eligibility", class: "alert", role: "status", "Eligibility is checked again during delivery. Seat availability, account restrictions, and identity-provider policies may prevent an invitation." }
                            }
                        },
                    }
                    PermissionSelect {
                        id: "permission",
                        name: fields.permission().field_name().to_string(),
                        value: form.permission.clone(),
                        help: "Default for the set-all helper. Each selected repository has its own permission below.",
                        error: form.errors.permission.clone(),
                        binding: handlers.permission,
                    }
                    button { r#type: "submit", name: "apply_all", value: "true", formnovalidate: true, class: "btn btn-sm", onclick: move |event| { if let Some(handler) = handlers.set_all { handler.call(event); } }, "Apply permission to all repositories" }
                    div { class: "alert alert-warning shadow-sm",
                        span { "Review elevated permissions before sharing. Approved invitation requests send GitHub invitations." }
                    }
                }
            }
            section { class: "mac-panel",
                div { class: "space-y-4 p-4",
                    div {
                        h2 { class: "text-base font-semibold", "Request handling" }
                        p { class: "mt-1 text-sm text-base-content/65", "Set approval, usage, and expiration guardrails." }
                    }
                    div { class: "form-control",
                        label { class: "label cursor-pointer justify-start gap-3 whitespace-normal",
                            input { r#type: "checkbox", name: fields.approval_required().field_name().to_string(), value: "true", checked: form.approval_required, class: "checkbox", ..approval_listeners }
                            span { class: "label-text", "Require account admin approval before GitHub invitations are sent" }
                        }
                        p { class: "text-sm text-base-content/65", "Leave unchecked to auto-approve invitation requests that use this invitation link." }
                    }
                    div { class: "grid grid-cols-1 gap-4 md:grid-cols-2",
                        Field {
                            id: "max_uses",
                            name: fields.max_uses().field_name().to_string(),
                            label: "Max use",
                            kind: FieldKind::RegistryNumber { min: Some(1), max: None },
                            value: form.max_uses.clone(),
                            placeholder: "Unlimited",
                            help: "Blank means unlimited invitation requests.",
                            error: form.errors.max_uses.clone(),
                            binding: handlers.max_uses,
                        }
                        Field {
                            id: "expires_in_days",
                            name: fields.expires_in_days().field_name().to_string(),
                            label: "Expires in days",
                            kind: FieldKind::RegistryNumber { min: Some(1), max: None },
                            value: form.expires_in_days.clone(),
                            help: "Default is 14 days, including when blank. Select Never expires to remove expiration.",
                            error: form.errors.expires_in_days.clone(),
                            binding: handlers.expires_in_days,
                        }
                    }
                    if access_removal_enabled || !form.access_lifetime_hours.is_empty() {
                        Field {
                            id: "access_lifetime_hours",
                            name: fields.access_lifetime_hours().field_name().to_string(),
                            label: "Access lifetime",
                            kind: FieldKind::RegistryText { maxlength: Some(11) },
                            value: form.access_lifetime_hours.clone(),
                            help: "Enter whole hours or exact seconds with an s suffix (for example, 3601s). Blank keeps accepted access. A finite lifetime starts at acceptance and removes only access newly granted by this link; upgrades and pre-existing access are excluded. Promoted or custom roles are skipped. This cannot be changed after creation.",
                            error: form.errors.access_lifetime_hours.clone(),
                            binding: handlers.access_lifetime_hours,
                        }
                        p { class: "text-sm text-warning", "Removal is best effort. Inherited access may remain, organization removal can affect other resources, and access removal may delete private forks. Review these consequences before creating the link." }
                        if !access_removal_enabled {
                            p { class: "text-sm text-error", "Access expiry is unavailable. Clear the lifetime or contact the operator; it has not been applied." }
                        }
                    }
                    div { class: "grid grid-cols-1 gap-4 md:grid-cols-2",
                        Field {
                            id: "invitation_lifetime_hours",
                            name: fields.invitation_lifetime_hours().field_name().to_string(),
                            label: "Invitation lifetime in hours",
                            kind: FieldKind::RegistryNumber { min: Some(1), max: Some(168) },
                            value: form.invitation_lifetime_hours.clone(),
                            help: "1–168 hours; blank means 168 (7 days). Shorter lifetimes cancel still-pending invitations created by this link. This cannot be changed after creation.",
                            error: form.errors.invitation_lifetime_hours.clone(),
                            binding: handlers.invitation_lifetime_hours,
                        }
                        Field {
                            id: "abuse_threshold_per_hour",
                            name: fields.abuse_threshold_per_hour().field_name().to_string(),
                            label: "Hourly abuse threshold",
                            kind: FieldKind::RegistryNumber { min: Some(1), max: None },
                            value: form.abuse_threshold_per_hour.clone(),
                            placeholder: "Automatic",
                            help: "Blank uses the greater of 20 or 10% of maximum uses, rounded up (20 for unlimited links). Exceeding the threshold auto-pauses the link. This cannot be changed after creation.",
                            error: form.errors.abuse_threshold_per_hour.clone(),
                            binding: handlers.abuse_threshold_per_hour,
                        }
                        Field {
                            id: "budget_share_pct",
                            name: fields.budget_share_pct().field_name().to_string(),
                            label: "Daily invitation budget share (%)",
                            kind: FieldKind::RegistryNumber { min: Some(1), max: Some(100) },
                            value: form.budget_share_pct.clone(),
                            placeholder: "50",
                            help: "Maximum share of each applicable daily invitation budget for this link. Use 1–100%; blank uses 50%. This cannot be changed after creation.",
                            error: form.errors.budget_share_pct.clone(),
                            binding: handlers.budget_share_pct,
                        }
                    }
                    Field { id: "email_domains", name: fields.email_domains().field_name().to_string(), label: "Allowed email domains (optional)", kind: FieldKind::RegistryText { maxlength: Some(4096) }, value: form.email_domains.clone(), help: "Up to 32 unique rules and 4096 characters. Comma-separated verified email domains. example.com matches only that domain; *.example.com matches subdomains, not example.com. GitHub noreply addresses never qualify.", error: form.errors.email_domains.clone(), binding: handlers.email_domains }
                    details { class: "space-y-3", open: !form.expiration_timestamp.is_empty() || !form.invitation_lifetime_seconds.is_empty(),
                        summary { class: "cursor-pointer font-medium", "Exact expiration and lifetime" }
                        p { class: "text-sm", "Duplicate preserves exact source values. These fields override days and hours; clear them to use the duration controls instead." }
                        Field { id: "expiration_timestamp", name: fields.expiration_timestamp().field_name().to_string(), label: "Exact expiration (RFC 3339)", kind: FieldKind::RegistryText { maxlength: Some(64) }, value: form.expiration_timestamp.clone(), help: "For example 2027-01-01T12:00:00Z. An expired source needs a new expiration before creating its duplicate.", error: form.errors.expiration_timestamp.clone(), binding: handlers.expiration_timestamp }
                        Field { id: "invitation_lifetime_seconds", name: fields.invitation_lifetime_seconds().field_name().to_string(), label: "Exact invitation lifetime in seconds", kind: FieldKind::RegistryText { maxlength: Some(6) }, value: form.invitation_lifetime_seconds.clone(), help: "3600–604800 seconds. Blank uses invitation lifetime in hours.", error: form.errors.invitation_lifetime_seconds.clone(), binding: handlers.invitation_lifetime_seconds }
                    }
                    div { class: "form-control",
                        label { class: "label cursor-pointer justify-start gap-3 whitespace-normal",
                            input { id: "never_expires", r#type: "checkbox", name: fields.never_expires().field_name(), value: "true", checked: form.never_expires, class: "checkbox", ..never_listeners }
                            span { class: "label-text", "Never expires" }
                        }
                        p { class: "text-sm text-base-content/65", "When selected, expiration days are ignored. Require approval or set a maximum use limit." }
                    }
                    if !form.never_expires {
                        p { class: "text-sm text-base-content/65", "Choosing Never expires allows anyone holding the URL to request access until the link is revoked or its maximum uses are reached." }
                    }
                    if form.never_expires {
                        p { class: "text-sm text-warning-content bg-warning rounded-box p-3", role: "note", "This invitation link will never expire. Anyone holding its URL can request access until it is revoked or its maximum uses are reached." }
                    }
                }
            }
            section { class: "mac-panel",
                div { class: "space-y-4 p-4",
                    if let Some(message) = repository_error.clone() {
                        h2 { class: "text-base font-semibold", "Repository scope unavailable" }
                        p { role: "alert", "{message}" }
                        p { "Your entered values and repository selections are preserved. Reload repositories, then review the scope before creating the invitation link." }
                        for grant in form.repo_grants.iter() {
                            input { r#type: "hidden", name: "repo_grants", value: "{grant}" }
                        }
                        for id in form.selected_repo_ids.iter() {
                            input { r#type: "hidden", name: "repo_ids", value: "{id}" }
                            p { "Selected repository ID: {id} (availability unverified)" }
                        }
                        a { class: "link", href: settings_href, "Review installation settings" }
                    } else {
                    RepositoryScopeGroup {
                        name: fields.repo_ids().field_name().to_string(),
                        repos: repos.clone(),
                        selected: form.selected_repo_ids.clone(),
                        help: "Select every repository this invitation link may grant access to.",
                        error: form.errors.repo_scope.clone(),
                        grants: form.repo_grants.clone(),
                        grant_error: form.errors.repo_grants.clone(),
                        ongrant: handlers.repo_grant,
                        ongrantblur: handlers.repo_grant_blur,
                        onchange: handlers.repo_scope.onchange,
                        onblur: handlers.repo_scope.onblur,
                    }
                    }
                }
            }
            section { class: "mac-panel p-4 space-y-2", aria_label: "Invitation preview", aria_live: "polite",
                h2 { class: "font-semibold", "Invitation preview" }
                if let Ok(rules) = crate::link_form::parse_email_domains(&form.email_domains) {
                    if !rules.is_empty() {
                        p { "Requires a verified, non-noreply email matching:" }
                        ul { for rule in rules { li { "{rule}" } } }
                    }
                }
                if form.include_organization {
                    if let crate::link_form::OrganizationOffer::Available(org) = &organization {
                        p { "Organization: {org.login} — Member" }
                        for team in org.teams.iter().filter(|team| form.selected_team_ids.contains(&team.id)) { p { "Team: {team.name} — Member" } }
                    }
                }
                for repo in repos.iter().filter(|repo| form.selected_repo_ids.contains(&repo.id)) {
                    p { {format!("Repository: {} — {}", repo.full_name, crate::link_form::raw_repo_grant(&form.repo_grants, repo.id).and_then(|raw| raw.parse::<ghinvite_core::Permission>().ok()).map(|grant|grant.to_string()).unwrap_or_else(||"Review permission".into()))} }
                }
                if !form.include_organization && form.selected_repo_ids.is_empty() { p { "Choose organization membership or repositories to preview the requested access." } }
            }
            div { class: "flex justify-end",
                if repository_error.is_some() {
                    button { r#type: "submit", name: "reload_repos", value: "true", formnovalidate: true, class: "btn btn-primary", "Retry loading repositories" }
                } else {
                    Button { r#type: "submit", color: ButtonColor::Primary, "Create invitation link" }
                }
            }
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct PermissionSelectProps {
    /// DOM id of the select; prefixes the `{id}-help` / `{id}-error` ids.
    pub id: String,
    /// Form field name submitted with the POST.
    pub name: String,
    /// The level to pre-select. A value that is not one of
    /// [`PERMISSION_LEVELS`] (a tampered POST) displays `pull`, matching the
    /// native first-option fallback, without changing the form model.
    pub value: String,
    pub help: Option<String>,
    /// Field-level error; its presence sets `aria-invalid` and error styling.
    pub error: Option<String>,
    /// Browser form producer; absent during server rendering.
    pub binding: Option<dioxus_field::Binding<Option<String>>>,
}

/// The permission-level `<select>`: a labelled control listing exactly the
/// five supported levels, with help and error wired like [`Field`].
#[component]
pub fn PermissionSelect(props: PermissionSelectProps) -> Element {
    let help_id = format!("{}-help", props.id);
    let error_id = format!("{}-error", props.id);
    let label_id = format!("{}-label", props.id);
    let described_by = described_by(
        props.help.as_ref().map(|_| help_id.as_str()),
        props.error.as_ref().map(|_| error_id.as_str()),
    );
    // NativeSelect writes its value property, so an unmatched value would
    // blank the control rather than use the browser's first-option fallback.
    // Normalize display only; the original value must still fail validation.
    let displayed = use_memo(use_reactive(&props.value, |raw| {
        Some(if PERMISSION_LEVELS.contains(&raw.as_str()) {
            raw
        } else {
            PERMISSION_LEVELS[0].to_string()
        })
    }));
    let metadata = dioxus_field::FieldMetaValues {
        id: Some(props.id.into()),
        name: Some(props.name.into()),
        errors: props
            .error
            .iter()
            .map(|error| error.as_str().into())
            .collect(),
        ..Default::default()
    };
    let context = props
        .binding
        .map_or_else(
            dioxus_field::FieldContext::empty,
            dioxus_field::FieldContext::new,
        )
        .with_meta_values(metadata);
    let options = PERMISSION_LEVELS
        .iter()
        .map(|&level| NativeSelectOption::new(level.to_string(), level).form_value(level))
        .collect();

    rsx! {
        RegistryField { context, appearance: FieldAppearance::None, class: "form-control gap-2",
            FieldLabel { id: label_id, span { class: "label-text font-medium", "Permission level" } }
            NativeSelect::<String> {
                options,
                value: Some(displayed.into()),
                class: "select-bordered w-full",
                // Later sibling registrations cannot supply first-pass SSR.
                aria_describedby: described_by,
                aria_errormessage: props.error.as_ref().map(|_| error_id.clone()),
            }
            {help_text(&help_id, props.help.as_deref())}
            FieldError { id: error_id, class: "text-sm font-medium" }
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct RepositoryScopeGroupProps {
    /// Form field name of every checkbox (repeated per repository).
    pub name: String,
    /// Available repositories, in the order the account makes them available.
    pub repos: Vec<RepositoryChoice>,
    /// Repository ids to render checked. Ids not in `repos` are ignored and
    /// never surface in the markup.
    pub selected: Vec<u64>,
    #[props(default)]
    pub grants: Vec<String>,
    pub grant_error: Option<String>,
    pub ongrant: Option<EventHandler<(u64, String)>>,
    pub ongrantblur: Option<EventHandler<FocusEvent>>,
    pub help: Option<String>,
    /// Section-level error rendered under the list; its presence sets the
    /// error border and extends `aria-describedby`.
    pub error: Option<String>,
    /// Fired with `(repository id, checked)` when a checkbox changes (island
    /// only; ignored by SSR).
    pub onchange: Option<EventHandler<(u64, bool)>>,
    /// Fired when focus leaves any checkbox (island only; ignored by SSR).
    pub onblur: Option<EventHandler<FocusEvent>>,
}

/// The repository-scope section body: heading, help, the checkbox group (or
/// the "no repositories" notice), and the error under it.
///
/// The group is the "control" for the repository scope: labelled by the
/// heading, described by the help text and, on failure, the error under the
/// list — the same `repo_ids` / `repo_ids-help` / `repo_ids-error` id scheme
/// as [`Field`]. ARIA does not permit `aria-invalid` on `role="group"`, so the
/// error is associated through `aria-describedby` and the visible border only.
#[component]
pub fn RepositoryScopeGroup(props: RepositoryScopeGroupProps) -> Element {
    let has_error = props.error.is_some();
    let described_by = described_by(
        props.help.as_ref().map(|_| "repo_ids-help"),
        props.error.as_ref().map(|_| "repo_ids-error"),
    );
    let group_class = if has_error {
        "max-h-80 space-y-1 overflow-y-auto rounded-box border border-error bg-base-200 p-3"
    } else {
        "max-h-80 space-y-1 overflow-y-auto rounded-box border border-base-300 bg-base-200 p-3"
    };

    rsx! {
        div {
            h2 { id: "repo_ids-label", class: "text-base font-semibold", "Repository scope" }
            {props.help.as_ref().map(|help| rsx! {
                p { id: "repo_ids-help", class: "mt-1 text-sm text-base-content/65", "{help}" }
            })}
        }
        {if props.repos.is_empty() {
            rsx! { div { class: "alert shadow-sm", span { "No repositories are available for this installation." } } }
        } else {
            rsx! {
                div {
                    id: "repo_ids",
                    role: "group",
                    class: "{group_class}",
                    aria_labelledby: "repo_ids-label",
                    aria_describedby: described_by,
                    {props.repos.iter().map(|repo| {
                        let id = repo.id;
                        let checked = props.selected.contains(&id);
                        let full_name = repo.full_name.clone();
                        let raw = crate::link_form::raw_repo_grant(&props.grants, id).unwrap_or("");
                        let displayed = if PERMISSION_LEVELS.contains(&raw) { raw } else { "pull" };
                        let invalid = checked && props.grant_error.is_some() &&
                            (!PERMISSION_LEVELS.contains(&raw) || props.grants.iter().filter(|value| value.split_once(':').is_some_and(|(key, _)| key.parse::<u64>() == Ok(id))).count() != 1);

                        // Built from plain closures (`ListenerCallback`, an
                        // `Rc`) rather than wrapped in a new `EventHandler`:
                        // `EventHandler::new` allocates in the component's
                        // scope and would not be freed until unmount, so a
                        // reactive caller would leak one per option per render.
                        let mut listeners: Vec<Attribute> = Vec::new();
                        if let Some(handler) = props.onchange {
                            listeners.push(dioxus_elements::events::onchange(
                                move |event: FormEvent| handler.call((id, event.checked())),
                            ));
                        }
                        if let Some(handler) = props.onblur {
                            listeners.push(dioxus_elements::events::onblur(handler));
                        }
                        rsx! {
                            label { class: "repo-choice-row flex cursor-pointer items-center gap-3 rounded-lg px-2 py-1.5 text-sm hover:bg-base-100",
                                input { r#type: "checkbox", name: "{props.name}", value: "{id}", checked: checked, class: "checkbox checkbox-sm", aria_labelledby: "repo-choice-{id}-label", ..listeners }
                                span { id: "repo-choice-{id}-label", class: "text-sm", "{full_name}" }
                            }
                            label { id: "repo-grant-{id}-label", r#for: "repo-grant-{id}", class: "text-sm", "Permission for {full_name}" }
                            select {
                                id: "repo-grant-{id}", name: "repo_grants", class: if invalid { "select select-bordered select-sm select-error" } else { "select select-bordered select-sm" },
                                value: "{id}:{displayed}",
                                aria_describedby: if props.grant_error.is_some() { Some("repo_grants-error") } else { None },
                                aria_invalid: invalid.to_string(),
                                aria_labelledby: "repo-grant-{id}-label",
                                aria_errormessage: if invalid { Some("repo_grants-error") } else { None },
                                onblur: move |event: FocusEvent| { if let Some(handler) = props.ongrantblur { handler.call(event); } },
                                onchange: move |event: FormEvent| { if let Some(handler) = props.ongrant { handler.call((id, event.value())); } },
                                for level in PERMISSION_LEVELS {
                                    option { value: "{id}:{level}", selected: displayed == level, "{level}" }
                                }
                            }
                        }
                    })}
                }
            }
        }}
        {error_text("repo_ids-error", props.error.as_deref())}
        {props.grant_error.as_ref().map(|message| rsx! { p { id: "repo_grants-error", aria_live: "polite", class: "text-sm font-medium text-error", "{message}" } })}
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct LinkDetailProps {
    #[props(default)]
    pub eligibility: crate::organization_eligibility::Eligibility,
    pub signed_in_login: Option<String>,
    pub flash: Option<Flash>,
    pub account_login: String,
    pub link: InvitationLink,
    pub now: DateTime<Utc>,
    #[props(default)]
    pub cascade_progress: Option<ghinvite_core::revocation::CascadeProgress>,
    #[props(default)]
    pub cascade_unavailable: bool,
    #[props(default)]
    pub share_limited: bool,
    #[props(default)]
    pub budget_unavailable: bool,
}

#[component]
pub fn LinkDetailPage(props: LinkDetailProps) -> Element {
    let login = props.account_login.clone();
    let id_str = props.link.id.to_string();
    let rotate_operation = ghinvite_core::RequestId::new().to_string();
    let lifecycle_operation = ghinvite_core::RequestId::new().to_string();
    let paused = props.link.pause.is_some();
    let lifecycle_action = if paused { "resume" } else { "pause" };
    let can_resume = paused
        && props.link.revoked_at.is_none()
        && props
            .link
            .expires_at
            .is_none_or(|expiry| expiry > props.now)
        && props
            .link
            .max_uses
            .is_none_or(|max| props.link.uses_count < max);
    let description = props.link.description.clone();
    let active = props.link.is_active(props.now);
    let badge_class = if active {
        "badge badge-success"
    } else {
        "badge badge-ghost"
    };
    let badge_label = if paused && props.link.revoked_at.is_none() {
        "paused"
    } else if active {
        "active"
    } else {
        "inactive"
    };
    let uses = props
        .link
        .max_uses
        .map(|n| format!("{} / {}", props.link.uses_count, n))
        .unwrap_or_else(|| format!("{} / unlimited", props.link.uses_count));
    let expires = props
        .link
        .expires_at
        .map(|when| when.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "No expiration".into());
    let approval = if props.link.approval_required {
        "Account admin approval required"
    } else {
        "Requests are auto-approved"
    };

    rsx! {
        ConsoleLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "{description} · {login}",
            account_login: Some(props.account_login.clone()),
            active_nav: Some("links".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6 flex flex-col gap-3 md:flex-row md:items-center md:justify-between",
                    div {
                        h1 { class: "text-2xl font-bold", "{description}" }
                        p { class: "text-sm text-base-content/70", "Link ID: ", span { class: "font-mono", "{id_str}" } }
                    }
                    div { class: "flex flex-wrap items-center gap-3",
                        span { class: "{badge_class}", "{badge_label}" }
                        a { class: "btn btn-outline btn-sm", href: "/console/accounts/{login}/links/{id_str}/requests", "Request history" }
                        a { class: "btn btn-outline btn-sm", href: "/console/accounts/{login}/links/{id_str}/edit", "Edit details" }
                        a { class: "btn btn-outline btn-sm", href: "/console/accounts/{login}/links/{id_str}/duplicate", "Duplicate as new link" }
                    }
                }
                crate::organization_eligibility::Panel {facts:props.eligibility.clone(),targets:props.link.targets.clone()}
                if !props.link.email_domains.is_empty() {
                    section { class: "mac-panel p-4",
                        h2 { "Verified email domain required" }
                        ul { for rule in &props.link.email_domains { li { "{rule}" } } }
                        p { "Only verified, non-noreply emails qualify. Subdomain rules exclude the parent domain." }
                    }
                }
                if props.share_limited { p { role: "status", "Recorded delivery status: this link has used its share of today's invitations. Waiting deliveries retry when capacity is available. Updates may be delayed." } }
                if props.budget_unavailable { p { role: "status", "Invitation budget status is unavailable. Reload this page to try again." } }
                if props.cascade_unavailable {p {role:"status","Cascade progress is unavailable. This does not mean the cascade completed."}}
                if let Some(progress)=props.cascade_progress.as_ref() {
                    if progress.addressed_requests>0 || progress.addressed_deliveries>0 {
                        p {role:"status","Cascade: {progress.done_requests as u64 + progress.done_deliveries as u64 + progress.done_access_grants as u64}/{progress.addressed_requests as u64 + progress.addressed_deliveries as u64 + progress.addressed_access_grants as u64} done, {progress.waiting_on_github} waiting on GitHub"}
                        if progress.unavailable>0 {p {"Status unavailable for {progress.unavailable} addressed items."}}
                    }
                }
                if paused && props.link.revoked_at.is_none() {
                    section { class: "alert alert-warning mb-4", role: "status",
                        p { "This invitation link is paused. It is not accepting new requests." }
                        if matches!(props.link.pause.as_ref().map(|pause| &pause.reason), Some(ghinvite_core::link_lifecycle::PauseReason::Manual)) {
                            p { "Existing requests and GitHub invitations continue." }
                        } else {
                            p { "The link was automatically paused because of suspected abuse." }
                        }
                        if !can_resume { p { "Resuming cannot override the expiration or maximum uses." } }
                    }
                }
                if active || can_resume {
                    form { method: "post", action: "/console/accounts/{login}/links/{id_str}/{lifecycle_action}", class: "mb-4",
                        crate::csrf::CsrfField {}
                        input { r#type: "hidden", name: "operation_id", value: "{lifecycle_operation}" }
                        button { r#type: "submit", class: "btn btn-outline", if paused { "Resume invitation link" } else { "Pause invitation link" } }
                    }
                }
                section { class: "mac-panel mb-4 p-4",
                    h2 { class: "text-sm font-semibold", "Invitation link" }
                    p { class: "mt-2 text-sm", "The invitation URL is shown only once when created or rotated. Treat it like a password. The link ID does not grant access." }
                    p { class: "mt-2 text-sm", "Secret generation: {props.link.secret.secret_generation}" }
                    p { class: "mt-2 text-sm", "Rotating the invitation URL stops the previous URL from accepting new requests. Existing requests continue." }
                    form { method: "post", action: "/console/accounts/{login}/links/{id_str}/rotate", class: "mt-3",
                        crate::csrf::CsrfField {}
                        input { r#type: "hidden", name: "operation_id", value: "{rotate_operation}" }
                        button { r#type: "submit", class: "btn btn-outline btn-sm", "Rotate invitation URL" }
                    }
                }
                section { class: "mac-panel mb-4 overflow-hidden",
                    dl { class: "property-list",
                        div { class: "property-row",
                            dt { class: "property-label", "Uses" }
                            dd { class: "text-sm tabular-nums", "{uses}" }
                        }
                        div { class: "property-row",
                            dt { class: "property-label", "Expiration" }
                            dd { class: "text-sm", "{expires}" }
                        }
                        div { class: "property-row",
                            dt { class: "property-label", "Approval" }
                            dd { class: "text-sm", "{approval}" }
                        }
                    }
                }
                section { class: "mac-panel mb-4 overflow-hidden",
                    div { class: "border-b border-base-300 px-4 py-3",
                        h2 { class: "text-sm font-semibold", "Access targets" }
                    }
                    div { class: "p-4",
                        crate::invitation::AccessSummary { targets: props.link.targets.clone(), approval_required: props.link.approval_required }
                    }
                }
                {match props.link.internal_note.as_deref() {
                    Some(note) => rsx! {
                        section { class: "mac-panel mb-4 overflow-hidden",
                            div { class: "border-b border-base-300 px-4 py-3",
                                h2 { class: "text-sm font-semibold", "Internal note" }
                            }
                            div { class: "p-4",
                                p { class: "text-sm text-base-content/80", "{note}" }
                            }
                        }
                    },
                    None => rsx! {},
                }}
                {if active || (paused && props.link.revoked_at.is_none()) {
                    rsx! {
                        section { class: "mac-panel border-error/30 bg-base-100",
                            div { class: "card-body gap-4",
                                div {
                                    h2 { class: "text-base font-semibold text-error", "Stop accepting new invitation requests" }
                                    p { class: "mt-1 text-sm text-base-content/70",
                                        "Choose whether to stop new requests only or also cancel pending requests on the confirmation page."
                                    }
                                }
                                a { id: "stop-link-trigger", class: "btn btn-error w-fit", href: "/console/accounts/{login}/links/{id_str}/revoke", "Stop accepting new requests" }
                            }
                        }
                    }
                } else {
                    rsx! { p { class: "text-base-content/70", "This invitation link is no longer accepting invitation requests." } }
                }}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use ghinvite_core::targets::{RepoRole, RepoTarget, Targets};
    use ghinvite_core::{InvitationLink, InvitationLinkId};

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn sample_link() -> InvitationLink {
        InvitationLink {
            batch_source: None,
            access_lifetime_seconds: None,
            email_domains: vec![],
            id: "01ARZ3NDEKTSV4RRFFQ69G5FAV"
                .parse::<InvitationLinkId>()
                .unwrap(),
            organization_installation_id: None,
            installation_id: 1,
            account_id: 9001,
            created_by: 701,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: Some(dt("2026-06-03T12:00:00Z")),
            max_uses: Some(5),
            invitation_lifetime_seconds: 604800,
            abuse_threshold_per_hour: None,
            budget_share_pct: 50,
            uses_count: 2,
            approval_required: true,
            description: "AI coding workshop".into(),
            internal_note: Some("Contractor onboarding".into()),
            revoked_at: None,
            revoked_by: None,
            pause: None,
            targets: Targets {
                repos: vec![
                    RepoTarget {
                        repo_id: 10,
                        repo_full_name: "acme/api".into(),
                        grant: RepoRole::Write,
                    },
                    RepoTarget {
                        repo_id: 11,
                        repo_full_name: "acme/web".into(),
                        grant: RepoRole::Read,
                    },
                ],
                ..Default::default()
            },
            secret: ghinvite_core::LinkSecretState {
                secret_generation: 7,
                sha256: [17; 32].into(),
            },
        }
    }

    #[test]
    fn link_detail_offers_edit_details_for_active_expired_exhausted_and_revoked_links() {
        let now = dt("2026-05-04T12:00:00Z");
        for state in ["active", "expired", "exhausted", "revoked"] {
            let mut link = sample_link();
            match state {
                "expired" => link.expires_at = Some(now),
                "exhausted" => link.uses_count = 5,
                "revoked" => {
                    link.revoked_at = Some(now);
                    link.revoked_by = Some(701);
                }
                _ => {}
            }
            let id = link.id;
            let html = crate::testing::render(move || {
                rsx! {
                    LinkDetailPage {
                        signed_in_login: Some("admin".to_string()),
                        flash: None,
                        account_login: "acme",
                        link: link.clone(),
                        now,
                    }
                }
            });

            assert!(
                html.contains(&format!(
                    "href=\"/console/accounts/acme/links/{id}/edit\">Edit details</a>"
                )),
                "{state} link must remain editable"
            );
            assert_eq!(html.matches(">Edit details</a>").count(), 1);
            if state == "active" {
                assert!(html.contains(&format!("href=\"/console/accounts/acme/links/{id}/revoke\">Stop accepting new requests</a>")));
                assert!(!html.contains("Confirm revoke"));
            } else {
                assert!(!html.contains("/revoke"));
                assert!(
                    html.contains(
                        "This invitation link is no longer accepting invitation requests."
                    )
                );
            }
        }
    }

    #[test]
    fn link_form_defaults_are_safe() {
        let form = LinkFormValues::default();

        assert_eq!(form.permission, "pull");
        assert_eq!(form.expires_in_days, "14");
        assert_eq!(form.max_uses, "");
        assert!(!form.approval_required);
        let mut never = form.clone();
        never.expires_in_days.clear();
        never.never_expires = true;
        let (page, form) = render_page_and_form(never, vec![]);
        assert!(page.contains("This invitation link will never expire."));
        assert!(form.contains("This invitation link will never expire."));
    }

    // --- extracted form pieces (the island renders these directly) --------

    /// The whole page, and just the `<form>` it embeds, for the same inputs.
    fn render_page_and_form(
        form: LinkFormValues,
        repos: Vec<RepositoryChoice>,
    ) -> (String, String) {
        let (page_form, page_repos) = (form.clone(), repos.clone());
        let page = crate::testing::render(move || {
            rsx! {
                LinkCreateFormPage {
                    action: "/console/accounts/acme/links".to_string(),
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: page_repos.clone(),
                    form: page_form.clone(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });
        let standalone = crate::testing::render(move || {
            rsx! {
                LinkCreateForm {
                    action: "/console/accounts/acme/links".to_string(),
                    form: form.clone(),
                    repos: repos.clone(),
                }
            }
        });
        (page, standalone)
    }

    /// The `<form>…</form>` slice of a rendered page: what the admin sees and
    /// submits, as opposed to the props blob that follows it.
    fn form_markup(page: &str) -> &str {
        let start = page
            .find("<form method=\"post\" action=\"/console/accounts/acme/links\"")
            .expect("page renders a create form");
        let end =
            start + page[start..].find("</form>").expect("page closes the form") + "</form>".len();
        &page[start..end]
    }

    fn assert_create_submit_button(html: &str) {
        let form = form_markup(html);
        let button = form
            .split("<button ")
            .skip(1)
            .find(|button| {
                button.split_once('>').is_some_and(|(_, content)| {
                    content.starts_with("Create invitation link</button>")
                })
            })
            .expect("create form renders its labelled submit button");
        let attributes = button.split_once('>').unwrap().0;
        assert!(attributes.contains("type=\"submit\""));
        let classes = attributes
            .split_once("class=\"")
            .expect("submit button has classes")
            .1
            .split('"')
            .next()
            .unwrap()
            .split_ascii_whitespace()
            .collect::<Vec<_>>();
        assert_eq!(classes, ["btn", "btn-primary"]);
        assert!(!attributes.contains("disabled"));
    }

    fn assert_description_error_empty(html: &str) {
        let region = html.split_once("id=\"description-error\"").unwrap().1;
        let (attributes, content) = region.split_once('>').unwrap();
        assert!(attributes.contains("aria-live=\"polite\""));
        assert!(!attributes.contains("hidden"));
        assert!(content.starts_with("</div>"));
        let input = html
            .split("<input")
            .find(|input| {
                input
                    .split('>')
                    .next()
                    .unwrap()
                    .contains("id=\"description\"")
            })
            .unwrap();
        let attributes = input.split('>').next().unwrap();
        assert!(attributes.contains("aria-invalid=\"false\""));
        assert!(!attributes.contains("aria-errormessage="));
    }

    fn assert_numeric_input(html: &str, name: &str, raw: &str, error: Option<&str>) {
        let mut inputs = form_markup(html)
            .split("<input ")
            .skip(1)
            .map(|input| input.split_once('>').unwrap().0)
            .filter(|input| input.contains(&format!("name=\"{name}\"")));
        let input = inputs.next().expect("numeric input is present");
        assert!(inputs.next().is_none(), "numeric input is unique");
        for attribute in [
            format!("id=\"{name}\""),
            format!("value=\"{raw}\""),
            "type=\"number\"".into(),
            "inputmode=\"numeric\"".into(),
            "min=\"1\"".into(),
            format!("aria-labelledby=\"{name}-label\""),
            format!("aria-invalid=\"{}\"", error.is_some()),
        ] {
            assert!(input.contains(&attribute), "missing {attribute}: {input}");
        }
        if name == "invitation_lifetime_hours" {
            assert!(input.contains(" max=\"168\""));
        } else if name == "budget_share_pct" {
            assert!(input.contains(" max=\"100\""));
        } else {
            assert!(!input.contains(" max="));
        }
        assert!(!input.contains(" required="));
        let descriptions = if error.is_some() {
            format!("{name}-help {name}-error")
        } else {
            format!("{name}-help")
        };
        assert!(input.contains(&format!("aria-describedby=\"{descriptions}\"")));
        assert_eq!(
            input.contains(&format!("aria-errormessage=\"{name}-error\"")),
            error.is_some()
        );
        assert_eq!(input.contains("input-error"), error.is_some());
        let region = html.split_once(&format!("id=\"{name}-error\"")).unwrap().1;
        let (attributes, content) = region.split_once('>').unwrap();
        assert!(attributes.contains("aria-live=\"polite\""));
        assert!(!attributes.contains("hidden"));
        if let Some(error) = error {
            assert!(content.starts_with(&format!("<div>{error}</div></div>")));
        } else {
            assert!(content.starts_with("</div>"));
        }
    }

    // --- island mount points ------------------------------------------------

    const APP_SCRIPT: &str = "<script src=\"/static/app.js\"></script>";
    const PROPS_SCRIPT_OPEN: &str = "<script type=\"application/json\" id=\"link-form-props\">";
    const MODULE_SCRIPT: &str =
        "<script type=\"module\" src=\"/assets/ghinvite-island.js\"></script>";

    /// Every `<script` on the page is one of: the shared app script, the JSON
    /// data block, the island module. Nothing inline and executable.
    fn assert_no_inline_executable_script(html: &str) {
        let app = html.matches(APP_SCRIPT).count();
        let data = html.matches(PROPS_SCRIPT_OPEN).count();
        let module = html.matches(MODULE_SCRIPT).count();
        assert_eq!(app, 1);
        assert_eq!(data, 1);
        assert_eq!(module, 1);
        assert_eq!(
            html.matches("<script").count(),
            app + data + module,
            "page contains an inline executable <script>"
        );
    }

    #[test]
    fn link_create_form_page_wraps_the_form_in_the_island_root() {
        let (page, form) = render_page_and_form(LinkFormValues::default(), acme_repos());

        let wrapped = format!("<div id=\"link-form-island\">{form}</div>");
        assert!(
            page.contains(&wrapped),
            "island root does not wrap exactly the form"
        );
        assert_eq!(page.matches("id=\"link-form-island\"").count(), 1);
    }

    #[test]
    fn link_create_form_page_emits_props_blob_then_module_script_after_the_island() {
        let form = LinkFormValues {
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
            description: "AI coding workshop".to_string(),
            permission: "push".to_string(),
            approval_required: true,
            max_uses: "7".to_string(),
            expires_in_days: "45".to_string(),
            internal_note: "Keep this note".to_string(),
            selected_repo_ids: vec![11],
            repo_grants: vec!["11:push".into()],
            errors: LinkFormErrors {
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string(),
                ],
                description: Some("d".to_string()),
                ..LinkFormErrors::default()
            },
        };
        let (page, _) = render_page_and_form(form.clone(), acme_repos());

        assert_no_inline_executable_script(&page);

        let blob_start = page.find(PROPS_SCRIPT_OPEN).unwrap();
        let blob = &page[blob_start + PROPS_SCRIPT_OPEN.len()..];
        let blob = &blob[..blob.find("</script>").unwrap()];
        let props: LinkFormIslandProps = serde_json::from_str(blob).unwrap();
        assert_eq!(
            props,
            LinkFormIslandProps {
                access_removal_enabled: false,
                eligibility: Default::default(),
                organization: Default::default(),
                csrf_token: None,
                action: "/console/accounts/acme/links".to_string(),
                values: form,
                repos: acme_repos(),
                now: dt("2026-05-04T12:00:00Z"),
            }
        );
        assert!(blob.contains("\"now\":\"2026-05-04T12:00:00Z\""));

        let island_at = page.find("id=\"link-form-island\"").unwrap();
        let module_at = page.find(MODULE_SCRIPT).unwrap();
        assert!(
            island_at < blob_start,
            "props blob must follow the island root"
        );
        assert!(
            blob_start < module_at,
            "module script must follow the props blob"
        );
        assert!(module_at < page.find("</main>").unwrap());
    }

    #[test]
    fn link_create_form_page_props_blob_cannot_be_closed_by_a_description() {
        let form = LinkFormValues {
            description: "</script><script>alert(1)</script>".to_string(),
            ..LinkFormValues::default()
        };
        let (page, _) = render_page_and_form(form, acme_repos());

        assert_no_inline_executable_script(&page);
        // Three script elements in total: app, data block, module.
        assert_eq!(page.matches("</script>").count(), 3);
        assert!(page.contains("\\u003c/script>\\u003cscript>alert(1)\\u003c/script>"));
        // The form itself still escapes it as HTML.
        assert!(
            page.contains("value=\"&#60;/script&#62;&#60;script&#62;alert(1)&#60;/script&#62;\"")
        );
    }

    #[test]
    fn link_create_form_is_only_the_form_element() {
        let (_, html) = render_page_and_form(LinkFormValues::default(), acme_repos());

        assert!(html.starts_with("<form method=\"post\" action=\"/console/accounts/acme/links\""));
        assert!(
            html.contains(
                "data-repository-authority=\"/console/accounts/acme/links/new/selection\""
            )
        );
        assert!(html.ends_with("</form>"));
        assert!(!html.contains("<html"));
        assert!(!html.contains("<title>"));
        assert!(!html.contains("<h1"));
        assert!(html.contains("Create invitation link"));
    }

    #[test]
    fn link_create_form_page_embeds_the_standalone_form_byte_for_byte() {
        // Fresh form and a re-render with every kind of error: the island
        // re-renders `LinkCreateForm` from the props blob, so what the page
        // emits and what the component emits on its own must not drift.
        let with_errors = LinkFormValues {
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
            description: "   ".to_string(),
            permission: "owner".to_string(),
            approval_required: true,
            max_uses: "abc".to_string(),
            expires_in_days: "0".to_string(),
            internal_note: "Keep this note".to_string(),
            selected_repo_ids: vec![11],
            repo_grants: vec!["11:push".into()],
            errors: LinkFormErrors {
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
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string(),
                ],
                description: Some("d".to_string()),
                permission: Some("p".to_string()),
                max_uses: Some("m".to_string()),
                expires_in_days: Some("e".to_string()),
                repo_scope: Some("r".to_string()),
                repo_grants: None,
            },
        };

        for (form, repos) in [
            (LinkFormValues::default(), acme_repos()),
            (with_errors.clone(), acme_repos()),
            (with_errors, vec![]),
        ] {
            let (page, standalone) = render_page_and_form(form, repos);
            assert!(
                page.contains(&standalone),
                "page does not embed the standalone form verbatim"
            );
            assert_eq!(form_markup(&page).matches("<form").count(), 1);
        }
    }

    #[test]
    fn link_create_form_with_every_handler_wired_renders_the_same_markup_as_without() {
        // The island renders `LinkCreateForm` with `LinkFormHandlers` filled
        // in; the server renders it with none. SSR emits no listener
        // attributes, so the two must be byte-identical — this is what makes
        // "the island renders the same component" hold literally.
        let with_errors = LinkFormValues {
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
            description: "   ".to_string(),
            permission: "owner".to_string(),
            approval_required: true,
            max_uses: "abc".to_string(),
            expires_in_days: "0".to_string(),
            internal_note: "Keep this note".to_string(),
            selected_repo_ids: vec![11, 999],
            repo_grants: vec!["11:push".into(), "999:push".into()],
            errors: LinkFormErrors {
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
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string(),
                ],
                description: Some("d".to_string()),
                permission: Some("p".to_string()),
                max_uses: Some("m".to_string()),
                expires_in_days: Some("e".to_string()),
                repo_scope: Some("r".to_string()),
                repo_grants: None,
            },
        };

        for (form, repos) in [
            (LinkFormValues::default(), acme_repos()),
            (with_errors.clone(), acme_repos()),
            (with_errors, vec![]),
        ] {
            let (plain_form, plain_repos) = (form.clone(), repos.clone());
            let plain = crate::testing::render(move || {
                rsx! {
                    LinkCreateForm {
                        action: "/console/accounts/acme/links".to_string(),
                        form: plain_form.clone(),
                        repos: plain_repos.clone(),
                    }
                }
            });
            let wired = crate::testing::render(move || {
                let description = use_signal(|| form.description.clone());
                let permission = use_signal(|| Some(form.permission.clone()));
                let max_uses = use_signal(|| form.max_uses.clone());
                let expires_in_days = use_signal(|| form.expires_in_days.clone());
                let control = || ControlHandlers {
                    oninput: Some(EventHandler::new(|_event: FormEvent| {})),
                    onchange: Some(EventHandler::new(|_event: FormEvent| {})),
                    onblur: Some(EventHandler::new(|_event: FocusEvent| {})),
                };
                let handlers = LinkFormHandlers {
                    access_lifetime_hours: None,
                    email_domains: None,
                    expiration_timestamp: None,
                    invitation_lifetime_seconds: None,
                    include_organization: ControlHandlers::default(),
                    teams: RepositoryScopeHandlers::default(),
                    invitation_lifetime_hours: None,
                    abuse_threshold_per_hour: None,
                    budget_share_pct: None,
                    repo_grant: None,
                    repo_grant_blur: None,
                    set_all: None,
                    never_expires: ControlHandlers::default(),
                    onsubmit: Some(EventHandler::new(|_event: FormEvent| {})),
                    description: Some(description.into()),
                    internal_note: control(),
                    permission: Some(permission.into()),
                    approval_required: control(),
                    max_uses: Some(max_uses.into()),
                    expires_in_days: Some(expires_in_days.into()),
                    repo_scope: RepositoryScopeHandlers {
                        onchange: Some(EventHandler::new(|_change: (u64, bool)| {})),
                        onblur: Some(EventHandler::new(|_event: FocusEvent| {})),
                    },
                };
                rsx! {
                    LinkCreateForm {
                        action: "/console/accounts/acme/links".to_string(),
                        form: form.clone(),
                        repos: repos.clone(),
                        handlers: handlers,
                    }
                }
            });

            assert_eq!(wired, plain);
            for listener in ["onsubmit", "oninput", "onchange", "onblur"] {
                assert!(
                    !wired.contains(listener),
                    "SSR emitted a {listener} listener"
                );
            }
        }
    }

    #[test]
    fn permission_select_first_ssr_pass_preserves_every_supported_selection_and_association() {
        for value in ["pull", "triage", "push", "maintain", "admin"] {
            for help in [None, Some("Use pull for read-only access.")] {
                for error in [None, Some("Choose a supported permission level.")] {
                    let mut vdom = VirtualDom::new_with_props(
                        move || {
                            rsx! {
                                PermissionSelect {
                                    id: "permission", name: "permission", value,
                                    help: help.map(str::to_string), error: error.map(str::to_string),
                                }
                            }
                        },
                        (),
                    );
                    // No extra render to let later sibling registrations settle.
                    vdom.rebuild_in_place();
                    let html = dioxus_ssr::render(&vdom);
                    assert_permission_markup(&html, value, help.is_some(), error);
                    if let Some(help) = help {
                        assert!(html.contains(&format!(
                            "<p id=\"permission-help\" class=\"text-sm text-base-content/65\">{help}</p>"
                        )));
                    }
                }
            }
        }
    }

    #[test]
    fn permission_select_invalid_values_display_pull_without_exposing_an_extra_option() {
        for value in ["owner", "", "Push", " pull "] {
            let html = crate::testing::render(move || {
                rsx! {
                    PermissionSelect {
                        id: "permission", name: "permission", value,
                        error: "Choose a supported permission level.",
                    }
                }
            });
            assert_permission_markup(
                &html,
                "pull",
                false,
                Some("Choose a supported permission level."),
            );
            assert!(!html.contains("owner"));
        }
    }

    fn assert_permission_markup(html: &str, selected: &str, help: bool, error: Option<&str>) {
        let select = html
            .split_once("<select ")
            .unwrap()
            .1
            .split_once("</select>")
            .unwrap()
            .0;
        let (attributes, options) = select.split_once('>').unwrap();
        for attribute in [
            "id=\"permission\"",
            "name=\"permission\"",
            "aria-labelledby=\"permission-label\"",
        ] {
            assert!(
                attributes.contains(attribute),
                "missing {attribute}: {attributes}"
            );
        }
        assert!(attributes.contains(&format!("aria-invalid=\"{}\"", error.is_some())));
        let classes = attributes
            .split_once("class=\"")
            .unwrap()
            .1
            .split('"')
            .next()
            .unwrap()
            .split_ascii_whitespace()
            .collect::<Vec<_>>();
        for class in ["select", "select-bordered", "w-full"] {
            assert!(classes.contains(&class));
        }
        assert_eq!(classes.contains(&"select-error"), error.is_some());
        let descriptions = [
            help.then_some("permission-help"),
            error.map(|_| "permission-error"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
        if descriptions.is_empty() {
            assert!(!attributes.contains("aria-describedby="));
        } else {
            assert!(attributes.contains(&format!("aria-describedby=\"{descriptions}\"")));
        }
        assert_eq!(
            attributes.contains("aria-errormessage=\"permission-error\""),
            error.is_some()
        );
        let label = html
            .split("<label ")
            .skip(1)
            .map(|label| label.split_once('>').unwrap().0)
            .find(|label| label.contains("id=\"permission-label\""))
            .unwrap();
        assert!(label.contains("id=\"permission-label\""));
        assert!(label.contains("for=\"permission\""));
        assert!(html.contains("<span class=\"label-text font-medium\">Permission level</span>"));
        assert_eq!(options.matches("<option ").count(), 5);
        assert_eq!(options.matches(" selected=true").count(), 1);
        for level in ["pull", "triage", "push", "maintain", "admin"] {
            let selection = if level == selected {
                " selected=true"
            } else {
                ""
            };
            assert!(
                options.contains(&format!(
                    "<option value=\"{level}\"{selection}>{level}</option>"
                )),
                "{options}"
            );
        }
        assert!(!options.contains("disabled"));
        assert!(!options.contains("value=\"\""));
        assert_eq!(html.contains("id=\"permission-help\""), help);
        let region = html.split_once("id=\"permission-error\"").unwrap().1;
        let (attributes, content) = region.split_once('>').unwrap();
        assert!(attributes.contains("aria-live=\"polite\""));
        assert!(attributes.contains("text-sm font-medium"));
        assert!(!attributes.contains("hidden"));
        if let Some(error) = error {
            assert!(attributes.contains("text-error"));
            assert!(content.starts_with(&format!("<div>{error}</div></div>")));
        } else {
            assert!(content.starts_with("</div>"));
        }
        for id in ["permission", "permission-label", "permission-error"] {
            assert_eq!(html.matches(&format!(" id=\"{id}\"")).count(), 1);
        }
        assert_eq!(html.matches("name=\"permission\"").count(), 1);
    }

    #[test]
    fn repository_scope_group_renders_checkboxes_with_selection_and_no_error() {
        let html = crate::testing::render(|| {
            rsx! {
                RepositoryScopeGroup {
                    name: "repo_ids".to_string(),
                    repos: acme_repos(),
                    selected: vec![11],
                    help: "Select every repository.".to_string(),
                }
            }
        });

        assert!(html.contains(
            "<h2 id=\"repo_ids-label\" class=\"text-base font-semibold\">Repository scope</h2>"
        ));
        assert!(html.contains(
            "<p id=\"repo_ids-help\" class=\"mt-1 text-sm text-base-content/65\">Select every repository.</p>"
        ));
        assert!(html.contains(
            "<div id=\"repo_ids\" role=\"group\" class=\"max-h-80 space-y-1 overflow-y-auto rounded-box border border-base-300 bg-base-200 p-3\" aria-labelledby=\"repo_ids-label\" aria-describedby=\"repo_ids-help\">"
        ));
        assert_eq!(html.matches("name=\"repo_ids\"").count(), 2);
        assert!(html.contains(
            "<input type=\"checkbox\" name=\"repo_ids\" value=\"10\" class=\"checkbox checkbox-sm\" aria-labelledby=\"repo-choice-10-label\"/>"
        ));
        assert!(html.contains(
            "<input type=\"checkbox\" name=\"repo_ids\" value=\"11\" checked=true class=\"checkbox checkbox-sm\" aria-labelledby=\"repo-choice-11-label\"/>"
        ));
        assert!(html.contains("acme/api"));
        assert!(html.contains("acme/web"));
        assert!(!html.contains("repo_ids-error"));
        assert!(!html.contains("No repositories are available"));
    }

    #[test]
    fn repository_scope_group_with_error_and_handlers_renders_error_under_list() {
        let html = crate::testing::render(|| {
            rsx! {
                RepositoryScopeGroup {
                    name: "repo_ids".to_string(),
                    repos: acme_repos(),
                    selected: vec![],
                    help: "Select every repository.".to_string(),
                    error: "Repository scope is required.".to_string(),
                    onchange: move |_change: (u64, bool)| {},
                    onblur: move |_event: FocusEvent| {},
                }
            }
        });

        assert!(html.contains("border-error"));
        assert!(html.contains("aria-describedby=\"repo_ids-help repo_ids-error\""));
        assert!(html.contains(
            "<p id=\"repo_ids-error\" class=\"text-sm font-medium text-error\">Repository scope is required.</p>"
        ));
        assert!(
            html.find("name=\"repo_ids\"").unwrap() < html.find("id=\"repo_ids-error\"").unwrap()
        );
        assert!(
            !html
                .split("role=\"group\"")
                .nth(1)
                .unwrap()
                .split('>')
                .next()
                .unwrap()
                .contains("aria-invalid")
        );
        assert!(!html.contains("onchange"));
        assert!(!html.contains("onblur"));
    }

    #[test]
    fn repository_scope_group_without_repositories_renders_notice_and_error() {
        let html = crate::testing::render(|| {
            rsx! {
                RepositoryScopeGroup {
                    name: "repo_ids".to_string(),
                    repos: vec![],
                    selected: vec![],
                    help: "Select every repository.".to_string(),
                    error: "Repository scope is required.".to_string(),
                }
            }
        });

        assert!(html.contains("No repositories are available for this installation."));
        assert!(html.contains("id=\"repo_ids-error\""));
        assert!(!html.contains("name=\"repo_ids\""));
        assert!(!html.contains("role=\"group\""));
    }

    #[test]
    fn link_form_values_round_trip_through_json() {
        // The island deserialises these from the props blob.
        let form = LinkFormValues {
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
            description: "AI coding workshop".to_string(),
            permission: "push".to_string(),
            approval_required: true,
            max_uses: "7".to_string(),
            expires_in_days: "45".to_string(),
            internal_note: "Keep this note".to_string(),
            selected_repo_ids: vec![10, 11],
            repo_grants: vec!["10:push".into(), "11:push".into()],
            errors: LinkFormErrors {
                summary: vec!["summary".to_string()],
                description: Some("d".to_string()),
                ..LinkFormErrors::default()
            },
        };

        let json = serde_json::to_string(&form).unwrap();
        let back: LinkFormValues = serde_json::from_str(&json).unwrap();
        assert_eq!(back, form);

        let repos = acme_repos();
        let json = serde_json::to_string(&repos).unwrap();
        let back: Vec<RepositoryChoice> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, repos);
    }

    #[test]
    fn link_create_form_renders_sectioned_console_form() {
        let html = crate::testing::render(|| {
            rsx! {
                LinkCreateFormPage {
                    action: "/console/accounts/acme/links".to_string(),
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: vec![RepositoryChoice {
                        id: 10,
                        full_name: "acme/api".to_string(),
                    }],
                    form: LinkFormValues::default(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("Access configuration"));
        assert!(html.contains("Link details"));
        assert!(html.contains("name=\"description\""));
        assert_eq!(html.matches("name=\"description\"").count(), 1);
        assert!(html.contains("required"));
        assert!(html.contains("maxlength=\"120\""));
        assert!(html.contains("AI coding workshop"));
        assert!(html.contains("aria-describedby=\"description-help\""));
        assert!(html.contains("id=\"description-help\""));
        assert!(!html.contains("aria-invalid=\"true\""));
        assert_description_error_empty(&html);
        assert!(html.contains("aria-describedby=\"internal_note-help\""));
        assert!(html.contains("id=\"internal_note-help\""));
        assert!(html.contains("aria-describedby=\"max_uses-help\""));
        assert!(html.contains("aria-describedby=\"expires_in_days-help\""));
        assert_numeric_input(&html, "max_uses", "", None);
        assert_numeric_input(&html, "expires_in_days", "14", None);
        assert_numeric_input(&html, "invitation_lifetime_hours", "168", None);
        assert_numeric_input(&html, "abuse_threshold_per_hour", "", None);
        assert_numeric_input(&html, "budget_share_pct", "50", None);
        assert!(html.contains("max=\"168\""));
        assert!(html.contains("greater of 20 or 10%"));
        assert!(html.contains("This cannot be changed after creation."));
        assert!(html.contains("min=\"1\""));
        assert!(html.find("Link details").unwrap() < html.find("Access configuration").unwrap());
        assert!(html.contains("<title>New invitation link · acme</title>"));
        assert!(!html.contains("{props.account_login}"));
        assert!(html.contains("Create a controlled invitation link"));
        assert!(html.contains("Approved invitation requests send GitHub invitations"));
        assert!(html.contains("Require account admin approval before GitHub invitations are sent"));
        assert!(html.contains("Blank means unlimited invitation requests"));
        assert!(html.contains("Create invitation link"));
        assert!(!html.contains("collaborator access"));
        assert!(!html.contains("GitHub collaborator invitations"));
        assert!(!html.contains("before invitations are sent"));
        assert!(html.contains("Max use"));
        assert!(!html.contains("Max uses"));
        assert!(html.contains("Request handling"));
        assert!(html.contains("Repository scope"));
        assert!(html.contains("acme/api"));
        assert!(html.contains("mac-panel"));
        assert!(html.contains("repo-choice-row"));
    }

    #[test]
    fn link_create_form_renders_description_validation_errors_and_preserved_values() {
        let form = LinkFormValues {
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
            description: "   ".to_string(),
            permission: "push".to_string(),
            approval_required: true,
            max_uses: "7".to_string(),
            expires_in_days: "45".to_string(),
            internal_note: "Keep this note".to_string(),
            selected_repo_ids: vec![10],
            repo_grants: vec!["10:push".into()],
            errors: LinkFormErrors {
                summary: vec!["Fix the highlighted fields before creating this invitation link."
                    .to_string()],
                description: Some("Description is required. Use short, single-line admin-only context for this invitation link.".to_string()),
                ..LinkFormErrors::default()
            },
        };

        let html = crate::testing::render(move || {
            rsx! {
                LinkCreateFormPage {
                    action: "/console/accounts/acme/links".to_string(),
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: vec![
                        RepositoryChoice { id: 10, full_name: "acme/api".to_string() },
                        RepositoryChoice { id: 11, full_name: "acme/web".to_string() },
                    ],
                    form: form.clone(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("Fix the highlighted fields before creating this invitation link."));
        assert!(html.contains("Description is required. Use short, single-line admin-only context for this invitation link."));
        assert!(html.contains("id=\"link-form-errors\""));
        assert!(html.contains("role=\"alert\""));
        assert!(html.contains("aria-live=\"polite\""));
        assert!(html.contains("id=\"description-error\""));
        assert!(html.contains("aria-invalid=\"true\""));
        assert_eq!(html.matches("aria-invalid=\"true\"").count(), 1);
        assert!(html.contains("aria-describedby=\"description-help description-error\""));
        assert!(html.contains("input-error"));
        assert_eq!(html.matches("name=\"description\"").count(), 1);
        assert!(html.contains("value=\"push\" selected"));
        assert!(html.contains("name=\"approval_required\" value=\"true\" checked"));
        assert_numeric_input(&html, "max_uses", "7", None);
        assert_numeric_input(&html, "expires_in_days", "45", None);
        assert!(html.contains("Keep this note"));
        assert!(html.contains("value=\"10\" checked"));
        assert!(html.contains("value=\"11\""));
    }

    #[test]
    fn link_create_form_renders_numeric_guardrail_errors_and_preserved_raw_values() {
        let form = LinkFormValues {
            description: "AI coding workshop".to_string(),
            max_uses: "abc".to_string(),
            expires_in_days: "0".to_string(),
            selected_repo_ids: vec![10],
            repo_grants: vec!["10:push".into()],
            errors: LinkFormErrors {
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string(),
                ],
                max_uses: Some("Max use must be a whole number of 1 or more.".to_string()),
                expires_in_days: Some(
                    "Expiration must be a whole number of days, 1 or more.".to_string(),
                ),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };

        let html = crate::testing::render(move || {
            rsx! {
                LinkCreateFormPage {
                    action: "/console/accounts/acme/links".to_string(),
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: vec![
                        RepositoryChoice { id: 10, full_name: "acme/api".to_string() },
                    ],
                    form: form.clone(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("id=\"link-form-errors\""));
        assert!(html.contains("Fix the highlighted fields before creating this invitation link."));
        assert_numeric_input(
            &html,
            "max_uses",
            "abc",
            Some(crate::link_form::MAX_USES_NOT_POSITIVE),
        );
        assert_numeric_input(
            &html,
            "expires_in_days",
            "0",
            Some(crate::link_form::EXPIRES_IN_DAYS_NOT_POSITIVE),
        );
        assert!(html.contains("aria-describedby=\"max_uses-help max_uses-error\""));
        assert!(html.contains("aria-describedby=\"expires_in_days-help expires_in_days-error\""));
        assert_eq!(html.matches("aria-invalid=\"true\"").count(), 2);
        assert_eq!(html.matches("input-error").count(), 2);
        assert!(html.contains("value=\"AI coding workshop\""));
        assert_description_error_empty(&html);
        assert!(html.contains("value=\"10\" checked"));
        assert_create_submit_button(&html);
        assert!(!html.contains("disabled"));
    }

    #[test]
    fn link_create_form_renders_permission_error_under_select_with_preserved_values() {
        let form = LinkFormValues {
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
            description: "AI coding workshop".to_string(),
            // A tampered value displays pull without repairing the raw model.
            permission: "owner".to_string(),
            approval_required: true,
            max_uses: "7".to_string(),
            expires_in_days: "45".to_string(),
            internal_note: "Keep this note".to_string(),
            selected_repo_ids: vec![11],
            repo_grants: vec!["11:push".into()],
            errors: LinkFormErrors {
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string(),
                ],
                permission: Some(
                    "Choose a supported permission level: pull, triage, push, maintain, or admin."
                        .to_string(),
                ),
                ..LinkFormErrors::default()
            },
        };

        let html = crate::testing::render(move || {
            rsx! {
                LinkCreateFormPage {
                    action: "/console/accounts/acme/links".to_string(),
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: acme_repos(),
                    form: form.clone(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("id=\"link-form-errors\""));
        assert!(html.contains("Fix the highlighted fields before creating this invitation link."));
        assert_permission_markup(
            &html,
            "pull",
            true,
            Some(crate::link_form::PERMISSION_UNSUPPORTED),
        );
        assert!(html.contains("aria-describedby=\"permission-help permission-error\""));
        assert!(html.contains("id=\"permission-help\""));
        assert_eq!(html.matches("aria-invalid=\"true\"").count(), 1);
        assert!(html.contains("select-error"));
        assert!(
            html.find("name=\"permission\"").unwrap()
                < html.find("id=\"permission-error\"").unwrap(),
            "error is rendered under the permission select"
        );
        // The tampered value is never echoed into the form markup (the props
        // blob carries the submitted values verbatim, JSON-escaped, so the
        // island starts from the same state); the select still lists exactly
        // the five supported levels, with pull explicitly selected.
        assert!(!form_markup(&html).contains("owner"));
        assert_eq!(
            html.split("</select>")
                .next()
                .unwrap()
                .matches("<option")
                .count(),
            5
        );
        assert!(html.contains("value=\"pull\" selected=true"));
        assert!(html.contains("value=\"AI coding workshop\""));
        assert!(html.contains("name=\"approval_required\" value=\"true\" checked"));
        assert_numeric_input(&html, "max_uses", "7", None);
        assert_numeric_input(&html, "expires_in_days", "45", None);
        assert!(html.contains("Keep this note"));
        assert!(html.contains("value=\"11\" checked"));
        assert!(!html.contains("value=\"10\" checked"));
        assert_description_error_empty(&html);
        assert!(!html.contains("repo_ids-error"));
        assert!(!html.contains("input-error"));
        assert_create_submit_button(&html);
        assert!(!html.contains("disabled"));
    }

    #[test]
    fn link_create_form_permission_select_is_described_by_help_without_error() {
        let html = crate::testing::render(|| {
            rsx! {
                LinkCreateFormPage {
                    action: "/console/accounts/acme/links".to_string(),
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: acme_repos(),
                    form: LinkFormValues::default(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("id=\"permission\""));
        assert!(html.contains("name=\"permission\""));
        assert!(html.contains("id=\"permission-help\""));
        assert!(html.contains("aria-describedby=\"permission-help\""));
        assert_permission_markup(&html, "pull", true, None);
        assert!(!html.contains("select-error"));
        assert!(!html.contains("aria-invalid=\"true\""));
        assert_description_error_empty(&html);
        assert!(html.contains("value=\"pull\" selected"));
        assert_eq!(
            html.split("</select>")
                .next()
                .unwrap()
                .matches("<option")
                .count(),
            5
        );
    }

    fn acme_repos() -> Vec<RepositoryChoice> {
        vec![
            RepositoryChoice {
                id: 10,
                full_name: "acme/api".to_string(),
            },
            RepositoryChoice {
                id: 11,
                full_name: "acme/web".to_string(),
            },
        ]
    }

    #[test]
    fn link_create_form_repository_group_is_labelled_and_described_without_error() {
        let html = crate::testing::render(|| {
            rsx! {
                LinkCreateFormPage {
                    action: "/console/accounts/acme/links".to_string(),
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: acme_repos(),
                    form: LinkFormValues::default(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("id=\"repo_ids-label\""));
        assert!(html.contains("id=\"repo_ids-help\""));
        assert!(html.contains("id=\"repo_ids\""));
        assert!(html.contains("role=\"group\""));
        assert!(html.contains("aria-labelledby=\"repo_ids-label\""));
        assert!(html.contains("aria-describedby=\"repo_ids-help\""));
        assert!(!html.contains("repo_ids-error"));
        assert!(!html.contains("aria-invalid=\"true\""));
        assert_description_error_empty(&html);
        assert!(!html.contains("border-error"));
        assert!(!html.contains("value=\"10\" checked"));
        assert!(!html.contains("value=\"11\" checked"));
    }

    #[test]
    fn link_create_form_renders_repository_scope_error_under_group_with_preserved_selection() {
        let form = LinkFormValues {
            description: "AI coding workshop".to_string(),
            // 11 is available and stays checked; 999 is unknown and must not
            // surface anywhere in the markup.
            selected_repo_ids: vec![999, 11],
            repo_grants: vec!["999:push".into(), "11:push".into()],
            errors: LinkFormErrors {
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string(),
                ],
                repo_scope: Some(
                    "Repository scope is required. Select at least one available repository for this invitation link."
                        .to_string(),
                ),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };

        let html = crate::testing::render(move || {
            rsx! {
                LinkCreateFormPage {
                    action: "/console/accounts/acme/links".to_string(),
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: acme_repos(),
                    form: form.clone(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("id=\"link-form-errors\""));
        assert!(html.contains(
            "<p id=\"repo_ids-error\" class=\"text-sm font-medium text-error\">Repository scope is required. Select at least one available repository for this invitation link.</p>"
        ));
        assert!(html.contains("role=\"group\""));
        assert!(html.contains("aria-labelledby=\"repo_ids-label\""));
        assert!(html.contains("aria-describedby=\"repo_ids-help repo_ids-error\""));
        // ARIA does not permit aria-invalid on role="group"; the error is
        // associated through aria-describedby and the visible border only.
        assert_eq!(html.matches("aria-invalid=\"true\"").count(), 0);
        assert!(html.contains("border-error"));
        assert!(
            html.find("name=\"repo_ids\"").unwrap() < html.find("id=\"repo_ids-error\"").unwrap(),
            "error is rendered under the repository list"
        );
        assert!(html.contains("value=\"11\" checked"));
        assert!(html.contains("value=\"10\""));
        assert!(!html.contains("value=\"10\" checked"));
        // The unknown id never reaches the form markup (the props blob carries
        // the submitted selection verbatim).
        assert!(!form_markup(&html).contains("999"));
        assert_eq!(html.matches("name=\"repo_ids\"").count(), 2);
        assert_description_error_empty(&html);
        assert!(!html.contains("input-error"));
        assert_create_submit_button(&html);
        assert!(!html.contains("disabled"));
    }

    #[test]
    fn link_create_form_renders_repository_scope_error_when_no_repositories_are_available() {
        let form = LinkFormValues {
            description: "AI coding workshop".to_string(),
            errors: LinkFormErrors {
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string(),
                ],
                repo_scope: Some("Repository scope is required. Select at least one available repository for this invitation link.".to_string()),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };

        let html = crate::testing::render(move || {
            rsx! {
                LinkCreateFormPage {
                    action: "/console/accounts/acme/links".to_string(),
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: vec![],
                    form: form.clone(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("No repositories are available for this installation."));
        assert!(html.contains("id=\"repo_ids-error\""));
        assert!(html.contains("Repository scope is required."));
        assert!(!html.contains("name=\"repo_ids\""));
    }

    #[test]
    fn link_detail_renders_operational_context() {
        let link = sample_link();
        let html = crate::testing::render(move || {
            rsx! {
                LinkDetailPage {
                    signed_in_login: Some("admin".into()),
                    flash: None,
                    account_login: String::from("acme"),
                    link: link.clone(),
                    now: dt("2026-05-05T12:00:00Z"),
                }
            }
        });

        assert!(!html.contains("Open invitation request flow"));
        assert!(!html.contains("/i/01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        assert!(!html.contains("/i#"));
        assert!(html.contains("Secret generation: 7"));
        assert!(html.contains("/console/accounts/acme/links/01ARZ3NDEKTSV4RRFFQ69G5FAV/rotate"));
        assert!(html.contains("name=\"operation_id\""));
        assert!(html.contains("Rotate invitation URL"));
        assert!(html.contains("shown only once"));
        assert!(!html.contains("Invitation URL"));
        assert!(!html.contains("recipient preview"));
        assert!(html.contains("<title>AI coding workshop · acme</title>"));
        assert!(html.contains("<h1 class=\"text-2xl font-bold\">AI coding workshop</h1>"));
        assert!(html.contains("Link ID"));
        assert!(html.contains("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        assert!(!html.contains("{props.account_login}"));
        assert!(html.contains("Stop accepting new invitation requests"));
        assert!(
            html.contains(
                "Choose whether to stop new requests only or also cancel pending requests"
            )
        );
        assert!(!html.contains("Existing requests and invitations continue"));
        assert!(html.contains("acme/api"));
        assert!(html.contains("acme/web"));
        assert!(html.contains("Write (push)"));
        assert!(html.contains("Read (pull)"));
        assert!(html.contains("2 / 5"));
        assert!(html.contains("Contractor onboarding"));
        assert!(html.contains("/console/accounts/acme/links/01ARZ3NDEKTSV4RRFFQ69G5FAV/revoke"));
        assert!(!html.contains("role=\"dialog\""));
        assert!(!html.contains("Confirm revoke"));
        assert!(html.contains("property-list"));
        assert!(html.contains("property-row"));
        assert!(html.contains("mac-panel"));
        assert!(!html.contains("md:grid-cols-2 gap-4"));
        assert!(!html.contains(concat!("Revoke this ", "link")));
    }
    #[test]
    fn access_lifetime_is_explicitly_enabled_and_caveats_precede_submission() {
        for enabled in [false, true] {
            let html = crate::testing::render(move || {
                rsx! {
                    LinkCreateForm {action:"/console/accounts/acme/links".to_string(),form:LinkFormValues::default(),repos:vec![],access_removal_enabled:enabled}
                }
            });
            assert_eq!(html.contains("id=\"access_lifetime_hours\""), enabled);
            if enabled {
                assert!(html.contains("starts at acceptance"));
                assert!(html.contains("upgrades and pre-existing access are excluded"));
                assert!(html.contains("private forks"));
                assert!(
                    html.find("private forks").unwrap()
                        < html.rfind("Create invitation link").unwrap()
                );
            }
        }
        let html = crate::testing::render(move || {
            rsx! {
                LinkCreateForm {action:"/console/accounts/acme/links".to_string(),form:LinkFormValues{access_lifetime_hours:"3601s".into(),..Default::default()},repos:vec![]}
            }
        });
        assert!(html.contains("value=\"3601s\""));
        assert!(html.contains("Clear the lifetime"));
    }
}
