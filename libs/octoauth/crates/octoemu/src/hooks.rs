//! The App webhook routes (App JWT): its configuration
//! (`/app/hook/config`) and its deliveries (`/app/hook/deliveries…`),
//! docs.github.com/rest/apps/webhooks. Delivery itself is
//! `crate::webhooks`.

use crate::auth::{self, AppCaller};
use crate::paginate::{Pagination, query_pairs};
use crate::response::{self, json, json_with_link};
use crate::seed::Webhook;
use crate::state::SharedState;
use crate::views;
use crate::webhooks::Delivery;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};

const DELIVERIES_DOCS: &str =
    "https://docs.github.com/rest/apps/webhooks#list-deliveries-for-an-app-webhook";
const CONFIG_DOCS: &str =
    "https://docs.github.com/rest/apps/webhooks#update-a-webhook-configuration-for-an-app";

/// The cursor naming delivery `id` (opaque to clients; behaviour
/// `webhook-deliveries-cursor`).
fn cursor(id: u64) -> String {
    format!("v1_{id}")
}

fn parse_cursor(raw: &str) -> Option<u64> {
    raw.strip_prefix("v1_")?.parse().ok()
}

/// A query string with `cursor` replaced.
fn query_with_cursor(uri: &Uri, value: &str) -> String {
    let mut query: Vec<String> = query_pairs(uri)
        .filter(|(name, _)| *name != "cursor")
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    query.push(format!("cursor={value}"));
    query.join("&")
}

fn bad_request(shared: &SharedState, message: &str) -> Response {
    response::error(
        StatusCode::BAD_REQUEST,
        &shared.message("webhook-deliveries-cursor", message),
        DELIVERIES_DOCS,
    )
}

/// `GET /app/hook/deliveries`: the App's attempts, newest first, `per_page`
/// at a time from `cursor`, with `next`/`prev` cursors in `Link`
/// (documented), filtered by `status=success|failure` (documented: 200–399
/// is a success).
pub(crate) async fn list_deliveries(
    State(shared): State<SharedState>,
    AppCaller(app_id): AppCaller,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let per_page = Pagination::of(&uri).per_page;
    let mut from = None;
    let mut status = None;
    for (name, value) in query_pairs(&uri) {
        match name {
            "cursor" => match parse_cursor(value) {
                Some(id) => from = Some(id),
                None => return bad_request(&shared, "Invalid cursor"),
            },
            "status" => match value {
                "success" => status = Some(true),
                "failure" => status = Some(false),
                _ => return bad_request(&shared, "Invalid status: use success or failure"),
            },
            _ => {}
        }
    }
    if from.is_some()
        && let Some(refusal) = shared.refuse("webhook-deliveries-cursor")
    {
        return refusal;
    }
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let all: Vec<&Delivery> = state
        .deliveries
        .attempts()
        .rev()
        .filter(|d| d.app_id == app_id)
        .filter(|d| status.is_none_or(|ok| d.succeeded() == ok))
        .collect();
    let start = from.map_or(0, |id| {
        all.iter().position(|d| d.id <= id).unwrap_or(all.len())
    });
    let page: Vec<Value> = all
        .iter()
        .skip(start)
        .take(per_page)
        .map(|d| d.summary())
        .collect();
    let mut rels = Vec::new();
    let url = |id: u64| {
        format!(
            "<{origin}{}?{}>",
            uri.path(),
            query_with_cursor(&uri, &cursor(id))
        )
    };
    if let Some(next) = all.get(start + per_page) {
        rels.push(format!("{}; rel=\"next\"", url(next.id)));
    }
    if start > 0 {
        let previous = all[start.saturating_sub(per_page)];
        rels.push(format!("{}; rel=\"prev\"", url(previous.id)));
    }
    let link = (!rels.is_empty()).then(|| rels.join(", "));
    json_with_link(StatusCode::OK, &page, link)
}

/// The App's attempt `raw`, if any.
fn own(shared: &SharedState, app_id: u64, raw: &str) -> Option<Delivery> {
    let id: u64 = raw.parse().ok()?;
    shared
        .state()
        .deliveries
        .attempts()
        .find(|d| d.id == id && d.app_id == app_id)
        .cloned()
}

/// `GET /app/hook/deliveries/{delivery_id}`: one attempt with its request
/// and response; another App's or an unknown one is a 404.
pub(crate) async fn get_delivery(
    State(shared): State<SharedState>,
    AppCaller(app_id): AppCaller,
    Path(raw): Path<String>,
) -> Response {
    match own(&shared, app_id, &raw) {
        Some(delivery) => json(StatusCode::OK, &delivery.detail()),
        None => auth::not_found(),
    }
}

/// `POST /app/hook/deliveries/{delivery_id}/attempts`: 202 `{}` and the
/// same body sent again with the same GUID, as a redelivery (documented).
pub(crate) async fn redeliver(
    State(shared): State<SharedState>,
    AppCaller(app_id): AppCaller,
    Path(raw): Path<String>,
) -> Response {
    match own(&shared, app_id, &raw) {
        Some(delivery) if crate::webhooks::redeliver(&shared, app_id, delivery.id) => {
            json(StatusCode::ACCEPTED, &json!({}))
        }
        _ => auth::not_found(),
    }
}

/// The configuration GitHub reports: the secret masked, `content_type`
/// always `json`.
fn config(webhook: &Webhook, insecure_ssl: &str) -> Value {
    json!({
        "url": webhook.url,
        "content_type": "json",
        "secret": if webhook.secrets.iter().any(|s| !s.is_empty()) { Some("********") } else { None },
        "insecure_ssl": insecure_ssl,
    })
}

/// `GET /app/hook/config`. An App without a webhook: 404 (behaviour
/// `webhook-config-details`).
pub(crate) async fn get_config(
    State(shared): State<SharedState>,
    AppCaller(app_id): AppCaller,
) -> Response {
    let state = shared.state();
    let webhook = state
        .world
        .apps
        .iter()
        .find(|app| app.id == app_id)
        .and_then(|app| app.webhook.as_ref());
    let insecure = state
        .deliveries
        .insecure_ssl
        .get(&app_id)
        .map_or("0", String::as_str);
    match webhook {
        Some(webhook) => json(StatusCode::OK, &config(webhook, insecure)),
        None => {
            drop(state);
            shared
                .refuse("webhook-config-details")
                .unwrap_or_else(auth::not_found)
        }
    }
}

/// `PATCH /app/hook/config`'s body.
#[derive(Deserialize)]
struct ConfigPatch {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    secret: Option<String>,
    #[serde(default)]
    insecure_ssl: Option<Value>,
}

fn unprocessable(shared: &SharedState, field: &str, message: &str) -> Response {
    if let Some(refusal) = shared.refuse("webhook-config-details") {
        return refusal;
    }
    response::validation_failed(
        &[response::FieldError {
            resource: "Hook".into(),
            field: field.into(),
            code: "custom".into(),
            message: Some(message.into()),
        }],
        CONFIG_DOCS,
    )
}

/// `PATCH /app/hook/config` (`url`, `content_type`, `secret`,
/// `insecure_ssl`): a new secret replaces every seeded one (an empty one
/// stops signing); `content_type: form` is not implemented (422).
pub(crate) async fn patch_config(
    State(shared): State<SharedState>,
    AppCaller(app_id): AppCaller,
    body: Bytes,
) -> Response {
    let Ok(patch) = serde_json::from_slice::<ConfigPatch>(&body) else {
        return response::error(
            StatusCode::BAD_REQUEST,
            "Problems parsing JSON",
            CONFIG_DOCS,
        );
    };
    if let Some(url) = &patch.url
        && !(url.starts_with("http://") || url.starts_with("https://"))
    {
        return unprocessable(&shared, "url", "url must be an http(s) URL");
    }
    if patch.content_type.as_deref().is_some_and(|t| t != "json") {
        return unprocessable(
            &shared,
            "content_type",
            "octoemu: only content_type json is implemented",
        );
    }
    let insecure = match &patch.insecure_ssl {
        None => None,
        Some(Value::String(s)) if s == "0" || s == "1" => Some(s.clone()),
        Some(Value::Number(n)) if n.as_u64() == Some(0) || n.as_u64() == Some(1) => {
            Some(n.to_string())
        }
        Some(_) => return unprocessable(&shared, "insecure_ssl", "insecure_ssl must be 0 or 1"),
    };
    let mut state = shared.state();
    let Some(app) = state.world.apps.iter_mut().find(|app| app.id == app_id) else {
        return auth::not_found();
    };
    let webhook = match (app.webhook.take(), &patch.url) {
        (Some(mut webhook), url) => {
            if let Some(url) = url {
                webhook.url = url.clone();
            }
            webhook
        }
        (None, Some(url)) => Webhook {
            url: url.clone(),
            secrets: Vec::new(),
            active: true,
        },
        (None, None) => {
            return unprocessable(
                &shared,
                "url",
                "url is required for an App without a webhook",
            );
        }
    };
    let mut webhook = webhook;
    if let Some(secret) = patch.secret {
        webhook.secrets = if secret.is_empty() {
            Vec::new()
        } else {
            vec![secret]
        };
    }
    let body = config(
        &webhook,
        insecure.as_deref().unwrap_or_else(|| {
            state
                .deliveries
                .insecure_ssl
                .get(&app_id)
                .map_or("0", String::as_str)
        }),
    );
    if let Some(app) = state.world.apps.iter_mut().find(|app| app.id == app_id) {
        app.webhook = Some(webhook);
    }
    if let Some(insecure) = insecure {
        state.deliveries.insecure_ssl.insert(app_id, insecure);
    }
    json(StatusCode::OK, &body)
}
