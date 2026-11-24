// The create form's repository picker over a 120-repository installation
// (more than one GitHub listing page), on the production route: server-side
// search and paging keep every value and selection, and creation takes the
// selections from every page. console_flow/repository_picker.rs covers the
// route's GitHub reads and validation.
import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';

const repo = (id) => `acme/app-${String(id).padStart(3, '0')}`;
const box = (page, id) => page.getByLabel(repo(id), { exact: true });
const grant = (page, id) => page.getByLabel(`Permission for ${repo(id)}`, { exact: true });

async function listed(page) {
  return page.locator('input[name="repo_ids"]').evaluateAll((boxes) => boxes.map((b) => Number(b.value)));
}

// With the shared script, a checked repository submits only once its access
// check answered; picker buttons never wait for it.
async function accessChecked(page, count, testInfo) {
  if (testInfo.project.use.javaScriptEnabled) {
    await expect(page.getByText('Access checked', { exact: true })).toHaveCount(count);
  }
}

test('search and paging keep selections from every page until the link is created', async ({ page }, testInfo) => {
  await page.goto('/fixture-login');
  await expect(page.getByRole('heading', { name: 'New invitation link' })).toBeVisible();
  // One page of 50, not 120 checkboxes.
  expect(await listed(page)).toEqual(Array.from({ length: 50 }, (_, i) => i + 1));
  await expect(page.getByText('120 repositories in this installation.')).toBeVisible();
  await expect(page.getByText('Page 1 of 3')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Previous page' })).toBeDisabled();

  await page.getByLabel('Max use', { exact: true }).fill('7');
  await box(page, 3).check();
  await grant(page, 3).selectOption('3:triage');
  await page.getByRole('button', { name: 'Next page' }).click();

  // Page 2, with the page-1 selection kept ahead of it.
  await expect(page.getByText('Page 2 of 3')).toBeVisible();
  expect(await listed(page)).toEqual([3, ...Array.from({ length: 50 }, (_, i) => i + 51)]);
  await expect(page.getByText('Selected, not on this page')).toBeVisible();
  await expect(box(page, 3)).toBeChecked();
  await expect(grant(page, 3)).toHaveValue('3:triage');
  await expect(page.getByLabel('Max use', { exact: true })).toHaveValue('7');
  // The filter, the pinned and page captions, the page buttons and the
  // hidden default button keep the form's labelling intact. (axe runs as a
  // page script, so only where scripts run.)
  if (testInfo.project.use.javaScriptEnabled) {
    const accessibility = await new AxeBuilder({ page }).include('#link-form-island')
      .withRules(['label', 'button-name', 'select-name', 'aria-hidden-focus', 'aria-valid-attr',
        'aria-valid-attr-value', 'aria-allowed-attr', 'duplicate-id-aria', 'landmark-unique'])
      .analyze();
    expect(accessibility.violations).toEqual([]);
  }
  await box(page, 75).check();

  // Enter in the filter searches (the description is still empty, which a
  // search does not validate) and never applies a permission to every row.
  await page.getByLabel('Find repositories', { exact: true }).fill('app-11');
  await page.getByLabel('Find repositories', { exact: true }).press('Enter');
  await expect(page.getByText('10 repositories match “app-11”.')).toBeVisible();
  expect(await listed(page)).toEqual([3, 75, ...Array.from({ length: 10 }, (_, i) => i + 110)]);
  await expect(page.getByRole('navigation', { name: 'Repository pages' })).toHaveCount(0);
  await expect(page.getByRole('alert')).toHaveCount(0);
  await expect(grant(page, 3)).toHaveValue('3:triage');
  await expect(box(page, 75)).toBeChecked();
  await expect(page.getByLabel('Find repositories', { exact: true })).toHaveValue('app-11');

  await box(page, 115).check();
  await grant(page, 115).selectOption('115:maintain');
  await page.getByLabel('Description', { exact: true }).fill('Picker workshop');
  await accessChecked(page, 3, testInfo);
  await page.getByRole('button', { name: 'Create invitation link', exact: true }).click();

  await expect(page.getByRole('heading', { name: 'Save this invitation URL' })).toBeVisible();
  await expect(page.getByRole('link', { name: /\/i#[\w-]{43}$/ })).toBeVisible();
  await page.getByRole('link', { name: 'Back to link details' }).click();
  for (const id of [3, 75, 115]) {
    await expect(page.getByText(repo(id)).first()).toBeVisible();
  }
});

test('a search keeps the form and a repository the search left out stays selected', async ({ page }, testInfo) => {
  await page.goto('/fixture-login');
  await page.getByLabel('Description', { exact: true }).fill('Narrowed workshop');
  await box(page, 12).check();
  await page.getByLabel('Find repositories', { exact: true }).fill('APP-01');
  await page.getByRole('button', { name: 'Search', exact: true }).click();

  // app-012 itself matches, so it is listed in place rather than pinned.
  await expect(page.getByText('10 repositories match “APP-01”.')).toBeVisible();
  expect(await listed(page)).toEqual(Array.from({ length: 10 }, (_, i) => i + 10));
  await expect(page.getByText('Selected, not on this page')).toHaveCount(0);
  await expect(box(page, 12)).toBeChecked();
  await expect(page.getByLabel('Description', { exact: true })).toHaveValue('Narrowed workshop');

  // A filter matching nothing still keeps the selection.
  await page.getByLabel('Find repositories', { exact: true }).fill('nothing-here');
  await page.getByRole('button', { name: 'Search', exact: true }).click();
  await expect(page.getByText('No repositories match “nothing-here”.')).toBeVisible();
  expect(await listed(page)).toEqual([12]);
  await expect(box(page, 12)).toBeChecked();

  // Clearing it lists the first page again, the selection in place.
  await page.getByLabel('Find repositories', { exact: true }).fill('');
  await page.getByRole('button', { name: 'Search', exact: true }).click();
  await expect(page.getByText('Page 1 of 3')).toBeVisible();
  await expect(box(page, 12)).toBeChecked();
  await accessChecked(page, 1, testInfo);
});
