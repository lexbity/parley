import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties, partiesRegion } from './parties';

test('s05_keyboard_board_list_card_order_and_archive', async ({ page }) => {
  await page.goto('/');
  const suffix = randomUUID();
  const partyName = `Dana Lee ${suffix}`;
  await openParties(page);
  const parties = partiesRegion(page);
  await addParty(page, partyName, 'person');

  await expect(parties.getByRole('button', { name: partyName })).toBeVisible();
  await parties.getByLabel('Find a party').fill(suffix);
  await parties.getByRole('button', { name: partyName }).click();
  await page.getByRole('link', { name: 'Resources' }).click();
  await page.getByRole('link', { name: 'Parties' }).click();
  await expect(parties.getByLabel('Find a party')).toHaveValue(suffix);
  await expect(parties.getByRole('button', { name: partyName })).toHaveAttribute('aria-pressed', 'true');
  await page.getByRole('link', { name: 'Queue' }).click();

  // The Queue is a sheet now: no board, custom list management or drag ordering.
  await expect(page.locator('nav[aria-label="Queue views"]')).not.toContainText('Board');
  await expect(page.getByRole('button', { name: 'Manage lists' })).toHaveCount(0);

  const sheet = page.getByRole('region', { name: 'Scenario sheet' });
  for (const title of ['Onboarding map', 'Fieldwork review']) {
    await page.getByRole('button', { name: 'New opportunity' }).click();
    await page.getByLabel('Opportunity title').fill(title);
    await page.getByLabel('Participant').selectOption({ label: `${partyName} · person` });
    await page.getByRole('button', { name: 'Create opportunity' }).click();
    await expect(sheet.getByText(title)).toBeVisible();
  }

  // Keyboard: Enter activates a row and opens its inspector.
  await sheet.getByRole('button', { name: 'Onboarding map', exact: true }).focus();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('complementary', { name: 'Scenario details' })).toContainText('Onboarding map');

  // Phase edit through the native row select.
  await page.getByRole('combobox', { name: 'Phase for Onboarding map', exact: true }).selectOption('preparing');
  await expect(page.getByText('moved to Preparing')).toBeVisible();
  await page.getByLabel('Phase filter').selectOption('preparing');
  await expect(sheet.locator('tbody tr[data-opportunity-id]')).toHaveCount(1);

  // Changing phase through the row select leaves the filter; the announcement
  // names the filter and the sheet stays browsable.
  await page.getByRole('combobox', { name: 'Phase for Onboarding map', exact: true }).selectOption('in_conversation');
  await expect(page.getByText('is hidden by the Preparing filter')).toBeVisible();
  await expect(sheet.locator('tbody tr[data-opportunity-id]')).toHaveCount(0);

  // Undo is offered in the persistent toast and restores the prior phase.
  await page.getByRole('button', { name: 'Undo phase change' }).click();
  await expect(page.getByText('returned to Preparing')).toBeVisible();
  await expect(sheet.locator('tbody tr[data-opportunity-id]')).toHaveCount(1);

  // Escape dismisses an uncommitted choice/dialog; nothing changes.
  await page.keyboard.press('Escape');
  await expect(sheet.locator('tbody tr[data-opportunity-id]')).toHaveCount(1);

  // Clear the filter; name sort restores both rows deterministically.
  await page.getByLabel('Phase filter').selectOption('');
  await expect(sheet.locator('tbody tr[data-opportunity-id]')).toHaveCount(2);
  await page.getByLabel('Order').selectOption('Scenario name');
  await expect(sheet.locator('tbody tr[data-opportunity-id] button.open')).toHaveText(['Fieldwork review', 'Onboarding map']);
});
