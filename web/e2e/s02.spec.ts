import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';

test('s02_source_only_capture_keeps_two_occurrences_without_identity_merge', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Leads' }).click();
  const leads = page.getByRole('region', { name: 'Lead directory and sources' });
  const suffix = randomUUID();
  const person = `Dana Lee ${suffix}`;
  const sourceText = `The next engineer ${suffix} will work with Dana on onboarding.`;
  const before = await page.locator('.capture li').count();
  await leads.getByLabel('Name').fill(person);
  await leads.getByRole('button', { name: 'Add lead' }).click();
  await expect(page.getByRole('status')).toContainText('No opportunity or outreach was created');
  await leads.getByLabel('Captured text').fill(sourceText);
  await leads.getByRole('button', { name: 'Save source' }).click();
  await expect(page.getByRole('status')).toContainText('No person was inferred or linked');
  await leads.getByLabel('Captured text').fill(sourceText);
  await leads.getByRole('button', { name: 'Save source' }).click();
  await expect(page.locator('.capture li')).toHaveCount(before + 2);
  await page.reload();
  await page.getByRole('button', { name: 'Leads' }).click();
  await expect(page.getByText(person, { exact: false })).toBeVisible();
  await expect(page.locator('.capture li')).toHaveCount(before + 2);
});
