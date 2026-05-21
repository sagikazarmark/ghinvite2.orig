//! Home page: logged-out marketing or signed-in redirect.

use crate::views::layouts::HomeLayout;
use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct HomePageProps {
    pub signed_in_login: Option<String>,
}

#[component]
pub fn HomePage(props: HomePageProps) -> Element {
    rsx! {
        HomeLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "ghinvite — invite collaborators with one shareable link".to_string(),
            account_login: None,
            active_nav: None,
            flash: None,
            children: rsx! {
                section { class: "mx-auto grid max-w-5xl gap-8 py-10 lg:grid-cols-[minmax(0,1fr)_22rem] lg:items-start",
                    div { class: "max-w-2xl",
                        p { class: "mb-3 text-sm font-medium text-primary", "GitHub access operations" }
                        h1 { class: "text-3xl font-semibold tracking-tight text-base-content sm:text-4xl", "Access console for GitHub collaborators" }
                        p { class: "mt-4 max-w-xl text-base leading-7 text-base-content/70",
                            "Create share links, review access requests, and keep repository invitations moving without ad-hoc admin work."
                        }
                        div { class: "mt-6 flex flex-wrap gap-3",
                            {match props.signed_in_login.as_deref() {
                                Some(_) => rsx! {
                                    a { class: "btn btn-primary", href: "/install", "Install on another account" }
                                },
                                None => rsx! {
                                    a { class: "btn btn-primary", href: "/login", "Sign in with GitHub" }
                                }
                            }}
                        }
                    }
                    aside { class: "card border border-base-300 bg-base-100 shadow-sm",
                        div { class: "card-body gap-4",
                            h2 { class: "card-title text-base", "What admins control" }
                            ul { class: "space-y-3 text-sm text-base-content/75",
                                li { "Repository permissions, expiration, and usage limits" }
                                li { "Approval queues before invitations are sent" }
                                li { "captured history for access workflows" }
                            }
                        }
                    }
                }
            },
        }
    }
}
