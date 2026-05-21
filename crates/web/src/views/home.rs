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
            children: rsx! {
                div {
                    class: "hero",
                    div {
                        class: "hero-content text-center max-w-2xl",
                        div {
                            h1 { class: "text-5xl font-bold", "Invite collaborators with one link." }
                            p {
                                class: "py-6",
                                "ghinvite turns ad-hoc \"can you add this person to our repo?\" \
                                 messages into shareable links with optional approval, expiration, \
                                 and captured history."
                            }
                            {match props.signed_in_login.as_deref() {
                                Some(_) => rsx! {
                                    a {
                                        class: "btn btn-primary btn-lg",
                                        href: "/install",
                                        "Install on a new account"
                                    }
                                },
                                None => rsx! {
                                    a {
                                        class: "btn btn-primary btn-lg",
                                        href: "/login",
                                        "Sign in with GitHub"
                                    }
                                }
                            }}
                        }
                    }
                }
            },
        }
    }
}
