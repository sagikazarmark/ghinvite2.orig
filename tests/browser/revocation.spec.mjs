import { test, expect } from '@playwright/test';

async function openActiveLink(page, enhanced = true) {
  await page.goto('/fixture-login');
  await page.getByLabel('Description', { exact: true }).fill('Keyboard confirmation');
  await page.getByLabel('octocat/api', { exact: true }).check();
  if (enhanced) await expect(page.getByRole('status').filter({ hasText: 'Access checked' })).toBeVisible();
  await page.getByRole('button', { name: 'Create invitation link' }).click();
  await page.getByRole('button', { name: 'Retry original attempt' }).click();
  await expect(page.getByRole('heading', { name: 'Keyboard confirmation' })).toBeVisible();
}

async function tabTo(page, target) {
  for (let i = 0; i < 30; i++) {
    await page.keyboard.press('Tab');
    if (await target.evaluate(element => element === document.activeElement)) return;
  }
  await expect(target).toBeFocused();
}

test('keyboard opens a protected confirmation page and cancellation preserves the active link', async ({ page }) => {
  await openActiveLink(page);
  const detail = page.url();
  await tabTo(page, page.getByRole('link', { name: 'Stop accepting new requests' }));
  await page.keyboard.press('Enter');
  await expect(page.getByRole('heading', { name: 'Revoke invitation link' })).toBeVisible();
  await expect(page.getByText('Revoking stops new requests permanently.', { exact: false })).toBeVisible();
  await expect(page.getByRole('checkbox', { name: /Cancel .* pending GitHub/ })).toBeDisabled();
  await tabTo(page, page.getByRole('link', { name: 'Cancel', exact: true }));
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(detail);
  await expect(page.getByText('active', { exact: true })).toBeVisible();
});

test('a bookmarked confirmation and reload perform no mutation', async ({ page }) => {
  await openActiveLink(page);
  const detail = page.url();
  await page.goto(`${detail}/revoke`);
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Revoke invitation link' })).toBeVisible();
  await page.getByRole('link', { name: 'Cancel', exact: true }).click();
  await expect(page).toHaveURL(detail);
  await expect(page.getByText('active', { exact: true })).toBeVisible();
});

test('cascade confirmation refuses missing text and preserves the operation and selection', async ({ page }) => {
  await openActiveLink(page);
  await page.getByRole('link', { name: 'Stop accepting new requests' }).click();
  const operation = await page.locator('input[name="operation_id"]').inputValue();
  await page.getByRole('checkbox', { name: /Decline .* pending requests/ }).check();
  await page.getByRole('button', { name: 'Confirm revoke' }).click();
  await expect(page.getByRole('alert')).toContainText('Type REVOKE');
  await expect(page.locator('input[name="operation_id"]')).toHaveValue(operation);
  await expect(page.getByRole('checkbox', { name: /Decline .* pending requests/ })).toBeChecked();
  await page.getByRole('link', { name: 'Cancel', exact: true }).click();
  await expect(page.getByText('active', { exact: true })).toBeVisible();
});

for (const javaScriptEnabled of [true, false]) {
  test.describe(javaScriptEnabled ? 'enhanced form' : 'native fallback form', () => {
    test.use({ javaScriptEnabled });

    test('keyboard confirmation submits one native CSRF-protected POST', async ({ page }) => {
      await openActiveLink(page, javaScriptEnabled);
      const detail = page.url();
      // Exercise the real HTTP boundary: a forged request must not revoke.
      const rejected = await page.request.post(`${detail}/revoke`, {
        form: { csrf_token: 'invalid' },
      });
      expect(rejected.status()).toBe(403);
      await page.reload();
      const opener = page.getByText('Stop accepting new requests', { exact: true });
      await tabTo(page, opener);
      await page.keyboard.press('Enter');
      const confirmation = page.locator('form').filter({ has: page.getByRole('button', { name: 'Confirm revoke' }) });
      const operation = await confirmation.locator('input[name="operation_id"]').inputValue();
      const token = await confirmation.locator('input[name="csrf_token"]').inputValue();
      expect(token).not.toBe('');
      await tabTo(page, confirmation.getByRole('button', { name: 'Confirm revoke' }));
      const posts = [];
      page.on('request', request => {
        if (request.url() === `${detail}/revoke` && request.method() === 'POST') posts.push(request);
      });
      await page.keyboard.press('Enter');
      // The shared transport fixture commits but drops the first acknowledgement.
      await expect(page.getByRole('status')).toContainText('Outcome unknown');
      expect(posts).toHaveLength(1);
      expect(posts[0].isNavigationRequest()).toBe(true);
      expect(new URLSearchParams(posts[0].postData()).getAll('operation_id')).toEqual([operation]);
      expect(new URLSearchParams(posts[0].postData()).getAll('csrf_token')).toEqual([token]);
      await page.getByRole('button', { name: 'Retry original attempt' }).click();
      await expect(page).toHaveURL(detail);
      await expect(page.getByText('This invitation link is no longer accepting invitation requests.')).toBeVisible();
      await expect(page.getByText('Stop accepting new requests', { exact: true })).toHaveCount(0);
    });
  });
}
