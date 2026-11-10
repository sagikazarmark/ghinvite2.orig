import { test, expect } from '@playwright/test';

for (const javaScriptEnabled of [false, true]) {
  test.describe(javaScriptEnabled ? 'enhanced page' : 'native forms', () => {
    test.use({ javaScriptEnabled });
    test('manual pause and resume preserve operation identity and existing request history', async ({ page }) => {
      await page.goto('/fixture-login');
      await page.getByLabel('Description', { exact: true }).fill('Pause control');
      await page.getByLabel('octocat/api', { exact: true }).check();
      await page.getByRole('button', { name: 'Create invitation link' }).click();
      await page.getByRole('button', { name: 'Retry original attempt' }).click();
      const detail = page.url();
      const pause = page.locator('form[action$="/pause"]');
      const operation = await pause.locator('[name="operation_id"]').inputValue();
      const csrf = await pause.locator('[name="csrf_token"]').inputValue();
      const forged = await page.request.post(`${detail}/pause`, { form: { operation_id: operation, csrf_token: 'invalid' } });
      expect(forged.status()).toBe(403);
      await page.getByRole('button', { name: 'Pause invitation link', exact: true }).click();
      await expect(page).toHaveURL(detail);
      await expect(page.getByText('paused', { exact: true })).toBeVisible();
      await expect(page.getByText('Existing requests and GitHub invitations continue.', { exact: true })).toBeVisible();
      await expect(page.getByRole('link', { name: 'Request history' })).toBeVisible();
      await page.reload();
      await page.getByRole('button', { name: 'Resume invitation link', exact: true }).click();
      await expect(page.getByText('active', { exact: true })).toBeVisible();
      // Replaying the original pause confirms its old receipt, without undoing resume.
      const replay = await page.request.post(`${detail}/pause`, { form: { operation_id: operation, csrf_token: csrf } });
      expect(replay.status()).toBe(200);
      await page.reload();
      await expect(page.getByText('active', { exact: true })).toBeVisible();
      const changed = await page.request.post(`${detail}/resume`, { form: { operation_id: operation, csrf_token: csrf } });
      expect(changed.status()).toBe(409);
    });
  });
}
