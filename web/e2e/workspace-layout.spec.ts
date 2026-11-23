import { expect, type Locator } from '@playwright/test';
import { test } from './fixtures';

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
  await beside(network, page.locator('.inspector-column'));
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
});
