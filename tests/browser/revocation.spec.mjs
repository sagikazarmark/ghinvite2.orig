import { test, expect } from '@playwright/test';

async function openActiveLink(page) {
  await page.goto('/fixture-login');
  await page.getByLabel('Description', { exact: true }).fill('Keyboard confirmation');
  await page.getByLabel('acme/api', { exact: true }).check();
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

test('keyboard opening names the confirmation and focuses the safe action', async ({ page }) => {
  await openActiveLink(page);
  const opener = page.getByText('Stop accepting new requests', { exact: true });
  await tabTo(page, opener);
  await page.keyboard.press('Enter');
  const dialog = page.getByRole('dialog', { name: 'Confirm stop' });
  await expect(dialog).toBeVisible();
  await expect(dialog).toHaveAccessibleDescription('GitHub users will no longer be able to create invitation requests from this invitation link. Existing invitation requests and GitHub invitations continue.');
  await expect(dialog.getByText('Cancel', { exact: true })).toBeFocused();
});

test('modal contains forward and backward tabbing and restores focus after dismissal', async ({ page }) => {
  await openActiveLink(page);
  const opener = page.getByRole('button', { name: 'Stop accepting new requests' });
  await tabTo(page, opener);
  await page.keyboard.press('Enter');
  const dialog = page.getByRole('dialog', { name: 'Confirm stop' });
  const cancel = dialog.getByRole('button', { name: 'Cancel' });
  const confirm = dialog.getByRole('button', { name: 'Confirm stop' });
  await page.keyboard.press('Shift+Tab');
  await expect(confirm).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(cancel).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(confirm).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(cancel).toBeFocused();
  // Even an attempted programmatic focus move cannot reach the inert page.
  await page.getByRole('link', { name: 'Edit details' }).evaluate(element => element.focus());
  await expect(cancel).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(dialog).not.toBeVisible();
  await expect(opener).toBeFocused();
  await page.keyboard.press('Space');
  await expect(cancel).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(dialog).not.toBeVisible();
  await expect(opener).toBeFocused();
  await expect(page.getByText('active', { exact: true })).toBeVisible();
});

test('a bookmarked confirmation can be dismissed and reopened', async ({ page }) => {
  await openActiveLink(page);
  await page.goto(`${page.url()}#stop-link-confirmation`);
  await page.reload();
  const dialog = page.getByRole('dialog', { name: 'Confirm stop' });
  await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(dialog).not.toBeVisible();
  await expect(page.getByRole('button', { name: 'Stop accepting new requests' })).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused();
});

test.describe('without JavaScript', () => {
  test.use({ javaScriptEnabled: false });

  test('inline confirmation supports keyboard navigation and cancellation without claiming modality', async ({ page }) => {
    await openActiveLink(page);
    const opener = page.getByRole('link', { name: 'Stop accepting new requests' });
    const confirmation = page.getByRole('region', { name: 'Confirm stop' });
    await expect(confirmation).not.toBeVisible();
    await tabTo(page, opener);
    await page.keyboard.press('Enter');
    await expect(confirmation).toBeVisible();
    await expect(confirmation).toBeFocused();
    await expect(confirmation).toHaveAccessibleDescription(/Existing invitation requests and GitHub invitations continue/);
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await expect(page.locator('[aria-modal="true"]')).toHaveCount(0);
    await page.keyboard.press('Tab');
    const cancel = confirmation.getByRole('link', { name: 'Cancel' });
    await expect(cancel).toBeFocused();
    await page.keyboard.press('Tab');
    await expect(confirmation.getByRole('button', { name: 'Confirm stop' })).toBeFocused();
    await page.keyboard.press('Shift+Tab');
    await expect(cancel).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(confirmation).not.toBeVisible();
    await expect(opener).toBeFocused();
  });
});

for (const javaScriptEnabled of [true, false]) {
  test.describe(javaScriptEnabled ? 'enhanced form' : 'native fallback form', () => {
    test.use({ javaScriptEnabled });

    test('keyboard confirmation submits one native CSRF-protected POST', async ({ page }) => {
      await openActiveLink(page);
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
      const confirmation = page.getByRole(javaScriptEnabled ? 'dialog' : 'region', { name: 'Confirm stop' });
      const token = await confirmation.locator('input[name="csrf_token"]').inputValue();
      expect(token).not.toBe('');
      await tabTo(page, confirmation.getByRole('button', { name: 'Confirm stop' }));
      const posts = [];
      page.on('request', request => {
        if (request.url() === `${detail}/revoke` && request.method() === 'POST') posts.push(request);
      });
      await page.keyboard.press('Enter');
      // The shared transport fixture commits but drops the first acknowledgement.
      await expect(page.getByRole('status')).toContainText('Outcome unknown');
      expect(posts).toHaveLength(1);
      expect(posts[0].isNavigationRequest()).toBe(true);
      expect(new URLSearchParams(posts[0].postData()).getAll('csrf_token')).toEqual([token]);
      await page.getByRole('button', { name: 'Retry original attempt' }).click();
      await expect(page).toHaveURL(detail);
      await expect(page.getByText('This invitation link is no longer accepting invitation requests.')).toBeVisible();
      await expect(page.getByText('Stop accepting new requests', { exact: true })).toHaveCount(0);
    });
  });
}
