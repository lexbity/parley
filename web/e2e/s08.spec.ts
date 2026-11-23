import { expect } from '@playwright/test';
import { test } from './fixtures';

test('s08_owner_interviewer_bank_chat_history_and_prompt_versions', async ({ page }) => {
  await page.goto('/');
  const resources = page.getByRole('region', { name: 'Interviewer' });
  await resources.getByRole('button', { name: 'Start Interviewer' }).click();
  await resources.getByRole('button', { name: 'Ask' }).click();
  await expect(resources.getByRole('list').first()).toContainText('What changed, and how did you measure the result?');
  await resources.getByRole('button', { name: 'Unknown' }).click();
  await page.getByRole('link', { name: 'Settings' }).click();
  const prompt = page.getByLabel('Prompt');
  await prompt.fill('Owner customized interviewer policy.');
  await page.getByRole('button', { name: 'Activate new prompt version' }).click();
  await expect(page.getByRole('status')).toContainText('new interviewer prompt version is active');
  await page.getByRole('link', { name: 'Resources' }).click();
  await resources.getByLabel('Saved owner chats').selectOption({ index: 1 });
  await resources.getByRole('button', { name: 'Open saved chat' }).click();
  await expect(resources.getByRole('list').first()).toContainText('What changed, and how did you measure the result?');
  await expect(resources.getByText('unknown', { exact: true })).toBeVisible();
});
