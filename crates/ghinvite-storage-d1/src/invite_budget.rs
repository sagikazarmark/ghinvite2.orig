use super::{D1Storage, int, text, wasm_send};
use async_trait::async_trait;
use ghinvite_core::{
    account::AccountInstallationRef,
    budget::BudgetSnapshot,
    storage::{
        Result,
        invite_budget::{self, InviteBudgetStorage},
    },
};
#[async_trait]
impl InviteBudgetStorage for D1Storage {
    async fn share_limited_links(
        &self,
        owner: AccountInstallationRef,
        link_id: Option<ghinvite_core::InvitationLinkId>,
    ) -> Result<Vec<invite_budget::ShareLimitedLink>> {
        wasm_send(async {
            self.all(
                invite_budget::SHARE_LIMITED_LINKS,
                &[
                    text(owner.app.as_str()),
                    int(owner.account_id)?,
                    text(&link_id.map(|id| id.to_string()).unwrap_or_default()),
                ],
            )
            .await
        })
        .await
    }
    async fn project_invite_budget(&self, snapshot: &BudgetSnapshot) -> Result<()> {
        wasm_send(async {
            let input = invite_budget::encode(snapshot)?;
            self.batch(invite_budget::APPLY, &input, invite_budget::classify)
                .await
        })
        .await
    }
    async fn get_invite_budget(
        &self,
        owner: AccountInstallationRef,
    ) -> Result<Option<BudgetSnapshot>> {
        wasm_send(async {
            self.first::<invite_budget::Row>(
                invite_budget::GET,
                &[text(owner.app.as_str()), int(owner.account_id)?],
            )
            .await?
            .map(invite_budget::Row::decode)
            .transpose()
        })
        .await
    }
    async fn list_invite_budget_buckets(
        &self,
        owner: AccountInstallationRef,
    ) -> Result<Vec<ghinvite_core::budget::BucketSnapshot>> {
        wasm_send(async {
            self.all::<invite_budget::BucketRow>(
                invite_budget::LIST,
                &[text(owner.app.as_str()), int(owner.account_id)?],
            )
            .await?
            .into_iter()
            .map(invite_budget::BucketRow::decode)
            .collect()
        })
        .await
    }
}
