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
            class: "navbar min-h-14 border-b border-base-300 bg-base-100 px-4 text-base-content",
            div { class: "flex-1",
                a {
                    class: "btn btn-ghost px-2 text-base font-semibold tracking-tight",
                    href: "/",
                    "ghinvite"
                }
            }
            div { class: "flex-none gap-2",
                {match props.signed_in_login.as_deref() {
                    Some(login) => rsx! {
                        span { class: "hidden px-2 text-xs text-base-content/65 sm:inline-flex", "@{login}" }
                        a { class: "btn btn-primary btn-sm", href: "/install", "Install on another account" }
                        a { class: "btn btn-ghost btn-sm", href: "/logout", "Sign out" }
                    },
                    None => rsx! {
                        a { class: "btn btn-primary btn-sm", href: "/login", "Sign in with GitHub" }
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
