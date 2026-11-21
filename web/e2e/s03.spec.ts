import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';

test('s03_create_opportunity_for_selected_participant_without_creating_a_send', async ({ page }) => {
  await page.goto('/');
  const name = `Dana Lee ${randomUUID()}`;
  await page.getByRole('button', { name: 'Leads' }).click();
  const leads = page.locator('section[aria-labelledby="leads-heading"]');
  const addLead = page.getByRole('heading', { name: 'Add lead' }).locator('..');
  await addLead.getByLabel('Name').fill(name);
  await addLead.getByRole('button', { name: 'Add lead' }).click();
  await expect(leads.getByRole('status')).toContainText('No opportunity or outreach was created');

  await page.getByRole('button', { name: 'Queue' }).click();
  await page.getByLabel('Opportunity title').fill('Onboarding exchange');
  await page.getByLabel('Participant').selectOption({ label: `${name} · person` });
  await page.getByRole('button', { name: 'Create opportunity' }).click();
  await expect(page.getByRole('status')).toContainText('No message was created or sent');
  await expect(page.getByRole('heading', { name: 'Onboarding exchange' })).toBeVisible();

  await page.getByRole('button', { name: 'Output' }).click();
  await expect(page.getByText('No materials yet')).toBeVisible();
});
