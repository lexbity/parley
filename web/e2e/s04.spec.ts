import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';

test('s04_saves_capture_without_model_and_reviews_only_explicit_proposals', async ({ page }, testInfo) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Settings' }).click();
  await page.getByLabel('Installed model tag').fill('unavailable-local-model');
  await page.getByRole('button', { name: 'Save provider' }).click();
  await expect(page.locator('main > p[role="status"]')).toContainText('provider settings saved');
  await page.getByRole('button', { name: 'Check local model' }).click();
  await expect(page.getByRole('region', { name: 'Local agent settings' }).getByRole('status')).toContainText('unavailable');

  const suffix = randomUUID();
  const name = `Dana Lee ${suffix}`;
  const sourceText = `Northstar ${suffix} says the next engineer will work with Dana Lee on onboarding.`;
  await page.getByRole('button', { name: 'Leads' }).click();
  const leads = page.getByRole('region', { name: 'Lead directory and sources' });
  await leads.getByLabel('Name').fill(name);
  await leads.getByRole('button', { name: 'Add lead' }).click();
  await expect(page.locator('main > p[role="status"]')).toContainText('No opportunity or outreach was created');
  await leads.getByLabel('Captured text').fill(sourceText);
  await leads.getByRole('button', { name: 'Save source' }).click();

  if (testInfo.project.name === 'mock') {
    await expect(page.getByText(/Job succeeded · attempt/)).toBeVisible();
    await leads.getByRole('button', { name: 'Review proposals' }).click();
    const review = leads.getByRole('region', { name: 'Capture review' });
    await expect(review).toContainText(sourceText);
    await expect(review).toContainText('Mention: Dana Lee');
    await expect(review).toContainText('unresolved until you explicitly link it');
    await review.getByLabel('Claim subject').selectOption({ label: `${name} · person` });
    await review.getByRole('button', { name: 'Accept claim for selected subject' }).click();
    await expect(page.locator('main > p[role="status"]')).toContainText('Claim accepted with its source evidence');
  } else {
    await expect(page.locator('main > p[role="status"]')).toContainText('Source saved independently');
    await page.reload();
    await page.getByRole('button', { name: 'Leads' }).click();
    await expect(page.locator('.capture li')).toHaveCount(1);
    await expect(page.locator('.capture li')).toContainText('Saved source');
    await expect(page.getByText(name, { exact: false })).toBeVisible();
  }
});
