//! Route-aware 404 page views.

use crate::views::layouts::{ConsoleLayout, HomeLayout, InvitationLayout};
use dioxus::prelude::*;

const PUBLIC_NOT_FOUND_MESSAGE: &str = "The link may be incorrect or no longer available.";
const CONSOLE_NOT_FOUND_MESSAGE: &str = "This console page is not available.";

#[derive(Clone, PartialEq, Props)]
pub struct NotFoundContentProps {
    pub message: String,
    pub primary_href: String,
    pub primary_label: String,
    pub secondary_href: Option<String>,
    pub secondary_label: Option<String>,
}

#[component]
pub fn NotFoundContent(props: NotFoundContentProps) -> Element {
    let secondary = match (props.secondary_href.clone(), props.secondary_label.clone()) {
        (Some(href), Some(label)) => rsx! {
            a { class: "btn btn-ghost h-10 min-h-0 px-5", href: "{href}", "{label}" }
        },
        _ => rsx! {},
    };

    rsx! {
        section { class: "mx-auto flex w-full max-w-lg flex-col items-center py-8 text-center sm:py-12",
            p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-base-content/45", "404" }
            h1 { class: "mt-3 text-2xl font-semibold tracking-tight text-base-content", "Page not found" }
            p { class: "mt-2 text-sm leading-6 text-base-content/68", "{props.message}" }
            div { class: "mt-6 flex flex-col justify-center gap-2 sm:flex-row",
                a { class: "btn btn-primary h-10 min-h-0 px-5 shadow-sm", href: "{props.primary_href}", "{props.primary_label}" }
                {secondary}
            }
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct PublicNotFoundPageProps {
    pub signed_in_login: Option<String>,
}

#[component]
pub fn PublicNotFoundPage(props: PublicNotFoundPageProps) -> Element {
    rsx! {
        HomeLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Page not found - ghinvite".to_string(),
            account_login: None,
            active_nav: None,
            flash: None,
            children: rsx! {
                NotFoundContent {
                    message: PUBLIC_NOT_FOUND_MESSAGE.to_string(),
                    primary_href: "/".to_string(),
                    primary_label: "Go home".to_string(),
                    secondary_href: None::<String>,
                    secondary_label: None::<String>,
                }
            },
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct InvitationNotFoundPageProps {
    pub signed_in_login: Option<String>,
}

#[component]
pub fn InvitationNotFoundPage(props: InvitationNotFoundPageProps) -> Element {
    rsx! {
        InvitationLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Page not found - ghinvite".to_string(),
            account_login: None,
            active_nav: None,
            flash: None,
            children: rsx! {
                NotFoundContent {
                    message: PUBLIC_NOT_FOUND_MESSAGE.to_string(),
                    primary_href: "/".to_string(),
                    primary_label: "Go home".to_string(),
                    secondary_href: None::<String>,
                    secondary_label: None::<String>,
                }
            },
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct ConsoleNotFoundPageProps {
    pub signed_in_login: Option<String>,
    pub account_login: String,
}

#[component]
pub fn ConsoleNotFoundPage(props: ConsoleNotFoundPageProps) -> Element {
    let overview_href = format!("/console/accounts/{}", props.account_login);

    rsx! {
        ConsoleLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Page not found - ghinvite".to_string(),
            account_login: Some(props.account_login.clone()),
            active_nav: None,
            flash: None,
            children: rsx! {
                NotFoundContent {
                    message: CONSOLE_NOT_FOUND_MESSAGE.to_string(),
                    primary_href: overview_href,
                    primary_label: "Go to account overview".to_string(),
                    secondary_href: Some("/".to_string()),
                    secondary_label: Some("Go home".to_string()),
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_not_found_page_uses_generic_copy() {
        let html = crate::views::render::render(|| {
            rsx! { PublicNotFoundPage { signed_in_login: None::<String> } }
        });

        assert!(html.contains("Page not found"));
        assert!(html.contains(PUBLIC_NOT_FOUND_MESSAGE));
        assert!(html.contains("Go home"));
        assert!(html.contains("app-header"));
        assert!(!html.contains(CONSOLE_NOT_FOUND_MESSAGE));
    }

    #[test]
    fn invitation_not_found_page_uses_invitation_layout() {
        let html = crate::views::render::render(|| {
            rsx! { InvitationNotFoundPage { signed_in_login: Some("octocat".to_string()) } }
        });

        assert!(html.contains("Page not found"));
        assert!(html.contains(PUBLIC_NOT_FOUND_MESSAGE));
        assert!(html.contains("mac-panel"));
        assert!(html.contains("<script src=\"/static/app.js\"></script>"));
        assert_eq!(html.matches("<script").count(), 1);
        assert!(!html.contains("app-header"));
        assert!(!html.contains("theme-toggle"));
        assert!(!html.contains("Sign out"));
        assert!(!html.contains("Console"));
    }

    #[test]
    fn console_not_found_page_has_no_active_sidebar_item() {
        let html = crate::views::render::render(|| {
            rsx! {
                ConsoleNotFoundPage {
                    signed_in_login: Some("admin".to_string()),
                    account_login: "acme".to_string(),
                }
            }
        });

        assert!(html.contains("console-frame"));
        assert!(html.contains(CONSOLE_NOT_FOUND_MESSAGE));
        assert!(html.contains("Go to account overview"));
        assert!(html.contains("href=\"/console/accounts/acme\""));
        assert!(!html.contains("app-nav-row-active"));
    }
}
