//! Native forms for mandatory batch review. Rendering never sends a command.
use dioxus::prelude::*;
use ghinvite_core::{
    bulk::{Entry, Review, ReviewStatus},
    bulk_commands::{BatchProgress, Observation},
    bulk_preview::{ObservedAccess, PreviewAction, PreviewTarget},
};

/// Native form shares the repository selection component and target validator
/// with link creation, without mounting another browser island.
#[component]
pub fn BulkEntryPage(
    signed_in_login: Option<String>,
    account_login: String,
    batch_id: String,
    repos: Vec<crate::link_form::RepositoryChoice>,
    organization: crate::link_form::OrganizationOffer,
    #[props(default)] repository_query: String,
    #[props(default)] access_removal_enabled: bool,
    #[props(default)] message: Option<String>,
) -> Element {
    rsx! {
        crate::layouts::ConsoleLayout { signed_in_login,title:"Bulk invite",account_login:Some(account_login.clone()),active_nav:Some("bulk".into()),children:rsx!{
            h1 { class:"mb-4 text-xl font-semibold tracking-tight", "Bulk invite" }
            if let Some(message)=&message { p {role:"status","{message}"} }
            p { "Paste one GitHub handle or email address per line, up to 200 entries. Email addresses require an organization target. Resolve the entries, then review every row before submitting." }
            form {method:"get",action:format!("/console/accounts/{account_login}/bulk/new"),
                label {r#for:"bulk-repository-search","Find repositories"}
                input {id:"bulk-repository-search",name:"q",value:repository_query}
                button {r#type:"submit","Search repositories"}
            }
            form {class:"mt-5 space-y-4",method:"post",action:format!("/console/accounts/{account_login}/bulk/{batch_id}/resolve"),
                crate::csrf::CsrfField {}
                label {r#for:"bulk-entries","Handles and email addresses"}
                textarea {class:"textarea textarea-bordered block w-full font-mono",id:"bulk-entries",name:"entries",rows:12,required:true,autocomplete:"off",spellcheck:"false"}
                crate::links::RepositoryScopeGroup {name:"repo_ids",repos,selected:vec![],grants:vec![],help:"Choose each repository and its role."}
                match &organization {
                    crate::link_form::OrganizationOffer::Available(org)=>rsx!{
                        fieldset {
                            legend {"Organization and teams"}
                            label {input {r#type:"checkbox",name:"include_organization",value:"true"} "Invite to {org.login} as a member"}
                            for team in &org.teams {label {input {r#type:"checkbox",name:"team_ids",value:team.id.to_string()} "{team.name}"}}
                        }
                    },
                    crate::link_form::OrganizationOffer::Unavailable=>rsx!{p {"Organization targets could not be checked. Refresh before selecting them."}},
                    _=>rsx!{},
                }
                if access_removal_enabled {
                    label {r#for:"bulk-access-lifetime","Access lifetime (optional)"}
                    input {id:"bulk-access-lifetime",name:"access_lifetime_hours",aria_describedby:"bulk-access-help"}
                    p {id:"bulk-access-help","Enter whole hours or exact seconds with an s suffix. Blank keeps accepted access without automatic expiry. A finite lifetime starts when acceptance is confirmed and is fixed for this batch and its personal links."}
                    AccessRemovalCaveats {}
                }
                button {class:"btn btn-primary btn-sm",r#type:"submit","Resolve and review"}
            }
        }}
    }
}

/// Shared before both resolving a finite lifetime and confirming its reviewed value.
#[component]
fn AccessRemovalCaveats() -> Element {
    rsx! {
        p {"Only access newly granted by P1 is eligible. Upgrades and pre-existing access are excluded; P1 never restores a previous role. Promoted organization owners, team maintainers, and higher or custom repository roles are skipped."}
        p {"Email organization invitations have no access expiry. Repository access accepted through a personal link uses this lifetime."}
        ul {class:"list-disc pl-5 text-sm",
            li {"Organization, team, or base access can remain after repository collaborator removal."}
            li {"Removing a repository collaborator can delete their private forks."}
            li {"Removing an organization member removes their team and repository access in that organization."}
        }
        p {"Removal is best effort and checks current access again before any change."}
    }
}
fn access_duration(seconds: u32) -> String {
    if seconds.is_multiple_of(3600) {
        let hours = seconds / 3600;
        format!("{hours} {}", if hours == 1 { "hour" } else { "hours" })
    } else {
        format!("{seconds} seconds")
    }
}

pub fn status_label(status: &ReviewStatus) -> String {
    match status {
        ReviewStatus::Found => "Found".into(),
        ReviewStatus::NotFound => "Not found — remove this row".into(),
        ReviewStatus::Unavailable => {
            "Could not verify — remove this row or start a new batch".into()
        }
        ReviewStatus::EmailOrgOnly => "Email (organization invite only)".into(),
        ReviewStatus::NeedsPersonalLink => "Needs a personal link for repository access".into(),
        ReviewStatus::AlreadySatisfied => "Already has the required role".into(),
        ReviewStatus::OverBudget { until } => format!("Queued for budget; next slot {until}"),
        ReviewStatus::Duplicate { of_row } => {
            format!("Duplicate of row {of_row}; no separate invitation")
        }
    }
}
pub fn ready(review: &Review) -> bool {
    review.rows.iter().any(|row| {
        !review.removed.contains(&row.row) && !matches!(row.status, ReviewStatus::Duplicate { .. })
    }) && review.rows.iter().all(|row| {
        review.removed.contains(&row.row)
            || match row.status {
                ReviewStatus::NotFound | ReviewStatus::Unavailable => false,
                ReviewStatus::NeedsPersonalLink => review.personal_link_rows.contains(&row.row),
                _ => true,
            }
    })
}
fn preview_label(preview: &ghinvite_core::bulk_preview::TargetPreview) -> String {
    let target = match preview.target {
        PreviewTarget::Repository { repo_id } => format!("Repository {repo_id}"),
        PreviewTarget::Organization { org_id } => format!("Organization {org_id}"),
        PreviewTarget::Team { team_id } => format!("Team {team_id}"),
    };
    let observed = match &preview.observed {
        ObservedAccess::None => "no existing access".into(),
        ObservedAccess::Unavailable => "unavailable".into(),
        ObservedAccess::Repository {
            role: ghinvite_core::targets::EffectiveRole::Custom(name),
        } => format!("custom role {}", name.as_str()),
        ObservedAccess::Repository {
            role: ghinvite_core::targets::EffectiveRole::None,
        } => "no existing access".into(),
        ObservedAccess::Repository { role } => format!("{role:?}"),
        ObservedAccess::Membership { state, role } => format!("{state:?} {role:?}"),
    };
    let action = match preview.action {
        PreviewAction::Satisfied => "already satisfied",
        PreviewAction::WillInvite => "will invite",
        PreviewAction::WillRaise => "will raise access",
        PreviewAction::CustomRole => "custom role; delivery will check",
        PreviewAction::Pending => "membership pending",
        PreviewAction::Unavailable => "preview unavailable",
    };
    format!("{target}: {observed}; {action}")
}

#[component]
pub fn BulkReviewPage(
    signed_in_login: Option<String>,
    account_login: String,
    review: Review,
    edit_operation: String,
    submit_operation: String,
    #[props(default)] cancel_operation: String,
    #[props(default)] recover_submission: bool,
    #[props(default)] reissue_enabled: bool,
    #[props(default)] organization_reissue_enabled: bool,
    #[props(default)] progress_next: Option<String>,
    #[props(default)] progress_refresh: String,
    #[props(default)] progress: Option<BatchProgress>,
    #[props(default)] message: Option<String>,
    #[props(default)] shown_links: Vec<(u16, String)>,
    /// Transient labels restored from this browser session, outside batch data.
    #[props(default)]
    email_labels: std::collections::BTreeMap<u16, String>,
) -> Element {
    let base = format!(
        "/console/accounts/{account_login}/bulk/{}",
        review.manifest.batch_id
    );
    let submitted = review.submitted.is_some();
    let can_submit = ready(&review);
    let retained_rows: Vec<_> = review
        .rows
        .iter()
        .filter(|row| {
            !review.removed.contains(&row.row)
                && !matches!(row.status, ReviewStatus::Duplicate { .. })
        })
        .collect();
    let handles = retained_rows
        .iter()
        .filter(|row| {
            matches!(
                row.evidence.resolution,
                ghinvite_core::bulk::Resolution::Found { .. }
            )
        })
        .count();
    let emails = retained_rows
        .iter()
        .filter(|row| {
            matches!(
                row.evidence.resolution,
                ghinvite_core::bulk::Resolution::Email
            )
        })
        .count();
    let duplicates = review
        .rows
        .iter()
        .filter(|row| matches!(row.status, ReviewStatus::Duplicate { .. }))
        .count();
    let personal = retained_rows
        .iter()
        .filter(|row| review.personal_link_rows.contains(&row.row))
        .count();
    let next_slot = retained_rows
        .iter()
        .filter_map(|row| row.evidence.next_budget_slot)
        .min();
    rsx! {
        crate::layouts::ConsoleLayout { signed_in_login,title:"Bulk invite",account_login:Some(account_login.clone()),active_nav:Some("bulk".into()),children:rsx!{
            h1 { class:"mb-4 text-xl font-semibold tracking-tight", "Bulk invite" }
            a {href:format!("/console/accounts/{account_login}/bulk/new"),"Start another batch"}
            if let Some(message)=&message { p { role:"status","{message}" } }
            if !submitted {
                p { "Review every row and target before submitting. No GitHub invitation has been sent for this batch. Role previews are advisory; each delivery checks current access again." }
            } else {
                p { "The batch was submitted. Submission and delivery are separate from acceptance or access being granted." }
            }
            if submitted {
                form {method:"post",action:format!("{base}/cancel"),
                    crate::csrf::CsrfField {}
                    input {r#type:"hidden",name:"operation_id",value:cancel_operation}
                    p {"Stop work that has not been sent. Sent invitations and uncertain GitHub outcomes remain tracked."}
                    button {class:"btn btn-warning btn-sm my-3",r#type:"submit","Cancel unsent work"}
                }
            }
            p { "Review revision {review.revision}. Removed and duplicate rows do not create requests." }
            if duplicates>0 {p {"Removing an original row does not turn a later duplicate into a separate invitation. Start a new batch if you need to change that choice."}}
            p {"Selected work — approved requests: {handles}; organization email invitations: {emails}; personal links: {personal}. Removed rows: {review.removed.len()}. Duplicate rows: {duplicates}."}
            if let Some(at)=next_slot {p {"Earliest observed slot for queued work: {at}. Delivery checks the budget again before sending."}}
            h2 {class:"mt-5 text-base font-semibold","Reviewed target scope"}
            ul {
                for repo in &review.manifest.targets.repos {li {"{repo.repo_full_name}: {repo.grant:?}"}}
                if let Some(org)=&review.manifest.targets.organization {li {"{org.org_login}: organization member"}}
                for team in &review.manifest.targets.teams {li {"Team {team.team_slug}: member"}}
            }
            section {class:"mt-5 space-y-2",aria_labelledby:"bulk-access-policy-heading",
                h2 {id:"bulk-access-policy-heading",class:"text-base font-semibold","Access expiry policy"}
                if let Some(seconds)=review.manifest.access_lifetime_seconds {
                    p {"Remove eligible new access {access_duration(seconds)} after acceptance is confirmed. This lifetime is fixed for this batch and its personal links."}
                    AccessRemovalCaveats {}
                } else {
                    p {"No automatic access expiry. This batch does not schedule removal of accepted access."}
                }
            }
            if recover_submission {
                p {role:"status","An original submission is retained. Recover that operation before making further review changes."}
                form {method:"post",action:format!("{base}/retry"),
                    crate::csrf::CsrfField {}
                    input {r#type:"hidden",name:"record_id",value:"submit"}
                    button {class:"btn btn-secondary btn-sm my-3",r#type:"submit","Recover original submission"}
                }
            }
            form {class:"mt-4 overflow-x-auto", method:"post",action:format!("{base}/rows/remove"),
                crate::csrf::CsrfField {}
                input { r#type:"hidden",name:"operation_id",value:edit_operation }
                input { r#type:"hidden",name:"expected_revision",value:review.revision.to_string() }
                table {class:"table table-sm w-full",
                    caption { "Reviewed entries and target previews" }
                    thead { tr { th { scope:"col","Row" } th { scope:"col","Entry" } th { scope:"col","Status" } th { scope:"col","Targets" } th { scope:"col","Review choices" } } }
                    tbody { for row in &review.rows {
                        tr { key:"{row.row}",
                            th { scope:"row","{row.row}" }
                            td { match &review.manifest.entries[usize::from(row.row)-1] {
                                Entry::Handle{login}=>rsx!{"@{login}"},
                                Entry::Email{..}=>rsx!{{email_labels.get(&row.row).cloned().unwrap_or_else(||format!("Email entry {}",row.row))}},
                            } }
                            td { if review.removed.contains(&row.row) {"Removed"} else {"{status_label(&row.status)}"} }
                            td {
                                if review.removed.contains(&row.row) {"Excluded from delivery"}
                                else if matches!(row.status,ReviewStatus::Duplicate{..}) {"No separate delivery"}
                                else if row.evidence.previews.is_empty() { "No user role preview" }
                                else { ul { for preview in &row.evidence.previews { li { "{preview_label(preview)}" } } } }
                            }
                            td { if !submitted && !recover_submission {
                                label { input { r#type:"checkbox",name:"remove_rows",value:row.row.to_string(),checked:review.removed.contains(&row.row),disabled:review.removed.contains(&row.row) } "Remove row {row.row}" }
                                if matches!(row.status,ReviewStatus::NeedsPersonalLink) {
                                    label { input { r#type:"checkbox",name:"personal_link_rows",value:row.row.to_string(),checked:review.personal_link_rows.contains(&row.row),disabled:review.removed.contains(&row.row) } "Create a single-use personal link for repository access" }
                                }
                            } }
                        }
                    } }
                }
                if !submitted && !recover_submission { button {class:"btn btn-secondary btn-sm mt-3", r#type:"submit","Apply review choices" } }
            }
            if !submitted && !recover_submission {
                p { "Personal links allow one request, require no approval and expire after seven days. Their URLs are shown once after creation; distribute them yourself. This application sends no email." }
                form { method:"post",action:format!("{base}/submit"),
                    crate::csrf::CsrfField {}
                    input { r#type:"hidden",name:"operation_id",value:submit_operation }
                    input { r#type:"hidden",name:"expected_revision",value:review.revision.to_string() }
                    button {class:"btn btn-primary btn-sm my-3", r#type:"submit",disabled:!can_submit,"Submit batch" }
                }
                if !can_submit {p {"Remove unresolved rows and apply personal-link choices before submitting."}}
            }
            if !shown_links.is_empty() {
                h2 {class:"mt-6 text-base font-semibold", "Save these personal links" }
                p { "These URLs are shown once. Copy them now and send each one to the intended person." }
                ul { for (row,url) in &shown_links { li { "Row {row}: " a {class:"link break-all",href:url.clone(),rel:"noreferrer", "{url}"} } } }
            }
            if let Some(submission)=&review.submitted
                && !submission.personal_links.is_empty() {
                h2 {"Personal link records"}
                p {"Public URLs are shown once. If a URL was not saved, open its link record to rotate it."}
                ul {for link in &submission.personal_links {li {"Row {link.source.row}: " a {href:format!("/console/accounts/{account_login}/links/{}",link.link_id),"Open personal link record"}}}}
            }
            if let Some(progress)=&progress {
                h2 {class:"mt-6 text-base font-semibold", "Delivery progress" }
                p {"Progress is read from delivery owners in bounded pages. A page reports only the rows and targets shown here."}
                if progress.handoff_pending {p {role:"status","Some requests are still being prepared. Refresh to check again."}}
                ul { for row in &progress.requests {
                    li { "Row {row.source.row}: "
                        match &row.initialized {
                            Observation::Pending=>rsx!{"Preparing request"},Observation::Unavailable=>rsx!{"Request observation unavailable; refresh to check again"},Observation::Observed(_)=>rsx!{"Approved request; checking delivery"},
                        }
                        if !row.final_results.is_empty() {
                            p {"Recorded outcomes for the current delivery generations:"}
                            ul {for result in &row.final_results {li {"{final_result_label(result)}"}}}
                        }
                        ul { for repo in &row.repositories {
                            li { "Repository {repo.repo_id}, generation {repo.delivery_generation}: "
                                match &repo.observation {
                                    Observation::Pending=>rsx!{"Awaiting delivery"},Observation::Unavailable=>rsx!{"Observation unavailable"},
                                    Observation::Observed(snapshot)=>rsx!{"{repository_status(snapshot)}"},
                                }
                                if reissue_enabled {
                                    a {class:"link",href:format!("/console/accounts/{account_login}/requests/{}/reissue/repo/{}",row.request_id,repo.repo_id),"Review repository reissue"}
                                }
                            }
                        } }
                        if row.next_target_offset.is_some() {p {"More targets for this row are on the next progress page."}}
                        if let Some(org)=&row.organization {
                            p { "Organization: " match org {
                                Observation::Pending=>rsx!{"Awaiting delivery"},Observation::Unavailable=>rsx!{"Observation unavailable"},Observation::Observed(snapshot)=>rsx!{"{organization_status(snapshot)}"},
                            } }
                            if reissue_enabled && organization_reissue_enabled && let Some(target)=&review.manifest.targets.organization {
                                a {class:"link",href:format!("/console/accounts/{account_login}/requests/{}/reissue/org/{}",row.request_id,target.org_id),"Review organization reissue"}
                            }
                            if let Observation::Observed(snapshot)=org { ul { for (team_id,team) in &snapshot.teams {
                                li { "Team {team_id}: {team_status(team)}" }
                            } } }
                        }
                    }
                } }
                ul { for email in &progress.emails { li { "Email row {email.source.row}: " match &email.observation {
                    Observation::Pending=>rsx!{"Awaiting delivery"},Observation::Unavailable=>rsx!{"Observation unavailable"},Observation::Observed(snapshot)=>rsx!{"{email_status(snapshot)}"},
                } } } }
                a { href:progress_refresh.clone(),"Refresh this progress page" }
                if let Some(next)=&progress_next {a {href:next.clone(),rel:"next","Next progress page"}}
                a {href:base.clone(),"First progress page"}
                if progress.cancellation_pending {p {role:"status","Stopping unsent work; some delivery owners have not answered yet."}}
                if let Some(cancel)=&progress.cancellation {
                    p { role:"status","Stopped before sending: {cancel.aborted}. Already sent: {cancel.already_written}. Already complete: {cancel.already_completed}. Waiting on GitHub: {cancel.uncertain}." }
                }
            }
            if submitted && progress.is_none() {p {role:"status","Delivery progress is temporarily unavailable. Refresh this batch to check again."}}
        } }
    }
}

fn final_result_label(result: &ghinvite_core::request_completion::TargetResult) -> String {
    use ghinvite_core::{delivery::FinalResult, request_completion::TargetKind};
    let target = match result.target.kind {
        TargetKind::Repository => "Repository",
        TargetKind::Organization => "Organization",
        TargetKind::Team => "Team",
    };
    let outcome = match result.result {
        FinalResult::Satisfied => "Required access confirmed",
        FinalResult::LapsedInvite => "Invitation lapsed",
        FinalResult::Declined => "Invitation declined",
        FinalResult::Unfulfillable => "Delivery closed without confirming access",
    };
    format!(
        "{target} {}, generation {}: {outcome}",
        result.target.id, result.target.generation
    )
}

fn organization_status(
    snapshot: &ghinvite_core::org_delivery::OrganizationDeliverySnapshot,
) -> String {
    if let Some(settlement) = &snapshot.settlement {
        return format!("{:?}", settlement.state);
    }
    use ghinvite_core::org_delivery::OrganizationCreateOutcome as Outcome;
    match &snapshot.create.outcome {
        Outcome::Planned | Outcome::Submitted => "Preparing delivery".into(),
        Outcome::Held { .. } => "Delivery paused for review".into(),
        Outcome::Blocked { .. } => "Waiting for a delivery prerequisite".into(),
        Outcome::Throttled { until, .. } => format!("Queued; next slot {until}"),
        Outcome::OutcomeUnknown => "GitHub outcome unknown; no automatic repeat write".into(),
        Outcome::Created { .. } => "Invitation sent; awaiting acceptance".into(),
        Outcome::AlreadySatisfied { .. } => "Already has the required membership".into(),
        Outcome::AlreadyPending { .. } => "Tracking an existing invitation".into(),
        Outcome::Aborted { .. } => "Stopped before sending".into(),
        Outcome::Failed { .. } => "GitHub refused this delivery".into(),
    }
}
fn team_status(snapshot: &ghinvite_core::org_delivery::TeamDeliverySnapshot) -> String {
    if let Some(settlement) = &snapshot.settlement {
        return format!("{:?}", settlement.state);
    }
    use ghinvite_core::org_delivery::TeamCreateOutcome as Outcome;
    match &snapshot.create.outcome {
        Outcome::Planned | Outcome::Submitted => "Preparing team delivery".into(),
        Outcome::Held { .. } => "Delivery paused for review".into(),
        Outcome::Blocked { .. } => "Waiting for a delivery prerequisite".into(),
        Outcome::Throttled { until, .. } => format!("Queued; next slot {until}"),
        Outcome::OutcomeUnknown => "GitHub outcome unknown".into(),
        Outcome::Created { .. } => "Team invitation pending".into(),
        Outcome::AlreadySatisfied { .. } => "Already has the required membership".into(),
        Outcome::Upgraded => "Team membership added".into(),
        Outcome::Aborted { .. } => "Stopped before sending".into(),
        Outcome::Failed { .. } => "GitHub refused this delivery".into(),
    }
}

fn repository_status(snapshot: &ghinvite_core::delivery::DeliverySnapshot) -> String {
    if let Some(settlement) = &snapshot.settlement {
        return format!("{:?}", settlement.state);
    }
    use ghinvite_core::delivery::CreateOutcome;
    match &snapshot.create.outcome {
        CreateOutcome::Created { .. } => "Invitation sent; awaiting acceptance".into(),
        CreateOutcome::Throttled { until, .. } => format!("Queued; next slot {until}"),
        CreateOutcome::OutcomeUnknown => "GitHub outcome unknown; no automatic repeat write".into(),
        CreateOutcome::Planned | CreateOutcome::Submitted => "Preparing delivery".into(),
        CreateOutcome::Held { .. } => "Delivery paused for review".into(),
        CreateOutcome::Blocked { .. } => "Waiting for a delivery prerequisite".into(),
        CreateOutcome::AlreadySatisfied { .. } => "Already has the required access".into(),
        CreateOutcome::AlreadyPending { .. } => "Tracking an existing invitation".into(),
        CreateOutcome::Upgraded { .. } => "Access raised to the requested role".into(),
        CreateOutcome::SkippedCustomRole { .. } => "Skipped to preserve a custom role".into(),
        CreateOutcome::Aborted { .. } => "Stopped before sending".into(),
        CreateOutcome::Failed { .. } => "GitHub refused this delivery".into(),
    }
}
fn email_status(snapshot: &ghinvite_core::email_delivery::EmailDeliverySnapshot) -> String {
    if let Some(settlement) = &snapshot.settlement {
        return match settlement {
            ghinvite_core::email_delivery::EmailSettlement::NoLongerPending => {
                "No longer pending (accepted or declined)".into()
            }
            ghinvite_core::email_delivery::EmailSettlement::FailedInvitation { .. } => {
                "GitHub reports a failed invitation".into()
            }
            ghinvite_core::email_delivery::EmailSettlement::Cancelled => {
                "Invitation cancelled".into()
            }
        };
    }
    use ghinvite_core::email_delivery::EmailCreateOutcome;
    match &snapshot.outcome {
        EmailCreateOutcome::Created { .. } => {
            "Organization invitation sent; awaiting settlement".into()
        }
        EmailCreateOutcome::Throttled { until, .. } => format!("Queued; next slot {until}"),
        EmailCreateOutcome::OutcomeUnknown => {
            "GitHub outcome unknown; no automatic repeat write".into()
        }
        EmailCreateOutcome::Planned => "Preparing delivery".into(),
        EmailCreateOutcome::Blocked { .. } => "Waiting for a delivery prerequisite".into(),
        EmailCreateOutcome::Aborted { .. } => "Stopped before sending".into(),
        EmailCreateOutcome::Failed { .. } => "GitHub refused this delivery".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghinvite_core::{
        bulk::{BulkBatchId, Manifest, Resolution, RowEvidence},
        email_delivery::EmailRef,
        invitation_link::AccountAdmin,
        targets::{MemberGrant, OrganizationTarget, RepoRole, RepoTarget, Targets},
    };
    fn review() -> Review {
        let at = chrono::DateTime::from_timestamp(1800000000, 0).unwrap();
        Review::new(
            Manifest {
                batch_id: BulkBatchId::new(),
                admin: AccountAdmin {
                    account_id: 100,
                    user_id: 7,
                },
                installation_id: 1,
                organization_installation_id: Some(2),
                access_lifetime_seconds: None,
                targets: Targets {
                    organization: Some(OrganizationTarget {
                        org_id: 100,
                        org_login: "acme".into(),
                        grant: MemberGrant::Member,
                    }),
                    teams: vec![],
                    repos: vec![RepoTarget {
                        repo_id: 10,
                        repo_full_name: "acme/api".into(),
                        grant: RepoRole::Write,
                    }],
                },
                entries: vec![Entry::Email {
                    email_ref: EmailRef::from_bytes([1; 32]),
                }],
            },
            vec![RowEvidence {
                row: 1,
                resolution: Resolution::Email,
                all_targets_satisfied: false,
                previews: vec![],
                observed_at: at,
                next_budget_slot: None,
            }],
            at,
        )
        .unwrap()
    }
    #[test]
    fn mandatory_review_forms_bind_revision_and_never_render_vault_reference() {
        let review = review();
        assert!(!ready(&review));
        let html = crate::testing::render(move || {
            rsx! {
                BulkReviewPage { signed_in_login:Some("owner".into()),account_login:"acme",review:review.clone(),edit_operation:"edit-operation",submit_operation:"submit-operation" }
            }
        });
        for marker in [
            "Reviewed entries and target previews",
            "name=\"csrf_token\"",
            "name=\"expected_revision\"",
            "name=\"operation_id\"",
            "name=\"personal_link_rows\"",
            "Apply review choices",
            "Submit batch",
        ] {
            assert!(html.contains(marker), "missing {marker}");
        }
        assert!(
            !html.contains(&"01".repeat(32)),
            "opaque vault reference has no display purpose"
        );
        assert!(
            !html.contains("link-form-island"),
            "bulk review uses native forms"
        );
    }
    #[test]
    fn personal_link_choice_requires_a_review_update_and_removed_rows_cannot_submit() {
        let mut review = review();
        assert!(!ready(&review));
        review
            .edit(1, Default::default(), std::collections::BTreeSet::from([1]))
            .unwrap();
        assert!(ready(&review));
        review
            .edit(2, std::collections::BTreeSet::from([1]), Default::default())
            .unwrap();
        assert!(!ready(&review));
    }
    #[test]
    fn progress_reissue_links_use_the_real_batch_request_and_feature_gates() {
        use ghinvite_core::{
            bulk::{BatchRef, RowRequest, Submission},
            bulk_commands::{RepositoryProgress, RequestProgress},
        };
        let original = review();
        let mut manifest = original.manifest;
        manifest.entries = vec![Entry::Handle {
            login: "alice".into(),
        }];
        let at = original.reviewed_at;
        let mut review = Review::new(
            manifest,
            vec![RowEvidence {
                row: 1,
                resolution: Resolution::Found {
                    user_id: 91,
                    login: "alice".into(),
                },
                previews: vec![],
                all_targets_satisfied: false,
                observed_at: at,
                next_budget_slot: None,
            }],
            at,
        )
        .unwrap();
        let request_id = ghinvite_core::RequestId::new();
        let source = BatchRef {
            batch_id: review.manifest.batch_id,
            row: 1,
        };
        review
            .submit(
                1,
                Submission {
                    submitted_by: 7,
                    requests: vec![RowRequest {
                        source,
                        request_id,
                        requester_id: 91,
                        requester_login: "alice".into(),
                    }],
                    emails: vec![],
                    personal_links: vec![],
                    submitted_at: at,
                },
            )
            .unwrap();
        let progress = BatchProgress {
            review: review.clone(),
            next_cursor: None,
            handoff_pending: false,
            requests: vec![RequestProgress {
                source,
                request_id,
                initialized: Observation::Observed(()),
                next_target_offset: None,
                final_results: vec![],
                repositories: vec![RepositoryProgress {
                    repo_id: 10,
                    delivery_generation: 1,
                    observation: Observation::Pending,
                }],
                organization: Some(Observation::Pending),
            }],
            emails: vec![],
            cancellation_pending: false,
            cancellation: None,
        };
        for (enabled, organization) in [(false, false), (true, false), (true, true)] {
            let review = review.clone();
            let progress = progress.clone();
            let html = crate::testing::render(move || {
                rsx! {BulkReviewPage {
                    signed_in_login:Some("owner".into()),account_login:"acme",review:review.clone(),
                    edit_operation:"edit",submit_operation:"submit",progress:Some(progress.clone()),
                    reissue_enabled:enabled,organization_reissue_enabled:organization,
                }}
            });
            assert_eq!(
                html.contains(&format!("/requests/{request_id}/reissue/repo/10")),
                enabled
            );
            assert_eq!(
                html.contains(&format!("/requests/{request_id}/reissue/org/100")),
                enabled && organization
            );
            assert!(!html.contains("/reissue/email/"));
        }
    }
    #[test]
    fn reviewed_access_lifetime_and_consequences_precede_submit_without_mutating_it() {
        for seconds in [3600, 3601] {
            let mut review = review();
            review.manifest.access_lifetime_seconds = Some(seconds);
            let before = review.clone();
            let html = crate::testing::render(move || {
                rsx! {
                    BulkReviewPage {signed_in_login:Some("owner".into()),account_login:"acme",review:review.clone(),edit_operation:"edit",submit_operation:"submit"}
                }
            });
            let submit = html.find("Submit batch").unwrap();
            for text in [
                "Access expiry policy",
                &access_duration(seconds),
                "Upgrades and pre-existing access are excluded",
                "Promoted organization owners",
                "Email organization invitations have no access expiry",
                "private forks",
                "removes their team and repository access",
            ] {
                assert!(
                    html.find(text).is_some_and(|position| position < submit),
                    "missing pre-submit caveat {text}"
                );
            }
            assert!(
                !html.contains("name=\"access_lifetime_hours\""),
                "review cannot edit an immutable guardrail"
            );
            assert_eq!(before.manifest.access_lifetime_seconds, Some(seconds));
        }
        let review = review();
        let html = crate::testing::render(
            move || rsx! {BulkReviewPage {account_login:"acme",review:review.clone(),edit_operation:"edit",submit_operation:"submit"}},
        );
        assert!(html.contains("No automatic access expiry"));
        assert!(!html.contains("Remove eligible new access"));
    }
}
