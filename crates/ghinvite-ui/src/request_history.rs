//! Read-only Console request investigation, separate from the decision queue.
use crate::invitation::{DeliveryPresentation, DeliveryStatus};
use crate::layouts::ConsoleLayout;
use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use ghinvite_core::{InvitationLink, InvitationRequest, RequestState};

pub fn time(value: Option<DateTime<Utc>>) -> String {
    value
        .map(|v| v.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "Unavailable".into())
}

#[derive(Clone, PartialEq)]
pub struct HistoryRow {
    pub request: InvitationRequest,
    pub requester: String,
}

#[component]
fn ProjectionNotice() -> Element {
    rsx! { p { class: "my-4 text-sm text-base-content/70",
        "Projected history — updates may be delayed or missing. This view does not establish that all requests or delivery outcomes have been recorded. Approval does not guarantee GitHub invitation delivery."
    } }
}

#[component]
pub fn RequestHistoryPage(
    signed_in_login: Option<String>,
    account_login: String,
    link_id: String,
    description: String,
    rows: Option<Vec<HistoryRow>>,
    older_href: Option<String>,
    in_range: bool,
) -> Element {
    let base = format!("/console/accounts/{account_login}");
    rsx! {
        ConsoleLayout {
            signed_in_login, title: "Request history · {account_login}",
            account_login: Some(account_login.clone()), active_nav: Some("links".into()), flash: None,
            children: rsx! {
                h1 { class: "text-2xl font-semibold", "Request history" }
                a { class: "link", href: "{base}/links/{link_id}", "{description}" }
                ProjectionNotice {}
                match rows {
                    None => rsx! { p { role: "alert", "Request history unavailable. Try again later." } },
                    Some(rows) if rows.is_empty() => rsx! { p { "No projected requests in this range." } },
                    Some(rows) => rsx! {
                        ol { class: "mac-panel divide-y divide-base-300", aria_label: "Invitation request history",
                            for row in rows {
                                li { class: "p-4 space-y-1",
                                    a { class: "link font-medium", href: "{base}/requests/{row.request.id}", "{row.requester}" }
                                    p { "State: {row.request.state}" }
                                    p { "Admitted: {time(Some(row.request.created_at))}" }
                                    p { "Decision deadline: {time(row.request.decision_deadline)}" }
                                    if row.request.state != RequestState::Pending {
                                        p { "Decision / terminal time: {time(row.request.decided_at)}" }
                                    }
                                }
                            }
                        }
                    },
                }
                nav { class: "my-4 flex gap-4", aria_label: "Request history pagination",
                    if in_range {
                        a { class: "link", href: "{base}/links/{link_id}/requests", "Latest requests" }
                    }
                    if let Some(href) = older_href {
                        a { class: "link", href, "Older requests" }
                    }
                }
            },
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct RepositoryDelivery {
    pub repo_id: u64,
    pub presentation: DeliveryPresentation,
    pub receipts: Vec<ghinvite_core::delivery::DeliverySnapshot>,
    pub invitations: Vec<ghinvite_core::GithubInvitation>,
}

#[component]
pub fn RequestDetailPage(
    signed_in_login: Option<String>,
    account_login: String,
    request: InvitationRequest,
    link: InvitationLink,
    requester: String,
    decision_actor: String,
    delivery: Vec<RepositoryDelivery>,
    delivery_unavailable: bool,
    now: DateTime<Utc>,
) -> Element {
    let history = format!(
        "/console/accounts/{account_login}/links/{}/requests",
        link.id
    );
    rsx! {
        ConsoleLayout {
            signed_in_login, title: "Invitation request · {account_login}",
            account_login: Some(account_login.clone()), active_nav: Some("requests".into()), flash: None,
            children: rsx! {
                h1 { class: "text-2xl font-semibold", "Invitation request" }
                p { class: "break-all text-sm", "Request ID: {request.id}" }
                a { class: "link", href: history, "Request history" }
                ProjectionNotice {}
                section { class: "mac-panel p-4 space-y-2",
                    h2 { class: "font-semibold", "Request and decision" }
                    p { "Requester: {requester}" }
                    p { "State: {request.state}" }
                    p { "Admitted: {time(Some(request.created_at))}" }
                    p { "Decision deadline: {time(request.decision_deadline)}" }
                    if request.state == RequestState::Pending && request.decision_deadline.is_some_and(|d| d <= now) {
                        p { "Decision deadline passed. The projected state may be delayed; this does not establish a decision." }
                    }
                    if request.state != RequestState::Pending {
                        p { "Decision / terminal time: {time(request.decided_at)}" }
                        p { "Decision actor: {decision_actor}" }
                    }
                    if let Some(reason) = request.decline_reason {
                        p { class: "whitespace-pre-wrap break-words", "Decline reason (admin-only): {reason}" }
                    }
                    if let Some(justification) = request.justification {
                        p { class: "whitespace-pre-wrap break-words", "Justification (admin-only): {justification}" }
                    }
                }
                section { class: "mac-panel p-4 mt-4 space-y-2",
                    h2 { class: "font-semibold", "Requested scope and delivery" }
                    p { "Immutable requested permission: {link.permission}" }
                    p { "Repository scope is fixed at admission. Current availability does not change it." }
                    if delivery_unavailable {
                        p { role: "alert", "Delivery information unavailable. Any known outcome below is retained history; current updates could not be loaded." }
                    }
                    if delivery.is_empty() {
                        p { "Repository details unavailable." }
                    }
                    ul { class: "space-y-4", aria_label: "Repository delivery",
                        for row in delivery {
                            li {
                                h3 { class: "font-medium break-words", "{row.presentation.repo}" }
                                p { "Repository ID: {row.repo_id}" }
                                if row.presentation.status != DeliveryStatus::Unavailable {
                                    p { "{row.presentation.status.label()}" }
                                } else if !row.presentation.observation_unavailable {
                                    if request.state == RequestState::Approved {
                                        p { "No projected delivery outcome yet. Dispatch or projection may be delayed; this does not mean delivery failed." }
                                    } else {
                                        p { "No projected delivery outcome. Only approval authorizes delivery." }
                                    }
                                }
                                if row.presentation.observation_unavailable {
                                    p { "Current delivery status unavailable. Any known outcome shown is retained history; the invitation may have changed since. Missing status does not mean delivery failed." }
                                }
                                for receipt in row.receipts {
                                    p { class: "text-sm break-words", "Delivery ID: {receipt.create.command.invitation_id}" }
                                    p { class: "text-sm", "Create outcome recorded: {time(receipt.create.confirmed_at)}" }
                                    if let ghinvite_core::delivery::CreateOutcome::Created { upstream_id } = receipt.create.outcome {
                                        p { class: "text-sm", "GitHub invitation ID: {upstream_id}" }
                                    }
                                }
                                for invitation in row.invitations {
                                    p { class: "text-sm", "GitHub invitation lifecycle: {invitation.state} · Updated: {time(Some(invitation.updated_at))}" }
                                }
                            }
                        }
                    }
                }
            },
        }
    }
}
