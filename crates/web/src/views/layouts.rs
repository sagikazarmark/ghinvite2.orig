//! Three Dioxus layouts: HomeLayout, DashboardLayout, InvitationLayout.
//! Each wraps page content in zone-specific chrome (per spec §12).

use crate::views::components::{Footer, Nav};
use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct LayoutProps {
    pub signed_in_login: Option<String>,
    pub title: String,
    /// `Some(login)` when rendered under account dashboard routes. `None` for
    /// HomeLayout / InvitationLayout.
    pub account_login: Option<String>,
    /// The current dashboard section for active navigation styling.
    pub active_nav: Option<String>,
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
            class: "min-h-screen bg-base-200 text-base-content antialiased",
            "data-theme": "ghinvite",
            Nav { signed_in_login: props.signed_in_login.clone() }
            main { class: "min-h-[calc(100vh-3.5rem)] px-4 py-8", {props.children} }
            Footer {}
        }
    }
}

#[component]
pub fn DashboardLayout(props: LayoutProps) -> Element {
    let login = props.account_login.clone().unwrap_or_default();
    let active_nav = props.active_nav.clone().unwrap_or_default();
    let overview_side = if active_nav == "overview" {
        "menu-active"
    } else {
        ""
    };
    let new_link_side = if active_nav == "new-link" {
        "menu-active"
    } else {
        ""
    };
    let requests_side = if active_nav == "requests" {
        "menu-active"
    } else {
        ""
    };
    let settings_side = if active_nav == "settings" {
        "menu-active"
    } else {
        ""
    };
    let overview_mobile = if active_nav == "overview" {
        "btn btn-primary btn-sm"
    } else {
        "btn btn-ghost btn-sm"
    };
    let new_link_mobile = if active_nav == "new-link" {
        "btn btn-primary btn-sm"
    } else {
        "btn btn-ghost btn-sm"
    };
    let requests_mobile = if active_nav == "requests" {
        "btn btn-primary btn-sm"
    } else {
        "btn btn-ghost btn-sm"
    };
    let settings_mobile = if active_nav == "settings" {
        "btn btn-primary btn-sm"
    } else {
        "btn btn-ghost btn-sm"
    };

    rsx! {
        head {
            title { "{props.title}" }
            link { rel: "stylesheet", href: "/static/styles.css" }
        }
        body {
            class: "min-h-screen bg-base-200 text-base-content antialiased",
            "data-theme": "ghinvite",
            Nav { signed_in_login: props.signed_in_login.clone() }
            nav { class: "border-b border-base-300 bg-base-100 md:hidden",
                div { class: "flex gap-2 overflow-x-auto px-4 py-2 whitespace-nowrap",
                    a { class: "{overview_mobile}", href: "/accounts/{login}", aria_current: if active_nav == "overview" { "page" } else { "false" }, "Overview" }
                    a { class: "{new_link_mobile}", href: "/accounts/{login}/links/new", aria_current: if active_nav == "new-link" { "page" } else { "false" }, "New link" }
                    a { class: "{requests_mobile}", href: "/accounts/{login}/requests", aria_current: if active_nav == "requests" { "page" } else { "false" }, "Requests" }
                    a { class: "btn btn-ghost btn-sm", href: "/accounts/{login}/audit", "Audit soon" }
                    a { class: "{settings_mobile}", href: "/accounts/{login}/settings", aria_current: if active_nav == "settings" { "page" } else { "false" }, "Settings" }
                }
            }
            div { class: "mx-auto flex w-full max-w-7xl flex-1 gap-6 px-4 py-6 lg:px-6",
                aside { class: "hidden w-56 shrink-0 md:block",
                    div { class: "sticky top-6 space-y-4",
                        div { class: "rounded-box border border-base-300 bg-base-100 p-3 shadow-sm",
                            p { class: "text-xs font-medium uppercase tracking-wide text-base-content/50", "Account" }
                            p { class: "mt-1 truncate text-sm font-semibold", "{login}" }
                        }
                        nav { class: "menu rounded-box border border-base-300 bg-base-100 p-2 shadow-sm",
                            li { a { class: "{overview_side}", href: "/accounts/{login}", aria_current: if active_nav == "overview" { "page" } else { "false" }, "Overview" } }
                            li { a { class: "{new_link_side}", href: "/accounts/{login}/links/new", aria_current: if active_nav == "new-link" { "page" } else { "false" }, "New link" } }
                            li { a { class: "{requests_side}", href: "/accounts/{login}/requests", aria_current: if active_nav == "requests" { "page" } else { "false" }, "Pending requests" } }
                            li { a { href: "/accounts/{login}/audit", "Audit log (coming soon)" } }
                            li { a { class: "{settings_side}", href: "/accounts/{login}/settings", aria_current: if active_nav == "settings" { "page" } else { "false" }, "Settings" } }
                        }
                    }
                }
                main { class: "min-w-0 flex-1",
                    {match &props.flash {
                        Some(f) => {
                            let alert_class = match f.level {
                                crate::session::FlashLevel::Success => "alert alert-success mb-4 shadow-sm",
                                crate::session::FlashLevel::Error => "alert alert-error mb-4 shadow-sm",
                                crate::session::FlashLevel::Info => "alert alert-info mb-4 shadow-sm",
                            };
                            rsx! { div { class: "{alert_class}", span { "{f.message}" } } }
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
            class: "min-h-screen bg-base-200 text-base-content antialiased",
            "data-theme": "ghinvite",
            Nav { signed_in_login: props.signed_in_login.clone() }
            main { class: "min-h-[calc(100vh-3.5rem)] px-4 py-8",
                div { class: "mx-auto flex min-h-[calc(100vh-7.5rem)] w-full max-w-lg items-center",
                    div { class: "card w-full border border-base-300 bg-base-100 shadow-sm",
                        div { class: "card-body", {props.children} }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_layout_marks_active_navigation() {
        let html = crate::views::render::render(|| {
            rsx! {
                DashboardLayout {
                    signed_in_login: Some("admin".to_string()),
                    title: "Requests".to_string(),
                    account_login: Some("acme".to_string()),
                    active_nav: Some("requests".to_string()),
                    flash: None,
                    children: rsx! { p { "Queue" } },
                }
            }
        });

        assert!(html.contains("data-theme=\"ghinvite\""));
        assert!(!html.contains("data-theme=\"ghinvite-dark\""));
        assert!(html.contains("aria-current=\"page\""));
        assert!(html.contains("Pending requests"));
        assert!(html.contains("menu-active"));
    }

    #[test]
    fn invitation_layout_uses_product_theme() {
        let html = crate::views::render::render(|| {
            rsx! {
                InvitationLayout {
                    signed_in_login: None,
                    title: "Invite".to_string(),
                    account_login: None,
                    active_nav: None,
                    flash: None,
                    children: rsx! { p { "Invitation" } },
                }
            }
        });

        assert!(html.contains("data-theme=\"ghinvite\""));
        assert!(html.contains("id=\"theme-selector\""));
        assert!(html.contains("ghinvite-theme"));
        assert!(html.contains("Invitation"));
        assert!(html.contains("card"));
    }
}
