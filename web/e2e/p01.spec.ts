import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, deskStatus, openParties, partiesRegion } from './parties';

test('p01_party_sheet_rename_alias_focus_edit_and_card_notes', async ({ page }) => {
  // Legacy /leads deep links resolve to the Parties tab.
  await page.goto('/leads');
  await expect(page.getByRole('heading', { name: 'Parties', exact: true })).toBeVisible();

  const suffix = randomUUID();
  const orgName = `Fieldwork ${suffix}`;
  await addParty(page, orgName, 'org');
  const region = partiesRegion(page);
  const row = region.getByRole('button', { name: orgName });
  await expect(row).toBeVisible();

  // Row selection scopes the desk; the focus cell is editable through the
  // typed party PATCH.
  await row.click();
  await region.getByRole('button', { name: 'Add focus' }).click();
  const focusInput = region.getByLabel(`Focus for ${orgName}`);
  await focusInput.fill('CSV tools');
  await focusInput.press('Enter');
  await expect(deskStatus(page)).toContainText('Focus saved');

  // The card editor opens anchored to the selected node and shows notes.
  await region.getByRole('button', { name: 'Open card' }).click();
  await expect(page.getByRole('region', { name: 'Card editor' })).toBeVisible();
  await expect(page.getByRole('heading', { name: /Edit card ·/ })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Save notes' })).toBeVisible();

  // Tab switcher and the graph heading use the renamed surface.
  await page.getByRole('link', { name: 'Resources' }).click();
  await openParties(page);
  await expect(region.getByRole('heading', { name: 'Party network' })).toBeVisible();
});
