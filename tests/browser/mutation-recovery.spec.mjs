import { test, expect } from '@playwright/test';

test('create, revoke, approve and decline recover their original attempts across navigation', async ({ page, context, request }) => {
  await page.goto('/fixture-login');
  await page.getByLabel('Description', { exact: true }).fill('Browser recovery');
  await page.getByLabel('acme/api', { exact: true }).check();
  await page.getByRole('button', { name: 'Create invitation link' }).click();
  await expect(page.getByRole('status')).toContainText('Outcome unknown');
  const creation = await page.getByRole('link', { name: 'Check original attempt status' }).getAttribute('href');
  const detail = await page.getByRole('link', { name: 'View invitation link details' }).getAttribute('href');
  await page.goto('/console/accounts/octocat/links');
  await page.getByRole('link', { name: 'Recover attempts' }).click();
  await expect(page.getByRole('link', { name: 'Check original attempt status' })).toHaveAttribute('href', creation);
  await page.getByRole('button', { name: 'Retry original attempt' }).click();
  await expect(page).toHaveURL(detail);
  await expect(page.getByText('Browser recovery', { exact: true }).first()).toBeVisible();
  await expect(page.getByText('Invitation link created.', { exact: true })).toBeVisible();
  await page.getByRole('link', { name: 'Stop accepting new requests' }).click();
  await page.getByRole('button', { name: 'Confirm stop' }).press('Enter');
  await expect(page.getByRole('status')).toContainText('Outcome unknown');
  const revocation = await page.getByRole('link', { name: 'Check original attempt status' }).getAttribute('href');
  await page.goto('/console/accounts/octocat/links');
  await page.getByRole('link', { name: 'Recover attempts' }).click();
  await page.locator(`form[action="${revocation}"]`).getByRole('button', { name: 'Retry original attempt' }).click();
  await expect(page).toHaveURL(detail);
  await expect(page.getByText('Invitation link stopped accepting new invitation requests.', { exact: true })).toBeVisible();
  await page.goto(revocation);
  await expect(page.getByRole('status')).toContainText('stopped accepting');

  for (const [index, action, outcome] of [[0, 'Approve', 'approved'], [1, 'Decline', 'declined']]) {
    await page.goto('/console/accounts/octocat/requests');
    const form = page.locator(`form[action$="/${action.toLowerCase()}"]`).nth(index);
    const operation = await form.locator('[name="operation_id"]').inputValue();
    await form.getByRole('button', { name: `${action} request` }).click();
    await expect(page.getByRole('status')).toContainText('Outcome unknown');
    const recovery = await page.getByRole('link', { name: 'Check original attempt status' }).getAttribute('href');
    expect(recovery).toContain(operation);
    await page.goto('/console/accounts/octocat/requests');
    await page.getByRole('button', { name: `${action} request` }).nth(index).click();
    await expect(page).toHaveURL(recovery);
    await expect(page.getByRole('status')).toContainText(`Request ${outcome}`);
    const second = await context.newPage();
    await second.goto('/console/accounts/octocat/requests');
    await second.getByRole('link', { name: 'Recover attempts' }).click();
    await second.locator(`form[action="${recovery}"]`).getByRole('button', { name: 'Retry original attempt' }).click();
    await second.goto(recovery);
    await expect(second.getByRole('status')).toContainText(`Request ${outcome}`);
    await second.close();
  }
  const evidence = await (await request.get('/fixture-effects')).json();
  expect(evidence.effects).toEqual({ create: 1, revoke: 1, decide: 2 });
  expect(evidence.calls).toEqual({ create: 2, revoke: 2, decide: 4 });
});
