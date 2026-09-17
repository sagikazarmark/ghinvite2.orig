//! Test-only access to the public storage boundary inside workerd.
use ghinvite_core::storage::{Storage, projection::ProjectionStorage};
use http_body_util::BodyExt;
use worker::{Body, Env, HttpRequest};

pub async fn fetch(req: HttpRequest, env: &Env) -> worker::Result<http::Response<Body>> {
    let path = req.uri().path().to_owned();
    let bytes = req.into_body().collect().await?.to_bytes();
    let input: serde_json::Value = serde_json::from_slice(&bytes).map_err(super::worker_err)?;
    let storage =
        ghinvite_storage_d1::D1Storage::new(env.d1(if path == "/__fixture/delivery-suite" {
            "DB_DELIVERY"
        } else {
            "DB"
        })?);
    let result = match path.as_str() {
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
        "/__fixture/delivery-suite" => {
            ghinvite_core::storage::test_suite::scenario_delivery_audit(storage).await;
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
            .get_projected_request(serde_json::from_value(input).map_err(super::worker_err)?)
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
