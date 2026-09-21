//! Test-only access to the public storage boundary inside workerd.
use ghinvite_core::storage::{Storage, projection::ProjectionStorage, test_suite};
use http_body_util::BodyExt;
use worker::{Body, Env, HttpRequest, wasm_bindgen::JsValue};

pub async fn fetch(req: HttpRequest, env: &Env) -> worker::Result<http::Response<Body>> {
    let path = req.uri().path().to_owned();
    let bytes = req.into_body().collect().await?.to_bytes();
    let input: serde_json::Value = serde_json::from_slice(&bytes).map_err(super::worker_err)?;
    let storage = ghinvite_storage_d1::D1Storage::new(env.d1("DB")?);
    let result = match path.as_str() {
        "/__fixture/installation" => storage
            .get_installation(serde_json::from_value(input).map_err(super::worker_err)?)
            .await
            .map(|value| serde_json::to_value(value).unwrap()),
        "/__fixture/member-binding" => {
            let (digest, invitation): (String, Option<ghinvite_core::GithubInvitationId>) =
                serde_json::from_value(input).map_err(super::worker_err)?;
            storage
                .bind_member_webhook(&digest, invitation)
                .await
                .map(|value| serde_json::to_value(value).unwrap())
        }
        "/__fixture/member-candidates" => {
            let (account, repo, requester): (u64, u64, u64) =
                serde_json::from_value(input).map_err(super::worker_err)?;
            storage
                .member_invitation_candidates(account, repo, requester)
                .await
                .map(|value| serde_json::to_value(value).unwrap())
        }
        "/__fixture/settle" => storage
            .settle_github_invitation(&serde_json::from_value(input).map_err(super::worker_err)?)
            .await
            .map(|_| serde_json::Value::Null),
        "/__fixture/insert_invitation" => storage
            .insert_github_invitation(&serde_json::from_value(input).map_err(super::worker_err)?)
            .await
            .map(|_| serde_json::Value::Null),
        "/__fixture/invitation" => storage
            .get_github_invitation(serde_json::from_value(input).map_err(super::worker_err)?)
            .await
            .map(|value| serde_json::to_value(value).unwrap()),
        "/__fixture/storage-suite" => Ok(serde_json::json!(test_suite::SCENARIOS)),
        // The caller supplies a freshly migrated DB per scenario. A failed
        // assertion panics, which aborts this Wasm instance with the message.
        _ if path.starts_with("/__fixture/storage-suite/") => {
            let name = &path["/__fixture/storage-suite/".len()..];
            if !test_suite::run_scenario(name, storage).await {
                return Err(super::worker_err("unknown storage suite scenario"));
            }
            Ok(serde_json::Value::Null)
        }
        "/__fixture/delivery" => storage
            .project_delivery(&serde_json::from_value(input).map_err(super::worker_err)?)
            .await
            .map(|_| serde_json::Value::Null),
        "/__fixture/delivery-read" => storage
            .list_delivery_for_request(serde_json::from_value(input).map_err(super::worker_err)?)
            .await
            .map(|value| serde_json::to_value(value).unwrap()),
        "/__fixture/apply" => storage
            .apply_transition(&serde_json::from_value(input).map_err(super::worker_err)?)
            .await
            .map(|_| serde_json::Value::Null),
        "/__fixture/link" => storage
            .get_invitation_link_by_id(serde_json::from_value(input).map_err(super::worker_err)?)
            .await
            .map(|value| serde_json::to_value(value).unwrap()),
        "/__fixture/request" => storage
            .get_invitation_request(serde_json::from_value(input).map_err(super::worker_err)?)
            .await
            .map(|value| serde_json::to_value(value).unwrap()),
        "/__fixture/audit" => storage
            .list_audit_events(
                input.as_u64().unwrap(),
                None,
                ghinvite_core::storage::AuditPosition::Latest,
            )
            .await
            .map(|value| serde_json::json!({"events": value.events})),
        "/__fixture/audit-page" => {
            let (account, filter, position) = audit_seek(input)?;
            storage
                .list_audit_events(account, filter, position)
                .await
                .map(|page| {
                    serde_json::json!({"events": page.events,
                        "has_older": page.has_older, "has_newer": page.has_newer})
                })
        }
        // The query plan of the page read above, with the same bindings.
        "/__fixture/audit-plan" => {
            use ghinvite_core::storage::audit_read;
            let (account, filter, position) = audit_seek(input)?;
            let boundary = position.boundary();
            let plan = env
                .d1("DB")?
                .prepare(format!(
                    "EXPLAIN QUERY PLAN {}",
                    audit_read::query(filter, position, false)
                ))
                .bind(&[
                    (account as f64).into(),
                    filter.map(|e| e.as_str().into()).unwrap_or(JsValue::NULL),
                    boundary
                        .map(|b| audit_read::boundary_time(b).into())
                        .unwrap_or(JsValue::NULL),
                    boundary
                        .map(|b| b.id.to_string().into())
                        .unwrap_or(JsValue::NULL),
                ])?
                .all()
                .await?
                .results::<serde_json::Value>()?;
            Ok(serde_json::json!(plan))
        }
        _ => return Err(super::worker_err("unknown fixture operation")),
    };
    let (status, value) = match result {
        Ok(value) => (200, value),
        Err(error) => (409, serde_json::json!({"error": error.to_string()})),
    };
    let body = Body::from_stream(
        http_body_util::Full::new(bytes::Bytes::from(serde_json::to_vec(&value).unwrap()))
            .into_data_stream(),
    )?;
    Ok(http::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(body)
        .unwrap())
}

/// `[account, event type | null, "latest" | "before" | "after", id, occurred_at]`.
fn audit_seek(
    input: serde_json::Value,
) -> worker::Result<(
    u64,
    Option<ghinvite_core::audit::EventType>,
    ghinvite_core::storage::AuditPosition,
)> {
    use ghinvite_core::storage::{AuditBoundary, AuditPosition};
    type Seek = (
        u64,
        Option<ghinvite_core::audit::EventType>,
        String,
        Option<ghinvite_core::AuditEventId>,
        Option<chrono::DateTime<chrono::Utc>>,
    );
    let (account, filter, kind, id, occurred_at): Seek =
        serde_json::from_value(input).map_err(super::worker_err)?;
    let boundary = || {
        id.zip(occurred_at)
            .map(|(id, occurred_at)| AuditBoundary { id, occurred_at })
            .ok_or_else(|| super::worker_err("audit boundary missing"))
    };
    let position = match kind.as_str() {
        "latest" => AuditPosition::Latest,
        "before" => AuditPosition::Before(boundary()?),
        "after" => AuditPosition::After(boundary()?),
        _ => return Err(super::worker_err("unknown audit position")),
    };
    Ok((account, filter, position))
}
