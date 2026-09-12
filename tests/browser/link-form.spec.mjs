import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';

const action = '/console/accounts/acme/links';
const form = (page) => page.locator('#link-form-island form');
const submit = (page) => page.getByRole('button', { name: 'Create invitation link', exact: true });
const approval = (page) => page.getByRole('checkbox', { name: /Require account admin approval/ });
const permissions = ['pull', 'triage', 'push', 'maintain', 'admin'];
const permissionError = 'Choose a supported permission level: pull, triage, push, maintain, or admin.';
const summaryMessage = 'Fix the highlighted fields before creating this invitation link.';
const descriptionError = 'Description is required. Use short, single-line admin-only context for this invitation link.';
// Synthetic transport messages: no production validator emits these.
const serverPermissionError = 'Transport fixture: server-only permission rejection.';
const serverFormError = 'Transport fixture: server-only form rejection.';
const numericFields = [
  {
    id: 'max_uses', label: 'Max use', initial: '', valid: '4294967295',
    help: 'Blank means unlimited invitation requests.',
    invalid: 'Max use must be a whole number of 1 or more.',
    overflow: 'Max use is too large. Use a smaller number, or leave it blank for unlimited invitation requests.',
  },
  {
    id: 'expires_in_days', label: 'Expires in days', initial: '30', valid: '45',
    help: 'Default is 30 days. Blank creates an invitation link with no expiration.',
    invalid: 'Expiration must be a whole number of days, 1 or more.',
    overflow: 'Expiration is too far in the future. Use fewer days, or leave it blank for no expiration.',
  },
];

async function expectNumeric(page, field, value, message = null) {
  const { id, label, help } = field;
  const control = page.getByRole('spinbutton', { name: label, exact: true });
  await expect(control).toHaveValue(value);
  await expect(control).toHaveAttribute('id', id);
  await expect(control).toHaveAttribute('name', id);
  await expect(control).toHaveAttribute('type', 'number');
  await expect(control).toHaveAttribute('min', '1');
  await expect(control).not.toHaveAttribute('max');
  await expect(control).toHaveJSProperty('required', false);
  await expect(control).toHaveAttribute('aria-labelledby', `${id}-label`);
  await expect(control).not.toHaveAttribute('aria-label');
  await expect(page.locator(`label#${id}-label`)).toHaveAttribute('for', id);
  await expect(page.locator(`p#${id}-help`)).toHaveText(help);
  await expect(control).toHaveAttribute('aria-invalid', String(message !== null));
  await expect(control).toHaveAttribute('aria-describedby',
    message === null ? `${id}-help` : `${id}-help ${id}-error`);
  const error = page.locator(`div#${id}-error`);
  await expect(error).toHaveAttribute('aria-live', 'polite');
  await expect(error).not.toHaveAttribute('hidden');
  if (message === null) {
    await expect(control).not.toHaveClass(/\binput-error\b/);
    await expect(control).not.toHaveAttribute('aria-errormessage');
    await expect(error).toBeEmpty();
  } else {
    await expect(control).toHaveClass(/\binput-error\b/);
    await expect(control).toHaveAttribute('aria-errormessage', `${id}-error`);
    await expect(error.locator(':scope > div')).toHaveCount(1);
    await expect(error).toHaveText(message);
  }
  for (const suffix of ['', '-label', '-help', '-error']) {
    await expect(page.locator(`[id="${id}${suffix}"]`)).toHaveCount(1);
  }
  await expect(page.locator(`[name="${id}"]`)).toHaveCount(1);
}

async function expectPermission(page, value, invalid, message = permissionError) {
  const control = page.getByRole('combobox', { name: 'Permission level', exact: true });
  await expect(control).toHaveValue(value);
  await expect(control).toHaveAttribute('id', 'permission');
  await expect(control).toHaveAttribute('name', 'permission');
  await expect(control).toHaveAttribute('aria-labelledby', 'permission-label');
  await expect(page.locator('#permission-label')).toHaveAttribute('for', 'permission');
  await expect(control).toHaveAttribute('aria-invalid', String(invalid));
  await expect(control).toHaveAttribute('aria-describedby',
    invalid ? 'permission-help permission-error' : 'permission-help');
  await expect(control.locator('option')).toHaveText(permissions);
  expect(await control.locator('option').evaluateAll((options) => options.map((option) => option.value)))
    .toEqual(permissions);
  await expect(control.locator('option:checked')).toHaveCount(1);
  await expect(control.locator('option:checked')).toHaveAttribute('value', value);
  await expect(control.locator('option[disabled], option[value=""]')).toHaveCount(0);
  const error = page.locator('#permission-error');
  await expect(error).toHaveAttribute('aria-live', 'polite');
  await expect(error).not.toHaveAttribute('hidden');
  if (invalid) {
    await expect(control).toHaveClass(/\bselect-error\b/);
    await expect(control).toHaveAttribute('aria-errormessage', 'permission-error');
    await expect(error.locator(':scope > div')).toHaveCount(1);
    await expect(error).toHaveText(message);
  } else {
    await expect(control).not.toHaveClass(/\bselect-error\b/);
    await expect(control).not.toHaveAttribute('aria-errormessage');
    await expect(error).toBeEmpty();
  }
  for (const id of ['permission', 'permission-label', 'permission-help', 'permission-error']) {
    await expect(page.locator(`[id="${id}"]`)).toHaveCount(1);
  }
  await expect(page.locator('[name="permission"]')).toHaveCount(1);
}

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
  const props = JSON.parse(await page.locator('#link-form-props').textContent());
  expect(props.csrf_token).toMatch(/^[A-Za-z0-9_-]{43}$/);
  await expect(form(page).locator('input[name="csrf_token"]')).toHaveValue(props.csrf_token);
}

async function post(page) {
  const csrfToken = await form(page).locator('input[name="csrf_token"]').inputValue();
  const responsePromise = page.waitForResponse((response) =>
    new URL(response.url()).pathname === action && response.request().method() === 'POST');
  await submit(page).click();
  const response = await responsePromise;
  expect(response.request().isNavigationRequest()).toBe(true);
  expect(response.request().resourceType()).toBe('document');
  expect(response.request().headers()['content-type']).toContain('application/x-www-form-urlencoded');
  expect(response.status()).toBe(200);
  await expect(page).toHaveURL(new RegExp(`${action}$`));
  const entries = new URLSearchParams((await response.json()).entries);
  expect(entries.getAll('csrf_token')).toEqual([csrfToken]);
  entries.delete('csrf_token'); // Remaining assertions describe domain form values.
  return entries;
}

async function expectRestoredRejection(page, field = null) {
  await expect(page.locator('#description')).toHaveValue('   ');
  await expect(page.locator('#description')).toHaveAttribute('aria-invalid', 'true');
  await expect(page.locator('#description')).toHaveAttribute('aria-describedby', 'description-help description-error');
  await expect(page.locator('#description')).toHaveAttribute('aria-errormessage', 'description-error');
  await expect(page.locator('#description-error > div')).toHaveText([descriptionError]);
  await expectPermission(page, 'push', true, serverPermissionError);
  await expect(page.locator('#link-form-errors')).toHaveAttribute('role', 'alert');
  await expect(page.locator('#link-form-errors li')).toHaveText([summaryMessage, serverFormError]);
  await expect(page.locator('#internal_note')).toHaveValue('Preserved transport note');
  await expect(page.getByRole('checkbox', { name: 'acme/api', exact: true })).toBeChecked();
  for (const numeric of numericFields) {
    await expectNumeric(page, numeric, numeric === field ? '' : numeric.id === 'max_uses' ? '7' : '45',
      numeric === field ? numeric.invalid : null);
  }
}

test('browser rejection clears only related field errors on edit and never reapplies on rerender', async ({ page }) => {
  await open(page, '/rejection-mixed');
  await expectRestoredRejection(page);
  // An unrelated write rerenders the island without replaying restoration.
  await page.locator('#internal_note').fill('First unrelated edit');
  await expect(page.locator('#description-error')).toHaveText(descriptionError);
  await expectPermission(page, 'push', true, serverPermissionError);
  await expect(page.locator('#link-form-errors li')).toHaveText([summaryMessage, serverFormError]);

  await page.locator('#description').fill('Corrected transport workshop');
  // Restored errors retire on input, before on-commit client validation.
  await expect(page.locator('#description-error')).toBeEmpty();
  await expect(page.locator('#description')).toHaveAttribute('aria-invalid', 'false');
  await expect(page.locator('#description')).not.toHaveAttribute('aria-errormessage');
  await expectPermission(page, 'push', true, serverPermissionError);
  await page.locator('#permission').selectOption('triage');
  await expectPermission(page, 'triage', false);
  // Field edits do not retire a form-level rejection.
  await expect(page.locator('#link-form-errors li')).toHaveText([summaryMessage, serverFormError]);
  await page.locator('#internal_note').fill('Second unrelated edit');
  await approval(page).check();
  await expect(page.locator('#description-error')).toBeEmpty();
  await expectPermission(page, 'triage', false);
  await expect(page.locator('#description')).toHaveValue('Corrected transport workshop');
  await expect(page.locator('#link-form-errors li')).toHaveText([summaryMessage, serverFormError]);
  const data = await post(page);
  expect(data.get('description')).toBe('Corrected transport workshop');
  expect(data.get('permission')).toBe('triage');
  expect(data.get('internal_note')).toBe('Second unrelated edit');
});

for (const field of numericFields) {
  test(`browser rejection ${field.id} parse-blocked preflight retains errors; a fresh submit retires them and validates the client`, async ({ page }) => {
    const path = `/rejection-${field.id}`;
    await open(page, path);
    await expectRestoredRejection(page, field);
    const posts = [];
    page.on('request', (request) => { if (request.method() === 'POST') posts.push(request.url()); });
    // Sanitized abc is optional/empty in the DOM, and whitespace satisfies
    // required: a click reaches progressive_submit, not native invalid UI.
    expect(await form(page).evaluate((element) => element.checkValidity())).toBe(true);
    for (let attempt = 0; attempt < 2; attempt++) {
      await submit(page).click();
      await expectRestoredRejection(page, field);
      await expect(page).toHaveURL(path);
      expect(posts).toEqual([]);
    }
    await page.locator(`#${field.id}`).fill('9');
    await page.keyboard.press('Tab');
    await expectNumeric(page, field, '9');
    await expectPermission(page, 'push', true, serverPermissionError);
    await expect(page.locator('#link-form-errors li')).toHaveText([summaryMessage, serverFormError]);
    await submit(page).click();
    // No parse blocker remains: retire the old response and run the client
    // rules. The untouched whitespace description is still a real blocker.
    await expect(page.locator('#description-error > div')).toHaveText([descriptionError]);
    await expectPermission(page, 'push', false);
    await expect(page.locator('#link-form-errors li')).toHaveText([summaryMessage]);
    await expect(page).toHaveURL(path);
    expect(posts).toEqual([]);
    await page.locator('#internal_note').fill('Rerender after fresh preflight');
    await approval(page).check();
    await expectNumeric(page, field, '9');
    await expectPermission(page, 'push', false);
    await expect(page.locator('#link-form-errors li')).toHaveText([summaryMessage]);
    await page.locator('#description').fill('Corrected transport workshop');
    await page.keyboard.press('Tab');
    await expect(page.locator('#description-error')).toBeEmpty();
    await expect(page.locator('#link-form-errors')).toHaveCount(0);
    const data = await post(page);
    expect(data.get(field.id)).toBe('9');
    expect(data.get('permission')).toBe('push');
    expect(posts).toHaveLength(1);
  });
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

for (const [path, raw] of [['/permission-invalid', 'owner'], ['/permission-empty', '']]) {
  test(`permission ${JSON.stringify(raw)} remains invalid after mount despite displaying pull`, async ({ page }) => {
    await open(page, path);
    const posts = [];
    page.on('request', (request) => { if (request.method() === 'POST') posts.push(request.url()); });
    const props = JSON.parse(await page.locator('#link-form-props').textContent());
    expect(props.values.permission).toBe(raw);
    await expectPermission(page, 'pull', true);
    expect(await form(page).evaluate((element) => element.checkValidity())).toBe(true);
    await submit(page).click();
    await expect(page.locator('#link-form-errors')).toBeVisible();
    await expectPermission(page, 'pull', true);
    await expect(page).toHaveURL(path);
    expect(posts).toEqual([]);
    // An unchanged native option does not emit input/change. Focus and blur
    // must not copy the displayed fallback into the still-invalid model.
    await page.locator('#permission-label').click();
    await expect(page.locator('#permission')).toBeFocused();
    await page.keyboard.press('Tab');
    await expectPermission(page, 'pull', true);
    await submit(page).click();
    await expect(page).toHaveURL(path);
    expect(posts).toEqual([]);
    // Choose another permitted value before pull to generate a real change.
    await page.locator('#permission').selectOption('triage');
    await page.locator('#permission').focus();
    await page.keyboard.press('Tab');
    await expectPermission(page, 'triage', false);
    await page.locator('#permission').selectOption('pull');
    await expectPermission(page, 'pull', false);
    const data = await post(page);
    expect(data.getAll('permission')).toEqual(['pull']);
    expect(posts).toHaveLength(1);
  });
}

test('permission validates on unchanged focus exit and ignores malformed DOM input', async ({ page }) => {
  await open(page, '/permission-unvalidated');
  await expectPermission(page, 'pull', false);
  await page.locator('#permission-label').click();
  await expect(page.locator('#permission')).toBeFocused();
  await expect(page.locator('#permission-error')).toBeEmpty();
  await page.keyboard.press('Tab');
  await expectPermission(page, 'pull', true);
  for (const raw of ['owner', '', '0']) {
    // Unknown strings cannot be represented by an option. Neither the empty
    // DOM value nor an injected input event may silently repair dioform state.
    await page.locator('#permission').evaluate((select, raw) => {
      select.value = raw;
      select.dispatchEvent(new Event('input', { bubbles: true }));
      select.dispatchEvent(new Event('change', { bubbles: true }));
    }, raw);
    await expect(page.locator('#permission-error')).toHaveText(permissionError);
    await submit(page).click();
    await expect(page).toHaveURL('/permission-unvalidated');
    await expect(page.locator('#link-form-errors')).toBeVisible();
  }
  await page.locator('#permission').selectOption('push');
  await page.locator('#permission').focus();
  await page.keyboard.press('Tab');
  await expectPermission(page, 'push', false);
  expect((await post(page)).getAll('permission')).toEqual(['push']);
});

for (const field of numericFields) {
  test(`numeric ${field.id} preserves repeated same-error raw edits and clears metadata when corrected`, async ({ page }) => {
    await open(page);
    await expectNumeric(page, field, field.initial);
    const control = page.locator(`#${field.id}`);
    await page.locator(`#${field.id}-label`).click();
    await expect(control).toBeFocused();
    await page.keyboard.press('Tab');
    await expectNumeric(page, field, field.initial);
    const error = await page.locator(`#${field.id}-error`).elementHandle();
    for (const raw of ['0', '00', '000']) {
      await control.fill(raw);
      await expectNumeric(page, field, raw, field.invalid);
      // A different control forces a render even when the parse error is unchanged.
      await page.locator('#internal_note').fill(`Raw edit ${raw}`);
      await expectNumeric(page, field, raw, field.invalid);
    }
    for (const raw of ['7', '', '0', '9']) {
      await control.fill(raw);
      await page.keyboard.press('Tab');
      await expectNumeric(page, field, raw, raw === '0' ? field.invalid : null);
    }
    expect(await error.evaluate((element) => element.isConnected)).toBe(true);
  });

  test(`numeric ${field.id} blocks native-valid parser failures before allowing a native POST`, async ({ page }) => {
    await open(page);
    await page.locator('#description').fill('Numeric workshop');
    await page.getByRole('checkbox', { name: 'acme/api', exact: true }).check();
    const posts = [];
    page.on('request', (request) => { if (request.method() === 'POST') posts.push(request.url()); });
    // 1.0 is an integer for native step validation but not an ASCII digit string.
    // Do not use abc, which Chromium sanitizes, or min/step failures as proof
    // that the progressive submit handler ran.
    const cases = [['4294967296', field.overflow], ['1e2', field.invalid], ['1.0', field.invalid]];
    if (field.id === 'expires_in_days') cases.push(['4294967295', field.overflow]);
    for (const [raw, message] of cases) {
      await page.locator(`#${field.id}`).fill(raw);
      await page.keyboard.press('Tab');
      await expectNumeric(page, field, raw, message);
      expect(await form(page).evaluate((element) => element.checkValidity())).toBe(true);
      await submit(page).click();
      await expect(page.locator('#link-form-errors')).toHaveAttribute('role', 'alert');
      await expectNumeric(page, field, raw, message);
      await expect(page).toHaveURL('/');
      expect(posts).toEqual([]);
    }
    await page.locator(`#${field.id}`).fill(field.valid);
    await page.keyboard.press('Tab');
    await expectNumeric(page, field, field.valid);
    expect((await post(page)).getAll(field.id)).toEqual([field.valid]);
    expect(posts).toHaveLength(1);
  });

  test(`numeric ${field.id} does not silently repair seeded abc on blur or unrelated edits`, async ({ page }) => {
    const path = `/${field.id}-invalid`;
    await open(page, path);
    const props = JSON.parse(await page.locator('#link-form-props').textContent());
    expect(props.values[field.id]).toBe('abc');
    await expectNumeric(page, field, '', field.invalid);
    const posts = [];
    page.on('request', (request) => { if (request.method() === 'POST') posts.push(request.url()); });
    await page.locator(`#${field.id}-label`).click();
    await expect(page.locator(`#${field.id}`)).toBeFocused();
    await page.keyboard.press('Tab');
    await page.locator('#description').fill('   ');
    await page.keyboard.press('Tab');
    await expect(page.locator('#description-error')).not.toBeEmpty();
    await page.locator('#description').fill('Corrected numeric workshop');
    await page.locator('#permission').selectOption('push');
    await page.locator('#internal_note').fill('Unrelated edit');
    const other = field.id === 'max_uses' ? 'expires_in_days' : 'max_uses';
    await page.locator(`#${other}`).fill('7');
    await page.keyboard.press('Tab');
    await expect(page.locator('#description-error')).toBeEmpty();
    await expectNumeric(page, field, '', field.invalid);
    expect(await form(page).evaluate((element) => element.checkValidity())).toBe(true);
    await submit(page).click();
    await expect(page.locator('#link-form-errors')).toBeVisible();
    await expectNumeric(page, field, '', field.invalid);
    await expect(page).toHaveURL(path);
    expect(posts).toEqual([]);
    // Filling an already-sanitized empty input need not fire input. Make a
    // real numeric edit before clearing it to explicitly choose None.
    await page.locator(`#${field.id}`).fill('1');
    await page.locator(`#${field.id}`).fill('');
    await page.keyboard.press('Tab');
    await expectNumeric(page, field, '');
    const data = await post(page);
    expect(data.getAll(field.id)).toEqual(['']);
    expect(data.getAll(other)).toEqual(['7']);
    expect(data.get('description')).toBe('Corrected numeric workshop');
    expect(posts).toHaveLength(1);
  });
}

for (const mode of ['mounted', 'no-js', 'blocked-bundle']) {
  test.describe(mode, () => {
    if (mode === 'no-js') test.use({ javaScriptEnabled: false });
    test.beforeEach(async ({ page }) => {
      if (mode === 'blocked-bundle') await page.route('**/assets/**', (route) => route.abort('blockedbyclient'));
    });

    for (const field of [null, ...numericFields]) {
      test(`browser rejection ${field?.id ?? 'mixed'} preserves SSR messages and metadata`, async ({ page }) => {
        await open(page, `/rejection-${field?.id ?? 'mixed'}`, mode === 'mounted');
        await expectRestoredRejection(page, field);
        const props = JSON.parse(await page.locator('#link-form-props').textContent());
        expect(props.values.errors.permission).toBe(serverPermissionError);
        expect(props.values.errors.summary).toEqual([summaryMessage, serverFormError]);
        if (field) expect(props.values[field.id]).toBe('abc');
      });
    }

    test('browser rejection form-only response permits an unchanged native POST retry', async ({ page }) => {
      await open(page, '/rejection-form', mode === 'mounted');
      await expect(page.locator('#link-form-errors li')).toHaveText([summaryMessage, serverFormError]);
      await expect(page.locator('#description-error')).toBeEmpty();
      await expectPermission(page, 'push', false);
      expect(await form(page).evaluate((element) => element.checkValidity())).toBe(true);
      const data = await post(page);
      expect(Object.fromEntries(data)).toEqual({
        description: 'Transport workshop', internal_note: 'Preserved transport note',
        permission: 'push', max_uses: '7', expires_in_days: '45', repo_ids: '10',
      });
    });

    for (const field of numericFields) {
      test(`numeric ${field.id} retains SSR error metadata and submits exact corrected or empty values`, async ({ page }) => {
        await open(page, `/${field.id}-invalid`, mode === 'mounted');
        const props = JSON.parse(await page.locator('#link-form-props').textContent());
        expect(props.values[field.id]).toBe('abc');
        await expectNumeric(page, field, '', field.invalid);
        await page.locator(`#${field.id}`).fill(field.valid);
        await page.keyboard.press('Tab');
        if (mode === 'mounted') await expectNumeric(page, field, field.valid);
        expect((await post(page)).getAll(field.id)).toEqual([field.valid]);

        await open(page, '/', mode === 'mounted');
        await expectNumeric(page, field, field.initial);
        await page.locator('#description').fill('Blank numeric workshop');
        await page.getByRole('checkbox', { name: 'acme/api', exact: true }).check();
        await page.locator(`#${field.id}`).fill('1');
        await page.locator(`#${field.id}`).fill('');
        await page.keyboard.press('Tab');
        await expectNumeric(page, field, '');
        expect((await post(page)).getAll(field.id)).toEqual(['']);
      });
    }

    for (const permission of permissions) {
      test(`permission ${permission} submits its explicit native form value`, async ({ page }) => {
        await open(page, '/', mode === 'mounted');
        await expectPermission(page, 'pull', false);
        await page.locator('#description').fill('Permission workshop');
        await page.getByRole('checkbox', { name: 'acme/api', exact: true }).check();
        await page.locator('#permission').selectOption(permission);
        await page.locator('#permission').focus();
        await page.keyboard.press('Tab');
        await expectPermission(page, permission, false);
        const data = await post(page);
        expect(data.getAll('permission')).toEqual([permission]);
      });
    }

    test('permission keyboard selection updates the native POST value', async ({ page }) => {
      await open(page, '/', mode === 'mounted');
      await page.locator('#description').fill('Keyboard workshop');
      await page.getByRole('checkbox', { name: 'acme/api', exact: true }).check();
      await page.locator('#permission').focus();
      await page.keyboard.press('t');
      await page.keyboard.press('Tab');
      await expectPermission(page, 'triage', false);
      expect((await post(page)).getAll('permission')).toEqual(['triage']);
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
      await expectNumeric(page, numericFields[0], '', numericFields[0].invalid);
      await expectNumeric(page, numericFields[1], '0', numericFields[1].invalid);
      const props = JSON.parse(await page.locator('#link-form-props').textContent());
      expect(props.values.max_uses).toBe('abc');
      expect(props.values.permission).toBe('owner');
      await expectPermission(page, 'pull', true);
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
  for (const control of await form(page).locator('input:not([type="hidden"]), textarea, select, button[type="submit"]').all()) {
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
