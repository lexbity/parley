import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';

test('s05_keyboard_board_list_card_order_and_archive', async ({ page }) => {
  await page.goto('/');
  const suffix = randomUUID();
  const partyName = `Dana Lee ${suffix}`;
  await page.getByRole('link', { name: 'Leads' }).click();
  const leads = page.getByRole('region', { name: 'Lead directory and sources' });
  await leads.getByRole('complementary').getByLabel('Name').fill(partyName);
  await leads.getByRole('button', { name: 'Add lead' }).click();

  await expect(leads.getByRole('button', { name: partyName + ' person' })).toBeVisible();
  await leads.getByLabel('Find a lead').fill(suffix);
  await leads.getByRole('button', { name: partyName + ' person' }).click();
  const leadUrl = page.url();
  await page.getByRole('link', { name: 'Resources' }).click();
  await page.getByRole('link', { name: 'Leads' }).click();
  await expect(page).toHaveURL(leadUrl);
  await expect(leads.getByLabel('Find a lead')).toHaveValue(suffix);
  await page.getByRole('link', { name: 'Queue' }).click();
  const queue = page.getByRole('region', { name: 'Opportunities' });
  await queue.getByLabel('Opportunity title').fill('Onboarding map');
  await queue.getByLabel('Participant').selectOption({ label: `${partyName} · person` });
  await queue.getByRole('button', { name: 'Create opportunity' }).click();
  await expect(queue.getByRole('heading', { name: 'Onboarding map' })).toBeVisible();

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
