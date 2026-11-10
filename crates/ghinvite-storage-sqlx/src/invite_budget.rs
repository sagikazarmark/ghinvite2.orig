use crate::{SqlxStorage, decode, to_db_err, u64_to_i64};
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
impl InviteBudgetStorage for SqlxStorage {
    async fn share_limited_links(
        &self,
        owner: AccountInstallationRef,
        link_id: Option<ghinvite_core::InvitationLinkId>,
    ) -> Result<Vec<invite_budget::ShareLimitedLink>> {
        let rows = sqlx::query(invite_budget::SHARE_LIMITED_LINKS)
            .bind(owner.app.as_str())
            .bind(u64_to_i64(owner.account_id)?)
            .bind(link_id.map(|id| id.to_string()).unwrap_or_default())
            .fetch_all(&self.pool)
            .await
            .map_err(to_db_err)?;
        rows.iter()
            .map(decode::<invite_budget::ShareLimitedLink>)
            .collect()
    }
    async fn project_invite_budget(&self, snapshot: &BudgetSnapshot) -> Result<()> {
        let input = invite_budget::encode(snapshot)?;
        let mut tx = self.pool.begin().await.map_err(to_db_err)?;
        for statement in invite_budget::APPLY {
            sqlx::query(statement)
                .bind(&input)
                .execute(&mut *tx)
                .await
                .map_err(|e| invite_budget::classify(e.to_string()))?;
        }
        tx.commit().await.map_err(to_db_err)
    }
    async fn get_invite_budget(
        &self,
        owner: AccountInstallationRef,
    ) -> Result<Option<BudgetSnapshot>> {
        let row = sqlx::query(invite_budget::GET)
            .bind(owner.app.as_str())
            .bind(u64_to_i64(owner.account_id)?)
            .fetch_optional(&self.pool)
            .await
            .map_err(to_db_err)?;
        row.as_ref()
            .map(|r| decode::<invite_budget::Row>(r)?.decode())
            .transpose()
    }
    async fn list_invite_budget_buckets(
        &self,
        owner: AccountInstallationRef,
    ) -> Result<Vec<ghinvite_core::budget::BucketSnapshot>> {
        let rows = sqlx::query(invite_budget::LIST)
            .bind(owner.app.as_str())
            .bind(u64_to_i64(owner.account_id)?)
            .fetch_all(&self.pool)
            .await
            .map_err(to_db_err)?;
        rows.iter()
            .map(|r| decode::<invite_budget::BucketRow>(r)?.decode())
            .collect()
    }
}
