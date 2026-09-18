//! Console overview page (Dioxus).

use crate::flash::Flash;
use crate::layouts::ConsoleLayout;
use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use ghinvite_core::{AccountType, InvitationLink};

#[derive(Clone, PartialEq)]
pub struct ConsoleAccountChoice {
    pub login: String,
    pub account_type: String,
}

#[derive(Clone, PartialEq)]
pub enum ConsoleIndexState {
    AccountPicker { accounts: Vec<ConsoleAccountChoice> },
    Empty,
    LoadError,
}

#[derive(Clone, PartialEq, Props)]
pub struct ConsoleIndexPageProps {
    pub signed_in_login: Option<String>,
    pub state: ConsoleIndexState,
}

#[component]
pub fn ConsoleIndexPage(props: ConsoleIndexPageProps) -> Element {
    rsx! {
        crate::layouts::HomeLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Console · ghinvite".to_string(),
            account_login: None,
            active_nav: None,
            flash: None,
            children: rsx! {
                section { class: "mx-auto w-full max-w-3xl py-8 sm:py-12",
                    {match props.state.clone() {
                        ConsoleIndexState::AccountPicker { accounts } => rsx! {
                            h1 { class: "text-2xl font-semibold tracking-tight", "Choose an account" }
                            p { class: "mt-2 text-sm text-base-content/65", "Select the account you want to manage in the Console." }
                            div { class: "mac-panel mt-5 divide-y divide-base-300 overflow-hidden",
                                {accounts.into_iter().map(|account| {
                                    let href = format!("/console/accounts/{}", account.login);
                                    rsx! {
                                        a { class: "flex items-center justify-between gap-3 px-4 py-3 hover:bg-base-200/60", href: "{href}",
                                            span { class: "min-w-0",
                                                span { class: "block truncate text-sm font-medium", "{account.login}" }
                                                span { class: "mt-0.5 block text-xs text-base-content/60", "{account.account_type}" }
                                            }
                                            span { class: "text-sm text-primary", "Open" }
                                        }
                                    }
                                })}
                            }
                            a { class: "btn btn-ghost btn-sm mt-4", href: "/install", "Install another account" }
                        },
                        ConsoleIndexState::Empty => rsx! {
                            div { class: "mac-panel p-6",
                                p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-base-content/45", "Console" }
                                h1 { class: "mt-3 text-2xl font-semibold tracking-tight", "No accounts connected" }
                                p { class: "mt-2 text-sm leading-6 text-base-content/70", "Install the GitHub App on a personal account or organization before creating invitation links." }
                                div { class: "mt-5 flex flex-col gap-2 sm:flex-row",
                                    a { class: "btn btn-primary", href: "/install", "Install GitHub App" }
                                    a { class: "btn btn-ghost", href: "/", "Go home" }
                                }
                            }
                        },
                        ConsoleIndexState::LoadError => rsx! {
                            div { class: "mac-panel p-6",
                                p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-error", "Account loading failed" }
                                h1 { class: "mt-3 text-2xl font-semibold tracking-tight", dangerous_inner_html: "We couldn't load your accounts" }
                                p { class: "mt-2 text-sm leading-6 text-base-content/70", "GitHub account discovery did not complete. Try again before changing installation settings." }
                                div { class: "mt-5 flex flex-col gap-2 sm:flex-row",
                                    a { class: "btn btn-primary", href: "/console", "Try again" }
                                    a { class: "btn btn-ghost", href: "/install", "Install GitHub App" }
                                    a { class: "btn btn-ghost", href: "/", "Go home" }
                                }
                            }
                        },
                    }}
                }
            },
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct OverviewProps {
    pub signed_in_login: Option<String>,
    pub flash: Option<Flash>,
    pub account_login: String,
    pub account_type: AccountType,
    pub pending_requests: Option<u64>,
    pub active_links: Option<u64>,
    pub recent_links: Vec<InvitationLink>,
    pub now: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghinvite_core::{AccountType, InvitationLinkId, InvitationLinkRepo, Permission, Slug};

    fn sample_link() -> InvitationLink {
        InvitationLink {
            id: InvitationLinkId::new(),
            slug: Slug::from_string("abcdEFGH01234567".to_string()).unwrap(),
            installation_id: 77,
            account_id: 9001,
            created_by: 42,
            created_at: DateTime::parse_from_rfc3339("2026-05-04T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            expires_at: None,
            max_uses: Some(5),
            uses_count: 2,
            permission: Permission::Pull,
            approval_required: false,
            description: "AI coding workshop".into(),
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos: vec![InvitationLinkRepo {
                repo_id: 10,
                repo_full_name: "acme/api".to_string(),
            }],
        }
    }

    #[test]
    fn overview_page_renders_console_copy() {
        let html = crate::testing::render(|| {
            rsx! {
                OverviewPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    account_type: AccountType::Organization,
                    pending_requests: 2,
                    active_links: 3,
                    recent_links: vec![],
                    now: Utc::now(),
                }
            }
        });

        assert!(html.contains("Console overview"));
        assert!(html.contains("<title>acme · ghinvite</title>"));
        assert!(!html.contains("{props.account_login}"));
        assert!(html.contains("Review queue"));
        assert!(html.contains("Create first invitation link"));
        assert!(html.contains("Recent invitation links"));
        assert!(html.contains("Active invitation links"));
        assert!(html.contains("active invitation links can accept invitation requests"));
        assert!(!html.contains("Active access links"));
        assert!(!html.contains("collaborator requests"));
        assert!(html.contains("mac-panel"));
        assert!(html.contains("compact-table"));
        assert!(!html.contains("text-3xl"));
    }

    #[test]
    fn overview_links_to_active_and_all_collections_and_preserves_creation() {
        for recent_links in [vec![], vec![sample_link()]] {
            let html = crate::testing::render(move || {
                rsx! {
                    OverviewPage {
                        signed_in_login: Some("admin".to_string()),
                        flash: None,
                        account_login: "acme".to_string(),
                        account_type: AccountType::Organization,
                        pending_requests: 0,
                        active_links: 1,
                        recent_links: recent_links.clone(),
                        now: Utc::now(),
                    }
                }
            });
            let summary = html
                .split("<a ")
                .skip(1)
                .find(|anchor| {
                    anchor
                        .split_once("</a>")
                        .unwrap()
                        .0
                        .contains("Active invitation links")
                })
                .unwrap();
            assert!(summary.contains("href=\"/console/accounts/acme/links?filter=active\""));
            let recent = html.split_once("Recent invitation links</h2>").unwrap().1;
            assert!(
                recent.contains("href=\"/console/accounts/acme/links?filter=all\">View all</a>")
            );
            assert!(
                html.contains("href=\"/console/accounts/acme/links/new\">New invitation link</a>")
            );
        }
    }

    #[test]
    fn overview_page_uses_glossary_account_type_label() {
        let html = crate::testing::render(|| {
            rsx! {
                OverviewPage {
                    signed_in_login: Some("octocat".to_string()),
                    flash: None,
                    account_login: "octocat".to_string(),
                    account_type: AccountType::User,
                    pending_requests: 0,
                    active_links: 0,
                    recent_links: vec![],
                    now: Utc::now(),
                }
            }
        });

        assert!(html.contains("Personal account: octocat"));
        assert!(!html.contains("User account: octocat"));
    }

    #[test]
    fn overview_recent_links_show_description_before_code() {
        let html = crate::testing::render(|| {
            rsx! {
                OverviewPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    account_type: AccountType::Organization,
                    pending_requests: 0,
                    active_links: 1,
                    recent_links: vec![sample_link()],
                    now: Utc::now(),
                }
            }
        });

        assert!(html.contains("<th>Description</th>"));
        assert!(!html.contains("<th>Code</th>"));
        assert!(!html.contains("<th>Slug</th>"));
        assert!(html.contains("AI coding workshop"));
        assert!(html.contains("abcdEFGH01234567"));
        assert!(html.find("AI coding workshop").unwrap() < html.find("abcdEFGH01234567").unwrap());
    }

    #[test]
    fn console_index_renders_account_picker() {
        let html = crate::testing::render(|| {
            rsx! {
                ConsoleIndexPage {
                    signed_in_login: Some("admin".to_string()),
                    state: ConsoleIndexState::AccountPicker { accounts: vec![
                        ConsoleAccountChoice { login: "acme".to_string(), account_type: "Organization".to_string() },
                        ConsoleAccountChoice { login: "octocat".to_string(), account_type: "Personal account".to_string() },
                    ] },
                }
            }
        });

        assert!(html.contains("Choose an account"));
        assert!(html.contains("Select the account you want to manage in the Console."));
        assert!(!html.contains("account console"));
        assert!(html.contains("href=\"/console/accounts/acme\""));
        assert!(html.contains("Organization"));
        assert!(html.contains("Personal account"));
        assert!(html.contains("Install another account"));
    }

    #[test]
    fn console_index_renders_empty_state() {
        let html = crate::testing::render(|| {
            rsx! {
                ConsoleIndexPage {
                    signed_in_login: Some("admin".to_string()),
                    state: ConsoleIndexState::Empty,
                }
            }
        });

        assert!(html.contains("No accounts connected"));
        assert!(html.contains("Install GitHub App"));
        assert!(html.contains("href=\"/install\""));
        assert!(html.contains("Go home"));
        assert!(html.contains("href=\"/\""));
    }

    #[test]
    fn console_index_renders_load_error() {
        let html = crate::testing::render(|| {
            rsx! {
                ConsoleIndexPage {
                    signed_in_login: Some("admin".to_string()),
                    state: ConsoleIndexState::LoadError,
                }
            }
        });

        assert!(html.contains("We couldn't load your accounts"));
        assert!(html.contains("Try again"));
        assert!(html.contains("href=\"/console\""));
        assert!(html.contains("Install GitHub App"));
        assert!(html.contains("Go home"));
    }
}

#[component]
pub fn OverviewPage(props: OverviewProps) -> Element {
    let login = props.account_login.clone();
    let account_type = crate::components::account_type_label(props.account_type).to_string();
    let recent_links_view = props.recent_links.iter().map(|link| {
        let active = link.is_active(props.now);
        let badge = if active {
            "badge badge-success"
        } else {
            "badge badge-ghost"
        };
        let label = if active { "active" } else { "inactive" };
        let id_str = link.id.to_string();
        let slug_str = link.slug.as_str().to_string();
        rsx! {
            tr { class: "hover:bg-base-200/70",
                td {
                    a {
                        class: "link link-hover font-medium",
                        href: "/console/accounts/{login}/links/{id_str}",
                        "{link.description}"
                    }
                    p { class: "mt-0.5 font-mono text-xs text-base-content/55", "{slug_str}" }
                }
                td { span { class: "{badge} badge-sm", "{label}" } }
                td { class: "text-right tabular-nums", "{link.uses_count}" }
            }
        }
    });

    rsx! {
        ConsoleLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "{login} · ghinvite",
            account_login: Some(props.account_login.clone()),
            active_nav: Some("overview".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-4 flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between",
                    div {
                        h1 { class: "text-xl font-semibold tracking-tight", "Console overview" }
                        p { class: "mt-0.5 text-sm text-base-content/60", "{account_type}: {props.account_login}" }
                    }
                    a { class: "btn btn-primary btn-sm", href: "/console/accounts/{login}/links/new", "New invitation link" }
                }
                section { class: "mac-panel mb-4 overflow-hidden",
                    div { class: "grid divide-y divide-base-300 md:grid-cols-2 md:divide-x md:divide-y-0",
                        a { class: "block p-4 transition-colors hover:bg-base-200/60", href: "/console/accounts/{login}/requests",
                            p { class: "text-sm font-medium", "Review queue" }
                            if let Some(count) = props.pending_requests {
                                p { class: "mt-1 text-sm text-base-content/65", "{count} pending invitation requests need an account admin decision." }
                            } else {
                                p { role: "alert", "Pending invitation requests are unavailable." }
                            }
                        }
                        a { class: "block p-4 transition-colors hover:bg-base-200/60", href: "/console/accounts/{login}/links?filter=active",
                            p { class: "text-sm font-medium", "Active invitation links" }
                            if let Some(count) = props.active_links {
                                p { class: "mt-1 text-sm text-base-content/65", "{count} active invitation links can accept invitation requests." }
                            } else {
                                p { role: "alert", "Invitation links are unavailable." }
                            }
                        }
                    }
                }
                section { class: "mac-panel compact-table overflow-hidden",
                    if props.pending_requests.is_none() || props.active_links.is_none() {
                        a { class: "btn btn-primary btn-sm", href: "/console/accounts/{login}", "Try again" }
                    }
                    div { class: "flex items-center justify-between border-b border-base-300 px-4 py-3",
                        h2 { class: "text-sm font-semibold", "Recent invitation links" }
                        a { class: "btn btn-ghost btn-xs h-7 min-h-0", href: "/console/accounts/{login}/links?filter=all", "View all" }
                    }
                    {if props.active_links.is_none() {
                        rsx! { p { class: "p-5", "Recent invitation links could not be loaded. Try again." } }
                    } else if props.recent_links.is_empty() {
                        rsx! {
                            div { class: "p-5 text-sm text-base-content/70",
                                h3 { class: "font-medium text-base-content", "No invitation links yet" }
                                p { class: "mt-1", "Create an invitation link to let GitHub users request repository access without manual GitHub invitations." }
                                a { class: "btn btn-primary btn-sm mt-4", href: "/console/accounts/{login}/links/new", "Create first invitation link" }
                            }
                        }
                    } else {
                        rsx! {
                            div { class: "overflow-x-auto",
                                table { class: "table compact-table table-sm",
                                    thead { tr { th { "Description" } th { "State" } th { class: "text-right", "Uses" } } }
                                    tbody { {recent_links_view} }
                                }
                            }
                        }
                    }}
                }
            },
        }
    }
}
