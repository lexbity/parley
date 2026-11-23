import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty } from './parties';

test('p03_owner_file_binding_download_and_unlink', async ({ page }) => {
  await page.goto('/parties');
  await addParty(page, 'Fieldwork', 'org');
  await page.getByRole('button', { name: 'Open selected card' }).click();
  const files = page.getByRole('region', { name: 'Private card files' });
  const bytes = Buffer.from('fixture content\n');
  await files.getByLabel('Attach files').setInputFiles({ name: 'CSV fixture.txt', mimeType: 'text/plain', buffer: bytes });
  await files.getByRole('button', { name: 'Attach CSV fixture.txt' }).click();
  const download = files.getByRole('button', { name: 'Download CSV fixture.txt' });
  await expect(download).toBeVisible();
  const item = page.waitForEvent('download');
  await download.click();
  const stream = await (await item).createReadStream();
  const received = [];
  for await (const part of stream!) received.push(part);
  expect(Buffer.concat(received)).toEqual(bytes);
  await files.getByRole('button', { name: 'Unlink' }).click();
  await expect(download).toHaveCount(0);
});
