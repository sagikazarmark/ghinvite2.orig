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
