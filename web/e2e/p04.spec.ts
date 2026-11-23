import { expect } from '@playwright/test';
import { test } from './fixtures';

test('p04_csv_mapping_duplicate_decisions_and_receipt', async ({ page }) => {
  await page.goto('/parties');
  await page.getByRole('button', { name: 'Import CSV' }).click();
  const dialog = page.getByRole('dialog', { name: 'Import parties from CSV' });
  const csv = '\uFEFFName,Focus,Notes,Notes\r\n"Alder, Inc",Docs,"First\r\nsecond",skip\r\nMira,CSV,One,skip\r\nMira,Tools,Two,skip\r\n,missing,Missing,skip\r\n';
  await dialog.getByLabel('CSV file').setInputFiles({ name: 'parties.csv', mimeType: 'text/csv', buffer: Buffer.from(csv) });
  await dialog.getByRole('button', { name: 'Read columns' }).click();
  await dialog.getByRole('combobox', { name: 'Focus column', exact: true }).selectOption('1');
  await dialog.getByRole('combobox', { name: 'Markdown notes column', exact: true }).selectOption('2');
  await expect(dialog.getByText('Skipped columns · 4 · Notes')).toBeVisible();
  await dialog.getByRole('button', { name: 'Preview mapped rows' }).click();
  await expect(dialog.getByRole('button', { name: /Import \d+ parties/ })).toBeDisabled();
  await dialog.getByLabel('Decision for record 3').selectOption('distinct');
  await dialog.getByLabel('Decision for record 4').selectOption('skip');
  await dialog.getByLabel('Decision for record 5').selectOption('skip');
  await dialog.getByRole('button', { name: 'Import 2 parties' }).click();
  await expect(dialog.getByRole('status')).toContainText('2 created · 2 skipped');
  await dialog.getByRole('button', { name: 'View imported parties' }).click();
  const sheet = page.getByRole('region', { name: 'Party sheet', exact: true });
  await expect(sheet.getByRole('button', { name: 'Alder, Inc', exact: true })).toBeVisible();
  await expect(sheet.getByRole('button', { name: 'Mira', exact: true })).toHaveCount(1);
});

test('p04_recovers_commit_after_disconnect_and_reload', async ({ page }) => {
  await page.goto('/parties');
  await page.getByRole('button', { name: 'Import CSV' }).click();
  const dialog = page.getByRole('dialog', { name: 'Import parties from CSV' });
  await dialog.getByLabel('Or paste CSV').fill('Name\nFieldwork\n');
  await dialog.getByRole('button', { name: 'Read columns' }).click();
  await dialog.getByRole('button', { name: 'Preview mapped rows' }).click();
  let originalKey: string | undefined;
  const commitUrl = '**/api/v1/party-imports/*/commit';
  await page.route(commitUrl, async route => {
    originalKey = route.request().headers()['idempotency-key'];
    const result = await route.fetch();
    expect(result.ok()).toBeTruthy();
    await route.abort('failed');
  });
  await dialog.getByRole('button', { name: 'Import 1 parties' }).click();
  await expect(dialog.getByRole('alert')).toBeVisible();
  await page.unroute(commitUrl);
  await page.reload();
  await page.getByRole('button', { name: 'Import CSV' }).click();
  const retry = page.waitForRequest(request => request.url().endsWith('/commit'));
  await dialog.getByRole('button', { name: 'Check import result' }).click();
  expect((await retry).headers()['idempotency-key']).toBe(originalKey);
  await expect(dialog.getByRole('status')).toContainText('1 created · 0 skipped');
  await dialog.getByRole('button', { name: 'View imported parties' }).click();
  await expect(page.getByRole('region', { name: 'Party sheet', exact: true }).getByRole('button', { name: 'Fieldwork', exact: true })).toHaveCount(1);
});
