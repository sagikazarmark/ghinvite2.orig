import { readFileSync } from 'node:fs';

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
