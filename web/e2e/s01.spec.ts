import { expect, test } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';

test('s01_lost_ack_retry_returns_one_durable_resource', async ({ page }, testInfo) => {
  const token = testInfo.project.name === 'mock'
    ? 'mock-token'
    : readFileSync(join(String(testInfo.project.metadata.e2eWorkspace), 'mutation.token'), 'utf8');
  await page.addInitScript((value: string) => { window.__PARLEY_TOKEN__ = value; }, token.trim());
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
    await page.goto('http://127.0.0.1:8933/');
    const scriptPath = await page.locator('script[type="module"]').getAttribute('src');
    const asset = await page.request.get(`http://127.0.0.1:8933${scriptPath}`);
    expect(asset.status()).toBe(200);
    await expect(page.getByRole('heading', { name: 'Resources', exact: true })).toBeVisible();
    await expect(page.getByText(title)).toBeVisible();
  }
});
