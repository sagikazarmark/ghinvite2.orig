import { test, expect } from '@playwright/test';

test('native submission survives acknowledgement loss, two tabs, reload and explicit editing', async ({ page, context }) => {
  await page.goto('/fixture-login');
  const original = await page.locator('[name="operation_id"]').inputValue();
  const second = await context.newPage();
  await second.goto('/i/abcdEFGH01234567');
  expect(await second.locator('[name="operation_id"]').inputValue()).not.toBe(original);
  await page.getByLabel('Justification').fill('  original context  ');
  await page.getByRole('button', { name: 'Submit request' }).click();
  await expect(page.getByRole('status')).toContainText('Outcome unknown');
  await expect(page.getByLabel('Justification')).toHaveValue('original context');
  await expect(page.getByLabel('Justification')).toHaveAttribute('readonly');
  await page.getByRole('button', { name: 'Retry same attempt' }).click();
  await expect(page.getByRole('status')).toContainText('Request accepted at');
  await expect(page).toHaveURL(new RegExp(`operation_id=${original}`));
  await page.reload();
  await expect(page.getByRole('status')).toContainText('Request accepted at');
  await page.goto('/');
  await page.goto('/i/abcdEFGH01234567');
  await expect(page.getByRole('status')).toContainText('Request accepted at');
  await page.getByRole('link', { name: 'Start a fresh attempt with edited input' }).click();
  await expect(page.getByRole('status')).toContainText('This is a fresh attempt');
  expect(await page.locator('[name="operation_id"]').inputValue()).not.toBe(original);
  await page.getByLabel('Justification').fill('edited context');
  await expect(page.getByRole('link', { name: 'Recover original attempt / Check again' }))
    .toHaveAttribute('href', `/i/abcdEFGH01234567?operation_id=${original}`);
  await page.getByRole('link', { name: 'Recover original attempt / Check again' }).click();
  await expect(page.getByRole('status')).toContainText('Request accepted at');
});
