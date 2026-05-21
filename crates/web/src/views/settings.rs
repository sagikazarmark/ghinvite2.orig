//! Settings page (read-only in v1).

use crate::session::Flash;
use crate::views::layouts::DashboardLayout;
use dioxus::prelude::*;
use domain::{Account, SelectedRepos};

#[derive(Clone, PartialEq, Props)]
pub struct SettingsProps {
    pub signed_in_login: Option<String>,
    pub flash: Option<Flash>,
    pub account: Account,
}

#[component]
pub fn SettingsPage(props: SettingsProps) -> Element {
    let login = props.account.account_login.clone();
    let account_type = props.account.account_type.to_string();
    let repos_label = match &props.account.selected_repos {
        SelectedRepos::All => "All repositories".to_string(),
        SelectedRepos::Subset(ids) => format!("{} selected repository/repositories", ids.len()),
    };

    rsx! {
        DashboardLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Settings · {login}".to_string(),
            account_login: Some(login.clone()),
            active_nav: Some("settings".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6",
                    p { class: "text-sm font-medium text-primary", "Settings" }
                    h1 { class: "text-2xl font-semibold tracking-tight", "Account status" }
                    p { class: "mt-1 text-sm text-base-content/65", "Current GitHub App installation state for {login}." }
                }
                div { class: "grid max-w-4xl gap-4 md:grid-cols-2",
                    section { class: "card border border-base-300 bg-base-100 shadow-sm",
                        div { class: "card-body gap-3",
                            p { class: "text-xs font-medium uppercase tracking-wide text-base-content/50", "Account" }
                            h2 { class: "text-lg font-semibold", "{login}" }
                            p { class: "text-sm text-base-content/70", "{account_type}" }
                        }
                    }
                    section { class: "card border border-base-300 bg-base-100 shadow-sm",
                        div { class: "card-body gap-3",
                            p { class: "text-xs font-medium uppercase tracking-wide text-base-content/50", "Installation scope" }
                            h2 { class: "text-lg font-semibold", "{repos_label}" }
                            p { class: "text-sm text-base-content/70", "Change repository selection in the GitHub App settings in this version." }
                        }
                    }
                }
                div { class: "alert mt-4 max-w-4xl shadow-sm",
                    span { "Editable settings are planned for v1.1. This page reflects the active installation state." }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use domain::{Account, AccountType, SelectedRepos};

    #[test]
    fn settings_page_renders_account_status_panels() {
        let html = crate::views::render::render(|| {
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
        assert!(html.contains("Installation scope"));
        assert!(html.contains("GitHub App settings"));
    }
}
