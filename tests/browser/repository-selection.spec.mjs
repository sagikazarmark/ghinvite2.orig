// Real shared browser script and native form validation; HTTP authority outcomes
// are controlled here. console_authority.rs tests the actual authenticated route.
import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';

const script = readFileSync(new URL('../../crates/ghinvite-web/assets/app.js', import.meta.url), 'utf8');
const form = `<form method="post" action="/console/accounts/acme/links"
  data-repository-authority="/console/accounts/acme/links/new/selection">
  <input name="description" value="Retain my work">
  <label class="repo-choice-row"><input type="checkbox" name="repo_ids" value="10">acme/api</label>
  <button type="submit">Create invitation link</button></form>`;

async function fixture(page) {
  const requests = [];
  await page.route('http://selection.test/**', async route => {
    const url = new URL(route.request().url());
    if (url.pathname.endsWith('/selection')) {
      expect(url.searchParams.get('repo_id')).toBe('10');
      expect(route.request().method()).toBe('GET');
      requests.push(route);
    } else if (url.pathname === '/static/app.js') {
      await route.fulfill({ contentType: 'text/javascript; charset=utf-8', body: script });
    } else {
      await route.fulfill({ contentType: 'text/html; charset=utf-8', body:
        `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><script src="/static/app.js"></script></head><body>${form}</body></html>` });
    }
  });
  await page.goto('http://selection.test/');
  await page.evaluate(() => {
    window.submissions = 0;
    document.addEventListener('submit', event => { event.preventDefault(); window.submissions++; });
  });
  return requests;
}
async function submit(page, count) {
  await page.evaluate(() => document.querySelector('form').requestSubmit());
  expect(await page.evaluate(() => window.submissions)).toBe(count);
}
async function answer(route, status, body = { allowed: true }) {
  await route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
}

test('selection waits for authority and keeps denied or unavailable choices from submitting', async ({ page }) => {
  const requests = await fixture(page);
  const box = page.locator('input[name=repo_ids]');
  await box.check();
  await expect.poll(() => requests.length).toBe(1);
  await expect(page.getByRole('status')).toHaveText('Checking access…');
  await submit(page, 0);
  await answer(requests[0], 404);
  await expect(page.getByRole('status')).toHaveText('You cannot select this repository.');
  await submit(page, 0);
  await box.uncheck(); await box.check();
  await expect.poll(() => requests.length).toBe(2);
  await answer(requests[1], 503);
  await expect(page.getByRole('status')).toContainText('Access could not be checked');
  await submit(page, 0);
  await box.uncheck(); await box.check();
  await expect.poll(() => requests.length).toBe(3);
  await answer(requests[2], 200);
  await expect(page.getByRole('status')).toHaveText('Access checked');
  await expect(page.locator('input[name=description]')).toHaveValue('Retain my work');
  await submit(page, 1);
});

test('late response cannot authorize a replacement island form', async ({ page }) => {
  const requests = await fixture(page);
  await page.locator('input[name=repo_ids]').check();
  await expect.poll(() => requests.length).toBe(1);
  await page.evaluate(html => {
    document.querySelector('form').outerHTML = html.replace('value="10"', 'value="10" checked');
  }, form);
  await expect.poll(() => requests.length).toBe(2);
  await answer(requests[0], 200);
  await expect(page.getByRole('status')).toHaveText('Checking access…');
  await submit(page, 0);
  await answer(requests[1], 200);
  await expect(page.getByRole('status')).toHaveText('Access checked');
  await submit(page, 1);
});

test('deselecting while a check is pending clears its blocker and ignores its result', async ({ page }) => {
  const requests = await fixture(page);
  const box = page.locator('input[name=repo_ids]');
  await box.check();
  await expect.poll(() => requests.length).toBe(1);
  await box.uncheck();
  await answer(requests[0], 404);
  await expect(page.getByRole('status')).toHaveText('');
  expect(await box.evaluate(box => box.validationMessage)).toBe('');
  await submit(page, 1); // Required repository scope remains server/island validation.
});

test('a successful HTTP response without an affirmative authority result stays blocked', async ({ page }) => {
  const requests = await fixture(page);
  await page.locator('input[name=repo_ids]').check();
  await expect.poll(() => requests.length).toBe(1);
  await answer(requests[0], 200, { allowed: false });
  await expect(page.getByRole('status')).toContainText('Access could not be checked');
  await submit(page, 0);
});

test('form-only helpers remain usable while selection authority is pending', async ({ page }) => {
  const requests = await fixture(page);
  await page.locator('input[name=repo_ids]').check();
  await expect.poll(() => requests.length).toBe(1);
  await submit(page, 0);
  for (const name of ['apply_all', 'reload_repos']) {
    await page.evaluate(name => {
      const button = document.createElement('button');
      button.type = 'submit'; button.name = name; button.value = 'true';
      button.formNoValidate = true;
      const form = document.querySelector('form');
      form.appendChild(button);
      form.requestSubmit(button);
    }, name);
  }
  expect(await page.evaluate(() => window.submissions)).toBe(2);
  await submit(page, 2);
  await answer(requests[0], 404);
});
