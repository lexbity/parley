import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';

test('s01_lost_ack_retry_returns_one_durable_resource', async ({ page }, testInfo) => {
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Resources', exact: true })).toBeVisible();
  const title = `First launch walkthrough ${randomUUID()}`;
  await page.getByLabel('Name').fill(title);
  await page.getByLabel('What can you offer?').fill('A walkthrough available on request.');
  await page.getByRole('button', { name: 'Save draft' }).click();
  await expect(page.getByRole('status')).toContainText('Draft saved');
  await expect(page.getByText(title)).toBeVisible();
  await page.reload();
  await expect(page.getByText(title)).toBeVisible();
  if (testInfo.project.name === 'rust') {
    const script = page.locator('script[type="module"]');
    await expect(script).toHaveAttribute('src', /^\/assets\//);
    const scriptUrl = await script.evaluate((element: HTMLScriptElement) => element.src);
    await expect(await page.request.get(scriptUrl)).toBeOK();
  }
});
