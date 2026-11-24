import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties, partiesRegion } from './parties';

async function openResources(page: import('@playwright/test').Page) {
  await page.goto('/');
  await page.getByRole('link', { name: 'Resources' }).click();
  await expect(page.getByRole('heading', { name: 'Your resources' })).toBeVisible();
}

test('ar04_interviewer_resumes_after_reload', async ({ page }) => {
  await openResources(page);
  await page.getByRole('button', { name: 'Interviewer' }).click();
  await page.getByRole('button', { name: 'Start Interviewer' }).click();
  await page.getByLabel('Your answer').fill('I helped a teammate debug a release last quarter.');
  await page.getByRole('button', { name: 'Send' }).click();
  await expect(page.getByRole('status')).toContainText('proposal remains pending review');
  await expect(page.getByRole('heading', { name: 'Proposed candidate · not saved' })).toBeVisible({ timeout: 15_000 });

  // Reload: the durable chat and its proposed candidate resume by ID.
  await page.reload();
  await page.getByRole('button', { name: 'Interviewer' }).click();
  await page.getByLabel('Saved owner chats').selectOption({ index: 1 });
  await page.getByRole('button', { name: 'Open saved chat' }).click();
  await expect(page.getByRole('heading', { name: 'Proposed candidate · not saved' })).toBeVisible();
  await expect(page.getByLabel('Title')).toHaveValue(/./);
});

test('ar04_brief_review_returns_to_matching_context', async ({ page }) => {
  await page.goto('/');
  await openParties(page);
  const region = partiesRegion(page);
  await addParty(page, 'AR04 Brief Party');
  const partyRow = region.getByRole('button', { name: 'AR04 Brief Party' });
  await partyRow.click();
  await page.getByText('Research scope and brief', { exact: true }).click();
  await page.getByRole('button', { name: 'Generate brief proposals' }).click();
  await expect(page.getByRole('button', { name: 'Confirm' }).first()).toBeVisible({ timeout: 15_000 });
  await page.getByRole('button', { name: 'Confirm' }).first().click();
  await expect(page.getByRole('status')).toContainText('Confirmed as an owner statement');

  // Reload keeps the reviewed proposal and the scoped chat.
  await page.reload();
  await page.getByText('Research scope and brief', { exact: true }).click();
  await expect(page.getByRole('button', { name: 'Confirm' })).toHaveCount(0);
  await expect(region.getByText('Confirmed as an owner statement').first()).toBeHidden();
  await expect(page.getByText('Confirmed')).toHaveCount(1);
});
