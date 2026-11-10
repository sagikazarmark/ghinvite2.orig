//! Operator-only projected delivery signals. No account authority is implied.
use crate::{AppState, WebError, session};
use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use chrono::Utc;
use dioxus::prelude::*;
use ghinvite_core::storage::operational::{AgeSignal, DeliverySignals, SignalCounts};
use serde::Serialize;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/console/operations", get(page))
        .route("/console/operations.json", get(json))
}

enum ReadFailure {
    Session,
    Concealed,
    Storage,
}
impl IntoResponse for ReadFailure {
    fn into_response(self) -> Response {
        match self {
            Self::Session => (StatusCode::SERVICE_UNAVAILABLE, "Operator session unavailable").into_response(),
            Self::Concealed => WebError::NotFound.into_response(),
            Self::Storage => (StatusCode::SERVICE_UNAVAILABLE, "Delivery signals unavailable. Health cannot be determined; retry after projection storage recovers.").into_response(),
        }
    }
}
async fn load(
    state: &AppState,
    tower: &tower_sessions::Session,
) -> Result<(session::Session, DeliverySignals), ReadFailure> {
    let session = session::load(tower)
        .await
        .map_err(|_| ReadFailure::Session)?;
    if !session.is_authenticated() || !state.config.operator_ids.contains(&session.user_id) {
        return Err(ReadFailure::Concealed);
    }
    let report = state
        .storage
        .delivery_signals(Utc::now())
        .await
        .map_err(|_| {
            tracing::warn!(
                alert = "delivery_signals_unavailable",
                "Operator delivery signals read failed"
            );
            ReadFailure::Storage
        })?;
    Ok((session, report))
}

fn private(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("private, no-store"),
    );
    response
}
async fn page(State(state): State<AppState>, tower: tower_sessions::Session) -> Response {
    match load(&state, &tower).await {
        Ok((session, report)) => private(Html(crate::render::render(move || {
            view(session.login.clone(), &report)
        }))),
        Err(response) => private(response),
    }
}
#[derive(Serialize)]
struct Report {
    incomplete: bool,
    #[serde(flatten)]
    signals: DeliverySignals,
}
async fn json(State(state): State<AppState>, tower: tower_sessions::Session) -> Response {
    match load(&state, &tower).await {
        Ok((_, signals)) => private(Json(Report {
            incomplete: signals.incomplete(),
            signals,
        })),
        Err(response) => private(response),
    }
}

fn view(login: String, report: &DeliverySignals) -> Element {
    rsx! {
        ghinvite_ui::layouts::HomeLayout { signed_in_login:Some(login), title:"Delivery operations",
            section { class:"mx-auto max-w-5xl space-y-6",
                h1 { class:"text-2xl font-semibold", "Delivery operations" }
                p { "Projected repository, organization, team and email delivery steps, observed at {report.observed_at.to_rfc3339()}." }
                p { "This report reads projection storage. Missing or delayed projections may hide newer owner activity; these counts do not establish end-to-end health." }
                if report.incomplete() {
                    div { role:"alert", class:"alert alert-warning", "Incomplete observation: timestamps are missing or in the future. Do not treat unaged deliveries as healthy. Check projection progress and clock consistency." }
                }
                if report.counts.total_steps() == 0 {
                    p { "No projected delivery steps. There is no delivery health evidence in this report." }
                }
                h2 { class:"text-xl font-semibold", "All accounts" }
                {counts(&report.counts)}
                h2 { class:"text-xl font-semibold", "By account" }
                for account in &report.by_account {
                    section { class:"space-y-3 rounded border p-4",
                        h3 { class:"font-semibold", "Account {account.account_id}" }
                        {counts(&account.counts)}
                    }
                }
                h2 { class:"text-xl font-semibold", "Delivery steps" }
                p { "Each row identifies an independent target step. Observation ages use only recorded evidence; unknown outcomes never authorize another write." }
                table { class:"table w-full",
                    thead { tr { th { "Account" } th { "Kind / target" } th { "Delivery / step" } th { "Outcome" } th { "Unknown since" } th { "Last observation" } } }
                    tbody { for step in &report.steps {
                        tr {
                            td { "{step.account_id.unwrap_or_default()}" }
                            td { "{step.kind.label()} {step.target_id}" }
                            td { "{step.id} / {step.step}"
                                if let Some(request) = step.request_id { p { "Request {request}, generation {step.delivery_generation}" } }
                                if let Some(link) = step.link_id { p { "Link {link}" } }
                                if let (Some(batch), Some(row)) = (step.batch_id, step.batch_row) { p { "Batch {batch}, row {row}" } }
                            }
                            td { "{step.create_outcome}" }
                            td { "{step.unknown_since.map(|t| t.to_rfc3339()).unwrap_or_else(|| \"Unavailable\".into())}" }
                            td { "{step.last_observed_at.or(step.sent_at).map(|t| t.to_rfc3339()).unwrap_or_else(|| \"Unavailable\".into())}" }
                        }
                    } }
                }
                p { a { class:"link", href:"/console/operations.json", "Read the structured report" } }
                p { "Review affected account delivery records and invocation progress before recovery. An unknown outcome is not evidence that a GitHub write failed; do not repeat an unconfirmed write." }
            }
        }
    }
}
fn counts(counts: &SignalCounts) -> Element {
    rsx! {
        p { "Projected repositories: {counts.repository_deliveries}; organizations: {counts.organization_deliveries}; team steps: {counts.team_steps}; email invitations: {counts.email_deliveries}" }
        div { class:"overflow-x-auto",
            table { class:"table w-full",
                thead { tr { th { "Signal" } th { "Total" } th { "Alert count" } th { "Oldest age" } th { "Missing timestamps" } th { "Future timestamps" } } }
                tbody {
                    {age("Unknown outcomes", "more than 1 hour", &counts.unknown)}
                    {age("Pending invitations", "more than 2 hours without an observation", &counts.pending)}
                }
            }
        }
        if counts.unknown.over_threshold != 0 || counts.pending.over_threshold != 0 {
            p { role:"alert", class:"text-warning", "Threshold exceeded: inspect retained delivery state, reconciler scheduling and projection progress." }
        }
        if !counts.blocked.is_empty() {
            h4 { class:"font-semibold", "Blocked by reason" }
            ul { class:"list-disc pl-6",
                for blocked in &counts.blocked {
                    li { "{blocked.kind.label()} — {blocked.count}: {blocked.reason.description()}" }
                }
            }
        }
    }
}
fn age(label: &str, threshold: &str, signal: &AgeSignal) -> Element {
    let oldest = signal
        .oldest_age_seconds
        .map(|age| format!("{age} seconds"))
        .unwrap_or_else(|| "Unavailable".into());
    rsx! { tr {
        th { scope:"row", "{label}" }
        td { "{signal.total}" }
        td { "{signal.over_threshold} ({threshold})" }
        td { "{oldest}" }
        td { "{signal.missing_timestamp}" }
        td { "{signal.future_timestamp}" }
    } }
}
