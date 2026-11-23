import { randomUUID } from 'node:crypto';
import { expect, type Page, type Response } from '@playwright/test';
import { addParty, openParties } from './parties';

/** Shared user-facing helpers for Queue sheet and command palette proofs. */

export type OwnerSession = { mutation_token: string; epoch: string };
export type SheetRow = {
  opportunity_id: string;
  title: string;
  phase: { stage_id: string; pack_id: string; pack_version: string; revision: number } | null;
  pack_id?: string;
  pack_version?: string;
};
export type CommandReceipt = {
  ok: boolean;
  audit_id?: string;
  original_audit_id?: string | null;
  replayed?: boolean;
  error?: { code: string; message: string };
  result?: { stage_id: string | null; phase_revision: number; event_id: string | null };
};

export async function signIn(page: Page): Promise<OwnerSession> {
  let session: OwnerSession | null = null;
  page.on('response', (response: Response) => {
    if (response.url().endsWith('/api/v1/session') && response.request().method() === 'GET') {
      void response.json().then((value) => { session = value as OwnerSession; }).catch(() => undefined);
    }
  });
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Resources', exact: true })).toBeVisible();
  await expect.poll(() => session).not.toBeNull();
  return session as unknown as OwnerSession;
}

export async function createScenario(page: Page, title: string): Promise<string> {
  const partyName = `Scenario owner ${randomUUID()}`;
  await openParties(page);
  await addParty(page, partyName, 'person');
  await page.getByRole('link', { name: 'Queue' }).click();
  await page.getByRole('button', { name: 'New opportunity' }).click();
  await page.getByLabel('Opportunity title').fill(title);
  await page.getByLabel('Participant').selectOption({ label: `${partyName} · person` });
  await page.getByRole('button', { name: 'Create opportunity' }).click();
  const row = page.locator('tbody tr[data-opportunity-id]').filter({ hasText: title });
  await expect(row).toBeVisible();
  return (await row.getAttribute('data-opportunity-id'))!;
}

export async function authoritativeRow(page: Page, id: string, session: OwnerSession): Promise<SheetRow> {
  const response = await page.request.get('/api/v1/queue?view=sheet', {
    headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
  });
  expect(response.ok()).toBe(true);
  const sheet = await response.json() as { rows: SheetRow[]; pack?: { id: string; version: string } };
  const row = sheet.rows.find((item) => item.opportunity_id === id);
  expect(row).toBeDefined();
  return row!;
}

export async function invoke(
  page: Page,
  session: OwnerSession,
  id: string,
  args: Record<string, unknown>,
  key: string,
): Promise<{ status: number; body: CommandReceipt }> {
  const response = await page.request.post(`/api/v1/commands/${id}`, {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      'Idempotency-Key': key,
    },
    data: { version: 1, args, dry_run: false },
  });
  return { status: response.status(), body: await response.json() as CommandReceipt };
}

export function phaseArgs(row: SheetRow, stageId: string) {
  return {
    opportunity_id: row.opportunity_id,
    expected_phase_revision: row.phase?.revision ?? 1,
    expected_pack_id: row.phase?.pack_id ?? row.pack_id ?? '',
    expected_pack_version: row.phase?.pack_version ?? row.pack_version ?? '',
    stage_id: stageId,
  };
}

export function palette(page: Page) {
  return page.getByRole('dialog', { name: 'Command palette' });
}

export async function openPalette(page: Page): Promise<void> {
  await page.keyboard.press('Control+k');
  if (await palette(page).isVisible().catch(() => false)) return;
  // Headless Chromium may consume Control+k for its own shortcut; dispatch the
  // same chord to the shell so the keyboard path is still exercised.
  await page.evaluate(() => {
    globalThis.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', code: 'KeyK', ctrlKey: true, bubbles: true }));
  });
  await expect(palette(page)).toBeVisible();
}
