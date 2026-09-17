import { test, expect } from '@playwright/test';

test('native submission survives acknowledgement loss, revocation, two tabs and delayed projection', async ({ page, context, request }) => {
  await page.goto('/fixture-login');
  const original = await page.locator('[name="operation_id"]').inputValue();
  const second = await context.newPage();
  await second.goto('/i/abcdEFGH01234567');
  expect(await second.locator('[name="operation_id"]').inputValue()).not.toBe(original);
  await page.getByLabel('Justification').fill('  original context  ');
  await page.getByRole('button', { name: 'Submit request' }).click();
  await expect(page.getByRole('status')).toContainText('Outcome unknown');
  await expect(page.getByLabel('Justification')).toHaveValue('original context');
  await expect(page.getByLabel('Justification')).toHaveAttribute('readonly');
  await page.getByRole('button', { name: 'Retry same attempt' }).click();
  await expect(page.getByRole('status')).toContainText('Request accepted at');
  await expect(page).toHaveURL(new RegExp(`operation_id=${original}`));
  await page.reload();
  await expect(page.getByRole('status')).toContainText('Request accepted at');
  await page.goto('/');
  await page.goto('/i/abcdEFGH01234567');
  await expect(page.getByRole('status')).toContainText('Request accepted at');
  await expect(page.getByRole('link', { name: 'Start a fresh attempt with edited input' })).toHaveCount(0);
  await expect(page.locator('meta[http-equiv="refresh"]')).toHaveAttribute('content', '20');
  await request.post('/fixture-state?revoked=true');
  // Replaying the original native POST recovers acceptance despite revocation.
  const replay = await page.request.post(`/i/abcdEFGH01234567?operation_id=${original}`, {
    form: { csrf_token: await page.locator('[name="csrf_token"]').first().inputValue(), operation_id: original, justification: 'original context' },
  });
  expect(await replay.text()).toContain('Request accepted at');
  await page.reload();
  await expect(page.getByRole('status')).toContainText('Request accepted at');
  await request.post('/fixture-state?status=declined&revoked=false');
  // Native meta refresh observes the terminal authoritative state without SQL rows.
  await expect(page.getByText('Current request status: declined', { exact: true })).toBeVisible({ timeout: 25_000 });
  await expect(page.locator('meta[http-equiv="refresh"]')).toHaveCount(0);
  await page.getByRole('link', { name: 'Start a fresh attempt with edited input' }).click();
  await expect(page.getByRole('status')).toContainText('This is a fresh attempt');
  expect(await page.locator('[name="operation_id"]').inputValue()).not.toBe(original);
  await page.getByLabel('Justification').fill('edited context');
  await expect(page.getByRole('link', { name: 'Recover original attempt / Check again' }))
    .toHaveAttribute('href', `/i/abcdEFGH01234567?operation_id=${original}`);
  await page.getByRole('link', { name: 'Recover original attempt / Check again' }).click();
  await expect(page.getByRole('status')).toContainText('Request accepted at');
});

test('active confirmation and inactive fresh visits use the shared product surface', async ({ page, request }) => {
  await page.goto('/fixture-login');
  await expect(page.getByText('Signed in as', { exact: false })).toContainText('@octocat');
  await expect(page.getByText('Permission: Read (pull)')).toBeVisible();
  await expect(page.getByText('acme/api', { exact: true })).toBeVisible();
  await expect(page.getByText('Account admins review your request before access is approved.')).toBeVisible();
  await expect(page.getByLabel('Justification')).toHaveAttribute('aria-describedby', 'justification-help');
  await request.post('/fixture-state?revoked=true');
  const response = await page.reload();
  expect(response.status()).toBe(404);
  await expect(page.getByText('The link may be incorrect or no longer available.')).toBeVisible();
  await expect(page.getByText('acme/api', { exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Submit request' })).toHaveCount(0);
  const unknown = await page.goto('/i/not-a-code');
  expect(unknown.status()).toBe(404);
  await expect(page.getByText('The link may be incorrect or no longer available.')).toBeVisible();
});

test('oversized justification can be corrected and submitted under the same operation', async ({ page }) => {
  await page.goto('/fixture-login');
  const id = await page.locator('[name="operation_id"]').inputValue();
  const oversized = 'é'.repeat(8193);
  await page.getByLabel('Justification').fill(oversized);
  await page.getByRole('button', { name: 'Submit request' }).click();
  await expect(page.getByLabel('Justification')).toHaveValue(oversized);
  await expect(page.getByLabel('Justification')).toHaveAttribute('aria-invalid', 'true');
  await expect(page.getByText('Shorten your justification', { exact: false })).toBeVisible();
  await expect(page.locator('[name="operation_id"]')).toHaveValue(id);
  await page.getByLabel('Justification').fill('corrected');
  await page.getByRole('button', { name: 'Submit request' }).click();
  await expect(page.getByRole('status')).toContainText('Outcome unknown');
  await expect(page.locator('[name="operation_id"]')).toHaveValue(id);
  await page.getByRole('button', { name: 'Retry same attempt' }).click();
  await expect(page.getByRole('status')).toContainText('Request accepted at');
});

test('wrong-account escape signs out and OAuth returns to the intended invitation', async ({ page }) => {
  await page.goto('/fixture-login');
  await page.getByRole('button', { name: 'Submit request' }).click();
  await page.getByRole('button', { name: 'Retry same attempt' }).click();
  await expect(page.getByRole('status')).toContainText('Request accepted at');
  // Only the external GitHub authorization page is simulated; both local OAuth
  // endpoints and session rotation run through the production router.
  // Intercept the initial POST because redirect-chain requests do not get
  // separate Playwright route callbacks. Execute the local chain unchanged.
  await page.route('**/logout', async route => {
    const response = await route.fetch({ maxRedirects: 0 });
    expect(response.status()).toBe(303);
    expect(response.headers().location).toBe('/i/abcdEFGH01234567');
    const invitation = await page.request.get(response.headers().location, { maxRedirects: 0 });
    expect(invitation.headers().location).toBe('/login?return_to=%2Fi%2FabcdEFGH01234567');
    const login = await page.request.get(invitation.headers().location, { maxRedirects: 0 });
    const state = new URL(login.headers().location).searchParams.get('state');
    await route.fulfill({ status: 200, contentType: 'text/html', body: `<a href="/oauth/callback?code=other-code&state=${state}">Continue as othercat</a>` });
  });
  await page.getByRole('button', { name: 'Sign out and sign in again.' }).click();
  await page.getByRole('link', { name: 'Continue as othercat' }).click();
  await expect(page).toHaveURL('/i/abcdEFGH01234567');
  await expect(page.getByText('Signed in as', { exact: false })).toContainText('@othercat');
  await page.getByRole('button', { name: 'Submit request' }).click();
  await expect(page.getByRole('status')).toContainText('Outcome unknown');
  await page.getByRole('button', { name: 'Retry same attempt' }).click();
  await expect(page.getByRole('status')).toContainText('Request accepted at');
});
