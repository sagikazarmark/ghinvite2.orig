//! Three Dioxus layouts: HomeLayout, DashboardLayout, InvitationLayout.
//! Each wraps page content in zone-specific chrome (per spec §12).

use crate::views::components::{Footer, Nav};
use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct LayoutProps {
    pub signed_in_login: Option<String>,
    pub title: String,
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
    rsx! {
        head {
            title { "{props.title}" }
            link { rel: "stylesheet", href: "/static/styles.css" }
        }
        body {
            class: "min-h-screen bg-base-200",
            "data-theme": "light",
            Nav { signed_in_login: props.signed_in_login.clone() }
            main { class: "container mx-auto px-4 py-8", {props.children} }
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
