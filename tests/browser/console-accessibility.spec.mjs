import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';

const ROOT = '/console/accounts/octocat';
const DETAIL = `${ROOT}/links/01JQRFRM000000000000000000`;
const PAGES = [
  ['overview', ROOT],
  ['links', `${ROOT}/links`],
  ['request queue', `${ROOT}/requests`],
  ['detail', DETAIL],
  ['edit', `${DETAIL}/edit`],
  ['settings', `${ROOT}/settings`],
];

test.beforeEach(async ({ page }, testInfo) => {
  await page.addInitScript((theme) => {
    localStorage.setItem('ghinvite-theme', theme);
  }, testInfo.project.name === 'dark' ? 'ghinvite-dark' : 'ghinvite');
  await page.goto('/fixture-login');
});

for (const [label, path] of PAGES) {
  test(`${label}: full accessibility scan including text contrast`, async ({ page }) => {
    for (const width of [1440, 320]) {
      await page.setViewportSize({ width, height: 1000 });
      await page.goto(path);
      const results = await new AxeBuilder({ page }).analyze();
      expect(results.violations.map(({ id, nodes }) => ({
        id, nodes: nodes.map(({ html, failureSummary }) => ({ html, failureSummary })),
      }))).toEqual([]);
      await expect(page.getByRole('navigation', { name: 'Global', exact: true })).toBeVisible();
      const navigation = page.getByRole('navigation', { name: 'Console', exact: true });
      await expect(navigation).toBeVisible();
      const current = navigation.locator('[aria-current="page"]');
      await expect(current).toHaveCount(1);
      await expect(current).toHaveCSS('text-decoration-line', 'underline');
    }
  });
}

// Check painted text, not just the document width: overflow-hidden panels can
// conceal text without widening the page. Vertical scrolling is permitted.
async function expectUnclippedText(locator, checkHeight = false) {
  const clipped = await locator.evaluateAll((elements, checkHeight) => elements.flatMap((element) => {
    const bounds = element.getBoundingClientRect();
    const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
    const failures = [];
    while (walker.nextNode()) {
      const node = walker.currentNode;
      if (!node.textContent.trim()) continue;
      const range = document.createRange();
      range.selectNodeContents(node);
      for (const rect of range.getClientRects()) {
        if (rect.width && (rect.left < bounds.left - 1 || rect.right > bounds.right + 1
          || (checkHeight && (rect.top < bounds.top - 1 || rect.bottom > bounds.bottom + 1)))) {
          failures.push(node.textContent);
          break;
        }
      }
    }
    return failures;
  }), checkHeight);
  expect(clipped).toEqual([]);
}

test('access-decision properties and queue content reflow at 320px', async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 640 });
  for (const path of [DETAIL, `${ROOT}/settings`, `${ROOT}/requests`]) {
    await page.goto(path);
    const panels = page.locator('main .mac-panel');
    await expect(panels.first()).toBeVisible();
    await expectUnclippedText(panels);
    if (path.endsWith('/requests')) {
      await expect(page.getByText('Decision deadline:', { exact: false })).toContainText('UTC');
      await expectUnclippedText(page.locator('main .badge'), true);
    }
    expect(await page.locator('main').evaluate((el) => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
  }
});

test('sidebar account actions remain reachable by keyboard in short windows', async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 240 });
  await page.goto(`${ROOT}/settings`);
  const account = page.getByRole('link', { name: '@ octocat' });
  const install = page.getByRole('link', { name: 'Install another account' });
  await page.locator('aside').hover();
  await page.mouse.wheel(0, 600);
  await expect(install).toBeInViewport({ ratio: 1 });
  // Tab from the last Console navigation item, without scrollIntoView rescuing
  // an otherwise unreachable control.
  await page.locator('aside').getByRole('link', { name: 'Settings', exact: true }).focus();
  await page.keyboard.press('Tab');
  await expect(account).toBeFocused();
  await expect(account).toBeInViewport({ ratio: 1 });
  await page.keyboard.press('Tab');
  await expect(install).toBeFocused();
  await expect(install).toBeInViewport({ ratio: 1 });
});

for (const [label, path] of PAGES) {
  test(`${label}: keyboard controls remain reachable in a 400% reflow viewport`, async ({ page }) => {
    // 1280 x 960 at 400% browser zoom has a 320 x 240 CSS-pixel viewport.
    await page.setViewportSize({ width: 320, height: 240 });
    await page.goto(path);
    const names = new Set();
    for (let i = 0; i < 50; i++) {
      await page.keyboard.press('Tab');
      const focused = page.locator(':focus');
      if (await focused.count() === 0) break;
      await expect(focused).toBeInViewport({ ratio: 1 });
      names.add((await focused.textContent()).trim());
    }
    expect(names.has('Account: octocat')).toBe(true);
    expect(names.has('Install another account')).toBe(true);
    expect(names.has('Recover attempts')).toBe(true);
  });
}
