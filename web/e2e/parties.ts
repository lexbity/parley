import type { Locator, Page } from '@playwright/test';

/**
 * Shared Parties-workspace selectors (amendment 2026-11-22: the former Leads
 * tab is renamed Parties; legacy /leads deep links remain aliases).
 */
export function openParties(page: Page): Promise<void> {
  return page.getByRole('link', { name: 'Parties' }).click();
}

export function partiesRegion(page: Page): Locator {
  return page.getByRole('region', { name: 'Party sheet, neighborhood and inspector' });
}

export function addPartyForm(page: Page): Locator {
  return partiesRegion(page).locator('form.add-party');
}

export async function addParty(page: Page, name: string, kind: 'person' | 'org' = 'person', notes?: string): Promise<void> {
  const region = partiesRegion(page);
  await region.getByRole('button', { name: 'Add party' }).first().click();
  const form = addPartyForm(page);
  await form.getByLabel('Type').selectOption(kind);
  await form.getByLabel('Name').fill(name);
  if (notes) await form.getByRole('textbox', { name: 'Owner notes' }).fill(notes);
  await form.getByRole('button', { name: /^(Add party|Create distinct party)$/ }).click();
}

export function deskStatus(page: Page): Locator {
  return partiesRegion(page).locator('[role="status"]');
}
