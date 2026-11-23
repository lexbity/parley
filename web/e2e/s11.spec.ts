import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties } from './parties';

test('s11_backup_destination_schedule_and_verified_snapshot_are_saved', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('link', { name: 'Settings' }).click();
  const settings = page.getByRole('region', { name: 'Backup, export, restore, and erasure' });
  await settings.getByLabel('Owner-selected backup directory').fill('/tmp/parley-test-backups');
  await settings.getByLabel('Backup interval in hours').fill('24');
  await settings.getByLabel('Verified backups to retain').fill('14');
  await settings.getByRole('button', { name: 'Save backup settings' }).click();
  await expect(settings.getByRole('status')).toContainText('schedule saved');
  await settings.getByRole('button', { name: 'Create verified backup now' }).click();
  await expect(settings.getByRole('region', { name: 'Verified backups' })).toContainText('verified');
});

test('s11_personal_export_and_empty_target_import_are_distinct_from_recipient_material', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('link', { name: 'Settings' }).click();
  const settings = page.getByRole('region', { name: 'Backup, export, restore, and erasure' });
  await expect(settings.getByRole('region', { name: 'Personal data export' })).toContainText('does not approve or release recipient-facing content');
  await settings.getByRole('button', { name: 'Download personal data export' }).click();
  await expect(settings.getByRole('status')).toContainText('Personal export downloaded');
  await settings.getByLabel('Parley export archive').setInputFiles({ name: 'owner-data.zip', mimeType: 'application/zip', buffer: Buffer.from('fixture archive') });
  await settings.getByRole('button', { name: 'Validate import archive' }).click();
  await expect(settings).toContainText('Preview import-preview-1');
  await settings.getByLabel('I confirm this target workspace is empty.').check();
  await settings.getByRole('button', { name: 'Commit full data import' }).click();
  await expect(settings.getByRole('status')).toContainText('session reset is required');
});

test('s11_restore_preview_commit_rotates_session_and_reports_erasure_effects', async ({ page }) => {
  await page.route('**/api/v1/settings/maintenance', async (route) => {
    await page.waitForTimeout(500);
    await route.continue();
  });
  await page.goto('/');
  await page.getByRole('link', { name: 'Settings' }).click();
  const settings = page.getByRole('region', { name: 'Backup, export, restore, and erasure' });
  await settings.getByLabel('Owner-selected backup directory').fill('/tmp/parley-test-backups');
  await settings.getByRole('button', { name: 'Save backup settings' }).click();
  await settings.getByRole('button', { name: 'Create verified backup now' }).click();
  await settings.getByRole('button', { name: 'Verify restore preview' }).click();
  await expect(settings).toContainText('1 erased records may return');
  await settings.getByLabel('I understand this replaces current data and may restore erased information.').check();
  await settings.getByRole('button', { name: 'Commit verified restore' }).click();
  await expect(settings.getByRole('status')).toContainText('epoch changed');
});

test('s11_party_erasure_is_preview_bound_and_reports_external_copies', async ({ page }) => {
  await page.goto('/');
  const name = `S11 erase ${randomUUID()}`;
  await openParties(page);
  await addParty(page, name, 'person');
  await page.getByRole('link', { name: 'Settings' }).click();
  const settings = page.getByRole('region', { name: 'Backup, export, restore, and erasure' });
  await settings.getByRole('region', { name: 'Erase a party', exact: true }).getByLabel('Party').selectOption({ label: `${name} · person` });
  await settings.getByRole('button', { name: 'Preview erasure' }).click();
  await expect(settings).toContainText('1 shared sources');
  await settings.getByLabel('Type exact party name').fill(name);
  await settings.getByRole('button', { name: `Erase ${name}` }).click();
  await expect(settings.getByRole('status')).toContainText('Erasure completed');
  await expect(settings.getByRole('status')).toContainText('2 owner-managed copies');
});
