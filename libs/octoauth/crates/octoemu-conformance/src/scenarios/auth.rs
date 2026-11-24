//! App and installation auth, the API version header and the secondary
//! rate limit.

use super::{BoxFuture, repo_path};
use crate::ctx::{Ctx, Reply, Req, Stop, fail, skip};
use crate::target::UNREGISTERED_PRIVATE_KEY;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use std::time::Duration;

const NO_SUCH_CLIENT_ID: &str = "Iv23.octoemu-no-such-app";

fn rs256() -> Value {
    json!({"alg": "RS256", "typ": "JWT"})
}

pub(super) fn jwt_error_matrix(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        let now = ctx.server_time().await?;
        let iss = ctx.iss();
        let good = json!({"iat": now - 60, "exp": now + 540, "iss": iss});
        let valid = ctx.app_jwt_with(&good)?;
        ctx.send("valid", Req::get("/app").bearer(&valid)).await?;
        ctx.send("no-credential", Req::get("/app")).await?;
        ctx.send("not-a-jwt", Req::get("/app").bearer("not-a-jwt"))
            .await?;
        let unregistered = Ctx::sign_with(UNREGISTERED_PRIVATE_KEY, &rs256(), &good)?;
        ctx.send("unregistered-key", Req::get("/app").bearer(unregistered))
            .await?;
        let unsigned = format!(
            "{}.{}.",
            URL_SAFE_NO_PAD.encode(json!({"alg": "none", "typ": "JWT"}).to_string()),
            URL_SAFE_NO_PAD.encode(good.to_string())
        );
        ctx.send("alg-none", Req::get("/app").bearer(unsigned))
            .await?;
        let cases = [
            (
                "unknown-iss",
                json!({"iat": now - 60, "exp": now + 540, "iss": NO_SUCH_CLIENT_ID}),
            ),
            ("missing-iat", json!({"exp": now + 540, "iss": iss})),
            ("missing-exp", json!({"iat": now - 60, "iss": iss})),
            (
                "string-iat",
                json!({"iat": (now - 60).to_string(), "exp": now + 540, "iss": iss}),
            ),
            (
                "expired",
                json!({"iat": now - 700, "exp": now - 60, "iss": iss}),
            ),
            (
                "exp-too-far",
                json!({"iat": now - 60, "exp": now + 1200, "iss": iss}),
            ),
            // exp within 10 minutes of now, but 660 s after iat.
            (
                "exp-660s-after-iat",
                json!({"iat": now - 120, "exp": now + 540, "iss": iss}),
            ),
        ];
        for (name, claims) in cases {
            let jwt = ctx.app_jwt_with(&claims)?;
            ctx.send(name, Req::get("/app").bearer(jwt)).await?;
        }
        ctx.send("token-scheme", Req::get("/app").token(&valid))
            .await?;
        let token = ctx.token().await?;
        ctx.send(
            "installation-token-on-app-route",
            Req::get("/app").bearer(&token),
        )
        .await?;
        let repo = repo_path(ctx);
        ctx.send("jwt-on-installation-route", Req::get(repo).bearer(&valid))
            .await?;
        ctx.revoke(&token).await;
        Ok(())
    })
}

pub(super) fn jwt_skew_tolerance(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        let now = ctx.server_time().await?;
        let iss = ctx.iss();
        for ahead in [0, 10, 25, 45, 60, 300] {
            let claims = json!({"iat": now + ahead, "exp": now + 540, "iss": iss});
            let jwt = ctx.app_jwt_with(&claims)?;
            ctx.send(&format!("iat-ahead-{ahead}s"), Req::get("/app").bearer(jwt))
                .await?;
        }
        for ahead in [600, 630] {
            let claims = json!({"iat": now, "exp": now + ahead, "iss": iss});
            let jwt = ctx.app_jwt_with(&claims)?;
            ctx.send(&format!("exp-ahead-{ahead}s"), Req::get("/app").bearer(jwt))
                .await?;
        }
        Ok(())
    })
}

pub(super) fn token_downscoping_errors(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        let world = ctx.world().clone();
        let jwt = ctx.app_jwt().await?;
        let mint = format!("/app/installations/{}/access_tokens", world.installation_id);
        let too_many: Vec<u64> = (1..=501).collect();
        let cases = [
            (
                "narrow-permissions",
                json!({"permissions": {"members": "read"}}),
            ),
            (
                "ungranted-permission",
                json!({"permissions": {"issues": "write"}}),
            ),
            (
                "higher-level",
                json!({"permissions": {"metadata": "write"}}),
            ),
            (
                "unknown-level",
                json!({"permissions": {"members": "admin"}}),
            ),
            ("narrow-repository", json!({"repositories": [world.repo]})),
            (
                "unknown-repository",
                json!({"repositories": ["octoemu-no-such-repository"]}),
            ),
            (
                "repositories-and-ids",
                json!({"repositories": [world.repo], "repository_ids": [world.known.repo_id]}),
            ),
            (
                "too-many-repository-ids",
                json!({"repository_ids": too_many}),
            ),
        ];
        for (name, body) in cases {
            let reply = ctx
                .send(name, Req::post(&mint).bearer(&jwt).json(body))
                .await?;
            if let Some(token) = reply.str("token") {
                let token = token.to_owned();
                ctx.revoke(&token).await;
            }
        }
        ctx.send(
            "unknown-installation",
            Req::post("/app/installations/1/access_tokens").bearer(&jwt),
        )
        .await?;
        Ok(())
    })
}

/// What the token looks like, without the token: GitHub's stateless
/// `ghs_<APPID>_<JWT>` format.
fn token_shape(token: &str, app_id: u64) -> Value {
    let mut parts = token.splitn(3, '_');
    let prefix = parts.next().unwrap_or("");
    let second = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("");
    json!({
        "prefix": format!("{prefix}_"),
        "second_segment_is_app_id": second == app_id.to_string(),
        "rest_is_a_jwt": rest.split('.').count() == 3,
        "length_at_least_500": token.len() >= 500,
    })
}

pub(super) fn token_revocation_delete(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        let world = ctx.world().clone();
        let jwt = ctx.app_jwt().await?;
        let minted = ctx
            .send(
                "mint",
                Req::post(format!(
                    "/app/installations/{}/access_tokens",
                    world.installation_id
                ))
                .bearer(&jwt),
            )
            .await?;
        let Some(token) = minted.str("token").map(str::to_owned) else {
            return fail(format!("no token minted: {}", minted.status));
        };
        ctx.observe("token-shape", token_shape(&token, world.app.id));
        let repo = repo_path(ctx);
        ctx.send("use", Req::get(&repo).bearer(&token)).await?;
        ctx.send("revoke", Req::delete("/installation/token").bearer(&token))
            .await?;
        // GitHub's revocation takes a moment to reach every route: a use
        // right after the DELETE is sometimes accepted, so the scenario
        // records how soon it is refused, then a use once it is.
        let refused_after = refused_within(ctx, &repo, &token, Duration::from_secs(5)).await?;
        ctx.observe(
            "revocation-propagates",
            json!({"refused_within_2s": refused_after.is_some_and(|at| at <= Duration::from_secs(2))}),
        );
        ctx.send("use-after-revoke", Req::get(&repo).bearer(&token))
            .await?;
        ctx.send(
            "revoke-again",
            Req::delete("/installation/token").bearer(&token),
        )
        .await?;
        Ok(())
    })
}

/// How long after now `GET path` with `token` is first refused (polled
/// every 100 ms, unrecorded); `None` when it is still accepted after
/// `limit`.
async fn refused_within(
    ctx: &mut Ctx,
    path: &str,
    token: &str,
    limit: Duration,
) -> Result<Option<Duration>, Stop> {
    let started = std::time::Instant::now();
    while started.elapsed() <= limit {
        let reply = ctx.call(Req::get(path).bearer(token)).await?;
        if reply.status != 200 {
            return Ok(Some(started.elapsed()));
        }
        ctx.pause(Duration::from_millis(100)).await;
    }
    Ok(None)
}

pub(super) fn token_revocation_uninstall(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        let world = ctx.world().clone();
        let token = ctx.token().await?;
        let repo = repo_path(ctx);
        ctx.send("use", Req::get(&repo).bearer(&token)).await?;
        let jwt = ctx.app_jwt().await?;
        let installation = format!("/app/installations/{}", world.installation_id);
        ctx.send("uninstall", Req::delete(&installation).bearer(&jwt))
            .await?;
        ctx.send("use-after-uninstall", Req::get(&repo).bearer(&token))
            .await?;
        ctx.send("installation-after", Req::get(&installation).bearer(&jwt))
            .await?;
        ctx.send(
            "mint-after",
            Req::post(format!("{installation}/access_tokens")).bearer(&jwt),
        )
        .await?;
        Ok(())
    })
}

async fn suspended_steps(ctx: &mut Ctx, jwt: &str, token: &str) -> Result<(), Stop> {
    let world = ctx.world().clone();
    let installation = format!("/app/installations/{}", world.installation_id);
    let repo = repo_path(ctx);
    ctx.send("use-before", Req::get(&repo).bearer(token))
        .await?;
    ctx.send(
        "suspend",
        Req::put(format!("{installation}/suspended")).bearer(jwt),
    )
    .await?;
    ctx.send("installation", Req::get(&installation).bearer(jwt))
        .await?;
    ctx.send("use-while-suspended", Req::get(&repo).bearer(token))
        .await?;
    ctx.send(
        "mint-while-suspended",
        Req::post(format!("{installation}/access_tokens")).bearer(jwt),
    )
    .await?;
    ctx.send(
        "suspend-again",
        Req::put(format!("{installation}/suspended")).bearer(jwt),
    )
    .await?;
    ctx.send(
        "unsuspend",
        Req::delete(format!("{installation}/suspended")).bearer(jwt),
    )
    .await?;
    ctx.send("installation-after", Req::get(&installation).bearer(jwt))
        .await?;
    ctx.send("use-after-unsuspend", Req::get(&repo).bearer(token))
        .await?;
    Ok(())
}

pub(super) fn suspended_installation(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        let token = ctx.token().await?;
        let jwt = ctx.app_jwt().await?;
        let steps = suspended_steps(ctx, &jwt, &token).await;
        // Never leave the live installation suspended.
        let restored = unsuspend(ctx).await;
        ctx.revoke(&token).await;
        super::with_cleanup(steps, restored)
    })
}

/// Lifts the suspension (unrecorded), retrying, and checks that it is
/// lifted.
async fn unsuspend(ctx: &mut Ctx) -> Result<(), Stop> {
    let installation = format!("/app/installations/{}", ctx.world().installation_id);
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            ctx.pause(Duration::from_secs(2)).await;
        }
        let jwt = ctx.app_jwt().await?;
        let lifted = ctx
            .call(Req::delete(format!("{installation}/suspended")).bearer(&jwt))
            .await;
        let shown = ctx.call(Req::get(&installation).bearer(&jwt)).await;
        match (lifted, shown) {
            (Ok(lifted), Ok(shown))
                if lifted.status == 204
                    && shown.status == 200
                    && shown.body["suspended_at"].is_null() =>
            {
                return Ok(());
            }
            (Ok(lifted), Ok(shown)) => {
                last = format!(
                    "unsuspend {}, installation {} {}",
                    lifted.status, shown.status, shown.body["suspended_at"]
                );
            }
            (Err(Stop::Fail(error) | Stop::Skip(error)), _)
            | (_, Err(Stop::Fail(error) | Stop::Skip(error))) => last = error,
        }
    }
    fail(format!(
        "cleanup: the installation is still suspended ({last}): lift it by hand"
    ))
}

pub(super) fn api_version_unsupported(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        let jwt = ctx.app_jwt().await?;
        for (name, version) in [
            ("version-2022-11-28", "2022-11-28"),
            ("version-2026-03-10", "2026-03-10"),
            ("version-never-served", "2021-01-01"),
            ("version-future", "2099-01-01"),
            ("version-malformed", "latest"),
        ] {
            ctx.send(
                name,
                Req::get("/app")
                    .bearer(&jwt)
                    .header("x-github-api-version", version),
            )
            .await?;
        }
        let user = format!("/users/{}", ctx.world().invitee);
        ctx.send(
            "version-never-served-unauthenticated",
            Req::get(user).header("x-github-api-version", "2021-01-01"),
        )
        .await?;
        Ok(())
    })
}

pub(super) fn secondary_limit_shape(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        if !ctx.is_live() {
            return skip(
                "octoemu applies no secondary rate limit of its own (fault rules inject one), \
                 so there is no behaviour to compare",
            );
        }
        let token = ctx.token().await?;
        let repo = repo_path(ctx);
        let url = format!("{}{repo}", ctx.world().api_base);
        let line = ctx.request_line(&Req::get(&repo));
        let mut limited: Option<Reply> = None;
        let mut sent = 0;
        // Bursts of concurrent reads: GitHub's documented secondary limit
        // starts at 100 concurrent requests.
        'rounds: for _ in 0..5 {
            let mut set = tokio::task::JoinSet::new();
            for _ in 0..150 {
                let request = ctx
                    .http()
                    .get(&url)
                    .bearer_auth(&token)
                    .header("accept", "application/vnd.github+json")
                    .header("x-github-api-version", crate::ctx::API_VERSION);
                set.spawn(async move {
                    match request.send().await {
                        Ok(response) => Reply::read(response).await.ok(),
                        Err(_) => None,
                    }
                });
            }
            while let Some(joined) = set.join_next().await {
                sent += 1;
                if let Ok(Some(reply)) = joined
                    && matches!(reply.status, 403 | 429)
                {
                    limited = Some(reply);
                    set.abort_all();
                    break 'rounds;
                }
            }
            ctx.pause(Duration::from_secs(1)).await;
        }
        ctx.revoke(&token).await;
        match limited {
            Some(reply) => {
                ctx.record_reply("limited", line, &reply);
                ctx.observe("burst", json!({"limited_within_750": sent <= 750}));
                Ok(())
            }
            None => skip(format!("{sent} concurrent reads hit no secondary limit")),
        }
    })
}
