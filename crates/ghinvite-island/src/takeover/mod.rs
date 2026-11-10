//! Taking over the server-rendered form without discarding the admin's work.
//!
//! The island mounts whenever the wasm bundle finishes loading, which on a cold
//! cache is long after the page is usable. By then the admin may already have
//! typed a description, narrowed the repository scope, or cleared the approval
//! checkbox — none of which the props blob knows about, because the server
//! serialised it before the page was ever shown. Re-rendering from the props
//! alone silently reverts all of it (the failure #67 reproduces), and reverting
//! the approval checkbox from checked to unchecked would quietly turn
//! auto-approval back on.
//!
//! So the entrypoint reads the live form first and this module decides, per
//! control, whether the DOM or the props blob is the better description of what
//! the admin meant. The rule is the same everywhere: **a control the admin
//! changed is authoritative in the DOM; an unchanged control keeps its props
//! value.** "Unchanged" is the DOM's own notion — the `value` property still
//! equals the server-rendered default (`defaultValue`, `defaultChecked`, the
//! `selected` option) — so nothing has to observe the form before the bundle
//! loads for this to work.
//!
//! Keeping the props value for unchanged controls is not a micro-optimisation.
//! A control applies its own *value sanitisation* before anyone touches it, so
//! "the DOM differs from the markup" is not the same as "the admin changed
//! it", and adopting the difference would silently repair a model the server
//! rejected. Every control kind is therefore compared against what the browser
//! **shows** for the server's value, not against the markup that carried it:
//!
//! - A `<select>` whose server value is not one of the five permission levels
//!   renders with `pull` explicitly selected (see `PermissionSelect`), so the
//!   DOM shows `pull` while dioform must keep the raw `owner`.
//! - A `type="number"` input shows nothing at all for a value that is not a
//!   number it can display, so `value="abc"` reads back as `""` while the
//!   parse blocker must survive.
//! - A `type="text"` input strips CR and LF out of its value, so a multiline
//!   description the server preserved and rejected reads back shortened.
//! - A `<textarea>` keeps its newlines but normalises them to LF.
//!
//! Each of those gaps has a floor the DOM cannot lift. An admin who
//! *deliberately* picks `pull` over an unsupported `owner`, or clears a numeric
//! field the browser had already emptied, leaves the control identical to how
//! the server rendered it, so the takeover reads it as untouched and keeps the
//! raw value and its blocker. That is the safe direction — the form stays
//! visibly rejected until a distinguishable input corrects it, rather than
//! silently submitting a value the admin never chose — and the same edit made
//! after the island mounts is picked up normally.
//!
//! The browser knowledge this rests on — which rewrite each control kind
//! performs, and what a `type="number"` input will display — lives in
//! [`browser`], free of any reference to this form. This module is only the
//! binding: which field the server renders as which kind, and how an adopted
//! value folds over the props blob. Per ADR 0001, `browser` is the part a
//! second island would reuse; everything below it would be written again, so a
//! new claim about browser behaviour belongs there rather than here.
//!
//! Everything here is pure and compiles natively, so it is unit-tested beside
//! the shared form model; `main.rs` owns the `web-sys` reading and writing.

mod browser;

pub use browser::{CaretDirection, ControlText, FocusRestore, FocusTarget, Selection};

use browser::Sanitiser;
use dioform::advanced::FieldIdentity;
use dioform::prelude::Form;
use ghinvite_ui::link_form::{CreateLinkForm, LinkFormValues};

/// The sanitiser of the control the server renders for `field`.
///
/// The description is an `<input type="text">` whose multiline value the
/// server preserves and rejects
/// ([`ghinvite_ui::link_form::DESCRIPTION_SINGLE_LINE`]); the internal note is
/// a `<textarea>`; the two guardrails are `type="number"`. Everything else —
/// the permission `<select>`, the checkboxes — is carried verbatim.
fn sanitiser_for(field: &FieldIdentity) -> Sanitiser {
    let fields = CreateLinkForm::fields();
    if *field == fields.description().identity()
        || *field == fields.email_domains().identity()
        || *field == fields.expiration_timestamp().identity()
        || *field == fields.access_lifetime_hours().identity()
        || *field == fields.invitation_lifetime_seconds().identity()
    {
        Sanitiser::Line
    } else if *field == fields.internal_note().identity() {
        Sanitiser::Text
    } else if *field == fields.max_uses().identity()
        || *field == fields.expires_in_days().identity()
        || *field == fields.invitation_lifetime_hours().identity()
        || *field == fields.abuse_threshold_per_hour().identity()
        || *field == fields.budget_share_pct().identity()
    {
        Sanitiser::Number
    } else {
        Sanitiser::Plain
    }
}

/// Every control of the server-rendered form, read from the live DOM just
/// before the island replaces it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormSnapshot {
    pub access_lifetime_hours: ControlText,
    pub email_domains: ControlText,
    pub description: ControlText,
    pub internal_note: ControlText,
    pub permission: ControlText,
    pub repo_grants: Vec<(u64, ControlText)>,
    /// The approval checkbox's `checked` property. Read directly rather than
    /// through an edited/unchanged test: a boolean always round-trips, and the
    /// approval policy must never flip during mounting.
    pub approval_required: bool,
    pub expiration_timestamp: ControlText,
    pub invitation_lifetime_seconds: ControlText,
    pub never_expires: bool,
    pub include_organization: bool,
    pub checked_team_ids: Vec<u64>,
    pub default_team_ids: Vec<u64>,
    pub max_uses: ControlText,
    pub expires_in_days: ControlText,
    pub invitation_lifetime_hours: ControlText,
    pub abuse_threshold_per_hour: ControlText,
    pub budget_share_pct: ControlText,
    /// Repository ids whose checkbox is checked, in DOM order.
    pub checked_repo_ids: Vec<u64>,
    /// Repository ids the server rendered as checked, in DOM order.
    pub default_repo_ids: Vec<u64>,
}

/// The values the island should mount with: `values` from the props blob, with
/// every control the admin already changed taken from `dom`.
///
/// A changed control also drops the server error attached to it. That error
/// described the value the admin has since replaced, and dioform clears a
/// restored field error on a related edit anyway — an edit made a second before
/// the bundle loaded should not behave differently from one made a second
/// after. When that leaves no diagnostics at all, the summary alert goes with
/// them rather than pointing at highlighted fields that no longer exist.
pub fn adopt(values: &LinkFormValues, dom: &FormSnapshot) -> LinkFormValues {
    let fields = CreateLinkForm::fields();
    let mut adopted = values.clone();
    if dom.access_lifetime_hours.edited(Sanitiser::Line) {
        adopted.access_lifetime_hours = dom.access_lifetime_hours.value.clone();
        adopted
            .errors
            .retire(&fields.access_lifetime_hours().identity());
    }
    if dom.email_domains.edited(Sanitiser::Line) {
        adopted.email_domains = dom.email_domains.value.clone();
        adopted.errors.retire(&fields.email_domains().identity());
    }
    if dom.expiration_timestamp.edited(Sanitiser::Line) {
        adopted.expiration_timestamp = dom.expiration_timestamp.value.clone();
        adopted
            .errors
            .retire(&fields.expiration_timestamp().identity());
    }
    if dom.invitation_lifetime_seconds.edited(Sanitiser::Line) {
        adopted.invitation_lifetime_seconds = dom.invitation_lifetime_seconds.value.clone();
        adopted
            .errors
            .retire(&fields.invitation_lifetime_seconds().identity());
    }
    adopted.include_organization = dom.include_organization;
    if dom.checked_team_ids != dom.default_team_ids {
        adopted.selected_team_ids = dom.checked_team_ids.clone();
        adopted.errors.retire(&fields.team_ids().identity());
    }
    if adopted.include_organization != values.include_organization {
        adopted
            .errors
            .retire(&fields.include_organization().identity());
    }

    if dom
        .description
        .edited(sanitiser_for(&fields.description().identity()))
    {
        adopted.description = dom.description.value.clone();
        adopted.errors.retire(&fields.description().identity());
    }
    if dom
        .internal_note
        .edited(sanitiser_for(&fields.internal_note().identity()))
    {
        adopted.internal_note = dom.internal_note.value.clone();
        adopted.errors.retire(&fields.internal_note().identity());
    }
    if dom
        .permission
        .edited(sanitiser_for(&fields.permission().identity()))
    {
        adopted.permission = dom.permission.value.clone();
        adopted.errors.retire(&fields.permission().identity());
    }
    for (id, control) in &dom.repo_grants {
        if control.edited(Sanitiser::Plain)
            && let Some((_, grant)) = control.value.split_once(':')
        {
            ghinvite_ui::link_form::set_repo_grant(&mut adopted.repo_grants, *id, grant);
            adopted.errors.retire(&fields.repo_grants().identity());
        }
    }
    adopted.approval_required = dom.approval_required;
    adopted.never_expires = dom.never_expires;
    if dom
        .max_uses
        .edited(sanitiser_for(&fields.max_uses().identity()))
    {
        adopted.max_uses = dom.max_uses.value.clone();
        adopted.errors.retire(&fields.max_uses().identity());
    }
    if dom
        .expires_in_days
        .edited(sanitiser_for(&fields.expires_in_days().identity()))
    {
        adopted.expires_in_days = dom.expires_in_days.value.clone();
        adopted.errors.retire(&fields.expires_in_days().identity());
    }
    if dom.invitation_lifetime_hours.edited(sanitiser_for(
        &fields.invitation_lifetime_hours().identity(),
    )) {
        adopted.invitation_lifetime_hours = dom.invitation_lifetime_hours.value.clone();
        adopted
            .errors
            .retire(&fields.invitation_lifetime_hours().identity());
    }
    if dom
        .abuse_threshold_per_hour
        .edited(sanitiser_for(&fields.abuse_threshold_per_hour().identity()))
    {
        adopted.abuse_threshold_per_hour = dom.abuse_threshold_per_hour.value.clone();
        adopted
            .errors
            .retire(&fields.abuse_threshold_per_hour().identity());
    }
    if dom
        .budget_share_pct
        .edited(sanitiser_for(&fields.budget_share_pct().identity()))
    {
        adopted.budget_share_pct = dom.budget_share_pct.value.clone();
        adopted.errors.retire(&fields.budget_share_pct().identity());
    }
    if dom.checked_repo_ids != dom.default_repo_ids {
        adopted.selected_repo_ids = dom.checked_repo_ids.clone();
        adopted.errors.retire(&fields.repo_ids().identity());
    }

    adopted
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghinvite_ui::link_form::{
        DESCRIPTION_REQUIRED, DESCRIPTION_SINGLE_LINE, EXPIRES_IN_DAYS_NOT_POSITIVE,
        LinkFormErrors, MAX_USES_NOT_POSITIVE, REPO_SCOPE_REQUIRED, SUMMARY_MESSAGE,
    };

    /// A snapshot of the form as the server rendered it: nothing edited.
    fn untouched(values: &LinkFormValues) -> FormSnapshot {
        FormSnapshot {
            access_lifetime_hours: ControlText::new(
                &values.access_lifetime_hours,
                &values.access_lifetime_hours,
            ),
            email_domains: ControlText::new(&*values.email_domains, &*values.email_domains),
            description: ControlText::new(&*values.description, &*values.description),
            internal_note: ControlText::new(&*values.internal_note, &*values.internal_note),
            permission: ControlText::new(&*values.permission, &*values.permission),
            repo_grants: values
                .repo_grants
                .iter()
                .filter_map(|raw| {
                    raw.split_once(':')
                        .and_then(|(id, _)| id.parse().ok())
                        .map(|id| (id, ControlText::new(raw, raw)))
                })
                .collect(),
            approval_required: values.approval_required,
            expiration_timestamp: ControlText::new(
                &*values.expiration_timestamp,
                &*values.expiration_timestamp,
            ),
            invitation_lifetime_seconds: ControlText::new(
                &*values.invitation_lifetime_seconds,
                &*values.invitation_lifetime_seconds,
            ),
            never_expires: values.never_expires,
            include_organization: values.include_organization,
            checked_team_ids: values.selected_team_ids.clone(),
            default_team_ids: values.selected_team_ids.clone(),
            max_uses: ControlText::new(&*values.max_uses, &*values.max_uses),
            expires_in_days: ControlText::new(&*values.expires_in_days, &*values.expires_in_days),
            invitation_lifetime_hours: ControlText::new(
                &*values.invitation_lifetime_hours,
                &*values.invitation_lifetime_hours,
            ),
            abuse_threshold_per_hour: ControlText::new(
                &*values.abuse_threshold_per_hour,
                &*values.abuse_threshold_per_hour,
            ),
            budget_share_pct: ControlText::new(
                &*values.budget_share_pct,
                &*values.budget_share_pct,
            ),
            checked_repo_ids: values.selected_repo_ids.clone(),
            default_repo_ids: values.selected_repo_ids.clone(),
        }
    }

    fn rejected() -> LinkFormValues {
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
            description: "   ".into(),
            permission: "owner".into(),
            approval_required: true,
            max_uses: "abc".into(),
            expires_in_days: "30".into(),
            internal_note: "note".into(),
            selected_repo_ids: vec![999],
            repo_grants: vec!["999:push".into()],
            errors: LinkFormErrors {
                summary: vec![SUMMARY_MESSAGE.into(), "server said no".into()],
                description: Some(DESCRIPTION_REQUIRED.into()),
                max_uses: Some(MAX_USES_NOT_POSITIVE.into()),
                repo_scope: Some(REPO_SCOPE_REQUIRED.into()),
                ..LinkFormErrors::default()
            },
        }
    }

    #[test]
    fn explicit_never_choice_survives_takeover_independently_of_days() {
        let values = LinkFormValues::default();
        let mut dom = untouched(&values);
        dom.never_expires = true;
        dom.expires_in_days = ControlText::new("", "14");
        let adopted = adopt(&values, &dom);
        assert!(adopted.never_expires);
        assert_eq!(adopted.expires_in_days, "");
        dom.never_expires = false;
        assert!(!adopt(&adopted, &dom).never_expires);
    }

    #[test]
    fn an_untouched_rejected_form_mounts_with_the_props_values() {
        let values = rejected();
        assert_eq!(adopt(&values, &untouched(&values)), values);
    }

    #[test]
    fn an_untouched_fresh_form_mounts_with_the_props_values() {
        let values = LinkFormValues::default();
        assert_eq!(adopt(&values, &untouched(&values)), values);
    }

    #[test]
    fn every_edited_guardrail_is_taken_from_the_dom() {
        let values = LinkFormValues::default();
        let adopted = adopt(
            &values,
            &FormSnapshot {
                access_lifetime_hours: Default::default(),
                email_domains: Default::default(),
                expiration_timestamp: Default::default(),
                invitation_lifetime_seconds: Default::default(),
                include_organization: false,
                checked_team_ids: vec![],
                default_team_ids: vec![],
                invitation_lifetime_hours: ControlText::new("168", "168"),
                abuse_threshold_per_hour: ControlText::new("", ""),
                budget_share_pct: ControlText::new("", ""),
                repo_grants: Vec::new(),
                never_expires: false,
                description: ControlText::new("Workshop", ""),
                internal_note: ControlText::new("why", ""),
                permission: ControlText::new("maintain", "pull"),
                approval_required: true,
                max_uses: ControlText::new("5", ""),
                expires_in_days: ControlText::new("", "30"),
                checked_repo_ids: vec![10, 12],
                default_repo_ids: Vec::new(),
            },
        );

        assert_eq!(
            adopted,
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
                description: "Workshop".into(),
                permission: "maintain".into(),
                approval_required: true,
                max_uses: "5".into(),
                expires_in_days: String::new(),
                internal_note: "why".into(),
                selected_repo_ids: vec![10, 12],
                repo_grants: vec![],
                errors: LinkFormErrors::default(),
            }
        );
    }

    #[test]
    fn the_approval_checkbox_is_taken_from_the_dom_in_both_directions() {
        for (props, dom) in [(true, false), (false, true)] {
            let values = LinkFormValues {
                approval_required: props,
                ..LinkFormValues::default()
            };
            let adopted = adopt(
                &values,
                &FormSnapshot {
                    approval_required: dom,
                    ..untouched(&values)
                },
            );
            assert_eq!(adopted.approval_required, dom);
        }
    }

    #[test]
    fn an_unsupported_permission_survives_the_select_displaying_its_pull_fallback() {
        let values = LinkFormValues {
            permission: "owner".into(),
            ..LinkFormValues::default()
        };
        let adopted = adopt(
            &values,
            &FormSnapshot {
                // The server explicitly selects `pull` for a value it cannot show.
                permission: ControlText::new("pull", "pull"),
                ..untouched(&values)
            },
        );
        assert_eq!(adopted.permission, "owner");
    }

    #[test]
    fn a_browser_sanitised_numeric_field_keeps_its_raw_text_and_parse_blocker() {
        let values = LinkFormValues {
            max_uses: "abc".into(),
            expires_in_days: "abc".into(),
            errors: LinkFormErrors {
                summary: vec![SUMMARY_MESSAGE.into()],
                max_uses: Some(MAX_USES_NOT_POSITIVE.into()),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };
        let adopted = adopt(
            &values,
            &FormSnapshot {
                // Chromium shows nothing for `value="abc"`, untouched.
                max_uses: ControlText::new("", "abc"),
                expires_in_days: ControlText::new("", "abc"),
                ..untouched(&values)
            },
        );

        assert_eq!(adopted.max_uses, "abc");
        assert_eq!(adopted.expires_in_days, "abc");
        assert_eq!(
            adopted.errors.max_uses.as_deref(),
            Some(MAX_USES_NOT_POSITIVE)
        );
    }

    #[test]
    fn typing_into_a_sanitised_numeric_field_is_still_an_edit() {
        let values = LinkFormValues {
            max_uses: "abc".into(),
            ..LinkFormValues::default()
        };
        let adopted = adopt(
            &values,
            &FormSnapshot {
                max_uses: ControlText::new("5", "abc"),
                ..untouched(&values)
            },
        );
        assert_eq!(adopted.max_uses, "5");
    }

    #[test]
    fn clearing_a_numeric_field_the_server_rendered_as_a_number_is_an_edit() {
        let values = LinkFormValues::default();
        let adopted = adopt(
            &values,
            &FormSnapshot {
                expires_in_days: ControlText::new("", "30"),
                ..untouched(&values)
            },
        );
        assert_eq!(adopted.expires_in_days, "");
    }

    /// The server rejected `0` but the browser shows it, so clearing it is a
    /// real edit — reading "the domain parser rejects this" as "the browser
    /// emptied this" would hand the admin `0` back.
    #[test]
    fn clearing_a_displayable_number_the_server_rejected_is_still_an_edit() {
        let values = LinkFormValues {
            expires_in_days: "0".into(),
            errors: LinkFormErrors {
                summary: vec![SUMMARY_MESSAGE.into()],
                expires_in_days: Some(EXPIRES_IN_DAYS_NOT_POSITIVE.into()),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };
        let adopted = adopt(
            &values,
            &FormSnapshot {
                expires_in_days: ControlText::new("", "0"),
                ..untouched(&values)
            },
        );

        assert_eq!(adopted.expires_in_days, "");
        assert!(adopted.errors.is_empty());
    }

    /// Chromium shows `a\nb` as `ab` in a `type="text"` input while
    /// `defaultValue` keeps the newline, so an untouched multiline description
    /// must not read as an edit — the server rejected it for that newline.
    #[test]
    fn a_multiline_description_the_browser_shortened_is_not_an_edit() {
        let values = LinkFormValues {
            description: "one\ntwo".into(),
            errors: LinkFormErrors {
                summary: vec![SUMMARY_MESSAGE.into()],
                description: Some(DESCRIPTION_SINGLE_LINE.into()),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };
        let adopted = adopt(
            &values,
            &FormSnapshot {
                repo_grants: Vec::new(),
                description: ControlText::new("onetwo", "one\ntwo"),
                ..untouched(&values)
            },
        );

        assert_eq!(adopted.description, "one\ntwo");
        assert_eq!(
            adopted.errors.description.as_deref(),
            Some(DESCRIPTION_SINGLE_LINE)
        );
    }

    #[test]
    fn actually_retyping_a_shortened_description_is_an_edit() {
        let values = LinkFormValues {
            description: "one\ntwo".into(),
            errors: LinkFormErrors {
                summary: vec![SUMMARY_MESSAGE.into()],
                description: Some(DESCRIPTION_SINGLE_LINE.into()),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };
        let adopted = adopt(
            &values,
            &FormSnapshot {
                description: ControlText::new("one two", "one\ntwo"),
                ..untouched(&values)
            },
        );

        assert_eq!(adopted.description, "one two");
        assert!(adopted.errors.is_empty());
    }

    #[test]
    fn a_textarea_whose_line_endings_the_browser_normalised_is_not_an_edit() {
        let values = LinkFormValues {
            internal_note: "one\r\ntwo".into(),
            ..LinkFormValues::default()
        };
        let adopted = adopt(
            &values,
            &FormSnapshot {
                internal_note: ControlText::new("one\ntwo", "one\r\ntwo"),
                ..untouched(&values)
            },
        );
        assert_eq!(adopted.internal_note, "one\r\ntwo");
    }

    #[test]
    fn an_overflowing_numeric_guardrail_the_browser_emptied_keeps_its_blocker() {
        let values = LinkFormValues {
            max_uses: "1e999".into(),
            errors: LinkFormErrors {
                summary: vec![SUMMARY_MESSAGE.into()],
                max_uses: Some(MAX_USES_NOT_POSITIVE.into()),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };
        let adopted = adopt(
            &values,
            &FormSnapshot {
                max_uses: ControlText::new("", "1e999"),
                ..untouched(&values)
            },
        );

        assert_eq!(adopted.max_uses, "1e999");
        assert_eq!(
            adopted.errors.max_uses.as_deref(),
            Some(MAX_USES_NOT_POSITIVE)
        );
    }

    #[test]
    fn editing_a_field_drops_its_server_error_and_leaves_the_others() {
        let values = rejected();
        let adopted = adopt(
            &values,
            &FormSnapshot {
                description: ControlText::new("Workshop", "   "),
                ..untouched(&values)
            },
        );

        assert_eq!(adopted.description, "Workshop");
        assert_eq!(adopted.errors.description, None);
        assert_eq!(
            adopted.errors.max_uses.as_deref(),
            Some(MAX_USES_NOT_POSITIVE)
        );
        assert_eq!(
            adopted.errors.summary,
            vec![SUMMARY_MESSAGE.to_string(), "server said no".to_string()]
        );
    }

    #[test]
    fn selecting_a_repository_drops_the_scope_error() {
        let values = LinkFormValues {
            errors: LinkFormErrors {
                summary: vec![SUMMARY_MESSAGE.into()],
                repo_scope: Some(REPO_SCOPE_REQUIRED.into()),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };
        let adopted = adopt(
            &values,
            &FormSnapshot {
                checked_repo_ids: vec![11],
                default_repo_ids: Vec::new(),
                ..untouched(&values)
            },
        );

        assert_eq!(adopted.selected_repo_ids, vec![11]);
        assert!(adopted.errors.is_empty());
    }

    #[test]
    fn the_summary_alert_goes_once_the_last_diagnostic_is_edited_away() {
        let values = LinkFormValues {
            description: "   ".into(),
            errors: LinkFormErrors {
                summary: vec![SUMMARY_MESSAGE.into()],
                description: Some(DESCRIPTION_REQUIRED.into()),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };
        let adopted = adopt(
            &values,
            &FormSnapshot {
                description: ControlText::new("Workshop", "   "),
                ..untouched(&values)
            },
        );
        assert!(adopted.errors.is_empty());
    }

    #[test]
    fn a_form_level_message_keeps_the_summary_alert_after_every_field_is_corrected() {
        let values = LinkFormValues {
            description: "   ".into(),
            errors: LinkFormErrors {
                summary: vec![SUMMARY_MESSAGE.into(), "server said no".into()],
                description: Some(DESCRIPTION_REQUIRED.into()),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };
        let adopted = adopt(
            &values,
            &FormSnapshot {
                description: ControlText::new("Workshop", "   "),
                ..untouched(&values)
            },
        );
        assert_eq!(
            adopted.errors.summary,
            vec![SUMMARY_MESSAGE.to_string(), "server said no".to_string()]
        );
    }

    #[test]
    fn unselecting_the_only_repository_is_an_edit() {
        let values = LinkFormValues {
            selected_repo_ids: vec![10],
            repo_grants: vec!["10:push".into()],
            ..LinkFormValues::default()
        };
        let adopted = adopt(
            &values,
            &FormSnapshot {
                checked_repo_ids: Vec::new(),
                default_repo_ids: vec![10],
                ..untouched(&values)
            },
        );
        assert!(adopted.selected_repo_ids.is_empty());
    }

    /// Every control of the create form the server renders, as its element
    /// (`input`, `textarea`, `select`), its `type` attribute and its `name`.
    fn rendered_controls() -> Vec<(&'static str, Option<String>, String)> {
        use dioxus::prelude::*;
        use ghinvite_ui::link_form::RepositoryChoice;
        use ghinvite_ui::links::LinkCreateForm;

        let mut vdom = VirtualDom::new(|| {
            rsx! {
                LinkCreateForm {
                    action: "/console/accounts/acme/links".to_string(),
                    form: LinkFormValues::default(),
                    organization: ghinvite_ui::link_form::OrganizationOffer::Available(ghinvite_ui::link_form::OrganizationChoice {id:1,login:"acme".into(),teams:vec![ghinvite_ui::link_form::TeamChoice{id:7,slug:"learners".into(),name:"Learners".into(),externally_managed:false}]}),
                    repos: vec![RepositoryChoice { id: 10, full_name: "acme/api".into() }],
                }
            }
        });
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);

        fn attribute(tag: &str, name: &str) -> Option<String> {
            let (_, rest) = tag.split_once(&format!(" {name}=\""))?;
            Some(rest.split_once('"')?.0.to_string())
        }
        let mut controls = Vec::new();
        for element in ["input", "textarea", "select"] {
            for tag in html.split(&format!("<{element} ")).skip(1) {
                let tag = format!(" {}", tag.split_once('>').unwrap().0);
                let name = attribute(&tag, "name").expect("every control submits a name");
                controls.push((element, attribute(&tag, "type"), name));
            }
        }
        controls
    }

    /// `adopt` compares each control against what the browser shows for the
    /// server's value, and what it shows depends on the element the server
    /// rendered. The two are chosen in different crates, so this reads the
    /// sanitiser each control needs off the real markup: turning the
    /// description into a `<textarea>` must fail here, not only in Playwright.
    #[test]
    fn every_rendered_control_is_adopted_with_its_elements_sanitiser() {
        let fields = CreateLinkForm::fields();
        // Each field's rendered name next to the identity `adopt` looks up.
        let by_name = [
            fields.email_domains().field_name().to_string(),
            fields.expiration_timestamp().field_name().to_string(),
            fields
                .invitation_lifetime_seconds()
                .field_name()
                .to_string(),
            fields.include_organization().field_name().to_string(),
            fields.team_ids().field_name().to_string(),
            fields.description().field_name().to_string(),
            fields.internal_note().field_name().to_string(),
            fields.permission().field_name().to_string(),
            fields.approval_required().field_name().to_string(),
            fields.never_expires().field_name().to_string(),
            fields.max_uses().field_name().to_string(),
            fields.expires_in_days().field_name().to_string(),
            fields.invitation_lifetime_hours().field_name().to_string(),
            fields.abuse_threshold_per_hour().field_name().to_string(),
            fields.budget_share_pct().field_name().to_string(),
            fields.repo_ids().field_name().to_string(),
            fields.repo_grants().field_name().to_string(),
        ]
        .into_iter()
        .zip([
            fields.email_domains().identity(),
            fields.expiration_timestamp().identity(),
            fields.invitation_lifetime_seconds().identity(),
            fields.include_organization().identity(),
            fields.team_ids().identity(),
            fields.description().identity(),
            fields.internal_note().identity(),
            fields.permission().identity(),
            fields.approval_required().identity(),
            fields.never_expires().identity(),
            fields.max_uses().identity(),
            fields.expires_in_days().identity(),
            fields.invitation_lifetime_hours().identity(),
            fields.abuse_threshold_per_hour().identity(),
            fields.budget_share_pct().identity(),
            fields.repo_ids().identity(),
            fields.repo_grants().identity(),
        ])
        .collect::<Vec<_>>();

        let mut seen = Vec::new();
        for (element, kind, name) in rendered_controls() {
            let needed = match (element, kind.as_deref()) {
                ("input", Some("hidden")) => continue,
                ("input", None | Some("text")) => Sanitiser::Line,
                ("input", Some("number")) => Sanitiser::Number,
                ("input", Some("checkbox")) | ("select", None) => Sanitiser::Plain,
                ("textarea", None) => Sanitiser::Text,
                // ADR 0001: a new control kind is new sanitisation to establish
                // in a browser before the takeover can adopt it.
                other => panic!("{name} renders as {other:?}, which has no sanitiser"),
            };
            let (_, field) = by_name
                .iter()
                .find(|(field_name, _)| *field_name == name)
                .unwrap_or_else(|| panic!("{name} is not a field of the model"));
            assert_eq!(
                sanitiser_for(field),
                needed,
                "{name} renders as <{element}>"
            );
            seen.push(name);
        }

        for (name, _) in &by_name {
            assert!(seen.iter().any(|seen| seen == name), "{name} is rendered");
        }
    }
    #[test]
    fn target_grants_preserve_raw_rejection_and_adopt_only_deliberate_changes() {
        let values = LinkFormValues {
            selected_repo_ids: vec![10, 11],
            repo_grants: vec!["10:owner".into(), "11:maintain".into()],
            ..Default::default()
        };
        let mut dom = untouched(&values);
        dom.repo_grants = vec![
            (10, ControlText::new("10:pull", "10:pull")),
            (11, ControlText::new("11:admin", "11:maintain")),
        ];
        let adopted = adopt(&values, &dom);
        assert_eq!(adopted.repo_grants, vec!["10:owner", "11:admin"]);
        dom.repo_grants[0].1 = ControlText::new("10:triage", "10:pull");
        let adopted = adopt(&values, &dom);
        assert_eq!(adopted.repo_grants, vec!["10:triage", "11:admin"]);
    }
    #[test]
    fn lifetime_and_threshold_edits_survive_takeover_and_retire_only_their_errors() {
        let mut values = LinkFormValues::default();
        values.errors.invitation_lifetime_hours = Some("old lifetime error".into());
        values.errors.abuse_threshold_per_hour = Some("old threshold error".into());
        values.errors.budget_share_pct = Some("old share error".into());
        let mut dom = untouched(&values);
        dom.invitation_lifetime_hours = ControlText::new("1", "168");
        dom.abuse_threshold_per_hour = ControlText::new("25", "");
        dom.budget_share_pct = ControlText::new("17", "50");
        let adopted = adopt(&values, &dom);
        assert_eq!(adopted.invitation_lifetime_hours, "1");
        assert_eq!(adopted.abuse_threshold_per_hour, "25");
        assert!(adopted.errors.invitation_lifetime_hours.is_none());
        assert!(adopted.errors.abuse_threshold_per_hour.is_none());
        assert_eq!(adopted.to_model().0.invitation_lifetime_hours, Some(1));
        assert_eq!(adopted.to_model().0.abuse_threshold_per_hour, Some(25));
        assert_eq!(adopted.budget_share_pct, "17");
        assert!(adopted.errors.budget_share_pct.is_none());
        assert_eq!(adopted.to_model().0.budget_share_pct, Some(17));
    }
    #[test]
    fn email_domain_edits_survive_takeover_and_retire_only_their_error() {
        let values = LinkFormValues {
            email_domains: "EXAMPLE.com.".into(),
            errors: LinkFormErrors {
                email_domains: Some("old error".into()),
                description: Some("still relevant".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut dom = untouched(&values);
        dom.email_domains = ControlText::new("*.example.org", "EXAMPLE.com.");
        let adopted = adopt(&values, &dom);
        assert_eq!(adopted.email_domains, "*.example.org");
        assert!(adopted.errors.email_domains.is_none());
        assert!(adopted.errors.description.is_some());
    }
    #[test]
    fn access_lifetime_takeover_preserves_rejected_exact_text_and_accepts_an_explicit_edit() {
        let mut values = LinkFormValues {
            access_lifetime_hours: "bad\nvalue".into(),
            ..Default::default()
        };
        values.errors.access_lifetime_hours = Some("invalid".into());
        let mut dom = untouched(&values);
        dom.access_lifetime_hours.value = "badvalue".into();
        assert_eq!(adopt(&values, &dom), values);
        dom.access_lifetime_hours.value = "3601s".into();
        let edited = adopt(&values, &dom);
        assert_eq!(edited.access_lifetime_hours, "3601s");
        assert!(edited.errors.access_lifetime_hours.is_none());
    }
}
