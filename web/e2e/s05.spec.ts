import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';

test('s05_board_and_markdown_share_opportunity_records', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Queue' }).click();
  const queue = page.getByRole('region', { name: 'Opportunity queue' });
  const suffix = randomUUID();
  const title = `Board work ${suffix}`;
  await queue.getByRole('textbox', { name: 'New opportunity title' }).fill(title);
  await queue.getByRole('button', { name: 'Create opportunity' }).click();
  const card = queue.getByRole('button', { name: new RegExp(title) }).first();
  await expect(card).toBeVisible();
  await card.click();

  const notes = queue.getByRole('region', { name: 'Opportunity notes' });
  const markdown = `# ${title} 🧭\n- [ ] Same task\n- [ ] Same task\n\n\`\`\`md\n- [ ] fenced example\n\`\`\`\n  - [ ] nested task`;
  await notes.locator('.cm-content').fill(markdown);
  await expect(notes.getByRole('status')).toContainText('Notes saved.', { timeout: 5000 });
  const tasks = notes.getByRole('list', { name: 'Notes tasks' });
  const duplicateLabels = tasks.getByRole('checkbox');
  await duplicateLabels.nth(1).check();
  await expect(duplicateLabels.nth(1)).toBeChecked();
  await expect(duplicateLabels.nth(0)).not.toBeChecked();
  await expect(tasks.getByRole('checkbox')).toHaveCount(3);
  await expect(tasks.getByText('fenced example')).toBeVisible();
  await expect(tasks).toContainText('nested task');

  await queue.getByRole('textbox', { name: 'New list name' }).fill(`Friday ${suffix}`);
  await queue.getByRole('button', { name: 'Create list' }).click();
  await expect(queue.getByRole('heading', { name: `Friday ${suffix}` })).toBeVisible();
  const move = queue.getByLabel('Move to list').first();
  await move.selectOption({ label: `Friday ${suffix}` });
  await expect(queue.getByText('Opportunity moved. Status, dates, and contact history are unchanged.')).toBeVisible();
  await expect(queue.locator('.opportunity-detail > header').getByRole('heading', { name: title })).toBeVisible();
  await page.getByRole('button', { name: 'Output' }).click();
  await page.getByRole('button', { name: 'Queue' }).click();
  const reloadedQueue = page.getByRole('region', { name: 'Opportunity queue' });
  await reloadedQueue.getByRole('button', { name: title, exact: true }).click();
  const reloadedTasks = reloadedQueue.getByRole('region', { name: 'Opportunity notes' }).getByRole('list', { name: 'Notes tasks' });
  await expect(reloadedTasks.getByRole('checkbox').nth(1)).toBeChecked();
  await reloadedQueue.getByRole('button', { name: 'Opportunities', exact: true }).click();
  await expect(reloadedQueue.getByRole('table').getByRole('row').filter({ hasText: title })).toContainText(`Friday ${suffix}`);
});
