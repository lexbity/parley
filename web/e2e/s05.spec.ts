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
  const queue = page.getByRole('region', { name: 'Opportunities' });
  await queue.getByRole('button', { name: 'New opportunity' }).click();
  await queue.getByLabel('Opportunity title').fill('Onboarding map');
  await queue.getByLabel('Participant').selectOption({ label: `${partyName} · person` });
  await queue.getByRole('button', { name: 'Create opportunity' }).click();
  await expect(queue.getByRole('heading', { name: 'Onboarding map' })).toBeVisible();

  await queue.getByRole('button', { name: 'Manage lists' }).click();
  await queue.getByLabel('New list').fill('Friday follow-ups');
  await queue.getByRole('button', { name: 'Add list' }).click();
  await queue.getByText('Organize card').click();
  const moveToList = queue.getByLabel('Move Onboarding map to list');
  await moveToList.selectOption({ label: 'Friday follow-ups' });
  await queue.getByRole('button', { name: 'Move card' }).click();
  await expect(queue.getByRole('region', { name: 'Friday follow-ups list' })).toContainText('Onboarding map');

  await queue.getByRole('region', { name: 'Friday follow-ups list' }).getByText('List options').click();
  await queue.getByLabel('Rename Friday follow-ups').fill('Friday');
  await queue.getByRole('button', { name: 'Save list name' }).last().click();
  await queue.getByRole('button', { name: 'Move Friday up' }).click();
  const friday = queue.getByRole('region', { name: 'Friday list' });
  await friday.getByRole('button', { name: 'Open card' }).click();

  const markdown = '🧭\n- [ ] Prepare map\n\n```md\n- [ ] Literal example\n```\n';
  const notes = queue.getByRole('region', { name: 'Notes and tasks' });
  await notes.getByLabel('Markdown notes').fill(markdown);
  await notes.getByRole('button', { name: 'Save notes' }).click();
  await expect(notes.locator('.cm-line')).toHaveText(markdown.split('\n'));
  await expect(notes.locator('pre')).toContainText('- [ ] Literal example');
  await expect(notes.getByRole('checkbox', { name: 'Prepare map', exact: true })).toHaveCount(1);
  await notes.getByRole('checkbox', { name: 'Prepare map', exact: true }).check();
  await expect(notes.locator('.cm-line')).toHaveText(markdown.replace('[ ] Prepare', '[x] Prepare').split('\n'));
  await expect(notes.locator('.cm-content')).toContainText('- [ ] Literal example');

  const archiveDestination = queue.getByLabel('Archive destination for Friday');
  await archiveDestination.selectOption({ label: 'Inbox' });
  await queue.getByRole('region', { name: 'Friday list' }).getByRole('button', { name: 'Archive list' }).click();
  await expect(queue.getByRole('region', { name: 'Friday list' })).toHaveCount(0);
  await expect(queue.getByRole('region', { name: 'Inbox list' })).toContainText('Onboarding map');
});
