import { expect, type Page } from '@playwright/test';
import { test } from './fixtures';

async function openInsights(page: Page) {
  await page.getByRole('link', { name: 'Queue' }).click();
  await page.getByRole('navigation', { name: 'Queue views' }).getByRole('button', { name: 'Insights', exact: true }).click();
  const insights = page.getByRole('region', { name: 'Weekly Coach and insights' });
  await expect(insights.getByText('data as of')).toBeVisible();
  return insights;
}

test('ar01_offline_coach_retains_deterministic_insights', async ({ page }) => {
  const starts: string[] = [];
  page.on('request', request => {
    if (new URL(request.url()).pathname === '/api/v1/coach/weekly' && request.method() === 'POST') starts.push(request.url());
  });
  await page.goto('/');
  const insights = await openInsights(page);
  await expect(insights.getByText('Insufficient observations').first()).toBeVisible();
  await expect(insights.getByText('Calibrated no-match', { exact: true })).toBeVisible();
  await insights.getByRole('button', { name: 'Refresh report' }).click();
  expect(starts).toHaveLength(0);

  const response = page.waitForResponse(res => new URL(res.url()).pathname === '/api/v1/coach/weekly' && res.request().method() === 'POST');
  await insights.getByRole('button', { name: 'Run Coach for this week' }).press('Enter');
  expect((await response).status()).toBe(503);
  expect(starts).toHaveLength(1);
  await expect(insights.getByRole('alert')).toContainText('Settings');
  await expect(insights.getByText('data as of')).toBeVisible();
  await expect(insights.getByRole('button', { name: 'Refresh report' })).toBeEnabled();
  await page.getByRole('link', { name: 'Output' }).click();
  await expect(page.getByRole('link', { name: 'Output' })).toHaveAttribute('aria-current', 'page');
});

test.describe('Coach execution and runtime outage', () => {
  test.use({ coachOnline: true });

  test('ar01_weekly_coach_starts_cancels_and_recovers', async ({ page }, testInfo) => {
    await page.goto('/');
    let insights = await openInsights(page);
    if (testInfo.project.name === 'rust') {
      // Rust project always exercises the production service and database. A
      // This browser case deliberately proves the installed Rust service's
      // fail-closed manual path. Worker execution is qualified separately.
      const response = page.waitForResponse(res => new URL(res.url()).pathname === '/api/v1/coach/weekly' && res.request().method() === 'POST');
      await insights.getByRole('button', { name: 'Run Coach for this week' }).press('Enter');
      expect((await response).status()).toBe(503);
      await expect(insights.getByRole('alert')).toContainText('Settings');
      await expect(insights.getByText('data as of')).toBeVisible();
      await expect(insights.getByRole('button', { name: 'Refresh report' })).toBeEnabled();
      return;
    }
    const start = async () => {
      const response = page.waitForResponse(res => new URL(res.url()).pathname === '/api/v1/coach/weekly' && res.request().method() === 'POST');
      await insights.getByRole('button', { name: 'Run Coach for this week' }).press('Enter');
      const received = await response;
      expect(received.status()).toBe(202);
      expect(received.request().headers()['idempotency-key']).toBeTruthy();
      return (await received.json()) as { job_id: string };
    };
    const { job_id } = await start();
    await page.reload();
    insights = await openInsights(page);

    const cancelled = page.waitForResponse(res => new URL(res.url()).pathname === `/api/v1/jobs/${job_id}` && res.request().method() === 'POST');
    await insights.getByRole('button', { name: 'Cancel Coach job' }).press('Enter');
    const receipt = await cancelled;
    expect(receipt.status()).toBe(204);
    expect(receipt.request().headers()['idempotency-key']).toBeTruthy();
    await expect(insights.getByRole('status').filter({ hasText: 'cancelled' })).toBeVisible();
    await expect(insights.getByRole('button', { name: 'Run Coach for this week' })).toBeFocused();

    const second = await start();
    expect(second.job_id).not.toBe(job_id);
    await expect(insights.getByRole('status').filter({ hasText: 'ready for review' })).toBeVisible({ timeout: 15_000 });
    await expect(insights.getByRole('button', { name: 'Dismiss question' })).toHaveCount(1);
    await page.reload();
    insights = await openInsights(page);
    await expect(insights.getByRole('button', { name: 'Dismiss question' })).toHaveCount(1);
  });
});
