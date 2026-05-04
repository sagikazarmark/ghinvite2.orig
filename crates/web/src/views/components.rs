//! Shared Dioxus components used across layouts.

use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct NavProps {
    /// `Some(login)` if the user is signed in, `None` otherwise.
    pub signed_in_login: Option<String>,
}

#[component]
pub fn Nav(props: NavProps) -> Element {
    rsx! {
        nav {
            class: "navbar bg-base-100",
            div {
                class: "flex-1",
                a {
                    class: "btn btn-ghost text-xl",
                    href: "/",
                    "ghinvite"
                }
            }
            div {
                class: "flex-none",
                {match props.signed_in_login.as_deref() {
                    Some(login) => rsx! {
                        span { class: "px-2", "Signed in as @{login}" }
                        a { class: "btn btn-ghost", href: "/logout", "Sign out" }
                    },
                    None => rsx! {
                        a { class: "btn btn-primary", href: "/login", "Sign in with GitHub" }
                    }
                }}
            }
        }
    }
}

#[component]
pub fn Footer() -> Element {
    rsx! {
        footer {
            class: "footer footer-center p-4 bg-base-200 text-base-content",
            aside {
                p {
                    "ghinvite — GitHub repo collaborator invitations made easy"
                }
            }
        }
    }
}
