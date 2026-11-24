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
  await expect(queue.getByRole('heading', { name: 'S09 writing opportunity' })).toBeVisible();

  await page.getByRole('link', { name: 'Output' }).click();
  const workspace = page.getByRole('region', { name: 'Output workspace' });
  await workspace.getByRole('combobox', { name: 'Opportunity', exact: true }).selectOption({ label: 'S09 writing opportunity' });
  await workspace.getByRole('combobox', { name: 'Recipient', exact: true }).selectOption({ label: name });
  await expect(workspace.getByRole('heading', { name: 'Opportunity workspace' })).toBeVisible();

  // Interests and exchange are one reviewed scope record with an explicit
  // owner attestation.
  await workspace.getByRole('button', { name: 'Context', exact: true }).click();
  await workspace.getByRole('button', { name: 'Interests and exchange' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('What they may need').fill('A clearer onboarding checklist');
  await dialog.getByLabel('What you need').fill('A useful conversation');
  await dialog.getByLabel('They get', { exact: true }).fill('A concise checklist');
  await dialog.getByLabel('You get', { exact: true }).fill('A chance to learn whether the work fits');
  await dialog.getByRole('button', { name: 'Save exchange version' }).click();
  await expect(dialog.getByRole('status')).toContainText('Exchange version saved');
  await dialog.getByLabel(/I reviewed this saved exchange/).check();
  await dialog.getByRole('button', { name: 'Attest saved exchange' }).click();
  await expect(workspace.getByRole('status')).toContainText('Exchange attested');

  // The template library owns the immutable version lifecycle.
  await workspace.getByRole('button', { name: 'Insert', exact: true }).click();
  await workspace.getByRole('button', { name: 'Template library' }).click();
  const library = page.getByRole('dialog');
  await library.getByRole('button', { name: 'Save as new draft version' }).click();
  await expect(library.getByRole('status')).toContainText('Immutable draft version saved');
  await library.getByRole('button', { name: 'Owner approve version' }).click();
  await library.getByRole('button', { name: 'Activate validated version' }).click();
  await expect(library.getByRole('button', { name: 'Activate validated version' })).toBeDisabled();
  await library.getByRole('button', { name: 'Close', exact: true }).click();

  // Without an embedding digest nothing can rank: the library offers the
  // manual path and the calibrated matcher stays closed.
  await expect(workspace.getByText('This scope has no assessment yet')).toBeVisible();
  await workspace.getByRole('button', { name: 'Run matching' }).first().click();
  const matching = page.getByRole('dialog');
  await expect(matching.getByText('Without a matching model calibration, the system cannot rank resources.')).toBeVisible();
  await expect(matching.getByRole('button', { name: 'Run matching' })).toBeDisabled();
  await matching.getByRole('button', { name: 'Choose manually' }).click();
  await expect(workspace.getByText('Showing eligible resources in manual selection order.')).toBeVisible();
  await expect(workspace.getByText('No resources match this filter.')).toBeVisible();

  // Compose needs the local model, and release stays behind a saved and
  // approved exact version.
  await workspace.getByRole('button', { name: 'Review', exact: true }).click();
  await workspace.getByRole('button', { name: 'Generate a proposal' }).click();
  await expect(page.getByRole('dialog').getByRole('button', { name: 'Generate' })).toBeDisabled();
  await page.getByRole('dialog').getByRole('button', { name: 'Close', exact: true }).click();
  await workspace.getByRole('button', { name: 'Review', exact: true }).click();
  await workspace.getByRole('button', { name: 'Copy or download approved text' }).click();
  const release = page.getByRole('dialog');
  await expect(release.getByText(/approve that exact version in Gate checks/)).toBeVisible();
  await expect(release.getByRole('button', { name: 'Copy approved text' })).toBeDisabled();
  await release.getByRole('button', { name: 'Close', exact: true }).click();
  await expect(workspace.getByRole('button', { name: /Copy approved text|Download approved text/ })).toHaveCount(0);
});
