//! Settings page (read-only in v1).

use crate::components::account_type_label;
use crate::flash::Flash;
use crate::layouts::ConsoleLayout;
use dioxus::prelude::*;
use ghinvite_core::{Account, SelectedRepos};

#[derive(Clone, PartialEq, Props)]
pub struct SettingsProps {
    pub signed_in_login: Option<String>,
    pub flash: Option<Flash>,
    pub account: Account,
    #[props(default)]
    pub availability_error: Option<String>,
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
                section { class: "mac-panel max-w-3xl overflow-hidden",
                    dl { class: "property-list",
                        div { class: "property-row",
                            dt { class: "property-label", "Account" }
                            dd {
                                p { class: "text-sm font-medium", "{login}" }
                                p { class: "mt-0.5 text-xs text-base-content/60", "{account_type}" }
                            }
                        }
                        div { class: "property-row",
                            dt { class: "property-label", if historical { "Last recorded installation scope" } else { "Installation scope" } }
                            dd {
                                p { class: "text-sm font-medium", "{repos_label}" }
                                p { class: "mt-0.5 text-xs text-base-content/60", "Recorded repository selection. Review GitHub App settings for current access." }
                            }
                        }
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ghinvite_core::{Account, AccountType, SelectedRepos};

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
