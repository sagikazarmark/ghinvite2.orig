import { test, expect } from '@playwright/test';

test('investigate terminal requests from link details through bounded history', async ({ page }) => {
  await page.goto('/fixture-login');
  await page.getByRole('link', { name: 'Request history', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Request history', exact: true })).toBeVisible();
  const rows = page.getByRole('list', { name: 'Invitation request history' }).getByRole('listitem');
  await expect(rows).toHaveCount(25);
  await page.getByRole('link', { name: 'Older requests' }).click();
  await expect(rows).toHaveCount(3);
  await page.getByRole('link', { name: 'Latest requests' }).click();
  await rows.first().getByRole('link').click();
  await expect(page.getByRole('heading', { name: 'Invitation request', exact: true })).toBeVisible();
  await expect(page.getByText('State: declined', { exact: true })).toBeVisible();
  await expect(page.getByText('Immutable requested permission: pull')).toBeVisible();
  await expect(page.getByText('Decline reason (admin-only): Browser decision')).toBeVisible();
  await page.reload();
  await expect(page.getByText('State: declined', { exact: true })).toBeVisible();
  await expect(page.getByText(/Projected history — updates may be delayed or missing/)).toBeVisible();
});
