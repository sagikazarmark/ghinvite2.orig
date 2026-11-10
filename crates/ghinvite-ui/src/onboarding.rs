//! Installation onboarding contains presentation only; authority and app reads
//! remain at the web boundary.
use dioxus::prelude::*;

#[derive(Clone, PartialEq)]
pub struct PendingRequest {
    pub app: String,
    pub account: String,
    pub requested_at: String,
}
#[derive(Clone, PartialEq)]
pub struct AppStatus {
    pub name: String,
    pub status: String,
    pub install_path: String,
    pub available: bool,
}

#[component]
pub fn Chooser(signed_in_login: String, repo_available: bool, org_available: bool) -> Element {
    rsx! {
        crate::layouts::HomeLayout {
            signed_in_login: Some(signed_in_login), title: "Connect GitHub apps · ghinvite".to_string(),
            section { class: "mx-auto max-w-3xl space-y-6 py-8",
                h1 { class: "text-2xl font-semibold", "Choose what to invite people to" }
                p { "Connect either app, or both for invitations covering repositories, an organization and its teams." }
                div { class: "grid gap-4 md:grid-cols-2",
                    section { class: "mac-panel space-y-3 p-5",
                        h2 { class: "text-lg font-semibold", "Invite people to repositories" }
                        p { "The repo app reads current access and sends collaborator invitations to the repositories you select. Repository administration permission allows it to manage that access." }
                        p { class: "text-sm", "Repository permissions: Administration — read and write; Metadata — read. No organization permissions." }
                        if repo_available { a { class: "btn btn-primary", href: "/install", "Install repo app" } }
                        else { p { "The repo app is not configured in this deployment." } }
                    }
                    section { class: "mac-panel space-y-3 p-5",
                        h2 { class: "text-lg font-semibold", "Invite people to an organization and its teams" }
                        p { "The org app reads membership and sends organization and team invitations. Members permission allows it to manage organization and team membership." }
                        p { class: "text-sm", "Organization permissions: Members — read and write. Repository permissions: Metadata — read." }
                        if org_available { a { class: "btn btn-primary", href: "/install/org", "Install org app" } }
                        else { p { "The org app is not configured in this deployment." } }
                    }
                }
                p { "For an organization, an owner must approve installation. If GitHub offers Request, ask the owners to approve it; a request does not grant access to this app." }
                div { class: "flex flex-wrap gap-3",
                    a { class: "btn btn-outline", href: "/onboarding/requests", "Check pending installation requests" }
                    a { class: "btn btn-ghost", href: "/console", "Return to Console" }
                }
            }
        }
    }
}
#[component]
pub fn Requests(
    signed_in_login: String,
    account_login: Option<String>,
    apps: Vec<AppStatus>,
    requests: Vec<PendingRequest>,
    unavailable: bool,
    refresh_path: String,
) -> Element {
    let content = rsx! {
            section { class: "mx-auto max-w-3xl space-y-5 py-8",
                h1 { class: "text-2xl font-semibold", "GitHub app installations" }
                if let Some(account) = account_login.as_ref() {
                    p { "Apps for {account}" }
                }
                for app in apps {
                    section { class: "mac-panel space-y-2 p-4",
                        h2 { class: "font-semibold", "{app.name}" }
                        p { "{app.status}" }
                        if app.available { a { class: "link", href: app.install_path, "Install or manage this app" } }
                    }
                }
                h2 { class: "text-lg font-semibold", "Pending installation requests" }
                if unavailable {
                    p { role: "alert", "Some installation requests could not be loaded. Their status is unknown; retry to check again." }
                }
                if requests.is_empty() && !unavailable {
                    p { "No pending installation request is currently confirmed. If you just selected Request on GitHub, check again shortly." }
                }
                for request in requests {
                    article { class: "mac-panel space-y-1 p-4",
                        h3 { class: "font-semibold", "{request.account} — {request.app}" }
                        p { "Request sent to the organization owners; awaiting approval." }
                        p { class: "text-sm", "Requested {request.requested_at}" }
                    }
                }
                p { "Only requests confirmed by GitHub appear here. Installation requests do not grant invitation or account access." }
                div { class: "flex gap-3",
                    a { class: "btn btn-outline", href: refresh_path, "Check again" }
                    a { class: "btn btn-ghost", href: "/onboarding", "Choose apps" }
                    a { class: "btn btn-ghost", href: "/console", "Return to Console" }
                }
            }
    };
    match account_login {
        Some(login) => rsx! {
            crate::layouts::ConsoleLayout {
                signed_in_login: Some(signed_in_login), title: "GitHub app installations · ghinvite".to_string(),
                account_login: Some(login), active_nav: Some("installations".to_string()),
                {content}
            }
        },
        None => rsx! {
            crate::layouts::HomeLayout {
                signed_in_login: Some(signed_in_login), title: "GitHub app installations · ghinvite".to_string(),
                {content}
            }
        },
    }
}
