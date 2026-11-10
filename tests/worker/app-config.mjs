import { readFileSync } from 'node:fs';
import { Response } from 'miniflare';

// Explicit installed-app credentials for actual web startup, independent of OAuth.
export const installedWebApp = {
  GH_REPO_APP_ID: '123',
  GH_REPO_CLIENT_ID: 'Iv.fixture-repo',
  GH_REPO_PRIVATE_KEYS: `fixture:${readFileSync(new URL('../../test-fixtures/github-app-rsa.pem', import.meta.url)).toString('base64')}`,
  GH_REPO_WEBHOOK_SECRETS: 'fixture-repo-webhook-current;fixture-repo-webhook-previous',
};

// The sign-in app (typed, token-free sign-in is the only sign-in path).
export const signinApp = {
  GH_SIGNIN_CLIENT_ID: 'Iv.fixture-signin',
  GH_SIGNIN_CLIENT_SECRET: 'fixture-signin-secret',
};

// GitHub's revocation of a sign-in token: sign-in revokes every token it
// obtains, so outbound services answering the exchange and `/user` also
// answer this (204), or `null` for any other request.
export function signinRevocation(request) {
  const url = new URL(request.url);
  return request.method === 'DELETE' && url.origin === 'https://api.github.com'
    && decodeURIComponent(url.pathname) === `/applications/${signinApp.GH_SIGNIN_CLIENT_ID}/token`
    ? new Response(null, { status: 204 })
    : null;
}
