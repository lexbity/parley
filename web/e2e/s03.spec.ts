import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';

test('s03_reviewed_manual_message_copy_is_recipient_and_version_bound', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async (text: string) => { (window as Window & { parleyClipboardText?: string }).parleyClipboardText = text; } } });
  });
  await page.goto('/');
  await page.getByRole('button', { name: 'Leads' }).click();
  const suffix = randomUUID();
  // Letters-only suffix: generic sentences are grounded-checked, and digits
  // inside them are hard gate failures (RFC-0001 GR-2).
  const leadName = `Dana ${suffix.replace(/[^a-z]/gi, '')}`;
  const addLead = page.getByRole('heading', { name: 'Add lead' }).locator('..');
  await addLead.getByLabel('Name').fill(leadName);
  await addLead.getByRole('button', { name: 'Add lead' }).click();
  await page.getByRole('button', { name: 'Output' }).click();
  const workspace = page.getByLabel('Output registry workspace');
  await workspace.getByRole('textbox', { name: 'Title', exact: true }).fill(`Onboarding ${suffix}`);
  await workspace.getByLabel(new RegExp(`${leadName}.*person`)).check();
  await workspace.getByRole('button', { name: 'Create opportunity' }).click();
  await expect(workspace.getByRole('heading', { name: new RegExp(`Onboarding ${suffix}`) })).toBeVisible();
  await workspace.getByRole('button', { name: new RegExp(`Onboarding ${suffix}`) }).click();

  await workspace.getByLabel('Interest side').selectOption('them');
  await workspace.getByLabel('Interest statement').fill('They want a smoother onboarding handoff.');
  await workspace.getByRole('button', { name: 'Save interest' }).click();
  await expect(workspace.getByText('T3 · them: They want a smoother onboarding handoff.')).toBeVisible();
  await workspace.getByLabel('Interest side').selectOption('me');
  await workspace.getByLabel('Interest statement').fill('I want to learn whether the checklist is useful.');
  await workspace.getByRole('button', { name: 'Save interest' }).click();
  await expect(workspace.getByText('T3 · me: I want to learn whether the checklist is useful.')).toBeVisible();
  await workspace.getByLabel('They get').fill('A practical checklist they can use without replying.');
  await workspace.getByLabel('I get').fill('An opportunity to learn from their onboarding practice.');
  await workspace.getByRole('checkbox', { name: /them: They want/ }).check();
  await workspace.getByRole('checkbox', { name: /me: I want/ }).check();
  await workspace.getByRole('checkbox', { name: /I believe both sides come out ahead/ }).check();
  await workspace.getByRole('button', { name: 'Save exchange version' }).click();

  await workspace.getByLabel('Recipient').selectOption({ index: 1 });
  await workspace.getByLabel('Material title').fill('Checklist note');
  await workspace.getByRole('button', { name: 'Create draft material' }).click();
  await workspace.getByRole('button', { name: 'Start an incomplete draft version' }).click();
  const editor = workspace.getByLabel('Material editor');
  await editor.getByLabel('Message text').fill(`Hello ${leadName}.`);
  await editor.getByRole('button', { name: 'Add claim' }).click();
  await editor.getByLabel('Claim type').selectOption('generic');
  await editor.getByRole('button', { name: 'Save as a new version' }).click();
  await expect(editor.getByRole('status')).toContainText('Showing the saved version');
  for (const reason of await editor.locator('textarea[minlength="10"]').all()) {
    await reason.fill('I reviewed this soft failure and choose to proceed for this recipient.');
  }
  await editor.getByRole('button', { name: 'Approve this version' }).click();
  await expect(page.getByText('Exact version approved for its named recipient.')).toBeVisible();
  await editor.getByRole('button', { name: 'Prepare approved copy' }).click();
  await editor.getByRole('button', { name: 'Copy approved text' }).click();
  await expect(page.getByText(/Clipboard promise succeeded and its receipt was recorded/)).toBeVisible();
  await page.getByRole('button', { name: 'Output' }).click();
  await expect(page.getByRole('button', { name: /Checklist note/ })).toBeVisible();
  await expect(page.getByRole('region', { name: 'Material editor', exact: true })).toContainText(leadName);
  expect(await page.evaluate(() => (window as Window & { parleyClipboardText?: string }).parleyClipboardText)).toBe(`Hello ${leadName}.`);
  await expect(page.getByText(/Copying text does not mark it sent or start a reply timer/)).toBeVisible();
  await page.getByRole('button', { name: 'Queue' }).click();
  const queue = page.getByRole('region', { name: 'Opportunity queue' });
  await queue.getByRole('button', { name: new RegExp(`Onboarding ${suffix}`) }).first().click();
  await expect(queue.getByRole('button', { name: /Checklist note/ })).toBeVisible();
});
