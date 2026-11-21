import { expect, test } from '@playwright/test';
import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

test('s02_source_only_capture_keeps_two_occurrences_without_identity_merge', async ({ page }, testInfo) => {
  const token = testInfo.project.name === 'mock'
    ? 'mock-token'
    : readFileSync(join(String(testInfo.project.metadata.e2eWorkspace), 'mutation.token'), 'utf8');
  await page.addInitScript((value: string) => { window.__PARLEY_TOKEN__ = value; }, token.trim());
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
