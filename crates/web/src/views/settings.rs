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
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6", h1 { class: "text-2xl font-bold", "Settings" } }
                p { class: "opacity-60 mb-6", "Settings are read-only in v1. Edits coming in v1.1." }
                div { class: "space-y-4 max-w-lg",
                    div { class: "card bg-base-100 shadow",
                        div { class: "card-body",
                            h2 { class: "card-title text-base", "Account" }
                            p { strong { "{login}" } " ({account_type})" }
                        }
                    }
                    div { class: "card bg-base-100 shadow",
                        div { class: "card-body",
                            h2 { class: "card-title text-base", "Repository access" }
                            p { "{repos_label}" }
                        }
                    }
                }
            },
        }
    }
}
