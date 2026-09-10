import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';

const action = '/console/accounts/acme/links';
const form = (page) => page.locator('#link-form-island form');
const submit = (page) => page.getByRole('button', { name: 'Create invitation link', exact: true });
const approval = (page) => page.getByRole('checkbox', { name: /Require account admin approval/ });

test.beforeEach(async ({ page }, testInfo) => {
  // The app uses a stored preference, not prefers-color-scheme, for its theme.
  await page.addInitScript((theme) => localStorage.setItem('ghinvite-theme', theme),
    testInfo.project.use.colorScheme === 'dark' ? 'ghinvite-dark' : 'ghinvite');
});

async function open(page, path = '/', mounted = true) {
  const response = await page.goto(path);
  expect(response.status()).toBe(200);
  expect(response.headers()['content-security-policy']).toContain("script-src 'self' 'wasm-unsafe-eval'");
  if (mounted) {
    await expect(page.locator('#link-form-island')).toHaveAttribute('data-island', 'mounted');
  } else {
    await expect(page.locator('#link-form-island')).not.toHaveAttribute('data-island', 'mounted');
  }
  await expect(form(page)).toHaveCount(1);
  await expect(submit(page)).toBeVisible();
  await expect(form(page)).toHaveAttribute('method', 'post');
  await expect(form(page)).toHaveAttribute('action', action);
}

async function post(page) {
  const responsePromise = page.waitForResponse((response) =>
    new URL(response.url()).pathname === action && response.request().method() === 'POST');
  await submit(page).click();
  const response = await responsePromise;
  expect(response.request().isNavigationRequest()).toBe(true);
  expect(response.request().resourceType()).toBe('document');
  expect(response.request().headers()['content-type']).toContain('application/x-www-form-urlencoded');
  expect(response.status()).toBe(200);
  await expect(page).toHaveURL(new RegExp(`${action}$`));
  return new URLSearchParams((await response.json()).entries);
}

test('mounts the real bundle under CSP without runtime errors or missing assets', async ({ page }) => {
  const errors = [];
  const assets = [];
  page.on('pageerror', (error) => errors.push(error.message));
  page.on('console', (message) => {
    if (message.type() === 'error') errors.push(message.text());
  });
  page.on('response', (response) => {
    if (/\/(assets|static)\//.test(response.url())) assets.push(response);
  });
  await page.addInitScript(() => {
    window.cspViolations = [];
    document.addEventListener('securitypolicyviolation', (event) => {
      window.cspViolations.push(`${event.effectiveDirective}: ${event.blockedURI}`);
    });
  });
  await open(page);
  await expect(page.getByLabel('Description', { exact: true })).toHaveValue('');
  await expect(page.getByRole('checkbox', { name: 'acme/api', exact: true })).not.toBeChecked();
  expect(assets.some((response) => response.url().endsWith('.wasm'))).toBe(true);
  expect(assets.some((response) => response.url().endsWith('/static/styles.css'))).toBe(true);
  expect(assets.every((response) => response.ok())).toBe(true);
  expect(await page.evaluate(() => window.cspViolations)).toEqual([]);
  expect(errors).toEqual([]);
});

test('description validates on focus exit, even unchanged, but not during typing', async ({ page }) => {
  await open(page);
  const description = page.locator('#description');
  await description.focus();
  await page.keyboard.press('Tab');
  await expect(page.locator('#description-error')).toContainText('Description is required.');
  await description.fill('Workshop');
  await page.keyboard.press('Tab');
  await expect(page.locator('#description-error')).toBeEmpty();
  await open(page);
  await description.fill('Temporary');
  await description.fill('');
  // An untouched field does not show an error while typing, even if emptied.
  await expect(page.locator('#description-error')).toBeEmpty();
  await page.keyboard.press('Tab');
  await expect(page.locator('#description-error')).toContainText('Description is required.');
});

test('description metadata and color follow correct-invalid-correct without duplicate associations', async ({ page }) => {
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  page.on('console', (message) => {
    if (message.type() === 'error') errors.push(message.text());
  });
  await open(page);
  const description = page.getByRole('textbox', { name: 'Description', exact: true });
  const error = page.locator('#description-error');
  for (const [value, invalid] of [['Workshop', false], ['   ', true], ['Corrected workshop', false]]) {
    await description.fill(value);
    await page.keyboard.press('Tab');
    await expect(description).toHaveValue(value);
    await expect(description).toHaveAttribute('id', 'description');
    await expect(description).toHaveAttribute('name', 'description');
    await expect(description).toHaveJSProperty('required', true);
    await expect(description).toHaveAttribute('aria-invalid', String(invalid));
    await expect(description).toHaveAttribute('aria-labelledby', 'description-label');
    await expect(description).toHaveAccessibleName('Description');
    await expect(description).not.toHaveAttribute('aria-label');
    await expect(description).toHaveAttribute('aria-describedby',
      invalid ? 'description-help description-error' : 'description-help');
    await expect(page.locator('#description-label')).toHaveAttribute('for', 'description');
    await expect(page.locator('p#description-help')).toContainText('Visible only to admins.');
    await expect(error).toHaveAttribute('aria-live', 'polite');
    await expect(error).not.toHaveAttribute('hidden');
    if (invalid) {
      await expect(description).toHaveAttribute('aria-errormessage', 'description-error');
      await expect(description).toHaveClass(/\binput-error\b/);
      await expect(error.locator(':scope > div')).toHaveCount(1);
      await expect(error).toContainText('Description is required.');
    } else {
      await expect(description).not.toHaveAttribute('aria-errormessage');
      await expect(description).not.toHaveClass(/\binput-error\b/);
      await expect(error).toBeEmpty();
    }
    for (const id of ['description', 'description-label', 'description-help', 'description-error']) {
      await expect(page.locator(`[id="${id}"]`)).toHaveCount(1);
    }
    await expect(page.locator('[name="description"]')).toHaveCount(1);
  }
  expect(errors).toEqual([]);
});

test('native required and minimum constraints block before the submit handler', async ({ page }) => {
  await open(page);
  const posts = [];
  page.on('request', (request) => { if (request.method() === 'POST') posts.push(request.url()); });
  await submit(page).click();
  await expect(page.locator('#description')).toBeFocused();
  expect(await page.locator('#description').evaluate((input) => input.validity.valueMissing)).toBe(true);
  await page.locator('#description').fill('Workshop');
  await page.getByRole('checkbox', { name: 'acme/api', exact: true }).check();
  await page.locator('#max_uses').fill('0');
  await submit(page).click();
  await expect(page.locator('#max_uses')).toBeFocused();
  expect(await page.locator('#max_uses').evaluate((input) => input.validity.rangeUnderflow)).toBe(true);
  expect(posts).toEqual([]);
  await expect(page).toHaveURL('/');
});

test('dioform blocks native-valid whitespace and missing scope, then allows a native POST', async ({ page }) => {
  await open(page);
  const posts = [];
  page.on('request', (request) => { if (request.method() === 'POST') posts.push(request.url()); });
  // Whitespace satisfies HTML required, but fails the shared domain validator.
  await page.locator('#description').fill('   ');
  expect(await form(page).evaluate((element) => element.checkValidity())).toBe(true);
  await submit(page).click();
  await expect(page.locator('#description-error')).toBeVisible();
  await expect(page.locator('#repo_ids-error')).toBeVisible();
  await expect(page.locator('#link-form-errors')).toHaveAttribute('role', 'alert');
  expect(posts).toEqual([]);
  await expect(page).toHaveURL('/');
  await page.locator('#description').fill('Workshop & onboarding');
  await page.locator('#internal_note').fill('Keep <literal> text');
  await page.locator('#permission').selectOption('push');
  await page.locator('#max_uses').fill('7');
  await page.locator('#expires_in_days').fill('45');
  await page.getByRole('checkbox', { name: 'acme/api', exact: true }).check();
  await expect(page.locator('#repo_ids-error')).toHaveCount(0);
  const data = await post(page);
  expect(Object.fromEntries(data)).toEqual({
    description: 'Workshop & onboarding', internal_note: 'Keep <literal> text',
    permission: 'push', max_uses: '7', expires_in_days: '45', repo_ids: '10',
  });
  expect(posts).toHaveLength(1);
});

for (const mode of ['mounted', 'no-js', 'blocked-bundle']) {
  test.describe(mode, () => {
    if (mode === 'no-js') test.use({ javaScriptEnabled: false });
    test.beforeEach(async ({ page }) => {
      if (mode === 'blocked-bundle') await page.route('**/assets/**', (route) => route.abort('blockedbyclient'));
    });

    test('preserves failed values and error associations; correction submits the same values', async ({ page }) => {
      await open(page, '/preserved', mode === 'mounted');
      await expect(page.locator('#description')).toHaveValue('   ');
      await expect(page.locator('#permission')).toHaveValue('push');
      await expect(page.locator('#max_uses')).toHaveValue('7');
      await expect(page.locator('#expires_in_days')).toHaveValue('45');
      await expect(page.locator('#internal_note')).toHaveValue('Keep this note & its <literal> markup');
      await expect(approval(page)).toBeChecked();
      await expect(page.getByRole('checkbox', { name: 'acme/web', exact: true })).toBeChecked();
      await expect(page.getByRole('checkbox', { name: 'acme/docs', exact: true })).toBeChecked();
      await expect(page.locator('#description')).toHaveAttribute('aria-invalid', 'true');
      await expect(page.getByLabel('Description', { exact: true })).toHaveCount(1);
      await expect(page.locator('#description-label')).toHaveAttribute('for', 'description');
      await expect(page.locator('#description')).toHaveAttribute('aria-labelledby', 'description-label');
      await expect(page.locator('#description')).toHaveAttribute('aria-describedby', 'description-help description-error');
      await expect(page.locator('#description')).toHaveAttribute('aria-errormessage', 'description-error');
      await expect(page.locator('#description-error')).toHaveAttribute('aria-live', 'polite');
      await expect(page.locator('#description-error')).toContainText('Description is required.');
      await expect(page.locator('#link-form-errors')).toHaveAttribute('role', 'alert');
      await page.locator('#description').fill('Corrected workshop');
      const data = await post(page);
      expect(data.get('description')).toBe('Corrected workshop');
      expect(data.get('permission')).toBe('push');
      expect(data.get('max_uses')).toBe('7');
      expect(data.get('expires_in_days')).toBe('45');
      expect(data.get('internal_note')).toBe('Keep this note & its <literal> markup');
      expect(data.get('approval_required')).toBe('true');
      expect(data.getAll('repo_ids')).toEqual(['11', '12']);
    });

    test('all server errors survive, including browser-sanitized invalid numbers', async ({ page }) => {
      await open(page, '/failed', mode === 'mounted');
      await expect(page.locator('#description')).toHaveValue('   ');
      await expect(page.locator('#internal_note')).toHaveValue('Keep this note');
      // Chromium cannot display "abc" in a number input. The props and error
      // preserve the failed submission; expecting the DOM value "abc" is wrong.
      await expect(page.locator('#max_uses')).toHaveValue('');
      await expect(page.locator('#expires_in_days')).toHaveValue('0');
      const props = JSON.parse(await page.locator('#link-form-props').textContent());
      expect(props.values.max_uses).toBe('abc');
      for (const id of ['description', 'permission', 'max_uses', 'expires_in_days']) {
        await expect(page.locator(`#${id}`)).toHaveAttribute('aria-invalid', 'true');
        await expect(page.locator(`#${id}`)).toHaveAttribute('aria-describedby', `${id}-help ${id}-error`);
        await expect(page.locator(`#${id}-error`)).toBeVisible();
      }
      await expect(page.locator('#repo_ids')).toHaveAttribute('aria-describedby', 'repo_ids-help repo_ids-error');
      await expect(page.locator('#repo_ids-error')).toBeVisible();
      await expect(page.locator('[name="repo_ids"]:checked')).toHaveCount(0);
    });

    test('checkboxes retain label/Space behavior and native successful-control serialization', async ({ page }) => {
      await open(page, '/', mode === 'mounted');
      await page.locator('#description').fill('Checkbox workshop');
      const api = page.getByRole('checkbox', { name: 'acme/api', exact: true });
      await page.getByText('acme/api', { exact: true }).click();
      await expect(api).toBeChecked();
      await api.focus();
      await page.keyboard.press('Space');
      await expect(api).not.toBeChecked();
      await page.keyboard.press('Space');
      await expect(api).toBeChecked();
      await page.getByRole('checkbox', { name: 'acme/docs', exact: true }).check();
      await approval(page).check();
      await approval(page).focus();
      await page.keyboard.press('Space');
      await expect(approval(page)).not.toBeChecked();
      // Empty optional numbers stay empty rather than becoming zero/defaults.
      await page.locator('#expires_in_days').fill('');
      const data = await post(page);
      expect(data.has('approval_required')).toBe(false);
      expect(data.getAll('repo_ids')).toEqual(['10', '12']);
      expect(data.get('max_uses')).toBe('');
      expect(data.get('expires_in_days')).toBe('');
    });
  });
}

test('responsive themed form keeps controls reachable and accessibility wiring intact', async ({ page }, testInfo) => {
  await open(page, '/preserved');
  const dark = testInfo.project.use.colorScheme === 'dark';
  await expect(page.locator('body')).toHaveAttribute('data-theme', dark ? 'ghinvite-dark' : 'ghinvite');
  await expect(page.locator('body')).toHaveCSS('color-scheme', dark ? 'dark' : 'light');
  const mobile = testInfo.project.name.startsWith('mobile');
  await expect(page.locator('.console-sidebar')).toBeVisible({ visible: !mobile });
  await expect(page.locator('.mobile-console-nav')).toBeVisible({ visible: mobile });
  for (const selector of ['body', 'main', '#link-form-island']) {
    expect.soft(await page.locator(selector).evaluate((element) => element.scrollWidth - element.clientWidth),
      `${selector} must not overflow horizontally`).toBeLessThanOrEqual(1);
  }
  for (const control of await form(page).locator('input, textarea, select, button[type="submit"]').all()) {
    await control.scrollIntoViewIfNeeded();
    await expect(control).toBeInViewport();
    await expect(control).toHaveAccessibleName(/\S/);
    const box = await control.boundingBox();
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(testInfo.project.use.viewport.width + 1);
  }
  await page.locator('#description').focus();
  await page.keyboard.press('Tab');
  await expect(page.locator('#internal_note')).toBeFocused();
  // Scope to the pilot's semantics, not unrelated console landmarks/contrast.
  const accessibility = await new AxeBuilder({ page }).include('#link-form-island')
    .withRules(['label', 'button-name', 'select-name', 'aria-valid-attr',
      'aria-valid-attr-value', 'aria-allowed-attr', 'aria-required-attr', 'duplicate-id-aria'])
    .analyze();
  expect(accessibility.violations).toEqual([]);
});
