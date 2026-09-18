import { test, expect } from '@playwright/test';

test('repository outage preserves native form and reloads selections before creation', async ({ page, request }) => {
  await page.goto('/fixture-login');
  await page.getByLabel('Description', { exact: true }).fill('Recovery workshop');
  await page.getByLabel('Internal note', { exact: true }).fill('Keep this note');
  await page.getByLabel('Permission level', { exact: true }).selectOption('push');
  await page.getByLabel('Require account admin approval before GitHub invitations are sent').check();
  await page.getByLabel('Max use', { exact: true }).fill('7');
  await page.getByLabel('Expires in days', { exact: true }).fill('9');
  await page.getByLabel('acme/api', { exact: true }).check();
  await page.getByLabel('acme/web', { exact: true }).check();
  await request.post('/fixture-status/504');
  const failed = page.waitForResponse(response => response.request().method() === 'POST');
  await page.getByRole('button', { name: 'Create invitation link', exact: true }).click();
  expect((await failed).status()).toBe(504);
  await expect(page.getByRole('alert')).toContainText('GitHub did not respond in time');
  await expect(page.getByText('No repositories are available')).toHaveCount(0);
  await expect(page.getByLabel('Description', { exact: true })).toHaveValue('Recovery workshop');
  await expect(page.getByLabel('Internal note', { exact: true })).toHaveValue('Keep this note');
  await request.post('/fixture-status/200');
  await page.getByRole('button', { name: 'Retry loading repositories', exact: true }).click();
  await expect(page.getByLabel('acme/api', { exact: true })).toBeChecked();
  await expect(page.getByLabel('acme/web', { exact: true })).toBeChecked();
  await expect(page.getByLabel('Permission level', { exact: true })).toHaveValue('push');
  await expect(page.getByLabel('Max use', { exact: true })).toHaveValue('7');
  await expect(page.getByLabel('Expires in days', { exact: true })).toHaveValue('9');
  await expect(page.getByLabel('Require account admin approval before GitHub invitations are sent')).toBeChecked();
  await expect(page.getByRole('button', { name: 'Create invitation link', exact: true })).toBeVisible();

  await request.post('/fixture-status/503');
  expect((await page.goto('/console/accounts/octocat/settings')).status()).toBe(502);
  await expect(page.getByRole('alert')).toContainText('Installation availability could not be verified');
  await request.post('/fixture-status/200');
  await page.getByRole('link', { name: 'Try again', exact: true }).click();
  await expect(page.getByText('Installed. Repository access verified with GitHub.')).toBeVisible();
});
