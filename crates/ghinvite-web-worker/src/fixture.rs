//! Test-only access to production HTTP client boundaries inside workerd.
use ghinvite_github::{
    HttpTransport,
    transport::{Method, Request, ReqwestTransport},
};
use worker::{Env, HttpRequest};

pub async fn fetch(req: HttpRequest, env: &Env) -> worker::Result<axum::response::Response> {
    let method = req.uri().path().trim_start_matches("/__fixture/");
    let result = if method == "github" {
        ReqwestTransport::new()
            .map_err(super::worker_err)?
            .send(Request::new(Method::Put, "https://transport.test/github"))
            .await
            .map(|response| serde_json::json!({"status": response.status}))
            .map_err(|error| error.to_string())
    } else {
        let client =
            ghinvite_web::RestateClient::new(env.var("GHINVITE_RESTATE_INGRESS")?.to_string())
                .map_err(super::worker_err)?;
        let input =
            serde_json::json!({"operation_id": req.uri().query().unwrap_or("same-attempt")});
        match method {
            "send" => client
                .send("Fixture", "key", "mutate", &input)
                .await
                .map(|()| serde_json::Value::Null),
            "call" => client.call("Fixture", "key", "mutate", &input).await,
            "authoritative_call" => {
                client
                    .authoritative_call("Fixture", "key", "mutate", &input)
                    .await
            }
            _ => return Err(super::worker_err("unknown fixture operation")),
        }
        .map_err(|error| error.to_string())
    };
    use axum::response::IntoResponse;
    Ok(match result {
        Ok(value) => axum::Json(value).into_response(),
        Err(error) => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({"error": error})),
        )
            .into_response(),
    })
}
