//! One advisory facts cache and authenticated capacity-wake inbox per org.
//! Neither cache presence nor a member-removed payload authorizes a write.
use crate::AppState;
use chrono::{DateTime, Utc};
use ghinvite_core::{
    account::App,
    org_eligibility::{Facts, InstallationHint, MemberRemoved, Seats},
};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextSideEffects, ContextWriteState, ObjectContext,
        RunFuture,
    },
    errors::{HandlerError, TerminalError},
    serde::Json,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub struct OrganizationEligibility {
    pub state: AppState,
}
#[derive(Clone, Serialize, Deserialize)]
struct Cached {
    installation_id: u64,
    observed_at: DateTime<Utc>,
    facts: Option<Facts>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    input: MemberRemoved,
    received_at: DateTime<Utc>,
    keys: Vec<String>,
    next: usize,
}
#[derive(Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ContinueWake {
    pub event: String,
    pub next: usize,
}
fn invalid() -> TerminalError {
    TerminalError::new_with_code(400, "invalid organization eligibility identity")
}
fn account(key: &str) -> Result<u64, TerminalError> {
    let reference: key::Account = key.parse().map_err(|_| invalid())?;
    if reference.app != App::Org {
        return Err(invalid());
    }
    Ok(reference.account_id)
}
mod key {
    pub type Account = ghinvite_core::account::AccountInstallationRef;
}
async fn current(ctx: &ObjectContext<'_>) -> Result<Option<ghinvite_core::Account>, TerminalError> {
    let expected = account(ctx.key())?;
    Ok(ctx
        .object_client::<crate::availability::AccountInstallationClient>(ctx.key())
        .eligibility_installation()
        .call()
        .await?
        .0
        .filter(|value| {
            value.app == App::Org
                && value.suspended_at.is_none()
                && value.account.account_id == expected
                && value.account.account_type == ghinvite_core::AccountType::Organization
                && value.account.uninstalled_at.is_none()
                && value.account.installation_id > 0
                && value.account.installation_id <= ghinvite_core::storage::integer::MAX_EXACT
        })
        .map(|value| value.account))
}

async fn read(
    state: &AppState,
    account: &ghinvite_core::Account,
    at: DateTime<Utc>,
) -> Option<Facts> {
    use ghinvite_github::{
        AppSlot, endpoints,
        error::{ApiError, CallError},
        profiles::ORG_READ,
    };
    let apps = state.github_apps.as_ref()?;
    let app = apps.apps.app(AppSlot::Org).ok()?;
    let credential = app.app_rest().ok()?;
    let actual = endpoints::get_installation(&credential.rest(), account.installation_id)
        .await
        .ok()?;
    if actual.id != account.installation_id
        || actual.account.id != account.account_id
        || actual.account.account_type.as_deref() != Some("Organization")
        || actual.suspended_at.is_some()
        || app.id().get() != actual.app_id
    {
        return None;
    }
    app.read(account.installation_id, &ORG_READ, |handle| async move {
        let org = endpoints::organization::get_organization(&handle.rest(), &account.account_login)
            .await?;
        if org.id != account.account_id {
            return Err(CallError::<std::convert::Infallible>::Api(
                ApiError::Decode {
                    what: "organization_facts_identity",
                },
            ));
        }
        use endpoints::organization::BasePermission as Wire;
        use ghinvite_core::org_eligibility::BasePermission as Domain;
        let base_permission = match org.default_repository_permission {
            Some(Wire::None) => Some(Domain::None),
            Some(Wire::Read) => Some(Domain::Read),
            Some(Wire::Write) => Some(Domain::Write),
            Some(Wire::Admin) => Some(Domain::Admin),
            _ => None,
        };
        Ok(Facts {
            account_id: account.account_id,
            installation_id: account.installation_id,
            observed_at: at,
            seats: org
                .plan
                .as_ref()
                .and_then(|p| p.paid_seats())
                .filter(|(capacity, used)| {
                    *capacity <= ghinvite_core::storage::integer::MAX_EXACT
                        && *used <= ghinvite_core::storage::integer::MAX_EXACT
                })
                .map(|(capacity, used)| Seats { capacity, used }),
            two_factor_required: org.two_factor_requirement_enabled,
            base_permission,
            managed_enterprise: None,
        })
    })
    .await
    .ok()
}
impl OrganizationEligibility {
    async fn cached(
        &self,
        ctx: &ObjectContext<'_>,
        force: bool,
    ) -> Result<Option<Facts>, TerminalError> {
        let Some(account) = current(ctx).await? else {
            ctx.clear("facts");
            return Ok(None);
        };
        let now = crate::clock::now(ctx, "organization_facts_time").await?;
        if !force && let Some(Json(cached)) = ctx.get::<Json<Cached>>("facts").await? {
            let ttl = if cached.facts.is_some() {
                ghinvite_core::org_eligibility::CACHE_SECONDS
            } else {
                60
            };
            if cached.installation_id == account.installation_id
                && now >= cached.observed_at
                && now < cached.observed_at + chrono::Duration::seconds(ttl)
                && cached.facts.as_ref().is_none_or(|facts| {
                    facts.fresh(account.account_id, account.installation_id, now)
                })
            {
                return Ok(cached.facts);
            }
        }
        let facts = ctx
            .run(|| async { Ok::<_, HandlerError>(Json(read(&self.state, &account, now).await)) })
            .name("organization_facts_read")
            .await?
            .0;
        ctx.set(
            "facts",
            Json(Cached {
                installation_id: account.installation_id,
                observed_at: now,
                facts: facts.clone(),
            }),
        );
        Ok(facts)
    }
    async fn send_page(
        &self,
        ctx: &ObjectContext<'_>,
        event: &str,
        receipts: &mut BTreeMap<String, Receipt>,
    ) -> Result<(), TerminalError> {
        let Some(receipt) = receipts.get_mut(event) else {
            return Ok(());
        };
        let end = (receipt.next + 50).min(receipt.keys.len());
        for key in &receipt.keys[receipt.next..end] {
            ctx.object_client::<crate::org_delivery::OrganizationDeliveryClient>(key)
                .capacity_changed(Json(receipt.input.clone()))
                .send()
                .await?;
        }
        receipt.next = end;
        let more = end < receipt.keys.len();
        ctx.set("capacity_receipts", Json(receipts.clone()));
        if more {
            ctx.object_client::<OrganizationEligibilityClient>(ctx.key())
                .wake_next(Json(ContinueWake {
                    event: event.into(),
                    next: end,
                }))
                .send()
                .await?;
        }
        Ok(())
    }
}
#[restate_sdk::object]
impl OrganizationEligibility {
    #[handler]
    async fn facts(&self, ctx: ObjectContext<'_>) -> Result<Json<Option<Facts>>, TerminalError> {
        self.cached(&ctx, false).await.map(Json)
    }
    #[handler]
    async fn refresh(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<InstallationHint>,
    ) -> Result<(), TerminalError> {
        if account(ctx.key())? != input.account_id
            || input.installation_id == 0
            || input.installation_id > ghinvite_core::storage::integer::MAX_EXACT
        {
            return Err(invalid());
        }
        // An old installation's delayed event may never erase a newer cache.
        match current(&ctx).await? {
            Some(current) if current.installation_id == input.installation_id => {
                ctx.clear("facts");
                self.cached(&ctx, true).await?;
            }
            None => ctx.clear("facts"),
            _ => {}
        }
        Ok(())
    }
    #[handler]
    async fn member_removed(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<MemberRemoved>,
    ) -> Result<(), TerminalError> {
        input.validate().map_err(|_| invalid())?;
        if account(ctx.key())? != input.account_id {
            return Err(invalid());
        }
        let now = crate::clock::now(&ctx, "organization_capacity_time").await?;
        let mut receipts = ctx
            .get::<Json<BTreeMap<String, Receipt>>>("capacity_receipts")
            .await?
            .map(|j| j.0)
            .unwrap_or_default();
        let event = input.event_key();
        if let Some(old) = receipts.get(&event)
            && (old.next < old.keys.len() || now < old.received_at + chrono::Duration::days(7))
        {
            if old.input != input {
                return Err(TerminalError::new_with_code(
                    409,
                    "organization capacity event conflict",
                ));
            }
            return self.send_page(&ctx, &event, &mut receipts).await;
        }
        receipts.retain(|_, r| {
            r.next < r.keys.len() || r.received_at > now - chrono::Duration::days(7)
        });
        let matches = current(&ctx)
            .await?
            .is_some_and(|a| a.installation_id == input.installation_id);
        let keys = if matches {
            ctx.clear("facts");
            let schedule = ctx
                .object_client::<crate::sweep_schedule::SweepScheduleClient>(ctx.key())
                .status()
                .call()
                .await?
                .0;
            schedule
                .and_then(|schedule| {
                    schedule
                        .tracked
                        .get(&input.account_id)
                        .map(|keys| keys.iter().cloned().collect())
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        receipts.insert(
            event.clone(),
            Receipt {
                input,
                received_at: now,
                keys,
                next: 0,
            },
        );
        ctx.set("capacity_receipts", Json(receipts.clone()));
        self.send_page(&ctx, &event, &mut receipts).await
    }
    #[handler]
    async fn wake_next(
        &self,
        ctx: ObjectContext<'_>,
        Json(input): Json<ContinueWake>,
    ) -> Result<(), TerminalError> {
        account(ctx.key())?;
        let mut receipts = ctx
            .get::<Json<BTreeMap<String, Receipt>>>("capacity_receipts")
            .await?
            .map(|j| j.0)
            .unwrap_or_default();
        if receipts
            .get(&input.event)
            .is_some_and(|r| r.next == input.next)
        {
            self.send_page(&ctx, &input.event, &mut receipts).await?;
        }
        Ok(())
    }
}
