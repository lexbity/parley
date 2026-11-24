import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties, partiesRegion } from './parties';

async function openResources(page: import('@playwright/test').Page) {
  await page.goto('/');
  await page.getByRole('link', { name: 'Resources' }).click();
  await expect(page.getByRole('heading', { name: 'Your resources' })).toBeVisible();
}

test('ar04_interviewer_resumes_after_reload', async ({ page }, testInfo) => {
  await openResources(page);
  await page.getByRole('button', { name: 'Interviewer' }).click();
  await page.getByRole('button', { name: 'Start Interviewer' }).click();
  await page.getByLabel('Your answer').fill('I helped a teammate debug a release last quarter.');
  if (testInfo.project.name === 'rust') {
    // The packaged service has no qualified local model in this harness: the
    // owner turn is still saved, and the reliable job fails closed with a
    // typed capability error instead of ever publishing a candidate.
    const response = page.waitForResponse(res => new URL(res.url()).pathname.includes('/agent/chats/') && res.request().method() === 'POST');
    await page.getByRole('button', { name: 'Send' }).click();
    expect((await response).status()).toBe(202);
    await expect(page.getByRole('alert')).toContainText(/Settings|model/);
    await expect(page.getByRole('heading', { name: 'Proposed candidate · not saved' })).toHaveCount(0);
    return;
  }
  await page.getByRole('button', { name: 'Send' }).click();
  await expect(page.getByRole('status')).toContainText('proposed a candidate', { timeout: 15_000 });
  await expect(page.getByRole('heading', { name: 'Proposed candidate · not saved' })).toBeVisible();

  // Reload: the durable chat and its proposed candidate resume by ID.
  await page.reload();
  await page.getByRole('button', { name: 'Interviewer' }).click();
  await page.getByText('Chat options', { exact: true }).click();
  await page.getByLabel('Saved owner chats').selectOption({ index: 1 });
  await page.getByRole('button', { name: 'Open saved chat' }).click();
  await expect(page.getByRole('heading', { name: 'Proposed candidate · not saved' })).toBeVisible();
});

test('ar04_brief_review_returns_to_matching_context', async ({ page }, testInfo) => {
  await page.goto('/');
  await openParties(page);
  const region = partiesRegion(page);
  await addParty(page, 'AR04 Brief Party');
  await region.getByRole('button', { name: 'AR04 Brief Party' }).click();
  await page.getByRole('button', { name: 'Research chat' }).click();
  await page.getByText('Research scope and brief', { exact: true }).click();
  if (testInfo.project.name === 'rust') {
    // No qualified model: the brief job fails closed; the manual review
    // surface and deterministic list stay available.
    const response = page.waitForResponse(res => new URL(res.url()).pathname.includes('/briefs/') && res.request().method() === 'POST');
    await page.getByRole('button', { name: 'Generate brief proposals' }).click();
    expect((await response).status()).toBe(202);
    await expect(page.getByRole('alert')).toContainText(/Settings|model/);
    return;
  }
  await page.getByRole('button', { name: 'Generate brief proposals' }).click();
  await expect(page.getByRole('button', { name: 'Confirm' }).first()).toBeVisible({ timeout: 15_000 });
  await page.getByRole('button', { name: 'Confirm' }).first().click();
  await expect(page.getByRole('status').filter({ hasText: 'Confirmed as an owner statement' })).toBeVisible();

  // Reload keeps the reviewed proposal and the scoped chat; no proposed item
  // is left open for the same decision.
  await page.reload();
  await page.getByRole('button', { name: 'Research chat' }).click();
  await page.getByText('Research scope and brief', { exact: true }).click();
  await expect(page.getByRole('button', { name: 'Confirm' })).toHaveCount(0);
  await expect(region.getByText(/confirmed/).first()).toBeVisible();
});
