//! Shared Dioxus components used across layouts.

use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct NavProps {
    /// `Some(login)` if the user is signed in, `None` otherwise.
    pub signed_in_login: Option<String>,
}

const THEME_SYNC_SCRIPT: &str = r#"
(function () {
  var key = 'ghinvite-theme';
  var light = 'ghinvite';
  var dark = 'ghinvite-dark';

  function valid(value) {
    return value === light || value === dark;
  }

  function apply(value) {
    var theme = valid(value) ? value : light;
    document.documentElement.setAttribute('data-theme', theme);
    if (document.body) {
      document.body.setAttribute('data-theme', theme);
    }
    var buttons = document.querySelectorAll('[data-theme-value]');
    for (var i = 0; i < buttons.length; i += 1) {
      var active = buttons[i].getAttribute('data-theme-value') === theme;
      buttons[i].setAttribute('aria-pressed', active ? 'true' : 'false');
    }
  }

  var stored = light;
  try {
    stored = window.localStorage.getItem(key) || light;
  } catch (_) {
    stored = light;
  }
  apply(stored);

  document.addEventListener('DOMContentLoaded', function () {
    apply(stored);
  });

  document.addEventListener('click', function (event) {
    var button = event.target.closest('[data-theme-value]');
    if (!button) {
      return;
    }
    var next = valid(button.getAttribute('data-theme-value')) ? button.getAttribute('data-theme-value') : light;
    apply(next);
    try {
      window.localStorage.setItem(key, next);
    } catch (_) {}
  });
})();
"#;

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
                div { class: "theme-toggle", role: "group", aria_label: "Theme",
                    button {
                        r#type: "button",
                        class: "btn btn-ghost btn-xs",
                        aria_label: "Use light theme",
                        aria_pressed: "true",
                        "data-theme-value": "ghinvite",
                        svg { xmlns: "http://www.w3.org/2000/svg", view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "1.8", class: "size-4", "aria-hidden": "true",
                            path { stroke_linecap: "round", stroke_linejoin: "round", d: "M12 4.5V3m0 18v-1.5M4.5 12H3m18 0h-1.5M6.34 6.34 5.28 5.28m13.44 13.44-1.06-1.06m0-11.32 1.06-1.06M5.28 18.72l1.06-1.06M16.5 12a4.5 4.5 0 1 1-9 0 4.5 4.5 0 0 1 9 0Z" }
                        }
                    }
                    button {
                        r#type: "button",
                        class: "btn btn-ghost btn-xs",
                        aria_label: "Use dark theme",
                        aria_pressed: "false",
                        "data-theme-value": "ghinvite-dark",
                        svg { xmlns: "http://www.w3.org/2000/svg", view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "1.8", class: "size-4", "aria-hidden": "true",
                            path { stroke_linecap: "round", stroke_linejoin: "round", d: "M20.25 14.15A7.5 7.5 0 0 1 9.85 3.75a8.25 8.25 0 1 0 10.4 10.4Z" }
                        }
                    }
                }
                {match props.signed_in_login.as_deref() {
                    Some(login) => rsx! {
                        span { class: "hidden max-w-32 truncate px-1 text-xs text-base-content/60 sm:inline-flex", "@{login}" }
                        a { class: "btn btn-ghost btn-sm h-8 min-h-0 px-2", href: "/logout", "Sign out" }
                    },
                    None => rsx! {
                        a { class: "btn btn-primary btn-sm h-8 min-h-0 px-3", href: "/login", "Sign in" }
                    }
                }}
            }
            script { "{THEME_SYNC_SCRIPT}" }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_renders_compact_header_theme_icons_and_signed_in_controls() {
        let html = crate::views::render::render(|| {
            rsx! { Nav { signed_in_login: Some("admin".to_string()) } }
        });

        assert!(html.contains("app-header"));
        assert!(html.contains("theme-toggle"));
        assert!(html.contains("aria-label=\"Use light theme\""));
        assert!(html.contains("aria-label=\"Use dark theme\""));
        assert!(html.contains("data-theme-value=\"ghinvite\""));
        assert!(html.contains("data-theme-value=\"ghinvite-dark\""));
        assert!(html.contains("aria-pressed=\"true\""));
        assert!(html.contains("Sign out"));
        assert!(html.contains("@admin"));
        assert!(html.contains("window.localStorage.getItem(key)"));
        assert!(html.contains("window.localStorage.setItem(key, next)"));
        assert!(!html.contains("<select"));
        assert!(!html.contains("Install on another account"));
    }
}
