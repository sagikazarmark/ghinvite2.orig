//! Three Dioxus layouts: HomeLayout, DashboardLayout, InvitationLayout.
//! Each wraps page content in zone-specific chrome (per spec §12).

use crate::views::components::{Footer, Nav};
use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct LayoutProps {
    pub signed_in_login: Option<String>,
    pub title: String,
    /// `Some(login)` when rendered under `/accounts/{login}/...`. `None` for
    /// HomeLayout / InvitationLayout.
    pub account_login: Option<String>,
    /// One-shot status message rendered above `children`.
    pub flash: Option<crate::session::Flash>,
    /// The page content rendered inside the layout.
    pub children: Element,
}

#[component]
pub fn HomeLayout(props: LayoutProps) -> Element {
    rsx! {
        head {
            title { "{props.title}" }
            link { rel: "stylesheet", href: "/static/styles.css" }
        }
        body {
            class: "min-h-screen bg-base-100 flex flex-col",
            "data-theme": "light",
            Nav { signed_in_login: props.signed_in_login.clone() }
            main { class: "flex-1 container mx-auto px-4 py-8", {props.children} }
            Footer {}
        }
    }
}

#[component]
pub fn DashboardLayout(props: LayoutProps) -> Element {
    let login = props.account_login.clone().unwrap_or_default();
    rsx! {
        head {
            title { "{props.title}" }
            link { rel: "stylesheet", href: "/static/styles.css" }
        }
        body {
            class: "min-h-screen bg-base-200 flex flex-col",
            "data-theme": "light",
            Nav { signed_in_login: props.signed_in_login.clone() }
            nav { class: "md:hidden bg-base-100 border-y border-base-300 overflow-x-auto",
                div { class: "flex gap-2 px-4 py-2 whitespace-nowrap",
                    a { class: "btn btn-ghost btn-sm", href: "/accounts/{login}", "Overview" }
                    a { class: "btn btn-ghost btn-sm", href: "/accounts/{login}/links/new", "New link" }
                    a { class: "btn btn-ghost btn-sm", href: "/accounts/{login}/requests", "Requests" }
                    a { class: "btn btn-ghost btn-sm", href: "/accounts/{login}/audit", "Audit soon" }
                    a { class: "btn btn-ghost btn-sm", href: "/accounts/{login}/settings", "Settings" }
                }
            }
            div {
                class: "flex flex-1 container mx-auto px-4 py-6 gap-6",
                aside {
                    class: "w-56 hidden md:block",
                    nav {
                        class: "menu bg-base-100 rounded-box p-2",
                        li { a { href: "/accounts/{login}", "Overview" } }
                        li { a { href: "/accounts/{login}/links/new", "New link" } }
                        li { a { href: "/accounts/{login}/requests", "Pending requests" } }
                        li { a { href: "/accounts/{login}/audit", "Audit log (coming soon)" } }
                        li { a { href: "/accounts/{login}/settings", "Settings" } }
                    }
                }
                main {
                    class: "flex-1",
                    {match &props.flash {
                        Some(f) => {
                            let alert_class = match f.level {
                                crate::session::FlashLevel::Success => "alert alert-success mb-4",
                                crate::session::FlashLevel::Error => "alert alert-error mb-4",
                                crate::session::FlashLevel::Info => "alert alert-info mb-4",
                            };
                            rsx! {
                                div { class: "{alert_class}", span { "{f.message}" } }
                            }
                        }
                        None => rsx! {},
                    }}
                    {props.children}
                }
            }
        }
    }
}

#[component]
pub fn InvitationLayout(props: LayoutProps) -> Element {
    rsx! {
        head {
            title { "{props.title}" }
            link { rel: "stylesheet", href: "/static/styles.css" }
        }
        body {
            class: "min-h-screen bg-base-100 flex items-center justify-center",
            "data-theme": "light",
            div {
                class: "card w-full max-w-md bg-base-100 shadow-xl",
                div { class: "card-body", {props.children} }
            }
        }
    }
}
