import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, partiesRegion } from './parties';

/** AR06 owner flow: capture works independently of inference, the staged
 *  proposal review returns reliably, and the typed job surface shows a
 *  common working state. The mock project drives observable proposal state;
 *  the Rust project exercises the real HTTP boundary without a qualified
 *  local pairing (fail-closed, source-only). */

async function saveSource(page: import('@playwright/test').Page, text: string) {
  await page.goto('/parties');
  const workspace = partiesRegion(page);
  await workspace.getByRole('button', { name: 'Sources inbox' }).click();
  await workspace.getByLabel('Captured text').fill(text);
  await workspace.getByRole('button', { name: 'Save source', exact: true }).click();
  await expect(workspace.getByText(/Saved source .* occurrence/)).toBeVisible();
  return workspace;
}

test('ar06_capture_activity_returns_to_review', async ({ page }, testInfo) => {
  await page.goto('/parties');
  await addParty(page, 'Fieldwork', 'org');
  const workspace = await saveSource(page, 'Dana Lee helps with onboarding.\nFieldwork is hiring.');
  const review = workspace.getByRole('region', { name: 'Capture review', exact: true });

  if (testInfo.project.name === 'mock') {
    const hiring = review.locator('article').filter({ hasText: 'Fieldwork is hiring.' });
    await expect(hiring).toBeVisible({ timeout: 15_000 });
    await review.getByLabel('Claim subject').selectOption({ label: 'Fieldwork · org' });
    await hiring.getByRole('button', { name: 'Accept claim for selected subject' }).click();
    await expect(hiring).toContainText('accepted');

    // Reload returns to the same durable review state by occurrence id.
    await page.reload();
    await page.goto('/parties');
    const afterReload = partiesRegion(page);
    await afterReload.getByRole('button', { name: 'Sources inbox' }).click();
    await afterReload.getByRole('button', { name: 'Review proposals' }).first().click();
    const restored = afterReload.getByRole('region', { name: 'Capture review', exact: true });
    await expect(restored.locator('article').filter({ hasText: 'Fieldwork is hiring.' })).toContainText('accepted');
  } else {
    // No qualified local pairing in this harness: the source survives on its
    // own and no proposal review surface is offered.
    await expect(review.getByRole('button', { name: 'Accept claim for selected subject' })).toHaveCount(0);
  }
});

test('ar06_provider_disabled_source_survives', async ({ page }, testInfo) => {
  const workspace = await saveSource(page, 'A saved capture stays useful without inference.');
  const review = workspace.getByRole('region', { name: 'Capture review', exact: true });
  if (testInfo.project.name === 'mock') {
    // The mock still exposes extraction state; the owner can review it.
    await expect(review).toBeVisible();
  } else {
    await expect(review.getByRole('button', { name: 'Accept claim for selected subject' })).toHaveCount(0);
  }
});
