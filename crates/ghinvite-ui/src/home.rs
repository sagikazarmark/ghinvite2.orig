//! Public home page.
//!
//! The signed-in variant renders the invitation-code shortcut as plain markup
//! with `data-*` hooks; the behaviour lives in `assets/app.js` (served at
//! `/static/app.js`), never inline — the CSP blocks inline scripts.

use crate::layouts::HomeLayout;
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
                div { class: "mx-auto flex min-h-[calc(100vh-8rem)] w-full max-w-2xl items-center py-8 sm:py-12",
                    div { class: "mac-panel w-full",
                        {match props.signed_in_login.as_deref() {
                            Some(_) => rsx! {
                                section { class: "p-6 text-center sm:p-8",
                                    p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-primary", "Invitation code" }
                                    h1 { class: "mt-3 text-2xl font-semibold tracking-tight text-base-content sm:text-3xl", "Open an invitation link" }
                                    p { class: "mt-3 text-sm leading-6 text-base-content/66",
                                        "Enter the code from an invitation link to request repository access."
                                    }
                                    div { class: "mt-6 flex flex-col gap-3 sm:flex-row",
                                        input {
                                            class: "input input-bordered min-h-10 flex-1",
                                            r#type: "text",
                                            placeholder: "Paste invitation code",
                                            "data-invitation-code-input": "true",
                                        }
                                        button {
                                            class: "btn btn-primary min-h-10",
                                            r#type: "button",
                                            "data-open-invitation-code": "true",
                                            "Open invitation link"
                                        }
                                    }
                                    p { class: "mt-2 min-h-5 text-sm text-error", aria_live: "polite", "data-invitation-code-message": "true", "" }
                                    div { class: "divider my-6" }
                                    div { class: "flex flex-col items-center gap-3",
                                        p { class: "text-sm text-base-content/66", "Need to invite someone to a repository?" }
                                        a { class: "btn btn-outline min-h-10", href: "/console", "Create invitation link" }
                                    }
                                }
                            },
                            None => rsx! {
                                section { class: "p-6 text-center sm:p-8",
                                    p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-primary", "GitHub access" }
                                    h1 { class: "mt-3 text-3xl font-semibold tracking-tight text-base-content sm:text-4xl", "Request and manage repository access" }
                                    p { class: "mx-auto mt-4 max-w-xl text-base leading-7 text-base-content/66",
                                        "Use ghinvite to request and manage GitHub repository access through invitation links."
                                    }
                                    div { class: "mt-7",
                                        a { class: "btn btn-primary h-10 min-h-0 px-5 shadow-sm", href: "/login", "Sign in with GitHub" }
                                    }
                                }
                            }
                        }}
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_in_home_uses_invitation_link_language() {
        let html = crate::testing::render(|| {
            rsx! { HomePage { signed_in_login: Some("octocat".to_string()) } }
        });

        assert!(html.contains("Open invitation link"));
        assert!(!html.contains("Open invitation</button>"));
    }

    #[test]
    fn signed_in_home_exposes_invitation_code_hooks_without_inline_script() {
        let html = crate::testing::render(|| {
            rsx! { HomePage { signed_in_login: Some("octocat".to_string()) } }
        });

        assert!(html.contains("data-invitation-code-input=\"true\""));
        assert!(html.contains("data-open-invitation-code=\"true\""));
        assert!(html.contains("data-invitation-code-message=\"true\""));
        assert!(html.contains("<script src=\"/static/app.js\"></script>"));
        assert_eq!(
            html.matches("<script").count(),
            1,
            "home page must not render an inline <script> block"
        );
        assert!(!html.contains("encodeURIComponent"));
    }

    #[test]
    fn signed_out_home_has_no_inline_script() {
        let html = crate::testing::render(|| rsx! { HomePage { signed_in_login: None } });

        assert!(html.contains("<script src=\"/static/app.js\"></script>"));
        assert_eq!(html.matches("<script").count(), 1);
    }
}
