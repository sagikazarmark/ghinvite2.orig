import { test, expect } from '@playwright/test';

// The fixture (crates/ghinvite-web/tests/emulator_browser.rs) points GitHub's
// bases at an octoemu emulator. Nothing on the GitHub side is simulated by
// the browser test: the emulator serves the authorize page, checks PKCE and
// the callback, and answers /user and the installation reads.
const emulator = async request => (await (await request.get('/fixture-github')).text()).trim();

test('sign-in goes through the emulator authorize page and back', async ({ page, request }) => {
  const github = await emulator(request);
  await page.goto('/login?return_to=%2Fconsole');
  await expect(page).toHaveURL(new RegExp(`^${github}/login/oauth/authorize\\?`));
  await page.getByRole('button', { name: 'Authorize as octocat' }).click();
  await expect(page).toHaveURL(/^http:\/\/127\.0\.0\.1:4176\/console/);
  await expect(page.getByText('@octocat').first()).toBeVisible();
});

test('a form posted from an expired session bounces through the emulator sign-in', async ({ page, request, context }) => {
  const github = await emulator(request);
  const csp = [];
  page.on('console', message => { if (/Content Security Policy/i.test(message.text())) csp.push(message.text()); });
  await page.goto('/login?return_to=%2Fconsole%2Faccounts%2Foctocat%2Flinks%2Fnew');
  await page.getByRole('button', { name: 'Authorize as octocat' }).click();
  await expect(page).toHaveURL('/console/accounts/octocat/links/new');
  const policy = (await page.request.get('/console/accounts/octocat/links/new')).headers()['content-security-policy'];
  expect(policy).toContain(`form-action 'self' ${github}`);
  expect(policy).not.toContain('https://github.com');
  // The session ends while the form is open; submitting it must reach the
  // emulator's authorize page (303 → /login → 303 → emulator) instead of
  // being blocked by form-action.
  await context.clearCookies();
  await page.getByLabel('Description', { exact: false }).first().fill('Expired session');
  await page.locator('form[method="post"] button[type="submit"]').last().click();
  await expect(page).toHaveURL(new RegExp(`^${github}/login/oauth/authorize\\?`));
  await page.getByRole('button', { name: 'Authorize as octocat' }).click();
  await expect(page).toHaveURL(/^http:\/\/127\.0\.0\.1:4176\/console\/accounts\/octocat\/links/);
  expect(csp).toEqual([]);
});
