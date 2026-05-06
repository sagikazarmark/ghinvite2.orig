//! `POST /webhooks/github` — HMAC-verified webhook receiver. Plan 6.

use crate::state::AppState;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use chrono::Utc;
use domain::SelectedRepos;
use std::collections::BTreeSet;

pub fn router() -> Router<AppState> {
    Router::new().route("/webhooks/github", post(handle_webhook))
}

async fn handle_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // HMAC verification must happen on raw bytes before parsing.
    let signature = match headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
    {
        Some(s) => s.to_owned(),
        None => {
            tracing::warn!("webhook: missing X-Hub-Signature-256 header");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    };

    if !github::hmac::verify_signature_256(&state.config.webhook_secret, &body, &signature) {
        tracing::warn!("webhook: HMAC verification failed");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Parse after HMAC verification.
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = ?e, "webhook: body is not valid JSON");
            return StatusCode::OK.into_response(); // don't ask GitHub to retry malformed payloads
        }
    };

    let event_type = match headers.get("x-github-event").and_then(|v| v.to_str().ok()) {
        Some(e) => e.to_owned(),
        None => {
            tracing::warn!("webhook: missing X-GitHub-Event header");
            return StatusCode::OK.into_response();
        }
    };

    let delivery_id = headers
        .get("x-github-delivery")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let result = match event_type.as_str() {
        "repository_invitation" => {
            handle_repository_invitation(&state, &payload, &delivery_id).await
        }
        "member" => handle_member(&state, &payload, &delivery_id).await,
        "installation" => handle_installation(&state, &payload, &delivery_id).await,
        "installation_repositories" => {
            handle_installation_repositories(&state, &payload, &delivery_id).await
        }
        _ => {
            tracing::debug!(event = %event_type, "webhook: unknown event type, ignoring");
            Ok(())
        }
    };

    match result {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => {
            tracing::warn!(error = %e, event = %event_type, "webhook: routing to Restate failed");
            // Return 5xx so GitHub retries (72h retry window). Don't return 200 on failure.
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn handle_repository_invitation(
    state: &AppState,
    payload: &serde_json::Value,
    delivery_id: &str,
) -> Result<(), String> {
    let action = payload["action"].as_str().unwrap_or("");
    let github_inv_id = payload["invitation"]["id"]
        .as_u64()
        .ok_or_else(|| "repository_invitation: missing invitation.id".to_string())?;

    let webhook_action = match action {
        "accepted" => "Accepted",
        "declined" => "Declined",
        _ => {
            tracing::debug!(action, "repository_invitation: unhandled action");
            return Ok(());
        }
    };

    let inv = state
        .storage
        .get_github_invitation_by_github_id(github_inv_id)
        .await
        .map_err(|e| format!("storage lookup: {e}"))?;

    let Some(inv) = inv else {
        tracing::debug!(
            github_inv_id,
            "repository_invitation: no matching row (not our invitation)"
        );
        return Ok(());
    };

    let input = serde_json::json!({
        "invitation_id": inv.id.to_string(),
        "action": webhook_action,
        "at": Utc::now(),
    });

    state
        .restate
        .send(
            "GithubInvitation",
            &inv.id.to_string(),
            "on_webhook",
            &input,
        )
        .await
        .map_err(|e| format!("Restate send: {e}"))?;

    tracing::info!(
        github_inv_id,
        our_id = %inv.id,
        action = webhook_action,
        delivery = delivery_id,
        "repository_invitation routed"
    );
    Ok(())
}

async fn handle_member(
    _state: &AppState,
    payload: &serde_json::Value,
    delivery_id: &str,
) -> Result<(), String> {
    // v1.1: add Storage::get_pending_invitation_by_repo_and_user and handle fully.
    let action = payload["action"].as_str().unwrap_or("");
    tracing::debug!(
        action,
        delivery = delivery_id,
        "member event received (not yet handled — v1.1)"
    );
    Ok(())
}

async fn handle_installation(
    state: &AppState,
    payload: &serde_json::Value,
    delivery_id: &str,
) -> Result<(), String> {
    let action = payload["action"].as_str().unwrap_or("");
    let installation_id = payload["installation"]["id"]
        .as_u64()
        .ok_or_else(|| "installation: missing installation.id".to_string())?;

    match action {
        "deleted" => {
            let input = serde_json::json!({
                "installation_id": installation_id,
                "uninstalled_at": Utc::now(),
            });
            state
                .restate
                .send(
                    "Installation",
                    &installation_id.to_string(),
                    "uninstall",
                    &input,
                )
                .await
                .map_err(|e| format!("Restate send: {e}"))?;
            tracing::info!(
                installation_id,
                delivery = delivery_id,
                "installation.deleted routed"
            );
        }
        "created" => {
            // Setup URL is the primary onboarding path; installation.created is
            // only a reconciliation signal for now.
            tracing::debug!(
                installation_id,
                delivery = delivery_id,
                "installation.created observed; setup URL handles verified onboarding"
            );
        }
        _ => {
            tracing::debug!(action, "installation: unhandled action");
        }
    }
    Ok(())
}

fn repository_ids(value: &serde_json::Value) -> Vec<u64> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|repo| repo.get("id").and_then(|id| id.as_u64()))
        .collect()
}

fn selected_repos_from_repository_event(
    payload: &serde_json::Value,
    current: &SelectedRepos,
) -> Result<Option<SelectedRepos>, String> {
    match payload["repository_selection"]
        .as_str()
        .unwrap_or("selected")
    {
        "all" => Ok(Some(SelectedRepos::All)),
        "selected" => {
            let mut ids: BTreeSet<u64> = match current {
                SelectedRepos::All => {
                    return Ok(None);
                }
                SelectedRepos::Subset(existing) => existing.iter().copied().collect(),
            };
            for id in repository_ids(&payload["repositories_removed"]) {
                ids.remove(&id);
            }
            for id in repository_ids(&payload["repositories_added"]) {
                ids.insert(id);
            }
            Ok(Some(SelectedRepos::Subset(ids.into_iter().collect())))
        }
        other => Err(format!("unsupported repository_selection: {other}")),
    }
}

async fn handle_installation_repositories(
    state: &AppState,
    payload: &serde_json::Value,
    delivery_id: &str,
) -> Result<(), String> {
    let installation_id = payload["installation"]["id"]
        .as_u64()
        .ok_or_else(|| "installation_repositories: missing installation.id".to_string())?;

    let current = state
        .storage
        .get_installation(installation_id)
        .await
        .map_err(|e| format!("storage lookup: {e}"))?
        .ok_or_else(|| {
            format!("installation_repositories: unknown installation {installation_id}")
        })?;
    let Some(selected_repos) =
        selected_repos_from_repository_event(payload, &current.selected_repos)?
    else {
        tracing::warn!(
            installation_id,
            delivery = delivery_id,
            "installation_repositories selected-repo delta cannot repair prior all-repos state; setup update redirect will reconcile"
        );
        return Ok(());
    };

    let input = serde_json::json!({
        "installation_id": installation_id,
        "selected_repos": selected_repos,
    });

    state
        .restate
        .send(
            "Installation",
            &installation_id.to_string(),
            "repos_changed",
            &input,
        )
        .await
        .map_err(|e| format!("Restate send: {e}"))?;

    tracing::info!(
        installation_id,
        delivery = delivery_id,
        "installation_repositories routed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::SelectedRepos;

    #[test]
    fn repository_selection_webhook_all_maps_to_all() {
        let payload = serde_json::json!({
            "repository_selection": "all",
            "repositories_added": [],
            "repositories_removed": []
        });
        let current = SelectedRepos::Subset(vec![10, 11]);
        assert_eq!(
            selected_repos_from_repository_event(&payload, &current).unwrap(),
            Some(SelectedRepos::All)
        );
    }

    #[test]
    fn repository_selection_webhook_applies_selected_delta() {
        let payload = serde_json::json!({
            "repository_selection": "selected",
            "repositories_added": [{"id": 12}],
            "repositories_removed": [{"id": 10}]
        });
        let current = SelectedRepos::Subset(vec![10, 11]);
        assert_eq!(
            selected_repos_from_repository_event(&payload, &current).unwrap(),
            Some(SelectedRepos::Subset(vec![11, 12]))
        );
    }

    #[test]
    fn repository_selection_webhook_all_to_selected_is_not_repairable_from_delta() {
        let payload = serde_json::json!({
            "repository_selection": "selected",
            "repositories_added": [],
            "repositories_removed": [{"id": 10}]
        });
        let current = SelectedRepos::All;
        assert_eq!(
            selected_repos_from_repository_event(&payload, &current).unwrap(),
            None
        );
    }
}
