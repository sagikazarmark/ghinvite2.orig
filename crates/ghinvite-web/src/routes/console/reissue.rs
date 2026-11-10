//! Exact operation-bound administrator reissue. Unknown replies retain the same
//! form identity; an old projected terminal result never proves a new issuance.
use super::*;
use axum::extract::Path;
use ghinvite_core::{
    RequestId,
    admission::AdmissionOperationId,
    reissue::{ReissueCommand, ReissueRefusal, ReissueTarget},
    request_lifecycle::{AdminRequestStatus, ApprovedDispatch},
};

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Form {
    #[serde(rename = "csrf_token")]
    _csrf_token: String,
    operation_id: AdmissionOperationId,
    #[serde(default)]
    confirmation: String,
}
type Route = (String, String, String, String);
fn target(kind: &str, id: &str) -> crate::Result<ReissueTarget> {
    let id = id.parse::<u64>().map_err(|_| crate::WebError::NotFound)?;
    let target = match kind {
        "repo" => ReissueTarget::Repository { repo_id: id },
        "org" => ReissueTarget::Organization { org_id: id },
        _ => return Err(crate::WebError::NotFound),
    };
    target.validate().map_err(|_| crate::WebError::NotFound)?;
    Ok(target)
}
async fn context(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
    id: RequestId,
    target: &ReissueTarget,
) -> crate::Result<ApprovedDispatch> {
    if !state.config.reissue_enabled {
        return Err(crate::WebError::NotFound);
    }
    if matches!(target, ReissueTarget::Organization { .. })
        && !state.config.organization_delivery_enabled
    {
        return Err(crate::WebError::NotFound);
    }
    let plan = state
        .request_authority
        .admin_dispatch(AdminRequestStatus {
            request_id: id,
            admin: admin_assertion(admin),
        })
        .await?
        .ok_or(crate::WebError::NotFound)?;
    if plan.input.request.request_id != id
        || plan.input.request.account_id != admin.account.account_id
    {
        return Err(crate::WebError::NotFound);
    }
    let selected = match target {
        ReissueTarget::Repository { repo_id } => {
            plan.commands.iter().any(|c| c.repo_id == *repo_id)
        }
        ReissueTarget::Organization { org_id } => plan
            .organization_command
            .as_ref()
            .is_some_and(|c| c.organization.org_id == *org_id),
    };
    if !selected {
        return Err(crate::WebError::NotFound);
    }
    authorize_targets(admin, &plan.input.targets, true).await?;
    Ok(plan)
}
pub(super) async fn confirmation(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    Path((_, id, kind, target_id)): Path<Route>,
) -> axum::response::Response {
    let result = async {
        let id: RequestId = id.parse().map_err(|_| crate::WebError::NotFound)?;
        let target = target(&kind, &target_id)?;
        let plan = context(&state, &admin, id, &target).await?;
        Ok::<_, crate::WebError>(page(
            &admin,
            &plan,
            &target,
            Form {
                _csrf_token: String::new(),
                operation_id: RequestId::new().to_string().try_into().unwrap(),
                confirmation: String::new(),
            },
            StatusCode::OK,
            None,
            true,
        ))
    }
    .await;
    result.unwrap_or_else(IntoResponse::into_response)
}
pub(super) async fn submit(
    State(state): State<AppState>,
    admin: RequireConsoleAdminOf,
    Path((_, id, kind, target_id)): Path<Route>,
    CsrfForm(form): CsrfForm<Form>,
) -> axum::response::Response {
    let result=async {
        let id:RequestId=id.parse().map_err(|_|crate::WebError::NotFound)?;let target=target(&kind,&target_id)?;
        let plan=context(&state,&admin,id,&target).await?;
        if form.confirmation!="REISSUE" {return Ok(page(&admin,&plan,&target,form,StatusCode::BAD_REQUEST,Some("Type REISSUE to confirm.".into()),true));}
        match state.request_authority.reissue(ReissueCommand{operation_id:form.operation_id.clone(),request_id:id,admin:admin_assertion(&admin),target}).await {
            Ok(Ok(receipt))=>Ok(page(&admin,&plan,&target,form,StatusCode::OK,Some(format!("Delivery generation {} was issued. GitHub delivery and access are not yet confirmed.",receipt.delivery_generation)),false)),
            Ok(Err(ReissueRefusal::NotReissuable{..}))=>Ok(page(&admin,&plan,&target,form,StatusCode::CONFLICT,Some("This operation cannot reissue the target. It may still have pending or uncertain work, or its current delivery is not eligible. This refusal is retained; review the request before starting a new operation.".into()),false)),
            Ok(Err(ReissueRefusal::BudgetClosed{until}))=>Ok(page(&admin,&plan,&target,form,StatusCode::TOO_MANY_REQUESTS,Some(format!("No delivery was issued. The invitation budget is closed until {until}. This operation retains that refusal; start a new operation after the budget opens.")),false)),
            Err(AuthorityError::Unknown(_))=>Ok(page(&admin,&plan,&target,form,StatusCode::SERVICE_UNAVAILABLE,Some("The outcome is unknown. Retry this same operation to recover its retained result.".into()),true)),
            Err(e)=>Err(crate::WebError::from(e)),
        }
    }.await;
    result.unwrap_or_else(IntoResponse::into_response)
}
fn page(
    admin: &RequireConsoleAdminOf,
    plan: &ApprovedDispatch,
    target: &ReissueTarget,
    form: Form,
    status: StatusCode,
    message: Option<String>,
    show_form: bool,
) -> axum::response::Response {
    let id = plan.input.request.request_id;
    let batch = plan.input.request.batch;
    let login = admin.account.account_login.clone();
    let signed = Some(admin.session.login.clone());
    let (kind, target_id) = match target {
        ReissueTarget::Repository { repo_id } => ("repo", *repo_id),
        ReissueTarget::Organization { org_id } => ("org", *org_id),
    };
    let action = format!("/console/accounts/{login}/requests/{id}/reissue/{kind}/{target_id}");
    let (back, back_label) = match batch {
        Some(source) => (
            format!("/console/accounts/{login}/bulk/{}", source.batch_id),
            "Back to batch",
        ),
        None => (
            format!("/console/accounts/{login}/requests/{id}"),
            "Back to request",
        ),
    };
    let html = render(admin.render_context(), move || {
        rsx! {
            ghinvite_ui::layouts::ConsoleLayout { signed_in_login:signed.clone(),title:"Reissue delivery",account_login:Some(login.clone()),active_nav:Some("requests".into()),children:rsx!{
                h1 {"Reissue delivery"}
                p {"Reissue creates a new delivery generation for this target using the original approved scope. Organization reissue includes its attached teams. It does not consume another invitation-link use and is charged to the administrator budget."}
                p {"Only an expired, declined or cancelled delivery can be reissued. Pending invitations and access-removal work must first be definitively resolved."}
                if let Some(message)=&message {p {role:"status","{message}"}}
                if show_form {form {method:"post",action:action.clone(),
                    ghinvite_ui::csrf::CsrfField {}
                    input {r#type:"hidden",name:"operation_id",value:String::from(form.operation_id.clone())}
                    label {r#for:"reissue-confirmation","Type REISSUE to confirm"}
                    input {id:"reissue-confirmation",name:"confirmation",value:form.confirmation.clone(),required:true,autocomplete:"off"}
                    button {r#type:"submit","Reissue delivery"}
                }}
                a {href:back.clone(),"{back_label}"}
            }}
        }
    });
    (status, Html(html)).into_response()
}
