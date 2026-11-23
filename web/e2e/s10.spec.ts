import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties } from './parties';

test('s10_daily_queue_renders_ranked_items_blocked_reason_and_insight_counts_without_dispatch', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('link', { name: 'Queue' }).click();
  await page.getByRole('button', { name: 'Daily work and insights' }).click();
  const daily = page.getByRole('region', { name: 'Daily queue, insights, and weekly Coach' });
  await expect(daily.getByRole('heading', { name: 'Ranked daily work' })).toBeVisible();
  await expect(daily.getByText('Northstar onboarding')).toBeVisible();
  await expect(daily.getByRole('region', { name: 'Blocked work' })).toContainText('Missing measured proof');
  await expect(daily.getByRole('region', { name: 'Insights' })).toContainText('3/8');
  await expect(daily).toContainText('95% Wilson');
});

test('s10_coach_questions_are_impact_ordered_and_answer_pending_requires_interviewer_review', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('link', { name: 'Queue' }).click();
  await page.getByRole('button', { name: 'Daily work and insights' }).click();
  const coach = page.getByRole('region', { name: 'Weekly Coach', exact: true });
  await expect(coach).toContainText('impact 2');
  await coach.getByLabel('Your answer').fill('I improved activation by measuring new customer setup time.');
  await coach.getByRole('button', { name: 'Send answer to Interviewer' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Answer sent to Interviewer' })).toContainText('proposal');
  await expect(coach.getByRole('button', { name: 'Review Interviewer candidate in Resources' })).toBeVisible();
});

test('s10_opportunity_table_filters_and_sort_share_board_records', async ({ page }) => {
  await page.goto('/');
  const name = `S10 lead ${randomUUID()}`;
  await openParties(page);
  await addParty(page, name, 'person');
  await page.getByRole('link', { name: 'Queue' }).click();
  const queue = page.getByRole('region', { name: 'Opportunities' });
  await queue.getByLabel('Opportunity title').fill('S10 filtered table row');
  await queue.getByLabel('Participant').selectOption({ label: `${name} · person` });
  await queue.getByRole('button', { name: 'Create opportunity' }).click();
  await queue.getByRole('textbox', { name: 'Edit tags for S10 filtered table row' }).fill('onboarding, tested');
  await queue.getByRole('button', { name: 'Save tags' }).click();
  await queue.getByLabel('Filter by tag').fill('tested');
  await queue.getByRole('button', { name: 'Apply to board and table' }).click();
  await expect(queue.getByRole('table').getByText('S10 filtered table row')).toBeVisible();
  await expect(queue.locator('[aria-label="Opportunity board"]')).toContainText('S10 filtered table row');
  await queue.getByLabel('Sort opportunities').selectOption('next_task');
  await queue.getByRole('button', { name: 'Apply to board and table' }).click();
  await expect(queue.locator('tbody tr[data-opportunity-id]')).toHaveCount(1);
});

test('s10_outcome_correction_updates_insight_counts_with_history_preserved', async ({ page }) => {
  await page.goto('/');
  const name = `S10 outcome lead ${randomUUID()}`;
  await openParties(page);
  await addParty(page, name, 'person');
  await page.getByRole('link', { name: 'Queue' }).click();
  const queue = page.getByRole('region', { name: 'Opportunities' });
  await queue.getByLabel('Opportunity title').fill('S10 correction history');
  await queue.getByLabel('Participant').selectOption({ label: `${name} · person` });
  await queue.getByRole('button', { name: 'Create opportunity' }).click();
  await queue.getByRole('button', { name: 'Send history and outcomes' }).click();
  const history = queue.getByRole('region', { name: 'Send history and outcomes' });
  await expect(history).toContainText('Original: reply_neutral');
  await history.getByLabel('Corrected outcome').selectOption('reply_positive');
  await history.getByLabel('Correction reason').fill('The owner verified a positive reply.');
  await history.getByRole('button', { name: 'Append correction' }).click();
  await expect(history).toContainText('Original: reply_neutral');
  await expect(history).toContainText('Revision 2 · reply_positive');
  await queue.getByRole('button', { name: 'Daily work and insights' }).click();
  await expect(queue.getByRole('region', { name: 'Insights', exact: true })).toContainText('4/8');
});
