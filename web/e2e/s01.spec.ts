import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { openOwnerPage, test } from './fixtures';

test('s01_resource_first_launch_retry_and_cross_session_conflict', async ({ page, browser, ownerSession }, testInfo) => {
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Resources', exact: true })).toBeVisible();
  const title = `Onboarding checklist ${randomUUID()}`;
  const resources = page.getByRole('region', { name: 'Resources workspace' });
  await resources.getByRole('button', { name: 'Add resource' }).click();
  await resources.getByLabel('Name').fill(title);
  await resources.getByLabel('What is it?').fill('A short review guide for onboarding teams.');
  await page.getByRole('button', { name: 'Save draft' }).click();
  await expect(page.getByRole('status')).toContainText('Draft saved to this workspace.');

  await page.reload();
  const card = page.getByRole('button', { name: new RegExp(title) });
  await expect(card).toBeVisible();
  if (testInfo.project.name === 'rust') {
    // The packaged binary serves the embedded Vite bundle from hashed assets.
    const script = page.locator('script[type="module"]');
    await expect(script).toHaveAttribute('src', /^\/assets\//);
    const scriptUrl = await script.evaluate((element: HTMLScriptElement) => element.src);
    await expect(await page.request.get(scriptUrl)).toBeOK();
  }

  // A second independent session on the same workspace edits the resource.
  const second = await browser.newPage();
  await openOwnerPage(second, await ownerSession.secondBootstrapUrl());
  await second.getByRole('button', { name: new RegExp(title) }).click();
  await second.getByRole('region', { name: 'Resources workspace' }).getByLabel('Name').fill(`Shared ${title}`);
  await second.getByRole('button', { name: 'Save draft' }).click();
  await expect(second.getByRole('status')).toContainText('Draft saved to this workspace.');

  // The first session still holds the stale revision and gets the comparison.
  await page.getByRole('button', { name: new RegExp(title) }).click();
  await resources.getByLabel('Name').fill(`Mine ${title}`);
  await page.getByRole('button', { name: 'Save draft' }).click();
  await expect(page.getByRole('alert')).toContainText('Review the conflict');
  await expect(page.getByRole('alert')).toContainText(`Shared ${title}`);
  await expect(page.getByRole('alert')).toContainText(`Mine ${title}`);
  expect(await page.evaluate(() => Object.keys(localStorage))).toEqual([]);
  expect(await second.evaluate(() => Object.keys(localStorage))).toEqual([]);
});
