//! M1 invitation budget. Explicit AppState composition enables S3 and binds
//! this authority in the standard endpoint; production activation awaits SP-08.
use chrono::{DateTime, Utc};
use ghinvite_core::{
    account::{AccountInstallationRef, App},
    budget::{
        BudgetSnapshot, Configure, InvitationRefund, RecordLimit, RefundInvitationAfterPut204,
        Reservation, Reserve, SlotId,
    },
    storage::invite_budget::InviteBudgetStorage,
};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
        RunFuture, SharedObjectContext,
    },
    errors::TerminalError,
    serde::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
mod ledger;
mod model;
use model::State;

#[derive(Default)]
pub struct InviteBudget {
    #[cfg(feature = "integration")]
    pub clock: Option<Arc<std::sync::atomic::AtomicI64>>,
}
pub struct InviteBudgetProjection {
    storage: Arc<dyn InviteBudgetStorage>,
}
/// Explicit M1 composition, shared by custom fixtures and the standard endpoint.
/// Production activation is a separate SP-08/M1 release gate.
pub fn bind_pre_activation(
    builder: restate_sdk::endpoint::Builder,
    storage: Arc<dyn InviteBudgetStorage>,
    budget: InviteBudget,
) -> restate_sdk::endpoint::Builder {
    builder
        .bind(budget)
        .bind(InviteBudgetProjection { storage })
}
fn owner(key: &str) -> Result<AccountInstallationRef, TerminalError> {
    key.parse()
        .map_err(|_| TerminalError::new_with_code(400, "invalid budget owner"))
}
impl InviteBudget {
    async fn now(
        &self,
        ctx: &impl crate::clock::ContextClockExt,
        name: &'static str,
    ) -> Result<DateTime<Utc>, TerminalError> {
        #[cfg(feature = "integration")]
        if let Some(clock) = &self.clock {
            return crate::clock::with_clock(ctx, name, || {
                DateTime::from_timestamp_millis(clock.load(std::sync::atomic::Ordering::SeqCst))
                    .expect("fixture budget clock")
            })
            .await;
        }
        crate::clock::now(ctx, name).await
    }
    async fn load(
        &self,
        ctx: &ObjectContext<'_>,
    ) -> Result<(AccountInstallationRef, State), TerminalError> {
        let owner = owner(ctx.key())?;
        let mut state = ctx
            .get::<Json<State>>("budget")
            .await?
            .map(|v| v.0)
            .unwrap_or_default();
        state.prepare(owner, self.now(ctx, "budget_time").await?);
        Ok((owner, state))
    }
    fn save(
        &self,
        ctx: &ObjectContext<'_>,
        owner: AccountInstallationRef,
        mut state: State,
    ) -> Result<(), TerminalError> {
        state.advance()?;
        let snapshot = state.snapshot(owner);
        ctx.set("budget", Json(state));
        ctx.object_client::<InviteBudgetProjectionClient>(ctx.key())
            .apply(Json(snapshot))
            .send();
        Ok(())
    }
}
#[restate_sdk::object]
impl InviteBudget {
    #[handler]
    async fn reserve(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<Reserve>,
    ) -> Result<Json<Reservation>, TerminalError> {
        let (owner, mut state) = self.load(&ctx).await?;
        let result = state.reserve(owner, input)?;
        self.save(&ctx, owner, state)?;
        Ok(Json(result))
    }
    #[handler]
    async fn release(
        &self,
        ctx: ObjectContext<'_>,
        Json(slot): Json<SlotId>,
    ) -> Result<(), TerminalError> {
        let (owner, mut state) = self.load(&ctx).await?;
        state.release(slot);
        self.save(&ctx, owner, state)
    }
    #[handler]
    async fn refund_invitation_after_put204(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<RefundInvitationAfterPut204>,
    ) -> Result<Json<InvitationRefund>, TerminalError> {
        let (owner, mut state) = self.load(&ctx).await?;
        let result = state.refund_invitation_after_put204(input)?;
        self.save(&ctx, owner, state)?;
        Ok(Json(result))
    }
    #[handler]
    async fn record_limit(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<RecordLimit>,
    ) -> Result<(), TerminalError> {
        let (owner, mut state) = self.load(&ctx).await?;
        state.record_limit(owner, input)?;
        self.save(&ctx, owner, state)
    }
    #[handler]
    async fn set_org_cap(
        &self,
        ctx: ObjectContext<'_>,
        Json(policy): Json<ghinvite_core::budget::OrgCapPolicy>,
    ) -> Result<(), TerminalError> {
        let (owner, mut state) = self.load(&ctx).await?;
        state.set_org_cap(owner, policy)?;
        self.save(&ctx, owner, state)
    }
    /// Immutable link policy, independent of the organization-cap authority.
    #[handler]
    async fn set_link_share(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ghinvite_core::budget::LinkShare>,
    ) -> Result<(), TerminalError> {
        let (owner, mut state) = self.load(&ctx).await?;
        state.set_link_share(input)?;
        self.save(&ctx, owner, state)
    }
    #[handler]
    async fn configure(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<Configure>,
    ) -> Result<(), TerminalError> {
        let (owner, mut state) = self.load(&ctx).await?;
        state.configure(owner, input)?;
        self.save(&ctx, owner, state)
    }
    #[handler]
    async fn snapshot(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<BudgetSnapshot>, TerminalError> {
        let owner = owner(ctx.key())?;
        let mut state = ctx
            .get::<Json<State>>("budget")
            .await?
            .map(|v| v.0)
            .unwrap_or_default();
        state.prepare(owner, self.now(&ctx, "budget_snapshot_time").await?);
        Ok(Json(state.snapshot(owner)))
    }
}
#[restate_sdk::object]
impl InviteBudgetProjection {
    #[handler]
    async fn apply(
        &self,
        ctx: ObjectContext<'_>,
        Json(snapshot): Json<BudgetSnapshot>,
    ) -> Result<(), TerminalError> {
        if owner(ctx.key())? != snapshot.owner() {
            return Err(TerminalError::new_with_code(
                400,
                "budget projection owner mismatch",
            ));
        }
        ctx.run(|| async {
            self.storage
                .project_invite_budget(&snapshot)
                .await
                .map_err(restate_sdk::errors::HandlerError::from)
        })
        .name("apply_budget_snapshot")
        .await
    }
}
#[cfg(test)]
mod tests;
