//! Server-rendered, read-only account history. Rows contain selected safe text
//! only: raw event metadata never enters the render tree.
use crate::layouts::ConsoleLayout;
use dioxus::prelude::*;
use ghinvite_core::audit::EventType;

pub fn event_label(event: EventType) -> &'static str {
    match event {
        EventType::InstallationCreated => "Installation created",
        EventType::InstallationReposChanged => "Installation repositories changed",
        EventType::InstallationUninstalled => "Installation uninstalled",
        EventType::InvitationLinkCreated => "Invitation link created",
        EventType::InvitationLinkMetadataUpdated => "Invitation link metadata updated",
        EventType::InvitationLinkRevoked => "Invitation link revoked",
        EventType::InvitationLinkExpired => "Invitation link expired",
        EventType::InvitationLinkExhausted => "Invitation link exhausted",
        EventType::RequestCreated => "Invitation request created",
        EventType::RequestApproved => "Invitation request approved",
        EventType::RequestDeclined => "Invitation request declined",
        EventType::RequestExpired => "Invitation request expired",
        EventType::InvitationSent => "GitHub invitation sent",
        EventType::InvitationAccepted => "GitHub invitation accepted",
        EventType::InvitationDeclined => "GitHub invitation declined",
        EventType::InvitationExpired => "GitHub invitation expired",
        EventType::InvitationCancelled => "GitHub invitation cancelled",
        EventType::InvitationSendFailed => "GitHub invitation send failed",
    }
}

#[derive(Clone, PartialEq)]
pub struct AuditRow {
    pub time: chrono::DateTime<chrono::Utc>,
    pub event: EventType,
    pub actor: String,
    pub resource: String,
    pub resource_href: Option<String>,
    pub details: String,
}

#[derive(Clone, PartialEq, Props)]
pub struct AuditLogPageProps {
    pub signed_in_login: Option<String>,
    pub account_login: String,
    pub event: Option<EventType>,
    pub in_range: bool,
    pub rows: Option<Vec<AuditRow>>,
    pub latest_href: String,
    pub retry_href: String,
    pub older_href: Option<String>,
    pub newer_href: Option<String>,
}

#[component]
pub fn AuditLogPage(props: AuditLogPageProps) -> Element {
    let base = format!("/console/accounts/{}/audit", props.account_login);
    rsx! {
        ConsoleLayout {
            signed_in_login: props.signed_in_login,
            title: "Audit log · {props.account_login}",
            account_login: Some(props.account_login),
            active_nav: Some("audit".to_string()),
            flash: None,
            header { class: "mb-6",
                p { class: "text-sm font-medium text-primary", "Audit" }
                h1 { class: "text-2xl font-semibold tracking-tight", "Audit log" }
                p { class: "mt-1 text-sm text-base-content/65", "Recorded account activity for invitation links, invitation requests, and GitHub invitations." }
                p { class: "mt-1 text-sm text-base-content/65", "Logins are last known, not historical snapshots. This is a live operational history; some activity may not be recorded, and delayed events can appear later." }
            }
            form { method: "get", action: base.clone(), class: "mb-6 flex flex-wrap items-end gap-3",
                div {
                    label { r#for: "audit-event", class: "block text-sm font-medium", "Event type" }
                    select { id: "audit-event", name: "event", class: "select select-bordered select-sm",
                        option { value: "", selected: props.event.is_none(), "All events" }
                        for event in EventType::ALL {
                            option { value: event.as_str(), selected: props.event == Some(event), "{event_label(event)}" }
                        }
                    }
                }
                button { r#type: "submit", class: "btn btn-primary btn-sm", "Apply" }
            }
            if let Some(rows) = &props.rows {
                if rows.is_empty() {
                    section { class: "mac-panel p-6",
                        if props.in_range {
                            p { "No recorded events in this range." }
                        } else if props.event.is_some() {
                            p { "No recorded events match this event type." }
                            a { class: "link", href: base.clone(), "All events" }
                        } else {
                            p { "No recorded events for this account." }
                        }
                    }
                } else {
                    div { class: "mac-panel overflow-x-auto", tabindex: "0", role: "region", "aria-label": "Recorded audit events",
                        table { class: "table table-sm",
                            caption { class: "sr-only", "Recorded audit events, newest first. Times are UTC." }
                            thead { tr {
                                for heading in ["Time", "Event", "Actor", "Resource", "Details"] {
                                    th { scope: "col", "{heading}" }
                                }
                            } }
                            tbody {
                                for row in rows {
                                    tr {
                                        td { class: "whitespace-nowrap",
                                            time { datetime: row.time.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true), {row.time.format("%Y-%m-%d %H:%M:%S UTC").to_string()} }
                                        }
                                        td { "{event_label(row.event)}" }
                                        td { "{row.actor}" }
                                        td {
                                            if let Some(href) = &row.resource_href {
                                                a { class: "link", href: href.clone(), "{row.resource}" }
                                            } else { "{row.resource}" }
                                        }
                                        td { "{row.details}" }
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
                section { class: "mac-panel p-6", role: "alert",
                    h2 { class: "text-lg font-semibold", "Could not load the audit log." }
                    p { "Please try again." }
                    a { class: "btn btn-primary btn-sm mt-5", href: props.retry_href, "Retry" }
                }
            }
            nav { class: "mt-5 flex flex-wrap gap-3", "aria-label": "Audit history navigation",
                if let Some(href) = props.newer_href { a { class: "btn btn-sm", href, "Newer" } }
                if let Some(href) = props.older_href { a { class: "btn btn-sm", href, "Older" } }
                if props.in_range { a { class: "btn btn-sm", href: props.latest_href, "Back to latest" } }
            }
        }
    }
}
