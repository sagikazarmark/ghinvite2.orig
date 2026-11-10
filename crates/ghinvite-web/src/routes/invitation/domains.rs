//! The web can refuse an unsafe fresh submission; only the owner admits it.
use crate::{AppState, session::Session};
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use dioxus::prelude::*;
use ghinvite_core::{
    admission::RequesterPage,
    email_domain::{DomainEvidenceError, DomainRule},
};
pub(super) fn gate(state: &AppState, session: &Session, page: &RequesterPage) -> Option<Response> {
    if page.email_domains.is_empty() {
        return None;
    }
    if state.config.signin_apps.is_none() {
        return Some(prompt(session, &page.email_domains, false, true));
    }
    match session
        .domain_evidence
        .as_ref()
        .map(|e| e.check(&page.email_domains, page.link_id, chrono::Utc::now()))
    {
        Some(Ok(())) => None,
        Some(Err(DomainEvidenceError::Mismatch)) => {
            Some(prompt(session, &page.email_domains, true, false))
        }
        _ => Some(prompt(session, &page.email_domains, false, false)),
    }
}
pub(super) fn prompt(
    session: &Session,
    rules: &[DomainRule],
    mismatch: bool,
    disabled: bool,
) -> Response {
    let login = session.is_authenticated().then(|| session.login.clone());
    let rules = rules.iter().map(ToString::to_string).collect::<Vec<_>>();
    let heading = if disabled {
        "Email verification unavailable"
    } else if mismatch {
        "Domain mismatch"
    } else {
        "Confirm your email domain"
    };
    let html = crate::render::render_with_csrf(session.csrf_token.clone(), move || {
        rsx! {
            ghinvite_ui::layouts::InvitationLayout {signed_in_login:login.clone(),title:heading.to_owned(),account_login:None,active_nav:None,flash:None,
                h1{ "{heading}" }
                if disabled {p{"Email verification is unavailable. Contact the link administrator."}}
                else {p{"This link requires a verified GitHub email address in an allowed domain. GitHub noreply addresses do not qualify. Only matching domains are retained."}}
                if !rules.is_empty(){ul{for rule in &rules{li{"{rule}"}}}}
                if !disabled {a{href:"/login?return_to=%2Fjoin%3Ffresh%3Dtrue",class:"btn btn-primary","Confirm email domain with GitHub"}}
                p{"After confirming your email domain, return here to request access."}
            }
        }
    });
    (
        if disabled {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::OK
        },
        Html(html),
    )
        .into_response()
}
