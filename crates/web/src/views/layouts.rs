//! Three Dioxus layouts: HomeLayout, ConsoleLayout, InvitationLayout.
//! Each wraps page content in zone-specific chrome (per spec §12).

use crate::views::components::{Footer, Nav, ThemeSyncScript};
use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct LayoutProps {
    pub signed_in_login: Option<String>,
    pub title: String,
    /// `Some(login)` when rendered under account-scoped Console routes. `None` for
    /// HomeLayout / InvitationLayout.
    pub account_login: Option<String>,
    /// The current console section for active navigation styling.
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
            main { class: "min-h-[calc(100vh-3rem)] px-4 py-6", {props.children} }
            Footer {}
        }
    }
}

#[component]
pub fn ConsoleLayout(props: LayoutProps) -> Element {
    let login = props.account_login.clone().unwrap_or_default();
    let active_nav = props.active_nav.clone().unwrap_or_default();
    let overview_side = if active_nav == "overview" {
        "app-nav-row app-nav-row-active flex w-full items-center"
    } else {
        "app-nav-row flex w-full items-center"
    };
    let new_link_side = if active_nav == "new-link" {
        "app-nav-row app-nav-row-active flex w-full items-center"
    } else {
        "app-nav-row flex w-full items-center"
    };
    let requests_side = if active_nav == "requests" {
        "app-nav-row app-nav-row-active flex w-full items-center"
    } else {
        "app-nav-row flex w-full items-center"
    };
    let settings_side = if active_nav == "settings" {
        "app-nav-row app-nav-row-active flex w-full items-center"
    } else {
        "app-nav-row flex w-full items-center"
    };
    let audit_side = if active_nav == "audit" {
        "app-nav-row app-nav-row-active flex w-full items-center"
    } else {
        "app-nav-row flex w-full items-center"
    };

    rsx! {
        head {
            title { "{props.title}" }
            link { rel: "stylesheet", href: "/static/styles.css" }
        }
        body {
            class: "console-page min-h-screen bg-base-200 text-base-content antialiased",
            "data-theme": "ghinvite",
            Nav { signed_in_login: props.signed_in_login.clone() }
            div { class: "console-frame",
                aside { class: "console-sidebar hidden shrink-0 flex-col md:flex",
                    nav { class: "flex-1 space-y-1 p-3 text-sm",
                        a { class: "{overview_side}", href: "/console/accounts/{login}", aria_current: if active_nav == "overview" { "page" } else { "false" }, "Overview" }
                        a { class: "{new_link_side}", href: "/console/accounts/{login}/links/new", aria_current: if active_nav == "new-link" { "page" } else { "false" }, "New invitation link" }
                        a { class: "{requests_side}", href: "/console/accounts/{login}/requests", aria_current: if active_nav == "requests" { "page" } else { "false" }, "Pending requests" }
                        a { class: "{audit_side}", href: "/console/accounts/{login}/audit", aria_current: if active_nav == "audit" { "page" } else { "false" }, "Audit log" }
                        a { class: "{settings_side}", href: "/console/accounts/{login}/settings", aria_current: if active_nav == "settings" { "page" } else { "false" }, "Settings" }
                    }
                    div { class: "sidebar-account-switcher border-t border-base-300 p-3",
                        p { class: "text-[0.68rem] font-semibold uppercase tracking-wide text-base-content/45", "Active account" }
                        a { class: "mt-2 flex min-w-0 items-center gap-2 rounded-box px-2 py-2 text-sm hover:bg-base-100", href: "/console/accounts/{login}/settings",
                            span { class: "grid size-7 shrink-0 place-items-center rounded-lg bg-base-300 text-xs font-semibold", "@" }
                            span { class: "min-w-0 flex-1 truncate font-medium", "{login}" }
                        }
                        a { class: "btn btn-ghost btn-xs mt-2 h-7 min-h-0 w-full justify-start px-2", href: "/install", "Install another account" }
                    }
                }
                div { class: "console-content min-w-0 flex flex-1 flex-col",
                    nav { class: "mobile-console-nav border-b border-base-300 bg-base-100 px-3 py-2 md:hidden",
                        div { class: "flex gap-1 overflow-x-auto whitespace-nowrap text-sm",
                            a { class: "{overview_side}", href: "/console/accounts/{login}", aria_current: if active_nav == "overview" { "page" } else { "false" }, "Overview" }
                            a { class: "{new_link_side}", href: "/console/accounts/{login}/links/new", aria_current: if active_nav == "new-link" { "page" } else { "false" }, "New invitation link" }
                            a { class: "{requests_side}", href: "/console/accounts/{login}/requests", aria_current: if active_nav == "requests" { "page" } else { "false" }, "Requests" }
                            a { class: "{audit_side}", href: "/console/accounts/{login}/audit", aria_current: if active_nav == "audit" { "page" } else { "false" }, "Audit" }
                            a { class: "{settings_side}", href: "/console/accounts/{login}/settings", aria_current: if active_nav == "settings" { "page" } else { "false" }, "Settings" }
                        }
                    }
                    main { class: "console-main",
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
            a {
                class: "fixed left-4 top-3 z-10 inline-flex items-center gap-2 rounded-lg px-2 py-1.5 text-sm font-semibold tracking-tight text-base-content hover:bg-base-100",
                href: "/",
                span { class: "grid size-5 place-items-center rounded-md bg-primary text-[0.7rem] font-bold text-primary-content", "g" }
                span { "ghinvite" }
            }
            ThemeSyncScript {}
            main { class: "min-h-[calc(100vh-3rem)] px-4 py-6",
                div { class: "mx-auto flex min-h-[calc(100vh-6rem)] w-full max-w-lg items-center",
                    div { class: "mac-panel w-full",
                        div { class: "p-6", {props.children} }
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
    fn console_layout_renders_real_sidebar_and_account_control() {
        let html = crate::views::render::render(|| {
            rsx! {
                ConsoleLayout {
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
        assert!(html.contains("console-page min-h-screen bg-base-200 text-base-content antialiased"));
        assert!(html.contains("app-header"));
        assert!(html.contains("console-frame"));
        assert!(html.contains("console-sidebar"));
        assert!(html.contains("console-content min-w-0 flex flex-1 flex-col"));
        assert!(html.contains("mobile-console-nav"));
        assert!(html.contains("sidebar-account-switcher"));
        assert!(html.contains("Install another account"));
        assert!(html.contains("aria-current=\"page\""));
        assert!(html.contains("app-nav-row-active"));
        assert!(html.contains("app-nav-row app-nav-row-active flex w-full items-center"));
        assert!(html.contains("app-nav-row flex w-full items-center"));
        assert!(html.contains("href=\"/console/accounts/acme/audit\""));
        assert!(html.contains("Audit log"));
        assert!(html.contains("Pending requests"));
        assert!(!html.contains("rounded-box border border-base-300 bg-base-100 p-3 shadow-sm"));
    }

    #[test]
    fn console_layout_marks_audit_navigation_active() {
        let html = crate::views::render::render(|| {
            rsx! {
                ConsoleLayout {
                    signed_in_login: Some("admin".to_string()),
                    title: "Audit log".to_string(),
                    account_login: Some("acme".to_string()),
                    active_nav: Some("audit".to_string()),
                    flash: None,
                    children: rsx! { p { "Audit" } },
                }
            }
        });

        assert!(html.contains("href=\"/console/accounts/acme/audit\""));
        assert!(html.contains("Audit log"));
        assert!(html.contains("aria-current=\"page\""));
        assert!(html.contains("app-nav-row app-nav-row-active flex w-full items-center"));
        assert!(!html.contains("Audit log (coming soon)"));
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
        assert!(html.contains("ghinvite"));
        assert!(html.contains("href=\"/\""));
        assert!(html.contains("Invitation"));
        assert!(html.contains("mac-panel"));
        assert!(html.contains("window.localStorage.getItem(key)"));
        assert!(!html.contains("app-header"));
        assert!(!html.contains("theme-toggle"));
        assert!(!html.contains("Sign in"));
        assert!(!html.contains("Sign out"));
        assert!(!html.contains("Console"));
    }
}
