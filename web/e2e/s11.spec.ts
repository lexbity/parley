import { mkdir } from 'node:fs/promises';
import { expect, type Page } from '@playwright/test';
import { openOwnerPage, test, type OwnerSession } from './fixtures';
import { addParty, openParties } from './parties';

async function openDataSettings(page: Page) {
  await page.getByRole('link', { name: 'Settings', exact: true }).click();
  await page.getByRole('navigation', { name: 'Settings sections' }).getByRole('link', { name: 'Backup and data' }).click();
  return page.getByRole('region', { name: 'Backup, export, restore, and erasure' });
}

async function configureBackups(settings: Awaited<ReturnType<typeof openDataSettings>>, directory: string) {
  await mkdir(directory, { recursive: true, mode: 0o700 });
  await settings.getByLabel('Owner-selected backup directory').fill(directory);
  await settings.getByLabel('Backup interval in hours').fill('24');
  await settings.getByLabel('Verified backups to retain').fill('14');
  await settings.getByRole('button', { name: 'Save backup settings' }).click();
  await expect(settings.getByRole('status')).toContainText('schedule saved');
  await settings.getByRole('button', { name: 'Create verified backup now' }).click();
  await expect(settings.getByRole('status')).toHaveText(/Verified backup .+ created\./);
  await expect(settings.getByRole('region', { name: 'Verified backups' })).toContainText('verified');
}

async function awaitMaintenanceRestart(page: Page, ownerSession: OwnerSession, operation: { id: string; epoch: string }) {
  if (!ownerSession.nextBootstrapUrl) return;
  await openOwnerPage(page, await ownerSession.nextBootstrapUrl());
  const session = await (await page.request.get(new URL('/api/v1/session', page.url()).href)).json();
  expect(session.epoch).toBe(operation.epoch);
  const response = await page.request.get(new URL('/api/v1/maintenance/operations/' + operation.id, page.url()).href, {
    headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
  });
  await expect(response).toBeOK();
  expect(await response.json()).toMatchObject({ id: operation.id, state: 'complete' });
}

test('s11_backup_destination_schedule_and_verified_snapshot_are_saved', async ({ page }, testInfo) => {
  await page.goto('/');
  const settings = await openDataSettings(page);
  await configureBackups(settings, testInfo.outputPath('owner-backups'));
  await page.reload();
  await expect(settings.getByLabel('Backup interval in hours')).toHaveValue('24');
  await expect(settings.getByRole('region', { name: 'Verified backups' })).toContainText('verified');
});

test('s11_personal_export_and_empty_target_import_are_distinct_from_recipient_material', async ({ page }) => {
  await page.goto('/');
  const settings = await openDataSettings(page);
  const dataExport = settings.getByRole('region', { name: 'Personal data export' });
  await expect(dataExport).toContainText('does not approve or release recipient-facing content');
  const downloading = page.waitForEvent('download');
  await dataExport.getByRole('button', { name: 'Download personal data export' }).click();
  await expect(settings.getByRole('status')).toContainText('Personal export downloaded');
  const download = await downloading;
  expect(download.suggestedFilename()).toBe('parley-personal-export.zip');
  await settings.getByLabel('Parley export archive').setInputFiles(await download.path());
  await settings.getByRole('button', { name: 'Validate import archive' }).click();
  const confirm = settings.getByLabel('I confirm this target workspace is empty.');
  await expect(confirm).toBeVisible();
  const commit = settings.getByRole('button', { name: 'Commit full data import' });
  await expect(commit).toBeDisabled();
  await confirm.check();
  await commit.click();
  await expect(settings.getByRole('status')).toContainText('session reset is required');
});

test('s11_restore_preview_commit_rotates_session_and_reports_erasure_effects', async ({ page, ownerSession }, testInfo) => {
  await page.goto('/');
  const settings = await openDataSettings(page);
  await configureBackups(settings, testInfo.outputPath('owner-backups'));
  await settings.getByRole('button', { name: 'Verify restore preview' }).click();
  await expect(settings.getByText(/0 erased records may return/)).toBeVisible();
  const commit = settings.getByRole('button', { name: 'Commit verified restore' });
  await expect(commit).toBeDisabled();
  await settings.getByLabel('I understand this replaces current data and may restore erased information.').check();
  const committed = page.waitForResponse(response => response.url().endsWith('/api/v1/restore/commit'));
  await commit.click();
  await expect(settings.getByRole('status')).toContainText('epoch changed');
  await awaitMaintenanceRestart(page, ownerSession, await (await committed).json());
});

test('s11_party_erasure_is_preview_bound_and_reports_external_copies', async ({ page, ownerSession }) => {
  await page.goto('/');
  const name = 'Dana Lee';
  await openParties(page);
  await addParty(page, name);
  await addParty(page, 'Mira Chen');
  const settings = await openDataSettings(page);
  const erasure = settings.getByRole('region', { name: 'Erase a party', exact: true });
  const picker = erasure.getByRole('combobox', { name: 'Party', exact: true });
  await picker.selectOption({ label: name + ' · person' });
  await erasure.getByRole('button', { name: 'Preview erasure' }).click();
  const confirmation = erasure.getByLabel('Type exact party name');
  await expect(confirmation).toBeFocused();
  await picker.selectOption({ label: 'Mira Chen · person' });
  await expect(confirmation).toBeHidden();
  await picker.selectOption({ label: name + ' · person' });
  await erasure.getByRole('button', { name: 'Preview erasure' }).click();
  await expect(erasure.getByText(/shared sources.*owner-managed backups/)).toBeVisible();
  const commit = erasure.getByRole('button', { name: 'Erase ' + name, exact: true });
  await expect(commit).toBeDisabled();
  await confirmation.fill(name);
  const committed = page.waitForResponse(response => response.url().endsWith('/erase'));
  await commit.click();
  const response = await committed;
  expect(response.status()).toBe(202);
  const operation = await response.json();
  expect(operation).toMatchObject({ state: 'pending_restart', external_copies_requiring_disposal: expect.any(Number) });
  await expect(settings.getByRole('status')).toContainText('Erasure committed');
  await awaitMaintenanceRestart(page, ownerSession, operation);
  await openParties(page);
  const sheet = page.getByRole('region', { name: 'Party sheet', exact: true });
  await expect(sheet.getByRole('button', { name: 'Mira Chen', exact: true })).toBeVisible();
  await expect(sheet.getByRole('button', { name, exact: true })).toHaveCount(0);
});
