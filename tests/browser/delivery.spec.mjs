import { test, expect } from '@playwright/test';

test('requester sees mixed delivery and later settlement with safe GitHub next steps', async ({ page, request }) => {
  await page.goto('/fixture-login');
  await expect(page.getByRole('heading', { name: 'Approved', exact: true })).toBeVisible();
  const deliveries = page.getByRole('list', { name: 'Repository delivery' });
  const row = name => deliveries.getByRole('listitem').filter({ has: page.getByRole('heading', { name: `acme/${name}`, exact: true }) });
  for (const [name, status] of [
    ['waiting', 'GitHub invitation created — awaiting acceptance'],
    ['collaborator', 'Already a collaborator'],
    ['blocked', 'Blocked — waiting for availability or identity verification'],
    ['unknown', 'GitHub outcome unknown — awaiting reconciliation'],
    ['failed', 'GitHub rejected delivery'],
    ['approved', 'Approved — awaiting dispatch'],
    ['planned', 'Planned — awaiting dispatch'],
    ['submitted', 'Submitted — awaiting GitHub confirmation'],
    ['missing', 'Delivery status unavailable'],
  ]) {
    await expect(row(name)).toContainText(status);
  }
  await expect(row('waiting').getByRole('link', { name: 'Accept on GitHub' })).toHaveAttribute('href', 'https://github.com/acme/waiting/invitations');
  await expect(row('waiting')).toContainText('signed in as @octocat');
  await expect(row('unknown').getByRole('link', { name: 'GitHub notifications' })).toHaveAttribute('href', 'https://github.com/notifications');
  await expect(row('missing')).toContainText('Missing status does not mean delivery failed');
  await expect(row('blocked')).toContainText('contact an account admin');
  await expect(row('approved')).toContainText('Wait, then check again');
  await expect(page.getByRole('button', { name: /resend|retry|submit/i })).toHaveCount(0);
  await expect(page.locator('body')).not.toContainText('private ');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);

  // Settle projected lifecycle while retaining the original create receipts.
  expect((await request.post('/fixture-settle')).status()).toBe(204);
  await page.getByRole('link', { name: 'Check again', exact: true }).click();
  for (const [name, status] of [
    ['accept', 'Repository access accepted'], ['decline', 'GitHub invitation declined'],
    ['cancel', 'GitHub invitation cancelled'], ['expire', 'GitHub invitation expired'],
  ]) {
    await expect(row(name)).toContainText(status);
    await expect(row(name).getByRole('link', { name: 'Accept on GitHub' })).toHaveCount(0);
  }
  await expect(row('accept').getByRole('link', { name: 'Open repository' })).toHaveAttribute('href', 'https://github.com/acme/accept');
  await expect(row('expire')).toContainText('If you still need access, contact an account admin');
  await expect(row('waiting').getByRole('link', { name: 'Accept on GitHub' })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Approved', exact: true })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});

test('lapsed requester submits a fresh native attempt and sees actual pending review', async ({ page }) => {
  await page.goto('/fixture-lapsed/available');
  await expect(page.getByRole('heading', { name: 'Invitation lapsed', exact: true })).toBeVisible();
  await expect(page.getByRole('list', { name: 'Repository delivery' })).toBeVisible();
  await expect(page.getByText(/original approval and recorded delivery history remain/)).toBeVisible();
  const form = page.locator('form[action^="/join?operation_id="]');
  const operation = await form.locator('[name=operation_id]').inputValue();
  expect(operation).toMatch(/^[0-9A-HJKMNP-TV-Z]{26}$/);
  const submitted = page.waitForRequest(request => request.method() === 'POST' && new URL(request.url()).pathname === '/join');
  await page.getByRole('button', { name: 'Request again', exact: true }).click();
  const request = await submitted;
  const values = new URLSearchParams(request.postData());
  expect(values.get('operation_id')).toBe(operation);
  expect(values.get('csrf_token')).toBeTruthy();
  await expect(page.getByRole('heading', { name: 'Awaiting review', exact: true })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Approved', exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Request again', exact: true })).toHaveCount(0);
});

test('lapsed requester sees the durable retry date without an unauthorized fresh form', async ({ page }) => {
  await page.goto('/fixture-lapsed/limited');
  await expect(page.getByRole('heading', { name: 'Invitation lapsed', exact: true })).toBeVisible();
  await expect(page.getByText('You can request again after 2099-01-02T03:04:05+00:00. The link must still accept requests then.', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Request again', exact: true })).toHaveCount(0);
  await expect(page.locator('form[action^="/join?operation_id="]')).toHaveCount(0);
  await expect(page.getByRole('list', { name: 'Repository delivery' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Refresh organization delivery' })).toHaveCount(0);
});

test('organization two-factor notice leaves requesting available and unknown stays neutral', async ({ page }) => {
  for (const requirement of ['required', 'not-required', 'unknown']) {
    await page.goto(`/fixture-two-factor/${requirement}`);
    const notice = page.getByText("This organization requires two-factor authentication; you'll need it to accept.", { exact: true });
    if (requirement === 'required') await expect(notice).toBeVisible();
    else await expect(notice).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Submit request', exact: true })).toBeEnabled();
    await expect(page.locator('form[action^="/join?operation_id="] [name=csrf_token]')).toHaveCount(1);
    await expect(page.getByText(/you have two-factor authentication enabled/i)).toHaveCount(0);
  }
});

test('owned request-ID status refreshes through the native form without a selected link', async ({ page }) => {
  await page.goto('/fixture-request-status');
  await expect(page).toHaveURL(/\/requests\/[0-9A-HJKMNP-TV-Z]{26}$/);
  await expect(page.getByRole('heading', { name: 'Request status', exact: true })).toBeVisible();
  await expect(page.locator('body')).not.toContainText('private ');
  await expect(page.getByRole('button', { name: 'Submit request', exact: true })).toHaveCount(0);
  const statusPath = new URL(page.url()).pathname;
  const form = page.locator('form').filter({ has: page.locator('[name=target][value="repo:5"]') });
  await expect(form).toHaveAttribute('action', `${statusPath}/refresh`);
  const token = await form.locator('[name=csrf_token]').inputValue();
  expect(token).toBeTruthy();
  const sent = page.waitForRequest(request => request.method() === 'POST' && new URL(request.url()).pathname === `${statusPath}/refresh`);
  await form.getByRole('button', { name: 'Refresh repository delivery', exact: true }).click();
  const values = new URLSearchParams((await sent).postData());
  expect(values.get('target')).toBe('repo:5');
  expect(values.get('csrf_token')).toBe(token);
  await expect(page.getByRole('status')).toHaveText('Refresh requested. Check again shortly for verified updates.');
  await page.getByRole('link', { name: 'Check again', exact: true }).click();
  await expect(page).toHaveURL(new RegExp(`${statusPath}$`));
  await expect(page.getByRole('heading', { name: 'Request status', exact: true })).toBeVisible();
  await expect(page.getByRole('list', { name: 'Repository delivery' })).toContainText('GitHub invitation created');
  await expect(page.locator('body')).not.toContainText('private ');
});


test('Sent expiry uses retained source clocks and missing history stays unavailable', async ({ page }) => {
  const before = Date.now();
  await page.goto('/fixture-expiry');
  const rows = page.getByRole('list', { name: 'Repository delivery' });
  const row = name => rows.getByRole('listitem').filter({ has: page.getByRole('heading', { name: `acme/${name}`, exact: true }) });
  const own = row('waiting');
  const foreign = row('accept');
  await expect(own).toContainText('GitHub invitation created — awaiting acceptance');
  await expect(foreign).toContainText('Existing GitHub invitation — awaiting acceptance');
  for (const [target, seconds] of [[own, 3600], [foreign, 5 * 86400]]) {
    const clock = target.locator('time[datetime]');
    await expect(clock).toHaveCount(1);
    const expiry = Date.parse(await clock.getAttribute('datetime'));
    expect(expiry).toBeGreaterThanOrEqual(before + seconds * 1000);
    expect(expiry).toBeLessThanOrEqual(Date.now() + seconds * 1000);
    await expect(target).toContainText('passing it does not confirm expiry or cancellation');
  }
  await expect(own.getByRole('link', { name: 'Accept on GitHub' })).toBeVisible();
  await expect(foreign).toContainText('Check your GitHub notifications or invitation email');
  await expect(row('decline')).toContainText('Invitation expiry is unavailable because verified timing was not recorded.');
  await expect(row('decline').locator('time')).toHaveCount(0);
  await expect(row('unknown').locator('time')).toHaveCount(0);
  await expect(page.getByRole('button', { name: /resend|cancel|retry/i })).toHaveCount(0);
  await page.getByRole('link', { name: 'Check again', exact: true }).click();
  await expect(own.locator('time')).toHaveCount(1);
  await expect(row('decline')).toContainText('Invitation expiry is unavailable');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});
