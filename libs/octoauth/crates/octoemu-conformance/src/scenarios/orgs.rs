//! Organization invitations, memberships and teams.

use super::{BoxFuture, clear_organization, clear_organization_boxed, then_cleanup};
use crate::ctx::{Ctx, Req, Stop, fail, skip};
use serde_json::json;

fn invite_body(ctx: &Ctx) -> serde_json::Value {
    json!({"invitee_id": ctx.world().known.invitee_id, "role": "direct_member"})
}

async fn accept_steps(ctx: &mut Ctx) -> Result<(), Stop> {
    let world = ctx.world().clone();
    let org = format!("/orgs/{}", world.org);
    let membership = format!("{org}/memberships/{}", world.invitee);
    let token = ctx.token().await?;
    let invited = ctx
        .send(
            "invite",
            Req::post(format!("{org}/invitations"))
                .bearer(&token)
                .json(invite_body(ctx)),
        )
        .await?;
    if invited.status != 201 {
        return fail(format!("invite: expected 201, got {}", invited.status));
    }
    ctx.send("membership-pending", Req::get(&membership).bearer(&token))
        .await?;
    let pat = world.invitee_token.clone();
    let own = format!("/user/memberships/orgs/{}", world.org);
    ctx.send("invitee-membership", Req::get(&own).bearer(&pat))
        .await?;
    ctx.send(
        "accept",
        Req::patch(&own)
            .bearer(&pat)
            .json(json!({"state": "active"})),
    )
    .await?;
    ctx.send("membership-active", Req::get(&membership).bearer(&token))
        .await?;
    ctx.send("remove", Req::delete(&membership).bearer(&token))
        .await?;
    ctx.revoke(&token).await;
    Ok(())
}

pub(super) fn membership_accept(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        clear_organization(ctx).await?;
        let steps = accept_steps(ctx).await;
        then_cleanup(ctx, steps, clear_organization_boxed).await
    })
}

async fn limit_steps(ctx: &mut Ctx) -> Result<(), Stop> {
    let world = ctx.world().clone();
    let org = format!("/orgs/{}", world.org);
    let token = ctx.token().await?;
    let mut accepted = 0;
    for attempt in 0..600 {
        let req = Req::post(format!("{org}/invitations"))
            .bearer(&token)
            .json(invite_body(ctx));
        let line = ctx.request_line(&req);
        let reply = if attempt == 0 {
            ctx.send("first-invite", req).await?
        } else {
            ctx.call(req).await?
        };
        if let (201, Some(id)) = (reply.status, reply.u64("id")) {
            accepted += 1;
            ctx.call(Req::delete(format!("{org}/invitations/{id}")).bearer(&token))
                .await?;
            continue;
        }
        ctx.record_reply("refused", line, &reply);
        ctx.observe("count", json!({"invitations_before_refusal": accepted}));
        ctx.revoke(&token).await;
        return Ok(());
    }
    ctx.revoke(&token).await;
    fail(format!("{accepted} invitations, no refusal"))
}

pub(super) fn invitation_limit(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        clear_organization(ctx).await?;
        let steps = limit_steps(ctx).await;
        then_cleanup(ctx, steps, clear_organization_boxed).await
    })
}

pub(super) fn spammed(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        if ctx.is_live() {
            return skip(
                "GitHub flags an organization as spammy on its own and no public route sets it; \
                 a destructive run that meets it records it in org-invitation-limit",
            );
        }
        let org = format!("/orgs/{}", ctx.world().org);
        let token = ctx.token().await?;
        ctx.send(
            "invite",
            Req::post(format!("{org}/invitations"))
                .bearer(&token)
                .json(invite_body(ctx)),
        )
        .await?;
        ctx.revoke(&token).await;
        Ok(())
    })
}

pub(super) fn no_seat(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        let Some(full) = ctx.world().no_seat.clone() else {
            return skip(
                "needs a paid organization with every seat filled and the App installed: \
                 set OCTOEMU_LIVE_NO_SEAT_INSTALLATION_ID and OCTOEMU_LIVE_NO_SEAT_ORG",
            );
        };
        let token = ctx.token_for(full.installation_id).await?;
        let org = format!("/orgs/{}", full.account);
        let reply = ctx
            .send(
                "invite",
                Req::post(format!("{org}/invitations"))
                    .bearer(&token)
                    .json(invite_body(ctx)),
            )
            .await?;
        if let (201, Some(id)) = (reply.status, reply.u64("id")) {
            ctx.call(Req::delete(format!("{org}/invitations/{id}")).bearer(&token))
                .await?;
        }
        ctx.revoke(&token).await;
        Ok(())
    })
}

async fn team_steps(ctx: &mut Ctx) -> Result<(), Stop> {
    let world = ctx.world().clone();
    let org = format!("/orgs/{}", world.org);
    let team_membership = format!("{org}/teams/{}/memberships/{}", world.team, world.invitee);
    let token = ctx.token().await?;
    ctx.send(
        "put",
        Req::put(&team_membership)
            .bearer(&token)
            .json(json!({"role": "member"})),
    )
    .await?;
    ctx.send("get", Req::get(&team_membership).bearer(&token))
        .await?;
    ctx.send(
        "org-invitations",
        Req::get(format!("{org}/invitations")).bearer(&token),
    )
    .await?;
    ctx.send(
        "org-membership",
        Req::get(format!("{org}/memberships/{}", world.invitee)).bearer(&token),
    )
    .await?;
    ctx.send(
        "remove-team-membership",
        Req::delete(&team_membership).bearer(&token),
    )
    .await?;
    ctx.send(
        "org-invitations-after",
        Req::get(format!("{org}/invitations")).bearer(&token),
    )
    .await?;
    ctx.revoke(&token).await;
    Ok(())
}

pub(super) fn team_put_non_member(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        clear_organization(ctx).await?;
        let steps = team_steps(ctx).await;
        then_cleanup(ctx, steps, clear_organization_boxed).await
    })
}
