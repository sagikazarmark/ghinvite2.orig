// Quirks mode and the layout viewport are decided by the browser, not by the
// markup: only a real engine can say whether a 390px phone laid the page out at
// the device width or at the 980px desktop default. The Rust side asserts the
// markup (crates/ghinvite-web/tests/document_shell.rs); this asserts the result.
import { test, expect } from '@playwright/test';
import { createHash } from 'node:crypto';
const invitation = (id) => '/i#' + createHash('sha256').update('ghinvite-test-link-secret:' + id).digest('base64url');

// Kept in step with document_paths() in crates/ghinvite-web/tests/document_shell.rs,
// which serves these same paths from the same fixture.
const SIGNED_IN_PAGES = [
  ['signed-in home', '/'],
  ['account picker', '/console'],
  ['console overview', '/console/accounts/octocat'],
  ['console links', '/console/accounts/octocat/links'],
  ['console link detail', '/console/accounts/octocat/links/01JQRFRM000000000000000000'],
  ['console link edit', '/console/accounts/octocat/links/01JQRFRM000000000000000000/edit'],
  ['console pending requests', '/console/accounts/octocat/requests'],
  ['console audit log', '/console/accounts/octocat/audit'],
  ['console settings', '/console/accounts/octocat/settings'],
  ['console not found', '/console/accounts/octocat/nowhere'],
  ['invitation entry', '/i'],
  ['manual code entry', '/code'],
  ['invitation request form', invitation('01JQRFRM000000000000000000')],
  ['invitation request status', invitation('01JQSTAT000000000000000000')],
  ['invitation request not found', '/i/not-a-code'],
];

async function documentShape(page) {
  return page.evaluate(() => ({
    // 'CSS1Compat' is standards mode; 'BackCompat' is quirks mode.
    compatMode: document.compatMode,
    lang: document.documentElement.lang,
    roots: document.querySelectorAll('html').length,
    heads: document.querySelectorAll('head').length,
    bodies: document.querySelectorAll('body').length,
    viewports: document.querySelectorAll('head meta[name="viewport"]').length,
    // The layout viewport the page was laid out in, and the widest it renders.
    layoutWidth: document.documentElement.clientWidth,
    scrollWidth: document.documentElement.scrollWidth,
  }));
}

async function expectStandardsModeDocument(page, label) {
  const shape = await documentShape(page);
  const { width } = page.viewportSize();

  expect(shape.compatMode, `${label} renders in quirks mode`).toBe('CSS1Compat');
  expect(shape.lang, `${label} has no document language`).toBe('en');
  expect(shape, `${label} is not a single document`).toMatchObject({
    roots: 1,
    heads: 1,
    bodies: 1,
    viewports: 1,
  });
  // Without the viewport meta this is 980 on the 390px phone, whatever the
  // window size is. A scrollbar may take a few pixels off on desktop.
  expect(shape.layoutWidth, `${label} ignores the device width`).toBeLessThanOrEqual(width);
  expect(shape.layoutWidth, `${label} ignores the device width`).toBeGreaterThan(width - 20);
  expect(shape.scrollWidth, `${label} overflows horizontally`).toBeLessThanOrEqual(
    shape.layoutWidth + 1,
  );
}

test('the signed-out home page is a standards-mode document at the device width', async ({
  page,
}) => {
  await page.goto('/');
  await expect(page.getByRole('link', { name: 'Sign in' }).first()).toBeVisible();

  await expectStandardsModeDocument(page, 'signed-out home');
});

test('every signed-in page is a standards-mode document at the device width', async ({ page }) => {
  await page.goto('/fixture-login');
  await expect(page).toHaveURL('/');

  for (const [label, path] of SIGNED_IN_PAGES) {
    await page.goto(path);
    if (path.startsWith('/i#')) await expect(page).toHaveURL('/join');
    await expectStandardsModeDocument(page, label);
  }
});

// Both invitation request documents are the same route, so without this the
// pair could quietly become two form pages and still pass the shape checks.
test('the invitation request form and its status are two different documents', async ({ page }) => {
  await page.goto('/fixture-login');

  await page.goto(invitation('01JQRFRM000000000000000000'));
  await expect(page.getByRole('button', { name: 'Submit request' })).toBeVisible();

  await page.goto(invitation('01JQSTAT000000000000000000'));
  await expect(page.getByRole('heading', { name: 'Awaiting review' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Submit request' })).toHaveCount(0);
  // The one document here that reloads itself; the shared head must keep it.
  await expect(page.locator('head meta[http-equiv="refresh"]')).toHaveCount(1);
});

// Exercise the actual CSP-served script, native form navigation and session
// exchange. The capability belongs only in the POST body, never a request URL.
test('fragment bootstrap clears history before posting the capability', async ({ page }) => {
  await page.goto('/fixture-login');
  const path = invitation('01JQRFRM000000000000000000');
  const secret = path.split('#')[1];
  const observed = [];
  page.on('request', request => observed.push(request));
  await page.addInitScript(() => {
    const replace = history.replaceState.bind(history);
    history.replaceState = (...args) => {
      replace(...args);
      window.__capabilityHistoryCleared = location.hash === '';
    };
    const submit = HTMLFormElement.prototype.submit;
    HTMLFormElement.prototype.submit = function () {
      if (!window.__capabilityHistoryCleared || location.hash !== '') {
        throw new Error('Capability history was not cleared before submission');
      }
      return submit.call(this);
    };
  });
  await page.goto(path);
  await expect(page).toHaveURL('/join');
  await expect(page.getByRole('button', { name: 'Submit request' })).toBeVisible();
  const posts = observed.filter(request => request.method() === 'POST');
  expect(posts).toHaveLength(1);
  expect(new URL(posts[0].url()).pathname).toBe('/i');
  const fields = new URLSearchParams(posts[0].postData());
  expect(fields.get('secret')).toBe(secret);
  expect(fields.get('csrf_token')).toBeTruthy();
  for (const request of observed) {
    expect(request.url()).not.toContain(secret);
    expect(request.headers().referer ?? '').not.toContain(secret);
  }
  expect(await page.content()).not.toContain(secret);
  await page.goBack();
  expect(new URL(page.url()).hash).toBe('');
});

test('an internal link ID fragment cannot select a capability', async ({ page }) => {
  const posts = [];
  page.on('request', request => {
    if (request.method() === 'POST') posts.push(request);
  });
  await page.goto('/i#01JQRFRM000000000000000000');
  await expect(page).toHaveURL('/i');
  await expect(page.locator('input[name="secret"]')).toHaveValue('');
  expect(posts).toHaveLength(0);
});
