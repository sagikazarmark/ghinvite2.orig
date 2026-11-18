//! The OAuth web flow's token endpoint and user access tokens.

use super::BoxFuture;
use crate::ctx::{Ctx, Req, skip};

const TOKEN: &str = "/login/oauth/access_token";

pub(super) fn exchange_errors(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        let client = ctx.world().signin.clone();
        let id = client.client_id.as_str();
        let secret = client.client_secret.as_str();
        // 20 characters, like a real code, that no authorization issued.
        let code = "0123456789abcdef0123";
        let refresh = format!("ghr_{}", "0".repeat(76));
        let cases: [(&str, Vec<(&str, &str)>); 5] = [
            (
                "bad-code",
                vec![("client_id", id), ("client_secret", secret), ("code", code)],
            ),
            (
                "bad-client-secret",
                vec![
                    ("client_id", id),
                    ("client_secret", "octoemu-wrong-secret"),
                    ("code", code),
                ],
            ),
            (
                "unknown-client",
                vec![
                    ("client_id", "Iv23.octoemu-no-such-client"),
                    ("client_secret", secret),
                    ("code", code),
                ],
            ),
            (
                "bad-refresh-token",
                vec![
                    ("client_id", id),
                    ("client_secret", secret),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &refresh),
                ],
            ),
            (
                "unsupported-grant-type",
                vec![
                    ("client_id", id),
                    ("client_secret", secret),
                    ("grant_type", "password"),
                    ("code", code),
                ],
            ),
        ];
        for (name, form) in cases {
            ctx.send(name, Req::post(TOKEN).web().form(&form)).await?;
        }
        // Without Accept: application/json the answer is form-encoded.
        ctx.send(
            "bad-code-form-encoded",
            Req::post(TOKEN).web().header("accept", "*/*").form(&[
                ("client_id", id),
                ("client_secret", secret),
                ("code", code),
            ]),
        )
        .await?;
        Ok(())
    })
}

pub(super) fn user_permissionless(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(async move {
        let Some(token) = ctx.world().user_token.clone() else {
            return skip(
                "a GitHub App user access token comes only from GitHub's consent screen: \
                 set OCTOEMU_LIVE_USER_TOKEN to the invitee's token of the sign-in App",
            );
        };
        ctx.send("user", Req::get("/user").bearer(&token)).await?;
        ctx.send("user-emails", Req::get("/user/emails").bearer(&token))
            .await?;
        let pat = ctx.world().invitee_token.clone();
        ctx.send("user-with-pat", Req::get("/user").bearer(pat))
            .await?;
        Ok(())
    })
}
