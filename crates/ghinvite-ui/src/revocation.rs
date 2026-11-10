//! Native confirmation over a retained preview; no access changes while rendering.
use dioxus::prelude::*;
#[component]
pub fn RevokeConfirmation(
    account_login: String,
    description: String,
    link_id: String,
    operation_id: String,
    pending_requests: Option<u64>,
    pending_invitations: Option<u64>,
    decline_pending_requests: bool,
    confirmation: String,
    error: Option<String>,
    #[props(default)] access_enabled: bool,
    #[props(default)] access_preview: Option<ghinvite_core::revocation::AccessRemovalPreview>,
    #[props(default)] remove_granted_access: bool,
) -> Element {
    let pending = pending_requests
        .map(|n| n.to_string())
        .unwrap_or_else(|| "count unavailable".into());
    let invitations = pending_invitations
        .map(|n| n.to_string())
        .unwrap_or_else(|| "count unavailable".into());
    rsx! {
        h1 {"Revoke invitation link"}
        p {"{description}"}
        p {"Revoking stops new requests permanently. Existing requests and GitHub invitations continue unless you choose an available cascade option."}
        p {"The counts below are projections and may be delayed. The accepted cascade records a fixed set of requests and deliveries."}
        if let Some(error)=error {p {role:"alert","{error}"}}
        form {method:"post",action:"/console/accounts/{account_login}/links/{link_id}/revoke",
            crate::csrf::CsrfField {}
            input {r#type:"hidden",name:"operation_id",value:"{operation_id}"}
            label {input {r#type:"checkbox",name:"decline_pending_requests",value:"true",checked:decline_pending_requests} "Decline {pending} pending requests"}
            p {"These requests will be marked Cancelled because the link was revoked."}
            label {input {r#type:"checkbox",disabled:true} "Cancel {invitations} pending GitHub invitations"}
            p {"GitHub invitation cancellation is not enabled yet."}
            if access_enabled {
                if let Some(preview) = &access_preview {
                    label {input {r#type:"checkbox",name:"remove_granted_access",value:"true",checked:remove_granted_access} "Remove the accepted access listed below"}
                    p {"This fixed list was captured at {preview.captured_at}. Access accepted after this capture is not added to this operation."}
                    if preview.grants.is_empty() { p {"No eligible accepted access in this capture."} }
                    ul {
                        for grant in &preview.grants {
                            li { key:"{grant.storage_id()}",
                                "GitHub user {grant.requester_id}: {target_label(&grant.target)} (generation {grant.delivery_generation})"
                            }
                        }
                    }
                    p {"Each current role is checked again before removal. Promoted organization owners, team maintainers, and repository roles above the original grant or with custom roles are skipped."}
                    p {"Organization, team, or base access can survive repository removal. Removing a repository collaborator can delete private forks. Removing an organization member removes their team and repository access in that organization."}
                } else {
                    label {input {r#type:"checkbox",disabled:true} "Remove access for people who accepted"}
                    p {role:"status","The accepted-access preview is unavailable. Reload to capture a new preview before choosing removal."}
                }
            } else {
                label {input {r#type:"checkbox",disabled:true} "Remove access for people who accepted"}
                p {"Access removal is not enabled."}
            }
            label {r#for:"revoke-confirmation","Type REVOKE when choosing a cascade option"}
            input {id:"revoke-confirmation",name:"confirmation",value:"{confirmation}",autocomplete:"off"}
            button {r#type:"submit","Confirm revoke"}
            a {href:"/console/accounts/{account_login}/links/{link_id}","Cancel"}
        }
    }
}

fn target_label(target: &ghinvite_core::access_grant::GrantTarget) -> String {
    use ghinvite_core::access_grant::GrantTarget;
    match target {
        GrantTarget::Repository {
            repo_id, full_name, ..
        } => format!("repository {full_name} (ID {repo_id})"),
        GrantTarget::Organization { org_id, login } => {
            format!("organization {login} (ID {org_id})")
        }
        GrantTarget::Team {
            team_id,
            org_login,
            slug,
            ..
        } => format!("team {org_login}/{slug} (ID {team_id})"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_preserves_operation_and_refuses_to_offer_inactive_executors() {
        let html = crate::testing::render(|| {
            rsx! {
                RevokeConfirmation {
                    account_login:"acme", description:"Workshop", link_id:"link", operation_id:"operation",
                    pending_requests:Some(4), pending_invitations:None,
                    decline_pending_requests:true, confirmation:"wrong", error:Some("Type REVOKE".into())
                }
            }
        });
        assert!(html.contains("name=\"operation_id\" value=\"operation\""));
        assert!(html.contains("Decline 4 pending requests"));
        assert!(html.contains("Cancel count unavailable pending GitHub invitations"));
        assert!(html.contains("name=\"confirmation\" value=\"wrong\""));
        assert!(html.contains("role=\"alert\""));
        assert!(!html.contains("name=\"cancel_pending_invitations\""));
        assert!(!html.contains("name=\"remove_granted_access\""));
        assert!(html.contains("name=\"csrf_token\""));
    }
    #[test]
    fn empty_frozen_capture_still_explains_cut_and_removal_caveats() {
        let html = crate::testing::render(|| {
            rsx! {
                RevokeConfirmation {account_login:"acme", description:"Access",link_id:"link",operation_id:"exact-operation",
                    pending_requests:None,pending_invitations:None,decline_pending_requests:false,confirmation:"",error:None,
                    access_enabled:true,remove_granted_access:true,
                    access_preview:Some(ghinvite_core::revocation::AccessRemovalPreview {
                        captured_at:"2026-10-17T12:00:00Z".parse().unwrap(),grants:vec![],
                    })
                }
            }
        });
        assert!(html.contains("name=\"remove_granted_access\""));
        assert!(html.contains("No eligible accepted access"));
        assert!(html.contains("not added to this operation"));
        assert!(html.contains("private forks"));
        assert!(html.contains("team maintainers"));
        assert!(html.contains("exact-operation"));
    }
    #[test]
    fn unavailable_capture_cannot_offer_removal() {
        let html = crate::testing::render(|| {
            rsx! {
                RevokeConfirmation {account_login:"acme",description:"Access",link_id:"link",operation_id:"operation",
                    pending_requests:None,pending_invitations:None,decline_pending_requests:false,confirmation:"",error:None,
                    access_enabled:true,access_preview:None,
                }
            }
        });
        assert!(!html.contains("name=\"remove_granted_access\""));
        assert!(html.contains("preview is unavailable"));
    }
}
