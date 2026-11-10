//! Installation settings and owner-controlled repository admin delegation.

use crate::components::account_type_label;
use crate::flash::Flash;
use crate::layouts::ConsoleLayout;
use dioxus::prelude::*;
use ghinvite_core::{Account, SelectedRepos};

#[derive(Clone, PartialEq, Props)]
pub struct SettingsProps {
    #[props(default)]
    pub can_manage_delegation: bool,
    #[props(default)]
    pub delegation: bool,
    #[props(default)]
    pub delegation_operation: String,
    pub signed_in_login: Option<String>,
    pub flash: Option<Flash>,
    pub account: Account,
    #[props(default)]
    pub availability_error: Option<String>,
    #[props(default)]
    pub org_cap: Option<ghinvite_core::budget::OrgCapView>,
    #[props(default)]
    pub org_cap_operation: String,
    #[props(default)]
    pub org_cap_error: Option<String>,
    #[props(default)]
    pub budgets: Vec<ghinvite_core::budget::BudgetSnapshot>,
    #[props(default)]
    pub budget_error: bool,
    #[props(default)]
    pub share_limited: Vec<ghinvite_core::storage::invite_budget::ShareLimitedLink>,
}

#[component]
pub fn SettingsPage(props: SettingsProps) -> Element {
    let login = props.account.account_login.clone();
    let account_type = account_type_label(props.account.account_type).to_string();
    let uninstalled = props.account.uninstalled_at.is_some();
    let historical = uninstalled || props.availability_error.is_some();
    let settings_url = match props.account.account_type {
        ghinvite_core::AccountType::Organization => format!(
            "https://github.com/organizations/{login}/settings/installations/{}",
            props.account.installation_id
        ),
        ghinvite_core::AccountType::User => format!(
            "https://github.com/settings/installations/{}",
            props.account.installation_id
        ),
    };
    let repos_label = match &props.account.selected_repos {
        SelectedRepos::All => "All repositories".to_string(),
        SelectedRepos::Subset(ids) if ids.len() == 1 => "1 selected repository".to_string(),
        SelectedRepos::Subset(ids) => format!("{} selected repositories", ids.len()),
    };

    rsx! {
        ConsoleLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Settings · {login}",
            account_login: Some(login.clone()),
            active_nav: Some("settings".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6",
                    p { class: "text-sm font-medium text-primary", "Settings" }
                    h1 { class: "text-2xl font-semibold tracking-tight", "Account status" }
                    p { class: "mt-1 text-sm text-base-content/65", "GitHub App installation status for {login}." }
                    if uninstalled {
                        p { class: "mt-2", "Uninstalled. This account retains historical invitation requests, but the GitHub App must be installed again for repository access." }
                        a { class: "btn btn-primary mt-2", href: "/install", "Install GitHub App again" }
                    } else if let Some(error) = props.availability_error.clone() {
                        p { role: "alert", "Installation availability could not be verified. {error}" }
                        a { class: "btn btn-primary mt-2", href: "/console/accounts/{login}/settings", "Try again" }
                        a { class: "btn btn-ghost mt-2", href: settings_url.clone(), "Review GitHub App settings" }
                    } else {
                        p { class: "mt-2", "Installed. Repository access verified with GitHub." }
                        a { class: "btn btn-primary mt-2", href: settings_url.clone(), "Manage GitHub App settings" }
                    }
                }
                if !props.budgets.is_empty() || !props.share_limited.is_empty() || props.budget_error || props.org_cap.as_ref().is_some_and(|cap| cap.enabled) {
                    InviteBudgetPanel { snapshots: props.budgets.clone(), unavailable: props.budget_error,
                        share_limited: props.share_limited.clone(), account_login: login.clone() }
                }
                if let Some(cap) = &props.org_cap && cap.enabled {
                    OrgCapForm { account_login: login.clone(), operation_id: props.org_cap_operation.clone(),
                        confirmed: cap.policy.as_ref().is_some_and(|p| p.command.confirmed_established), error: props.org_cap_error.clone() }
                } else if let Some(error) = &props.org_cap_error {
                    p { role: "alert", "{error}" }
                }
                if props.can_manage_delegation {
                    section { class: "mac-panel max-w-3xl p-6 mb-6",
                        h2 { class: "text-lg font-semibold", "Repository admin delegation" }
                        p { class: "my-3 text-sm", "Repository admins will be able to create links and approve requests that add outside collaborators to repositories they administer. GitHub's 'repository admins can't invite outside collaborators' setting may not apply to P1, because P1's app sends the invitation." }
                        form { method: "post", action: "/console/accounts/{login}/settings/delegation",
                            crate::csrf::CsrfField {}
                            input { r#type: "hidden", name: "operation_id", value: props.delegation_operation.clone() }
                            label { class: "flex items-center gap-2 my-3",
                                input { r#type: "checkbox", name: "on", value: "true", checked: props.delegation, class: "checkbox" }
                                "Allow repository admins to manage their repositories' invitation links"
                            }
                            button { r#type: "submit", class: "btn btn-primary", "Save delegation" }
                        }
                    }
                }
                section { class: "mac-panel max-w-3xl overflow-hidden",
                    dl { class: "property-list",
                        div { class: "property-row",
                            dt { class: "property-label", "Account" }
                            dd {
                                p { class: "text-sm font-medium", "{login}" }
                                p { class: "mt-0.5 text-xs text-muted", "{account_type}" }
                            }
                        }
                        div { class: "property-row",
                            dt { class: "property-label", if historical { "Last recorded installation scope" } else { "Installation scope" } }
                            dd {
                                p { class: "text-sm font-medium", "{repos_label}" }
                                p { class: "mt-0.5 text-xs text-muted", "Recorded repository selection. Review GitHub App settings for current access." }
                            }
                        }
                    }
                }
            },
        }
    }
}

#[component]
pub fn InviteBudgetPanel(
    snapshots: Vec<ghinvite_core::budget::BudgetSnapshot>,
    unavailable: bool,
    #[props(default)] share_limited: Vec<ghinvite_core::storage::invite_budget::ShareLimitedLink>,
    #[props(default)] account_login: String,
) -> Element {
    use ghinvite_core::{account::App, budget::BucketId};
    rsx! {
        section { class: "mac-panel max-w-3xl p-6 mb-6",
            h2 { class: "text-lg font-semibold", "Invite Budget" }
            p { class: "my-3 text-sm", "P1's recorded usage; invites made directly in GitHub also count. Counts and availability may have changed since the last update." }
            if unavailable { p { role: "alert", "Some invitation budget information is unavailable. Reload settings to try again." } }
            if snapshots.is_empty() && !unavailable { p { "No recorded budget usage is available yet." } }
            if !share_limited.is_empty() {
                h3 { class: "font-semibold mt-4", "Links waiting for their invitation share" }
                p { class: "text-sm", "Up to 100 links per app, from recorded delivery status. Updates may be delayed." }
                ul {
                    for link in share_limited {
                        li {
                            a { class: "link", href: "/console/accounts/{account_login}/links/{link.link_id}", "{link.description}" }
                            ": this link has used its share of today's invitations."
                        }
                    }
                }
            }
            for snapshot in snapshots {
                div { class: "mt-4",
                    h3 { class: "font-semibold", if snapshot.app == App::Org { "Organization app" } else { "Repository app" } }
                    p { class: "text-sm text-muted",
                        "Last recorded: "
                        time { datetime: snapshot.observed_at.to_rfc3339(), {snapshot.observed_at.format("%Y-%m-%d %H:%M:%S UTC").to_string()} }
                    }
                    table { class: "table w-full",
                        caption { class: "sr-only", "Recorded invitation usage in the previous 24 hours" }
                        thead { tr { th { "Invitations" } th { "Used / cap (24 hours)" } th { "Next slot" } th { "Closed until" } } }
                        tbody {
                            for bucket in snapshot.buckets.iter().filter(|bucket| bucket.bucket != BucketId::Content) {
                                tr {
                                    th { scope: "row", {match bucket.bucket { BucketId::Org => "Organization".to_string(), BucketId::Repo { repo_id } => format!("Repository {repo_id}"), BucketId::Content => unreachable!() }} }
                                    td { "{bucket.used_24h} / {bucket.cap}" }
                                    td { {budget_time(bucket.next_slot_at, "No recorded delay")} }
                                    td { {budget_time(bucket.closed_until, "No recorded closure")} }
                                }
                            }
                        }
                    }
                    p { class: "mt-2 text-sm", "Content writes: {snapshot.content_used_minute} in the previous minute; {snapshot.content_used_hour} in the previous hour." }
                    p { class: "text-sm", "Content writes closed until: " {budget_time(snapshot.content_closed_until, "No recorded closure")} }
                    p { class: "text-sm", "Next content write: " {budget_time(snapshot.next_content_write_at, "No recorded delay")} }
                    if let Some(since) = snapshot.limit_counts.coverage_started_at {
                        p { class: "mt-2 text-sm", "Recorded in the previous 24 hours: {snapshot.limit_counts.spammed} GitHub spam rejections; {snapshot.limit_counts.rate_limited} rate-limit responses." }
                        p { class: "text-sm", "Counts recorded since " {budget_time(Some(since), "Unknown")} "." }
                        if snapshot.limit_counts.incomplete {
                            p { class: "text-sm", "These counts cover part of the window. Other activity may be missing." }
                        }
                        if snapshot.limit_counts.spammed > 0 {
                            p { class: "alert alert-warning", role: "status", "GitHub reported spam rejection. Review this app’s activity." }
                        }
                    } else {
                        p { class: "text-sm", "Rate-limit counts are unavailable." }
                    }
                }
            }
        }
    }
}

fn budget_time(at: Option<chrono::DateTime<chrono::Utc>>, absent: &str) -> String {
    at.map(|at| at.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| absent.to_string())
}

#[component]
pub fn OrgCapForm(
    account_login: String,
    operation_id: String,
    confirmed: bool,
    error: Option<String>,
) -> Element {
    rsx! {
        section { class: "mac-panel max-w-3xl p-6 mb-6",
            h2 { class: "text-lg font-semibold", "Organization invitation cap" }
            p { class: "my-3 text-sm", "The cap is 50 invitations per 24 hours, or 500 after an organization owner confirms eligibility. Changing it preserves invitations already counted." }
            if let Some(error) = error { p { role: "alert", "{error}" } }
            form { method: "post", action: "/console/accounts/{account_login}/settings/invitation-cap",
                crate::csrf::CsrfField {}
                input { r#type: "hidden", name: "operation_id", value: operation_id }
                label { class: "flex items-center gap-2 my-3",
                    input { r#type: "checkbox", name: "confirmed_established", value: "true", checked: confirmed, class: "checkbox" }
                    "I confirm this organization is older than one month or has a paid plan."
                }
                button { r#type: "submit", class: "btn btn-primary", "Save invitation cap" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ghinvite_core::{Account, AccountType, SelectedRepos};

    #[test]
    fn budget_panel_distinguishes_recorded_invitation_usage_from_content_windows() {
        use ghinvite_core::{
            account::App,
            budget::{BucketId, BucketSnapshot, BudgetSnapshot},
        };
        let observed_at = "2026-10-16T10:00:00Z".parse().unwrap();
        let snapshot = BudgetSnapshot {
            app: App::Org,
            account_id: 9001,
            revision: 2,
            observed_at,
            confirmed_established: true,
            buckets: vec![
                BucketSnapshot {
                    bucket: BucketId::Org,
                    used_24h: 27,
                    cap: 500,
                    closed_until: Some(observed_at + chrono::Duration::seconds(30)),
                    next_slot_at: Some(observed_at + chrono::Duration::seconds(30)),
                },
                BucketSnapshot {
                    bucket: BucketId::Content,
                    used_24h: 19,
                    cap: 80,
                    closed_until: None,
                    next_slot_at: None,
                },
            ],
            limit_counts: ghinvite_core::budget::LimitCounts {
                coverage_started_at: Some(observed_at),
                spammed: 2,
                rate_limited: 3,
                incomplete: true,
                ..Default::default()
            },
            content_used_minute: 3,
            content_used_hour: 19,
            content_closed_until: Some(observed_at + chrono::Duration::seconds(60)),
            next_content_write_at: Some(observed_at + chrono::Duration::seconds(90)),
        };
        let html = crate::testing::render(move || {
            rsx! {
                InviteBudgetPanel { snapshots: vec![snapshot.clone()], unavailable: true }
            }
        });
        assert!(html.contains("27 / 500"));
        assert!(
            !html.contains("19 / 80"),
            "content counts are not daily invitation usage"
        );
        assert!(html.contains("3 in the previous minute; 19 in the previous hour"));
        assert!(html.contains("2026-10-16 10:00:30 UTC"));
        assert!(html.contains("2026-10-16 10:01:30 UTC"));
        assert!(html.contains("Content writes closed until: 2026-10-16 10:01:00 UTC"));
        assert!(html.contains("2 GitHub spam rejections; 3 rate-limit responses"));
        assert!(html.contains("These counts cover part of the window"));
        assert!(html.contains("GitHub reported spam rejection"));
        assert!(html.contains("Last recorded:"));
        assert!(html.contains("may have changed since the last update"));
        assert!(html.contains("invites made directly in GitHub also count"));
        assert!(html.contains("role=\"alert\""));
    }

    #[test]
    fn settings_page_renders_account_status_panels() {
        let html = crate::testing::render(|| {
            rsx! {
                SettingsPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account: Account {
                        installation_id: 77,
                        account_id: 9001,
                        account_login: "acme".to_string(),
                        account_type: AccountType::Organization,
                        installed_at: Utc::now(),
                        uninstalled_at: None,
                        selected_repos: SelectedRepos::All,
                    },
                }
            }
        });

        assert!(html.contains("Account status"));
        assert!(html.contains("<title>Settings · acme</title>"));
        assert!(!html.contains("{login}"));
        assert!(html.contains("Installation scope"));
        assert!(html.contains("GitHub App settings"));
        assert!(html.contains("property-list"));
        assert!(html.contains("mac-panel"));
    }

    #[test]
    fn settings_page_uses_glossary_account_type_label() {
        let html = crate::testing::render(|| {
            rsx! {
                SettingsPage {
                    signed_in_login: Some("octocat".to_string()),
                    flash: None,
                    account: Account {
                        installation_id: 77,
                        account_id: 9001,
                        account_login: "octocat".to_string(),
                        account_type: AccountType::User,
                        installed_at: Utc::now(),
                        uninstalled_at: None,
                        selected_repos: SelectedRepos::Subset(vec![10]),
                    },
                }
            }
        });

        assert!(html.contains("Personal account"));
        assert!(html.contains("1 selected repository"));
        assert!(!html.contains(">User<"));
        assert!(!html.contains("selected repository/repositories"));
    }
}
