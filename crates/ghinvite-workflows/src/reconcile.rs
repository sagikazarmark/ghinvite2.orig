//! `Reconcile` Service: daily sweep over pending GitHub invitations.

use crate::state::AppState;
use chrono::{DateTime, Utc};
use restate_sdk::context::{Context, ContextClient, ContextSideEffects, ContextTimers, RunFuture};
use restate_sdk::errors::TerminalError;
use restate_sdk::serde::Json;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct DailyRunInput {
    pub at: DateTime<Utc>,
}

/// One invitation the sweep will observe, and the account whose GitHub quota
/// observing it spends. Rate limits belong to the installation, so the sweep
/// waits one out per account rather than once for every account at a time.
#[derive(Debug, Deserialize, Serialize)]
struct Candidate {
    #[serde(flatten)]
    row: ghinvite_core::GithubInvitation,
    account_id: u64,
}

/// What one sweep step learned about an invitation: the settlement evidence, or
/// the bounded wait GitHub asked for before it would answer at all.
#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum Observed {
    Throttled { throttled_for_secs: u64 },
    Evidence(Option<crate::settlement::ReconcileEvidence>),
}

#[restate_sdk::service]
pub trait Reconcile {
    async fn daily_run(input: Json<DailyRunInput>) -> std::result::Result<(), TerminalError>;
}

pub struct ReconcileImpl {
    pub state: AppState,
}

impl Reconcile for ReconcileImpl {
    async fn daily_run(
        &self,
        ctx: Context<'_>,
        Json(input): Json<DailyRunInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(rows) = ctx
            .run(|| async {
                let mut rows = Vec::new();
                for account in self.state.storage.list_active_installations().await? {
                    rows.extend(
                        self.state
                            .storage
                            .list_pending_github_invitations_for_account(account.account_id)
                            .await?
                            .into_iter()
                            .filter(crate::settlement::eligible)
                            .map(|row| Candidate {
                                account_id: account.account_id,
                                row,
                            }),
                    );
                }
                Ok::<_, restate_sdk::errors::HandlerError>(Json(rows))
            })
            .name("settlement_candidates")
            .await?;
        // One deferral per account, not per row and not per sweep. A quota is
        // the installation's, so a wait that clears one account's limit clears
        // it for that account's remaining rows and says nothing about any
        // other; and a limit outlasting GitHub's own guidance is tomorrow's
        // sweep to observe rather than this one's to hold open row after row.
        // The sweep is a service, so the wait holds no invitation object.
        let mut deferred = std::collections::HashSet::new();
        let mut exhausted = std::collections::HashSet::new();
        for Candidate { account_id, row } in rows {
            if exhausted.contains(&account_id) {
                continue;
            }
            let evidence = loop {
                let Json(observed) = ctx.run(|| async {
                    match crate::settlement::observe(&self.state, &row, input.at).await {
                        Ok(evidence) => Ok(Json(Observed::Evidence(evidence))),
                        Err(e) => match e.rate_limit() {
                            Some(limit) => Ok(Json(Observed::Throttled {
                                throttled_for_secs: crate::throttle::backoff(limit.retry_after).as_secs(),
                            })),
                            None if e.is_terminal() => {
                                tracing::warn!(invitation_id = %row.id, err = %e, "settlement observation failed");
                                Ok(Json(Observed::Evidence(None)))
                            },
                            None => Err(crate::error::to_sdk_handler_error(e)),
                        },
                    }
                }).name("observe_invitation").await?;
                match observed {
                    Observed::Evidence(evidence) => break evidence,
                    Observed::Throttled { throttled_for_secs } if deferred.insert(account_id) => {
                        ctx.sleep(std::time::Duration::from_secs(throttled_for_secs))
                            .await?;
                    }
                    // Throttled again after waiting out GitHub's own guidance.
                    // Probing this account's remaining rows would only spend
                    // more of the quota it has already run out of.
                    Observed::Throttled { .. } => {
                        exhausted.insert(account_id);
                        break None;
                    }
                }
            };
            if let Some(evidence) = evidence {
                ctx.object_client::<crate::github_invitation::GithubInvitationClient>(
                    row.id.to_string(),
                )
                .reconcile(Json(evidence))
                .call()
                .await?;
            }
        }
        Ok(())
    }
}
