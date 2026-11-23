import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties } from './parties';
import { createScenario, invoke, openPalette, palette, signIn, type OwnerSession } from './sheet';

type EntityRecord = { id: string; kind: string; name: string; focus: string; revision: number; etag: string };

async function entityById(page: import('@playwright/test').Page, session: OwnerSession, name: string): Promise<EntityRecord> {
  const response = await page.request.get('/api/v1/directory', {
    headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
  });
  expect(response.ok()).toBe(true);
  const rows = await response.json() as { data: EntityRecord[] };
  const match = rows.data.find((row) => row.name === name);
  expect(match, `directory row ${name}`).toBeDefined();
  return match!;
}

test('u04_palette_renames_party_and_updates_focus', async ({ page }) => {
  const session = await signIn(page);
  const original = `U04 palette ${randomUUID()}`;
  const renamed = `U04 renamed ${randomUUID()}`;
  await openParties(page);
  await addParty(page, original, 'person');
  const party = await entityById(page, session, original);

  await page.goto(`/parties/${party.id}`);
  await openPalette(page);
  await palette(page).getByRole('combobox').fill('rename');
  await palette(page).getByRole('combobox').press('Enter');
  await palette(page).getByLabel('Party name').fill(renamed);
  await palette(page).getByRole('button', { name: 'Continue' }).click();
  await expect(palette(page)).toContainText('Rename the selected party?');
  await palette(page).getByRole('button', { name: 'Confirm' }).click();
  await expect(palette(page)).toContainText('Rename party completed.');
  await palette(page).getByRole('button', { name: 'Done' }).click();

  const afterRename = await entityById(page, session, renamed);
  expect(afterRename.revision).toBe(party.revision + 1);
  expect(afterRename.name).toBe(renamed);

  // Focus update through the same scoped palette path.
  await page.goto(`/parties/${party.id}`);
  await openPalette(page);
  await palette(page).getByRole('combobox').fill('focus');
  await palette(page).getByRole('combobox').press('Enter');
  await palette(page).getByLabel('Focus').fill('Follow up next week');
  await palette(page).getByRole('button', { name: 'Continue' }).click();
  await palette(page).getByRole('button', { name: 'Confirm' }).click();
  await expect(palette(page)).toContainText('Set party focus completed.');
  await palette(page).getByRole('button', { name: 'Done' }).click();

  const afterFocus = await entityById(page, session, renamed);
  expect(afterFocus.revision).toBe(party.revision + 2);
  expect(afterFocus.focus).toBe('Follow up next week');
});

test('u04_palette_selects_second_identical_task_by_keyboard', async ({ page }) => {
  const session = await signIn(page);
  const title = `U04 task ${randomUUID()}`;
  const id = await createScenario(page, title);

  const notes = await page.request.patch(`/api/v1/opportunities/${id}/notes`, {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      'Idempotency-Key': randomUUID(),
    },
    data: { markdown: '- [ ] Same task\n- [ ] Same task\n', expected_revision: 1 },
  });
  expect(notes.ok()).toBe(true);

  await page.locator(`tr[data-opportunity-id="${id}"] button.open`).click();
  await expect(page.getByRole('complementary', { name: 'Scenario details' })).toContainText(title);

  await openPalette(page);
  await palette(page).getByRole('combobox').fill('task');
  await palette(page).getByRole('combobox').press('Enter');
  const picker = palette(page).getByLabel('Task');
  await expect(picker).toBeVisible();
  const options = picker.locator('option');
  await expect(options).toHaveCount(3, { timeout: 10000 });
  // Collect the distinct task values (a placeholder option may precede them).
  const values: string[] = [];
  for (let i = 0; i < await options.count(); i++) {
    const value = await options.nth(i).getAttribute('value');
    if (value && !values.includes(value)) values.push(value);
  }
  expect(values).toHaveLength(2);
  // The two identical tasks have distinct spans; pick the second one.
  await picker.selectOption(values[1]!);
  await palette(page).getByRole('button', { name: 'Continue' }).click();
  await expect(palette(page)).toContainText('Mark the selected task done?');
  await palette(page).getByRole('button', { name: 'Confirm' }).click();
  await expect(palette(page)).toContainText('Set task done completed.');
  await palette(page).getByRole('button', { name: 'Done' }).click();

  const response = await page.request.get(`/api/v1/opportunities/${id}/notes`, {
    headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
  });
  expect(response.ok()).toBe(true);
  const body = await response.json() as { revision: number; tasks: Array<{ checked: boolean; text: string }> };
  expect(body.revision).toBe(3);
  expect(body.tasks).toHaveLength(2);
  expect(body.tasks[0]!.checked).toBe(false);
  expect(body.tasks[1]!.checked).toBe(true);
});

test('u04_party_conflict_keeps_dirty_data_without_stale_overwrite', async ({ page }) => {
  const session = await signIn(page);
  const original = `U04 conflict ${randomUUID()}`;
  const renamed = `U04 conflict renamed ${randomUUID()}`;
  await openParties(page);
  await addParty(page, original, 'person');
  const party = await entityById(page, session, original);

  await page.goto(`/parties/${party.id}`);
  await openPalette(page);
  await palette(page).getByRole('combobox').fill('rename');
  await palette(page).getByRole('combobox').press('Enter');
  await palette(page).getByLabel('Party name').fill(renamed);
  await palette(page).getByRole('button', { name: 'Continue' }).click();

  // Another session advances the card before the palette confirms.
  const foreign = await page.request.patch(`/api/v1/entities/${party.id}`, {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      'If-Match': party.etag,
      'Idempotency-Key': randomUUID(),
    },
    data: { focus: 'Foreign session focus' },
  });
  expect(foreign.ok()).toBe(true);
  await page.evaluate(() => globalThis.dispatchEvent(new Event('focus')));

  await palette(page).getByRole('button', { name: 'Confirm' }).click();
  const alert = palette(page).getByRole('alert');
  await expect(alert).toBeVisible();
  await expect(alert).toContainText(/revision|stale|changed|selection/i);
  await palette(page).getByRole('button', { name: 'Close command palette' }).click();

  // The foreign focus stuck; the palette draft did not overwrite it.
  const after = await entityById(page, session, original);
  expect(after.name).toBe(original);
  expect(after.focus).toBe('Foreign session focus');
  expect(after.revision).toBe(party.revision + 1);
});

test('u04_command_invoke_party_and_task_discipline', async ({ page }) => {
  const session = await signIn(page);
  const partyName = `U04 direct ${randomUUID()}`;
  const title = `U04 direct task ${randomUUID()}`;
  await openParties(page);
  await addParty(page, partyName, 'person');
  const party = await entityById(page, session, partyName);
  const id = await createScenario(page, title);

  const notes = await page.request.patch(`/api/v1/opportunities/${id}/notes`, {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      'Idempotency-Key': randomUUID(),
    },
    data: { markdown: '- [ ] Direct task\n', expected_revision: 1 },
  });
  expect(notes.ok()).toBe(true);
  const notesBody = await notes.json() as { tasks: Array<{ byte_start: number; byte_end: number; line_sha256: string }> };
  const task = notesBody.tasks[0]!;

  const renamed = `U04 direct renamed ${randomUUID()}`;
  const rename = await invoke(page, session, 'party.rename', {
    party_id: party.id, expected_card_revision: party.revision, name: renamed,
  }, randomUUID());
  expect(rename.status).toBe(200);
  expect(rename.body.result?.revision).toBe(party.revision + 1);

  const toggle = await invoke(page, session, 'opportunity.set_task_done', {
    opportunity_id: id,
    expected_notes_revision: 2,
    byte_start: task.byte_start,
    byte_end: task.byte_end,
    line_sha256: task.line_sha256,
    checked: true,
  }, randomUUID());
  expect(toggle.status).toBe(200);

  const stale = await invoke(page, session, 'party.rename', {
    party_id: party.id, expected_card_revision: party.revision, name: 'Stale',
  }, randomUUID());
  expect(stale.status).toBe(409);
  expect(stale.body.error?.code).toBe('version_mismatch');
});
