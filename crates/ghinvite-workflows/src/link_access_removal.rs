//! Link-owned, operation-bound capture. SQL projections never authorize removal.
use crate::{access_grant::AccessGrantClient, request_owner::InvitationRequestClient};
use ghinvite_core::{
    access_grant::{
        AccessGrantCommand, RemovalBarrierQuery, accepted_organization, accepted_repository,
    },
    delivery_control::DeliveryKind,
    invitation_link::LinkSnapshot,
    request_lifecycle::InitializeRequest,
    revocation::{AccessRemovalPreview, PrepareAccessRemoval, RequestCascadeQuery, RevokeCommand},
};
use restate_sdk::{
    context::{ContextClient, ContextReadState, ContextWriteState, ObjectContext},
    errors::TerminalError,
    serde::Json,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Serialize, Deserialize)]
struct Capture {
    input: PrepareAccessRemoval,
    preview: AccessRemovalPreview,
}
fn key(input: &PrepareAccessRemoval) -> String {
    format!(
        "access_removal/{}",
        String::from(input.operation_id.clone())
    )
}
fn conflict() -> TerminalError {
    TerminalError::new_with_code(409, "access removal preview conflict")
}
/// A fresh preview of a revoked link changes nothing; its status tells it
/// apart from a conflicting operation identity.
fn revoked() -> TerminalError {
    TerminalError::new_with_code(
        ghinvite_core::invitation_link::LINK_REVOKED_STATUS,
        "link revoked",
    )
}
fn unavailable() -> TerminalError {
    TerminalError::new_with_code(503, "access removal preview unavailable")
}
pub(crate) fn query(command: &AccessGrantCommand) -> RemovalBarrierQuery {
    RemovalBarrierQuery {
        request_id: command.request_id,
        target: command.target.clone(),
        delivery_generation: command.delivery_generation,
    }
}
pub(crate) async fn retained(
    ctx: &ObjectContext<'_>,
    input: &RevokeCommand,
) -> Result<AccessRemovalPreview, TerminalError> {
    let wanted = PrepareAccessRemoval {
        link_id: input.link_id,
        admin: input.admin.clone(),
        operation_id: input.operation_id.clone(),
    };
    let Some(Json(capture)) = ctx.get::<Json<Capture>>(&key(&wanted)).await? else {
        return Err(unavailable());
    };
    if capture.input != wanted {
        return Err(conflict());
    }
    Ok(capture.preview)
}
pub(crate) async fn prepare(
    ctx: &ObjectContext<'_>,
    input: PrepareAccessRemoval,
    link: &LinkSnapshot,
) -> Result<AccessRemovalPreview, TerminalError> {
    if let Some(Json(capture)) = ctx.get::<Json<Capture>>(&key(&input)).await? {
        return if capture.input == input {
            Ok(capture.preview)
        } else {
            Err(conflict())
        };
    }
    if link.revoked_at.is_some() {
        return Err(revoked());
    }
    let captured_at = crate::clock::now(ctx, "access_preview_time").await?;
    let mut grants = BTreeMap::new();
    let mut keys = ctx.get_keys().await?;
    keys.sort();
    for entry in keys.into_iter().filter(|k| k.starts_with("admission/")) {
        let Some(Json(initialized)) = ctx.get::<Json<InitializeRequest>>(&entry).await? else {
            continue;
        };
        let request_id = initialized.request.request_id;
        let Json(manifest) = ctx
            .object_client::<InvitationRequestClient>(request_id.to_string())
            .cascade_manifest(Json(RequestCascadeQuery {
                link_id: input.link_id,
                request_id,
            }))
            .call()
            .await?;
        if manifest.request_id != request_id {
            return Err(conflict());
        }
        // The cascade manifest can retain an older submitted generation beside
        // the current reissue plan. Only the highest generation is a candidate.
        let mut current = BTreeMap::new();
        for identity in manifest.deliveries {
            if identity.link_id != Some(input.link_id) || identity.request_id != request_id {
                return Err(conflict());
            }
            let kind = match identity.kind {
                DeliveryKind::Repository => 0,
                DeliveryKind::Organization => 1,
            };
            let slot = current
                .entry((kind, identity.target_id))
                .or_insert(identity.clone());
            if slot.delivery_generation < identity.delivery_generation {
                *slot = identity;
            }
        }
        for identity in current.into_values() {
            let delivery_key = ghinvite_core::delivery::delivery_key(
                request_id,
                identity.target_id,
                identity.delivery_generation,
            );
            let accepted = match identity.kind {
                DeliveryKind::Repository => {
                    let Some(snapshot) = ctx
                        .object_client::<crate::delivery::RepositoryDeliveryClient>(&delivery_key)
                        .status()
                        .call()
                        .await?
                        .0
                    else {
                        continue;
                    };
                    crate::delivery::validate_notice_snapshot(&delivery_key, &snapshot)?;
                    accepted_repository(&snapshot)
                        .map_err(|_| conflict())?
                        .into_iter()
                        .collect::<Vec<_>>()
                }
                DeliveryKind::Organization => {
                    let Some(snapshot) = ctx
                        .object_client::<crate::org_delivery::OrganizationDeliveryClient>(
                            &delivery_key,
                        )
                        .status()
                        .call()
                        .await?
                        .0
                    else {
                        continue;
                    };
                    if snapshot.create.command.delivery_key() != delivery_key {
                        return Err(conflict());
                    }
                    accepted_organization(&snapshot).map_err(|_| conflict())?
                }
            };
            for grant in accepted {
                if grant.link_id != Some(input.link_id)
                    || grant.request_id != request_id
                    || grant.account_id != link.creation.account_id
                    || grant.delivery_generation != identity.delivery_generation
                {
                    return Err(conflict());
                }
                let owner = ctx.object_client::<AccessGrantClient>(grant.key());
                // Registration is acknowledged before the read. An absent row
                // then means an exact old-generation tombstone suppressed it.
                owner.schedule(Json(grant.clone())).call().await?;
                let Some(snapshot) = owner.status(Json(query(&grant))).call().await?.0 else {
                    continue;
                };
                if snapshot.command != grant {
                    return Err(conflict());
                }
                if !snapshot.outcome.terminal() {
                    grants.insert(grant.storage_id(), grant);
                }
            }
        }
    }
    let preview = AccessRemovalPreview {
        captured_at,
        grants: grants.into_values().collect(),
    };
    ctx.set(
        &key(&input),
        Json(Capture {
            input,
            preview: preview.clone(),
        }),
    );
    Ok(preview)
}
