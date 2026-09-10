//! Shared Dioxus components used across layouts.

use dioxus::prelude::*;

/// Human-readable label for an account type, shared by the console pages and
/// the console route's account picker.
pub fn account_type_label(account_type: ghinvite_core::AccountType) -> &'static str {
    match account_type {
        ghinvite_core::AccountType::User => "Personal account",
        ghinvite_core::AccountType::Organization => "Organization",
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct NavProps {
    /// `Some(login)` if the user is signed in, `None` otherwise.
    pub signed_in_login: Option<String>,
}

/// The one shared client script (`assets/app.js`, served at `/static/app.js`).
///
/// Rendered in every layout's `<head>` as a classic synchronous script — no
/// `defer`/`async` — so the theme sync runs before first paint. The enforced
/// Content-Security-Policy (`script-src 'self'`) blocks inline `<script>`
/// blocks; put new client behaviour in `assets/app.js` or a Dioxus island.
#[component]
pub fn AppScript() -> Element {
    rsx! { script { src: "/static/app.js" } }
}

#[component]
pub fn Nav(props: NavProps) -> Element {
    rsx! {
        nav {
            class: "app-header navbar min-h-0 gap-2 px-3 py-0 text-base-content sm:px-4",
            div { class: "min-w-0 flex-1",
                a {
                    class: "btn btn-ghost btn-sm h-8 min-h-0 px-2 text-sm font-semibold tracking-tight",
                    href: "/",
                    span { class: "grid size-5 place-items-center rounded-md bg-primary text-[0.7rem] font-bold text-primary-content", "g" }
                    span { class: "ml-1", "ghinvite" }
                }
            }
            div { class: "flex flex-none items-center justify-end gap-2",
                button {
                    r#type: "button",
                    class: "theme-toggle btn btn-ghost btn-sm h-8 min-h-0",
                    aria_label: "Switch to dark theme",
                    aria_pressed: "false",
                    title: "Switch to dark theme",
                    "data-theme-toggle": "true",
                    "data-theme-value": "ghinvite-dark",
                    "data-current-theme": "ghinvite",
                    span { class: "theme-icon", "data-theme-icon": "true", "data-current-icon": "moon", "aria-hidden": "true",
                        svg { xmlns: "http://www.w3.org/2000/svg", view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "1.8", class: "size-4",
                            path { "data-theme-icon-path": "true", stroke_linecap: "round", stroke_linejoin: "round", d: "M20.25 14.15A7.5 7.5 0 0 1 9.85 3.75a8.25 8.25 0 1 0 10.4 10.4Z" }
                        }
                    }
                }
                {match props.signed_in_login.as_deref() {
                    Some(login) => rsx! {
                        a { class: "btn btn-primary btn-sm h-8 min-h-0 px-3", href: "/console", "Console" }
                        span { class: "hidden max-w-32 truncate px-1 text-xs text-base-content/60 sm:inline-flex", "@{login}" }
                        a { class: "btn btn-ghost btn-sm h-8 min-h-0 px-2", href: "/logout", "Sign out" }
                    },
                    None => rsx! {
                        a { class: "btn btn-primary btn-sm h-8 min-h-0 px-3", href: "/login", "Sign in" }
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
                    "ghinvite: controlled GitHub repository access requests"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_registry_components_preserve_ssr_attributes() {
        let html = crate::testing::render(|| {
            let value = use_signal(|| "server value".to_string());
            rsx! {
                button::Button {
                    color: button::ButtonColor::Primary,
                    r#type: "submit",
                    name: "action",
                    value: "save",
                    class: "custom-button",
                    "Save"
                }
                alert::Alert {
                    color: alert::AlertColor::Error,
                    role: "alert",
                    "Failure"
                }
                input::Input {
                    value: Some(value.into()),
                    id: "registry-input",
                    name: "title",
                    r#type: "text",
                    required: true,
                    readonly: true,
                    aria_describedby: "title-help",
                    class: "custom-input",
                    "data-probe": "registry",
                }
            }
        });

        for expected in [
            "btn-primary",
            "custom-button",
            "type=\"submit\"",
            "name=\"action\"",
            "value=\"save\"",
            "alert-error",
            "role=\"alert\"",
            "value=\"server value\"",
            "id=\"registry-input\"",
            "name=\"title\"",
            "type=\"text\"",
            "required",
            "readonly",
            "aria-describedby=\"title-help\"",
            "custom-input",
            "data-probe=\"registry\"",
        ] {
            assert!(html.contains(expected), "missing {expected}: {html}");
        }
        assert_eq!(html.matches("id=\"registry-input\"").count(), 1);
    }

    #[test]
    fn nav_renders_single_theme_toggle_and_signed_in_controls() {
        let html = crate::testing::render(|| {
            rsx! { Nav { signed_in_login: Some("admin".to_string()) } }
        });

        assert!(html.contains("app-header"));
        assert!(html.contains("theme-toggle"));
        assert!(html.contains("data-theme-toggle=\"true\""));
        assert_eq!(html.matches("data-theme-value=").count(), 1);
        assert!(html.contains("data-theme-value=\"ghinvite-dark\""));
        assert!(html.contains("aria-label=\"Switch to dark theme\""));
        assert!(html.contains("data-current-theme=\"ghinvite\""));
        assert_eq!(html.matches("data-theme-icon=").count(), 1);
        assert!(html.contains("data-theme-icon=\"true\""));
        assert!(html.contains("data-theme-icon-path=\"true\""));
        assert!(html.contains("data-current-icon=\"moon\""));
        assert!(!html.contains("aria-label=\"Use light theme\""));
        assert!(!html.contains("aria-label=\"Use dark theme\""));
        assert!(!html.contains("role=\"group\""));
        assert!(html.contains("href=\"/console\""));
        assert!(html.contains("Console"));
        assert!(html.contains("Sign out"));
        assert!(html.contains("@admin"));
        assert!(
            !html.contains("<script"),
            "Nav must not render a script; the shared bundle is loaded from the layout head"
        );
        assert!(!html.contains("<select"));
        assert!(!html.contains("Install on another account"));
    }

    #[test]
    fn footer_uses_glossary_repository_access_language() {
        let html = crate::testing::render(|| rsx! { Footer {} });

        assert!(html.contains("controlled GitHub repository access requests"));
        assert!(!html.contains("repo collaborator invitations"));
        assert!(!html.contains("—"));
    }
}
pub mod alert;
pub mod button;
pub mod field;
pub mod input;
