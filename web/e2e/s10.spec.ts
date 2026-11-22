import { expect } from '@playwright/test';
import { test } from './fixtures';

test('s10_ranked_daily_work_insights_and_future_pack_activation', async ({ page }) => {
  test.setTimeout(90_000);
  const coachRuns: string[] = [];
  page.on('request', (request) => {
    if (request.url().endsWith('/api/v1/coach/run')) coachRuns.push(request.method());
  });
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Resources', exact: true })).toBeVisible();
  await expect(page.getByRole('status')).toContainText('Workspace ready');
  await page.getByRole('link', { name: 'Queue' }).click();
  for (let index = 0; index < 100; index += 1) {
    await page.getByRole('button', { name: 'Daily' }).click();
    await expect(page.getByRole('heading', { name: 'Daily priorities' })).toBeVisible();
    await page.getByRole('button', { name: 'Board' }).click();
  }
  expect(coachRuns).toHaveLength(0);

  await page.getByRole('button', { name: 'Insights' }).click();
  await expect(page.getByText('Insufficient observations').first()).toBeVisible();
  await page.getByRole('button', { name: 'Run Coach for this week' }).click();
  await expect(page.getByRole('status')).toContainText('Coach report refreshed');
  expect(coachRuns).toHaveLength(1);

  await page.getByRole('link', { name: 'Settings' }).click();
  const packs = page.getByRole('region', { name: 'Config packs' });
  await expect(packs.getByText(/Active: job_search v1\.0\.0/)).toBeVisible();
  await packs.getByLabel('Pack to review').selectOption('lead_gen@1.0.0');
  await packs.getByRole('button', { name: 'Validate pack' }).click();
  await expect(packs.getByRole('status')).toContainText('passed server validation');
  await packs.getByLabel('Stage mapping JSON').fill('{"Researched":"prepared"}');
  await packs.getByRole('button', { name: 'Activate validated version' }).click();
  await expect(packs.getByRole('status')).toContainText('is active for future work');
  await expect(packs.getByText(/Active: lead_gen v1\.0\.0/)).toBeVisible();
});
