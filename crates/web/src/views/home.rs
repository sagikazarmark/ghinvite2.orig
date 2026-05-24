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
            title: "ghinvite - controlled GitHub access requests".to_string(),
            account_login: None,
            active_nav: None,
            flash: None,
            children: rsx! {
                div { class: "mx-auto flex w-full max-w-6xl flex-col gap-10 py-8 sm:py-12",
                    section { class: "home-hero px-5 py-12 sm:px-8 sm:py-16",
                        div { class: "mx-auto flex max-w-3xl flex-col items-center text-center",
                            p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-primary", "GitHub access operations" }
                            h1 { class: "mt-4 text-3xl font-semibold tracking-tight text-base-content sm:text-5xl", "Controlled GitHub invitations without access guesswork" }
                            p { class: "mt-5 max-w-2xl text-base leading-7 text-base-content/68 sm:text-lg",
                                "Create share links, review incoming requests, and send repository invitations only after the access consequences are clear."
                            }
                        }
                        div { class: "home-hero-action mt-9 flex justify-center",
                            {match props.signed_in_login.as_deref() {
                                Some(_) => rsx! {
                                    a { class: "btn btn-primary h-10 min-h-0 px-5 shadow-sm", href: "/install", "Install on another account" }
                                },
                                None => rsx! {
                                    a { class: "btn btn-primary h-10 min-h-0 px-5 shadow-sm", href: "/login", "Sign in with GitHub" }
                                }
                            }}
                        }
                    }

                    section { class: "home-features",
                        div { class: "max-w-2xl",
                            p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-base-content/45", "What the app keeps under control" }
                            h2 { class: "mt-2 text-xl font-semibold tracking-tight text-base-content sm:text-2xl", "A focused workflow for collaborator access" }
                        }
                        div { class: "mt-5 grid gap-3 lg:grid-cols-3",
                            article { class: "home-feature-item",
                                p { class: "home-feature-kicker", "01" }
                                h3 { class: "mt-3 text-base font-semibold text-base-content", "Controlled share links" }
                                p { class: "mt-2 text-sm leading-6 text-base-content/66",
                                    "Define repositories, permission level, expiration, and usage limits before a request reaches the queue."
                                }
                            }
                            article { class: "home-feature-item",
                                p { class: "home-feature-kicker", "02" }
                                h3 { class: "mt-3 text-base font-semibold text-base-content", "Review requests before invitations" }
                                p { class: "mt-2 text-sm leading-6 text-base-content/66",
                                    "Confirm GitHub identity and requested access before ghinvite sends repository collaborator invitations."
                                }
                            }
                            article { class: "home-feature-item",
                                p { class: "home-feature-kicker", "03" }
                                h3 { class: "mt-3 text-base font-semibold text-base-content", "Operational history" }
                                p { class: "mt-2 text-sm leading-6 text-base-content/66",
                                    "Keep request outcomes understandable for follow-up, support, and access reviews."
                                }
                            }
                        }
                    }
                }
            },
        }
    }
}
