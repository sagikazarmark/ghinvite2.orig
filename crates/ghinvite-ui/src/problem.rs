//! The page a request gets when it could not be completed.
//!
//! A failure page has one job beyond saying what happened: leave the visitor
//! somewhere to go. The copy is written by the caller from a fixed set of
//! messages — nothing an upstream service said ever reaches it — and the page
//! always renders at least one link out.

use crate::layouts::HomeLayout;
use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct ProblemContentProps {
    /// Short all-caps kicker above the heading, e.g. the status class.
    pub eyebrow: String,
    pub heading: String,
    /// What the visitor can do about it. Written by ghinvite, never quoted
    /// from an upstream response.
    pub message: String,
    pub primary_href: String,
    pub primary_label: String,
    pub secondary_href: Option<String>,
    pub secondary_label: Option<String>,
}

#[component]
pub fn ProblemContent(props: ProblemContentProps) -> Element {
    let secondary = match (props.secondary_href.clone(), props.secondary_label.clone()) {
        (Some(href), Some(label)) => rsx! {
            a { class: "btn btn-ghost h-10 min-h-0 px-5", href: "{href}", "{label}" }
        },
        _ => rsx! {},
    };

    rsx! {
        section { class: "mx-auto flex w-full max-w-lg flex-col items-center py-8 text-center sm:py-12",
            p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-base-content/45", "{props.eyebrow}" }
            h1 { class: "mt-3 text-2xl font-semibold tracking-tight text-base-content", "{props.heading}" }
            p { class: "mt-2 text-sm leading-6 text-base-content/68", "{props.message}" }
            div { class: "mt-6 flex flex-col justify-center gap-2 sm:flex-row",
                a { class: "btn btn-primary h-10 min-h-0 px-5 shadow-sm", href: "{props.primary_href}", "{props.primary_label}" }
                {secondary}
            }
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct ProblemPageProps {
    pub signed_in_login: Option<String>,
    pub eyebrow: String,
    pub heading: String,
    pub message: String,
    pub primary_href: String,
    pub primary_label: String,
    pub secondary_href: Option<String>,
    pub secondary_label: Option<String>,
}

#[component]
pub fn ProblemPage(props: ProblemPageProps) -> Element {
    let title = format!("{} - ghinvite", props.heading);
    rsx! {
        HomeLayout {
            signed_in_login: props.signed_in_login.clone(),
            title,
            account_login: None,
            active_nav: None,
            flash: None,
            children: rsx! {
                ProblemContent {
                    eyebrow: props.eyebrow.clone(),
                    heading: props.heading.clone(),
                    message: props.message.clone(),
                    primary_href: props.primary_href.clone(),
                    primary_label: props.primary_label.clone(),
                    secondary_href: props.secondary_href.clone(),
                    secondary_label: props.secondary_label.clone(),
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> String {
        crate::testing::render(|| {
            rsx! {
                ProblemPage {
                    signed_in_login: Some("octocat".to_string()),
                    eyebrow: "Service".to_string(),
                    heading: "Something went wrong".to_string(),
                    message: "Try again in a moment.".to_string(),
                    primary_href: "/".to_string(),
                    primary_label: "Go home".to_string(),
                    secondary_href: None::<String>,
                    secondary_label: None::<String>,
                }
            }
        })
    }

    #[test]
    fn problem_page_renders_its_copy_and_a_way_out() {
        let html = page();
        assert!(html.contains("Something went wrong"));
        assert!(html.contains("Try again in a moment."));
        assert!(html.contains("href=\"/\""));
        assert!(html.contains("Go home"));
    }

    #[test]
    fn problem_page_renders_the_shared_chrome() {
        assert!(page().contains("app-header"));
    }

    #[test]
    fn problem_page_renders_a_secondary_action_when_given_one() {
        let html = crate::testing::render(|| {
            rsx! {
                ProblemPage {
                    signed_in_login: None::<String>,
                    eyebrow: "Sign-in".to_string(),
                    heading: "Sign-in could not be completed".to_string(),
                    message: "Start again from the sign-in page.".to_string(),
                    primary_href: "/login".to_string(),
                    primary_label: "Sign in".to_string(),
                    secondary_href: Some("/".to_string()),
                    secondary_label: Some("Go home".to_string()),
                }
            }
        });
        assert!(html.contains("href=\"/login\""));
        assert!(html.contains("href=\"/\""));
        assert!(html.contains("Go home"));
    }
}
