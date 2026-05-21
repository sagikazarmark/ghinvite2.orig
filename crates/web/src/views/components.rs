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
    var selector = document.getElementById('theme-selector');
    if (selector) {
      selector.value = theme;
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
    var selector = document.getElementById('theme-selector');
    if (!selector) {
      return;
    }
    selector.addEventListener('change', function (event) {
      var next = valid(event.target.value) ? event.target.value : light;
      apply(next);
      try {
        window.localStorage.setItem(key, next);
      } catch (_) {}
    });
  });
})();
"#;

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
            div { class: "flex-none items-center gap-2",
                label { class: "sr-only", r#for: "theme-selector", "Theme" }
                select {
                    id: "theme-selector",
                    class: "select select-bordered select-sm w-24",
                    aria_label: "Theme",
                    option { value: "ghinvite", selected: true, "Light" }
                    option { value: "ghinvite-dark", "Dark" }
                }
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
    fn nav_renders_theme_selector() {
        let html = crate::views::render::render(|| {
            rsx! { Nav { signed_in_login: None } }
        });

        assert!(html.contains("Theme"));
        assert!(html.contains("Light"));
        assert!(html.contains("Dark"));
        assert!(html.contains("ghinvite-theme"));
        assert!(html.contains("ghinvite-dark"));
    }
}
