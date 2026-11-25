//! Repository collaborators, invitations and the `member` webhook.

use super::{BoxFuture, clear_repository, clear_repository_boxed, repo_path, then_cleanup};
use crate::ctx::{Ctx, Req, Stop, fail, skip};
use chrono::DateTime;
use serde_json::{Value, json};
use std::time::Duration;

fn collaborator(ctx: &Ctx) -> String {
    format!("{}/collaborators/{}", repo_path(ctx), ctx.world().invitee)
}

fn invitation_id(reply: &crate::ctx::Reply, what: &str) -> Result<u64, Stop> {
    match (reply.status, reply.u64("id")) {
        (201, Some(id)) => Ok(id),
        (status, _) => fail(format!("{what}: expected a 201 invitation, got {status}")),
    }
}

async fn reput_steps(ctx: &mut Ctx) -> Result<(), Stop> {
    let token = ctx.token().await?;
    let put = collaborator(ctx);
    let repo = repo_path(ctx);
    let invited = ctx
        .send(
            "invite",
            Req::put(&put)
                .bearer(&token)
                .json(json!({"permission": "pull"})),
        )
        .await?;
    let id = invitation_id(&invited, "invite")?;
    ctx.send(
        "reput-same-role",
        Req::put(&put)
            .bearer(&token)
            .json(json!({"permission": "pull"})),
    )
    .await?;
    ctx.send(
        "reput-other-role",
        Req::put(&put)
            .bearer(&token)
            .json(json!({"permission": "push"})),
    )
    .await?;
    ctx.send(
        "list",
        Req::get(format!("{repo}/invitations")).bearer(&token),
    )
    .await?;
    let pat = ctx.world().invitee_token.clone();
    ctx.send(
        "invitee-list",
        Req::get("/user/repository_invitations").bearer(&pat),
    )
    .await?;
    ctx.send(
        "cancel",
        Req::delete(format!("{repo}/invitations/{id}")).bearer(&token),
    )
    .await?;
    ctx.send(
        "list-after-cancel",
        Req::get(format!("{repo}/invitations")).bearer(&token),
    )
    .await?;
    ctx.revoke(&token).await;
    Ok(())
}

pub(super) fn reput_pending_invitation(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        clear_repository(ctx).await?;
        let steps = reput_steps(ctx).await;
        then_cleanup(ctx, steps, clear_repository_boxed).await
    })
}

async fn expired_steps(ctx: &mut Ctx) -> Result<(), Stop> {
    let token = ctx.token().await?;
    let put = collaborator(ctx);
    let repo = repo_path(ctx);
    let invited = ctx
        .send(
            "invite",
            Req::put(&put)
                .bearer(&token)
                .json(json!({"permission": "push"})),
        )
        .await?;
    let id = invitation_id(&invited, "invite")?;
    // GitHub's invitations expire after 7 days.
    ctx.advance_clock(Duration::from_secs(7 * 24 * 3600 + 3600))
        .await?;
    // The token is long expired on the clock: a new one.
    let token = ctx.token().await?;
    ctx.send(
        "list",
        Req::get(format!("{repo}/invitations")).bearer(&token),
    )
    .await?;
    let pat = ctx.world().invitee_token.clone();
    ctx.send(
        "invitee-list",
        Req::get("/user/repository_invitations").bearer(&pat),
    )
    .await?;
    ctx.send(
        "invitee-accept",
        Req::patch(format!("/user/repository_invitations/{id}")).bearer(&pat),
    )
    .await?;
    ctx.send(
        "reput",
        Req::put(&put)
            .bearer(&token)
            .json(json!({"permission": "push"})),
    )
    .await?;
    ctx.send(
        "list-after-reput",
        Req::get(format!("{repo}/invitations")).bearer(&token),
    )
    .await?;
    ctx.revoke(&token).await;
    Ok(())
}

pub(super) fn expired_invitation(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        clear_repository(ctx).await?;
        let steps = expired_steps(ctx).await;
        then_cleanup(ctx, steps, clear_repository_boxed).await
    })
}

async fn limit_steps(ctx: &mut Ctx) -> Result<(), Stop> {
    let token = ctx.token().await?;
    let put = collaborator(ctx);
    let repo = repo_path(ctx);
    let mut accepted = 0;
    for attempt in 0..60 {
        let req = Req::put(&put)
            .bearer(&token)
            .json(json!({"permission": "pull"}));
        let line = ctx.request_line(&req);
        let reply = if attempt == 0 {
            ctx.send("first-invite", req).await?
        } else {
            ctx.call(req).await?
        };
        if let (201, Some(id)) = (reply.status, reply.u64("id")) {
            accepted += 1;
            ctx.call(Req::delete(format!("{repo}/invitations/{id}")).bearer(&token))
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
        clear_repository(ctx).await?;
        let steps = limit_steps(ctx).await;
        then_cleanup(ctx, steps, clear_repository_boxed).await
    })
}

pub(super) fn outside_collaborator_policy(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        let world = ctx.world().clone();
        let Some((policy, repo)) = world
            .policy
            .as_ref()
            .and_then(|policy| Some((policy, policy.repo.clone()?)))
        else {
            return skip(
                "needs an organization that lets only owners invite outside collaborators, \
                 with the App installed: set OCTOEMU_LIVE_POLICY_INSTALLATION_ID, \
                 OCTOEMU_LIVE_POLICY_ORG and OCTOEMU_LIVE_POLICY_REPO",
            );
        };
        let token = ctx.token_for(policy.installation_id).await?;
        let repo = format!("/repos/{}/{repo}", policy.account);
        let reply = ctx
            .send(
                "invite-outside-collaborator",
                Req::put(format!("{repo}/collaborators/{}", world.invitee))
                    .bearer(&token)
                    .json(json!({"permission": "pull"})),
            )
            .await?;
        // Should the policy not apply, take the invitation back.
        if let (201, Some(id)) = (reply.status, reply.u64("id")) {
            ctx.call(Req::delete(format!("{repo}/invitations/{id}")).bearer(&token))
                .await?;
        }
        ctx.revoke(&token).await;
        Ok(())
    })
}

/// The newest `member` `edited` delivery for the repository since `since`
/// (Unix time), polled for up to a minute.
async fn member_edited_delivery(ctx: &mut Ctx, since: i64) -> Result<Value, Stop> {
    let repo_id = ctx.world().known.repo_id;
    for _ in 0..30 {
        let jwt = ctx.app_jwt().await?;
        let list = ctx
            .call(Req::get("/app/hook/deliveries?per_page=50").bearer(&jwt))
            .await?;
        let found = list.body.as_array().into_iter().flatten().find(|delivery| {
            delivery["event"] == "member"
                && delivery["action"] == "edited"
                && delivery["repository_id"].as_u64() == Some(repo_id)
                && delivery["delivered_at"]
                    .as_str()
                    .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                    .is_some_and(|at| at.timestamp() >= since - 5)
        });
        if let Some(id) = found.and_then(|delivery| delivery["id"].as_u64()) {
            let delivery = ctx
                .call(Req::get(format!("/app/hook/deliveries/{id}")).bearer(&jwt))
                .await?;
            return Ok(delivery.body["request"]["payload"].clone());
        }
        ctx.pause(Duration::from_secs(2)).await;
    }
    skip(
        "no member.edited delivery within a minute: is the App's webhook active and \
         subscribed to member events?",
    )
}

async fn member_edited_steps(ctx: &mut Ctx) -> Result<(), Stop> {
    let since = ctx.server_time().await?;
    let token = ctx.token().await?;
    let put = collaborator(ctx);
    let invited = ctx
        .send(
            "invite",
            Req::put(&put)
                .bearer(&token)
                .json(json!({"permission": "pull"})),
        )
        .await?;
    let id = invitation_id(&invited, "invite")?;
    let pat = ctx.world().invitee_token.clone();
    ctx.send(
        "accept",
        Req::patch(format!("/user/repository_invitations/{id}")).bearer(&pat),
    )
    .await?;
    ctx.send(
        "change-role",
        Req::put(&put)
            .bearer(&token)
            .json(json!({"permission": "push"})),
    )
    .await?;
    let payload = member_edited_delivery(ctx, since).await?;
    ctx.observe("member-edited-payload", payload);
    ctx.send("remove", Req::delete(&put).bearer(&token)).await?;
    ctx.revoke(&token).await;
    Ok(())
}

pub(super) fn member_edited_changes(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        clear_repository(ctx).await?;
        let steps = member_edited_steps(ctx).await;
        then_cleanup(ctx, steps, clear_repository_boxed).await
    })
}
