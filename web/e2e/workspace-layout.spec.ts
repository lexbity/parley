import { expect, type Locator } from '@playwright/test';
import { test } from './fixtures';
import { addParty, partiesRegion } from './parties';

async function beside(left: Locator, right: Locator) {
  await expect(left).toBeVisible();
  await expect(right).toBeVisible();
  const a = (await left.boundingBox())!;
  const b = (await right.boundingBox())!;
  expect(Math.abs(a.y - b.y)).toBeLessThan(2);
  expect(a.x + a.width).toBeLessThanOrEqual(b.x);
}

test('desktop workspaces retain their pane layout with an empty inventory', async ({ page }) => {
  await page.setViewportSize({ width: 1500, height: 800 });
  await page.goto('/resources');
  await beside(page.getByRole('region', { name: 'Resource board', exact: true }), page.getByRole('region', { name: 'Connections graph', exact: true }));
  await expect(page.getByRole('region', { name: 'Resource inspector', exact: true })).toBeVisible();
  await expect(page.getByRole('form', { name: 'Resource card editor' })).toBeHidden();

  await page.getByRole('link', { name: 'Parties' }).click();
  const network = page.getByRole('region', { name: 'Party network' });
  await beside(page.getByRole('complementary', { name: 'Party sheet' }), network);
  const details = page.getByRole('complementary', { name: 'Inspector and chat' });
  await expect(details).toBeVisible();
  const graphBox = (await network.boundingBox())!;
  const detailsBox = (await details.boundingBox())!;
  expect(detailsBox.y).toBeGreaterThanOrEqual(graphBox.y + graphBox.height - 1);
  expect(Math.abs(detailsBox.x - graphBox.x)).toBeLessThan(2);
  await page.getByRole('button', { name: 'Research chat', exact: true }).click();
  await expect(page.getByRole('textbox', { name: 'Question', exact: true })).toBeDisabled();

  await page.getByRole('link', { name: 'Queue' }).click();
  const sheet = page.getByRole('region', { name: 'Scenario sheet' });
  const inspector = page.getByRole('complementary', { name: 'Scenario details' });
  await beside(sheet, inspector);
  await expect(page.getByLabel('Find scenario')).toBeVisible();
  await expect(page.getByLabel('Opportunity title')).toBeHidden();
  await page.getByRole('link', { name: 'Output' }).click();
  await expect(page.getByRole('columnheader')).toHaveText(['Material', 'Opportunity', 'Contact', 'Status', 'Release']);
  await expect(page.getByRole('button', { name: 'Create output' })).toHaveAttribute('aria-expanded', 'false');

  await page.getByRole('link', { name: 'Settings', exact: true }).click();
  await page.getByRole('navigation', { name: 'Settings sections' }).getByRole('link', { name: 'Backup and data' }).click();
  await expect(page.getByRole('region', { name: 'Backup, export, restore, and erasure' })).toBeFocused();
});

test('browser Back asks before leaving an edited party card', async ({ page }) => {
  await page.goto('/resources');
  await page.getByRole('link', { name: 'Parties', exact: true }).click();
  await addParty(page, 'Fieldwork', 'org');
  await partiesRegion(page).getByRole('button', { name: 'Open selected card' }).click();
  const editor = page.getByRole('region', { name: 'Card editor' });
  await editor.getByLabel('Name', { exact: true }).fill('Fieldwork draft');

  await page.goBack();
  const dialog = page.getByRole('alertdialog', { name: 'Unsaved changes' });
  await expect(dialog.getByRole('button', { name: 'Stay' })).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(editor.getByLabel('Name', { exact: true })).toHaveValue('Fieldwork draft');
  await expect(page).toHaveURL(/\/parties$/);

  await page.goBack();
  await dialog.getByRole('button', { name: 'Discard unsaved changes' }).click();
  await expect(page).toHaveURL(/\/resources$/);
  await page.getByRole('link', { name: 'Parties', exact: true }).click();
  await expect(partiesRegion(page).getByRole('button', { name: 'Fieldwork', exact: true })).toBeVisible();
});

test('narrow Queue opens a filtered-out link without clearing the saved search', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 800 });
  await page.goto('/parties');
  await addParty(page, 'Fieldwork', 'org');
  await page.getByRole('link', { name: 'Queue', exact: true }).click();
  await page.getByRole('button', { name: 'New opportunity' }).click();
  await expect(page.getByLabel('Opportunity title')).toBeFocused();
  await page.getByLabel('Opportunity title').fill('Contribute a fixture');
  await page.getByLabel('Participant').selectOption({ label: 'Fieldwork · org' });
  await page.getByRole('button', { name: 'Create opportunity', exact: true }).click();

  const inspector = page.getByRole('complementary', { name: 'Scenario details' });
  await expect(inspector.getByRole('heading', { name: 'Contribute a fixture' })).toBeFocused();
  await inspector.getByRole('button', { name: 'Back to list' }).click();
  const sheet = page.getByRole('region', { name: 'Scenario sheet' });
  const row = sheet.getByRole('row').filter({ hasText: 'Contribute a fixture' });
  await expect(row.getByRole('button', { name: 'Contribute a fixture' })).toBeFocused();
  const id = await row.getAttribute('data-opportunity-id');
  expect(id).toBeTruthy();
  await expect(sheet.getByRole('combobox', { name: 'Phase for Contribute a fixture' })).toBeInViewport();
  await sheet.getByLabel('Find scenario').fill('no matching scenario');
  await expect(sheet.getByText('No matching scenarios')).toBeVisible();

  await page.goto('/queue/' + id);
  await expect(inspector.getByRole('heading', { name: 'Contribute a fixture' })).toBeVisible();
  await expect(inspector.getByText('Opened from a link · outside the current sheet.')).toBeVisible();
  await inspector.getByRole('button', { name: 'Back to list' }).click();
  await expect(sheet.getByLabel('Find scenario')).toHaveValue('no matching scenario');
  await expect(sheet.getByLabel('Find scenario')).toBeFocused();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(390);
});
