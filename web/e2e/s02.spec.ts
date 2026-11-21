import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';

test('s02_source_only_capture_keeps_two_occurrences_without_identity_merge', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Leads' }).click();
  const suffix = randomUUID();
  const person = `Dana Lee ${suffix}`;
  const sourceText = `The next engineer ${suffix} will work with Dana on onboarding.`;
  const before = await page.locator('.capture li').count();
  await page.getByLabel('Name').fill(person);
  await page.getByRole('button', { name: 'Add lead' }).click();
  await expect(page.getByRole('status')).toContainText('No opportunity or outreach was created');
  await page.getByLabel('Captured text').fill(sourceText);
  await page.getByRole('button', { name: 'Save source' }).click();
  await expect(page.getByRole('status')).toContainText('No person was inferred or linked');
  await page.getByLabel('Captured text').fill(sourceText);
  await page.getByRole('button', { name: 'Save source' }).click();
  await expect(page.locator('.capture li')).toHaveCount(before + 2);
  await page.reload();
  await page.getByRole('button', { name: 'Leads' }).click();
  await expect(page.getByText(person, { exact: false })).toBeVisible();
  await expect(page.locator('.capture li')).toHaveCount(before + 2);
});
