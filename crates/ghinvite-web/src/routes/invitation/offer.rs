//! Capability-only public offer; no invented requester identity or SQL eligibility.
use super::*;
pub(super) async fn page(
    state: &AppState,
    session: &session::Session,
    capability: &session::PendingLink,
    return_to: &str,
) -> Response {
    let offer = match state.link_authority.offer(capability).await {
        Ok(offer) if offer.link_id == capability.link_id => offer,
        Ok(_) | Err(crate::link_authority::AuthorityError::Missing) => {
            return invitation_not_found_response(session);
        }
        Err(error) => return crate::WebError::from(error).into_response(),
    };
    let disabled = !offer.email_domains.is_empty() && state.config.signin_apps.is_none();
    let encoded: String = url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
    let login = format!("/login?return_to={encoded}");
    let html = render(session.csrf_token.clone(), move || {
        rsx! {
            ghinvite_ui::layouts::InvitationLayout {signed_in_login:None,title:"Invitation offer".to_owned(),account_login:None,active_nav:None,flash:None,
                h1{"Review this invitation"}
                if let Some(org)=&offer.targets.organization {p{"Organization: {org.org_login} — Member"}}
                for team in &offer.targets.teams {p{"Team: {team.team_slug} — Member"}}
                for repo in &offer.targets.repos {p{{format!("Repository: {} — {:?}",repo.repo_full_name,repo.grant)}}}
                if offer.approval_required {p{"Account admins review your request before access is approved."}}
                else {p{"Requests are automatically approved when eligible."}}
                if !offer.email_domains.is_empty(){
                    h2{"Verified email domain required"}
                    ul{for rule in &offer.email_domains {li{"{rule}"}}}
                    p{"Use a verified GitHub email in an allowed domain. GitHub noreply emails do not qualify. Subdomain rules exclude the parent domain. Only matching domains are retained."}
                }
                if offer.paused {p{role:"status","This link is paused and is not accepting new requests."}}
                else if disabled {p{role:"status","Email verification is unavailable. Contact the link administrator."}}
                else {a{href:login.clone(),class:"btn btn-primary","Continue with GitHub"}}
            }
        }
    });
    (
        if disabled {
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        } else {
            axum::http::StatusCode::OK
        },
        Html(html),
    )
        .into_response()
}
