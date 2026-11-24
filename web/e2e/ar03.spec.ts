import { expect } from '@playwright/test';
import { test } from './fixtures';

async function openWriting(page: import('@playwright/test').Page) {
  await page.goto('/');
  await page.getByRole('link', { name: 'Output' }).click();
  await page.getByRole('button', { name: 'Create output' }).click();
  const heading = page.getByRole('heading', { name: 'Composer, evidence and templates' });
  await expect(heading).toBeVisible();
  return heading;
}

test('ar03_manual_template_draft_survives_model_unavailability', async ({ page }) => {
  await openWriting(page);
  const family = `AR03 Family ${Math.random().toString(36).slice(2, 8)}`;
  await page.getByLabel('Family').fill(family);
  await page.getByLabel('Version name').fill('Introduction draft');
  await page.getByLabel('Template body').fill('{{their_name:name}},\nI can offer {{gift:guide}}.\nWould a short review be useful {{ask:next_step}}?');
  await page.getByRole('button', { name: 'Save as new draft version' }).click();
  await expect(page.getByRole('status')).toContainText('Immutable draft version saved');

  // The deterministic owner gates are independent of model availability.
  await page.getByRole('button', { name: 'Owner approve version' }).click();
  await expect(page.getByRole('button', { name: 'Activate validated version' })).toBeEnabled();
  await page.getByRole('button', { name: 'Activate validated version' }).click();
  await expect(page.getByRole('status')).toContainText('Template activated');
});
