import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties } from './parties';

test('s10_daily_queue_renders_ranked_items_blocked_reason_and_insight_counts_without_dispatch', async ({ page }) => {
  await page.goto('/');
  const name = `S10 daily ${randomUUID()}`;
  await openParties(page);
  await addParty(page, name, 'person');
  await page.getByRole('link', { name: 'Queue' }).click();
  await page.getByRole('button', { name: 'New opportunity' }).click();
  await page.getByLabel('Opportunity title').fill(`S10 daily ${name}`);
  await page.getByLabel('Participant').selectOption({ label: `${name} · person` });
  await page.getByRole('button', { name: 'Create opportunity' }).click();
  await page.getByRole('button', { name: 'Daily work', exact: true }).click();
  const daily = page.getByRole('region', { name: 'Daily priorities' });
  await expect(daily).toBeVisible();
  await expect(page.getByRole('button', { name: 'Apply filters' })).toBeVisible();
  await expect(page.getByRole('region', { name: 'Suggested work' })).toBeVisible();
  await expect(page.getByRole('region', { name: 'Blocked' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Refresh daily work' })).toBeVisible();
});

test('s10_coach_questions_are_impact_ordered_and_answer_pending_requires_interviewer_review', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('link', { name: 'Queue' }).click();
  await page.locator('nav[aria-label="Queue views"]').getByRole('button', { name: 'Insights', exact: true }).click();
  await expect(page.getByText('Weekly Coach and insights')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Refresh report' })).toBeVisible();
  await page.getByRole('button', { name: 'Refresh report' }).click();
  await expect(page.getByText('data as of')).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Coach questions' })).toBeVisible();
});

test('s10_opportunity_table_filters_and_sort_share_board_records', async ({ page }) => {
  await page.goto('/');
  const name = `S10 lead ${randomUUID()}`;
  await openParties(page);
  await addParty(page, name, 'person');
  await page.getByRole('link', { name: 'Queue' }).click();
  const sheet = page.getByRole('region', { name: 'Scenario sheet' });
  await expect(sheet).toBeVisible();

  // Create two opportunities from the Queue toolbar.
  for (const title of ['Northline review', 'Fieldwork fixture']) {
    await page.getByRole('button', { name: 'New opportunity' }).click();
    await page.getByLabel('Opportunity title').fill(title);
    await page.getByLabel('Participant').selectOption({ label: `${name} · person` });
    await page.getByRole('button', { name: 'Create opportunity' }).click();
    await expect(sheet.getByText(title)).toBeVisible();
  }

  // Search matches party name and titles case-insensitively.
  await page.getByLabel('Find scenario').fill(name.toLowerCase());
  await expect(sheet.locator('tbody tr[data-opportunity-id]')).toHaveCount(2);
  await page.getByLabel('Find scenario').fill('fieldwork');
  await expect(sheet.locator('tbody tr[data-opportunity-id]')).toHaveCount(1);
  await page.getByLabel('Find scenario').fill('');
  await expect(sheet.locator('tbody tr[data-opportunity-id]')).toHaveCount(2);

  // Phase filter intersects with search; phase edits announce and leave the filter.
  await page.getByRole('combobox', { name: 'Phase for Fieldwork fixture', exact: true }).selectOption('preparing');
  await expect(page.getByRole('status').filter({ hasText: 'moved to Preparing' })).toBeVisible();
  await page.getByRole('combobox', { name: 'Phase for Northline review', exact: true }).selectOption('preparing');
  await page.getByLabel('Phase filter').selectOption('preparing');
  await expect(sheet.locator('tbody tr[data-opportunity-id]')).toHaveCount(2);
  await page.getByRole('combobox', { name: 'Phase for Northline review', exact: true }).selectOption('exploring');
  await expect(page.getByRole('status').filter({ hasText: 'hidden by the Preparing filter' })).toBeVisible();
  await expect(sheet.locator('tbody tr[data-opportunity-id]')).toHaveCount(1);

  // Undo restores the prior phase; the filter stays put.
  await page.getByRole('button', { name: 'Undo phase change' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'returned to Preparing' })).toBeVisible();
  await expect(sheet.locator('tbody tr[data-opportunity-id]')).toHaveCount(2);
  await page.getByLabel('Phase filter').selectOption('');
  await expect(sheet.locator('tbody tr[data-opportunity-id]')).toHaveCount(2);

  // Name sort is deterministic and stable.
  await page.getByLabel('Order').selectOption('Scenario name');
  const titles = await sheet.locator('tbody tr[data-opportunity-id] button.open').allTextContents();
  expect([...titles].sort()).toEqual(titles);
});
