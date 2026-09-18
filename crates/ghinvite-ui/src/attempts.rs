//! Plain presentation props for authorized browser mutation recovery.
use crate::layouts::ConsoleLayout;
use dioxus::prelude::*;

#[derive(Clone, PartialEq)]
pub struct AttemptRow {
    pub label: String,
    pub id: String,
    pub status_url: String,
    pub detail_url: String,
}

#[component]
pub fn AttemptPage(
    signed_in_login: String,
    account_login: String,
    row: AttemptRow,
    message: String,
    retry: bool,
) -> Element {
    rsx! {
        ConsoleLayout {
            signed_in_login: Some(signed_in_login), account_login: Some(account_login), title: "Attempt recovery",
            section { class: "space-y-4",
                h1 { class: "text-xl font-semibold", "{row.label}: original attempt" }
                p { role: "status", "{message}" }
                AttemptControls { row, retry }
                p { class: "text-sm", "These attempts remain recoverable in this signed-in session. Retrying preserves the original identity and input." }
            }
        }
    }
}

#[component]
pub fn AttemptListPage(
    signed_in_login: String,
    account_login: String,
    rows: Vec<AttemptRow>,
) -> Element {
    rsx! {
        ConsoleLayout {
            signed_in_login: Some(signed_in_login), account_login: Some(account_login), title: "Attempt recovery",
            h1 { class: "text-xl font-semibold", "Attempt recovery" }
            p { "Retained attempts in this signed-in session. Check original status before starting another attempt." }
            if rows.is_empty() { p { "No retained attempts in this session." } }
            for row in rows {
                section { class: "space-y-4 mt-6", "data-attempt-id": row.id.clone(),
                    h2 { class: "font-semibold", "{row.label}" }
                    AttemptControls { row: row.clone(), retry: true }
                }
            }
        }
    }
}

#[component]
fn AttemptControls(row: AttemptRow, retry: bool) -> Element {
    rsx! {
        p { class: "break-all text-sm", "Attempt: {row.id}" }
        a { class: "btn btn-outline", href: row.status_url.clone(), "Check original attempt status" }
        if retry {
            form { method: "post", action: row.status_url.clone(),
                crate::csrf::CsrfField {}
                button { class: "btn btn-primary", r#type: "submit", "Retry original attempt" }
            }
        }
        a { class: "link block", href: row.detail_url, "View invitation link details" }
    }
}
