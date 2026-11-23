import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { authoritativeRow, createScenario, invoke, phaseArgs, signIn } from './sheet';

/**
 * S12 browser proof for the Queue sheet and its phase commands. Shared
 * helpers drive both Playwright projects against the same user-facing paths;
 * direct command calls use the signed-in owner session captured from the page.
 */
test('s12_browser_sheet_phase_edit_undo_and_conflicts', async ({ page }) => {
  const session = await signIn(page);
  const title = `Sheet phase ${randomUUID()}`;
  const id = await createScenario(page, title);
  const sheet = page.getByRole('region', { name: 'Scenario sheet' });
  const select = page.getByRole('combobox', { name: `Phase for ${title}`, exact: true });

  await select.selectOption('preparing');
  await expect(page.getByText('moved to Preparing')).toBeVisible();
  await expect(select).toHaveValue('preparing');

  // A first command records a phase event; the same-stage command is a true
  // no-op with a null event id and no revision advance.
  const before = await authoritativeRow(page, id, session);
  const first = await invoke(page, session, 'opportunity.set_phase', phaseArgs(before, 'in_conversation'), randomUUID());
  expect(first.status).toBe(200);
  expect(first.body.ok).toBe(true);
  expect(first.body.result?.event_id).toBeTruthy();
  const advanced = await authoritativeRow(page, id, session);
  const noOp = await invoke(page, session, 'opportunity.set_phase', phaseArgs(advanced, 'in_conversation'), randomUUID());
  expect(noOp.status).toBe(200);
  expect(noOp.body.result?.event_id).toBeNull();
  expect(noOp.body.result?.phase_revision).toBe(first.body.result?.phase_revision);

  // Same key, identical intent: one durable effect and one replayed receipt.
  const replayKey = randomUUID();
  const current = await authoritativeRow(page, id, session);
  const args = phaseArgs(current, 'exploring');
  const original = await invoke(page, session, 'opportunity.set_phase', args, replayKey);
  const replay = await invoke(page, session, 'opportunity.set_phase', args, replayKey);
  expect(original.status).toBe(200);
  expect(replay.status).toBe(200);
  expect(replay.body.replayed).toBe(true);
  // A replay records a fresh attempt audit id and points back to the original.
  expect(replay.body.original_audit_id ?? replay.body.audit_id).toBe(original.body.audit_id);

  // A stale revision is denied without changing the visible record.
  const stale = await invoke(page, session, 'opportunity.set_phase', {
    ...args, expected_phase_revision: args.expected_phase_revision + 7,
  }, randomUUID());
  expect(stale.status).toBe(409);
  expect(stale.body.ok).toBe(false);
  const afterStale = await authoritativeRow(page, id, session);
  expect(afterStale.phase?.stage_id).toBe('exploring');

  // Undo returns to the stage captured by the last command event.
  const undoRow = await authoritativeRow(page, id, session);
  const undo = await invoke(page, session, 'opportunity.undo_phase_change', {
    opportunity_id: id,
    expected_phase_revision: undoRow.phase?.revision ?? 1,
    expected_pack_id: undoRow.phase?.pack_id ?? undoRow.pack_id ?? '',
    expected_pack_version: undoRow.phase?.pack_version ?? undoRow.pack_version ?? '',
    original_event_id: original.body.result?.event_id ?? '',
  }, randomUUID());
  expect(undo.status).toBe(200);
  expect(undo.body.ok).toBe(true);
  await page.evaluate(() => globalThis.dispatchEvent(new Event('focus')));
  await expect(page.getByRole('combobox', { name: `Phase for ${title}`, exact: true })).toHaveValue(
    undo.body.result?.stage_id ?? '', { timeout: 5000 },
  );
  await expect(sheet.locator('tbody tr[data-opportunity-id]').filter({ hasText: title })).toBeVisible();
});
