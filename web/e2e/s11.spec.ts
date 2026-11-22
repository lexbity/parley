import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';

async function confirmPreview(region: import('@playwright/test').Locator, buttonName: string) {
  const panel = region.getByRole('article', { name: /Review before/ });
  const digest = await panel.locator('code').innerText();
  await panel.getByLabel(new RegExp('Confirm digest')).fill(digest.slice(-12));
  await panel.getByRole('button', { name: buttonName }).click();
}

test('s11_verified_backup_preview_restore_and_party_erasure', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByRole('status')).toContainText('Workspace ready');
  const partyName = `Dana Lee ${randomUUID()}`;
  await page.getByRole('link', { name: 'Leads' }).click();
  const addLead = page.getByRole('heading', { name: 'Add a lead' }).locator('..');
  await addLead.getByLabel('Name').fill(partyName);
  await addLead.getByRole('button', { name: 'Add lead' }).click();
  await expect(page.getByRole('button', { name: new RegExp(partyName) })).toBeVisible();

  await page.getByRole('link', { name: 'Settings' }).click();
  const data = page.getByRole('region', { name: 'Workspace backup, restore and erasure' });
  await data.getByRole('button', { name: 'Create and verify backup' }).click();
  await expect(data.getByRole('status').filter({ hasText: 'Verified backup created' })).toBeVisible();
  const backupSelect = data.locator('select').nth(1);
  const backupOption = await backupSelect.locator('option').first().getAttribute('value');

  await data.getByLabel('Operation').selectOption('erase_party');
  const partyOption = data.getByLabel('Party to erase').locator('option').filter({ hasText: partyName }).first();
  await data.getByLabel('Party to erase').selectOption(await partyOption.getAttribute('value') ?? '');
  await data.getByRole('button', { name: 'Preview exact impact' }).click();
  await expect(data.getByText('Party: ' + partyName)).toBeVisible();
  await confirmPreview(data, 'Confirm erase this party');
  await expect(data.getByRole('status').filter({ hasText: 'Party-scoped records were erased' })).toBeVisible();

  await page.getByRole('link', { name: 'Queue' }).click();
  await page.getByRole('button', { name: 'Daily' }).click();
  await expect(page.getByRole('heading', { name: 'Daily priorities' })).toBeVisible();
  await expect(page.getByText(new RegExp(partyName))).toHaveCount(0);
  await page.getByRole('button', { name: 'Insights' }).click();
  await expect(page.getByRole('heading', { name: 'Weekly Coach and insights' })).toBeVisible();
  await expect(page.getByText(new RegExp(partyName))).toHaveCount(0);

  await page.getByRole('link', { name: 'Settings' }).click();
  await data.getByLabel('Operation').selectOption('restore');
  await backupSelect.selectOption(backupOption!);
  await data.getByRole('button', { name: 'Preview exact impact' }).click();
  await expect(data.getByText(/Restore source:/)).toBeVisible();
  await confirmPreview(data, 'Confirm restore this backup');
  await expect(data.getByRole('status').filter({ hasText: 'Workspace restored and verified' })).toBeVisible();

  await page.getByRole('link', { name: 'Leads' }).click();
  await expect(page.getByRole('button', { name: new RegExp(partyName) })).toHaveCount(0);
});
