import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties } from './parties';

test('s09_recipient_exchange_template_lifecycle_and_uncalibrated_manual_path', async ({ page }) => {
  await page.goto('/');
  const name = `Dana Lee ${randomUUID()}`;
  await openParties(page);
  await addParty(page, name, 'person');

  await page.getByRole('link', { name: 'Queue' }).click();
  const queue = page.getByRole('region', { name: 'Opportunities' });
  await queue.getByRole('button', { name: 'New opportunity' }).click();
  await queue.getByLabel('Opportunity title').fill('S09 writing opportunity');
  await queue.getByLabel('Participant').selectOption({ label: `${name} · person` });
  await queue.getByRole('button', { name: 'Create opportunity' }).click();

  await page.getByRole('link', { name: 'Output' }).click();
  const output = page.getByRole('region', { name: 'Composer, evidence and templates' });
  await output.getByLabel('Opportunity').selectOption({ label: 'S09 writing opportunity' });
  await output.getByLabel('Named participant').selectOption({ label: name });
  await output.getByLabel('What they may need').fill('A clearer onboarding checklist');
  await output.getByRole('button', { name: 'Save as a hypothesis' }).click();
  await output.getByLabel('What you need').fill('A useful conversation');
  await output.getByRole('button', { name: 'Save owner-reported interest' }).click();
  await output.getByLabel('They get').fill('A concise checklist');
  await output.getByLabel('You get').fill('A chance to learn whether the work fits');
  await output.getByRole('button', { name: 'Save exchange version' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Exchange version 1 saved' })).toBeVisible();
  await output.getByRole('button', { name: 'Attest both sides benefit' }).click();
  await expect(output.getByText(/attested/).first()).toBeVisible();

  await output.getByRole('button', { name: 'Save as new draft version' }).click();
  await expect(output.getByText(/Immutable draft version saved/)).toBeVisible();
  await output.getByRole('button', { name: 'Owner approve version' }).click();
  await output.getByRole('button', { name: 'Activate validated version' }).click();
  await expect(output.locator('select').last()).toContainText('active');
  await expect(output.getByText(/calibrated kinds: none/)).toBeVisible();
  await expect(output.getByRole('button', { name: 'Run exact matcher' })).toBeDisabled();
  await expect(output.getByRole('button', { name: 'Generate recipient-bound proposal' })).toBeDisabled();
  await expect(output.getByRole('button', { name: /release/i })).toHaveCount(0);
});
