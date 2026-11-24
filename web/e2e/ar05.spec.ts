import { randomUUID } from 'node:crypto';
import { expect, type Locator, type Page } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties } from './parties';
import { signIn, type OwnerSession } from './sheet';

/** AR05 owner flow: an explicit scoped run, a real cancel, and review that
 * survives navigation. The mock project drives observable registry state; the
 * Rust project exercises the real HTTP boundary and its fail-closed capability
 * state on a host without a qualified local pairing. */

async function grantedParty(page: Page, session: OwnerSession, name: string) {
  await openParties(page);
  await addParty(page, name, 'person');
  const response = await page.request.get('/api/v1/entities?kind=person&kind=org', {
    headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
  });
  expect(response.ok()).toBe(true);
  const body = await response.json() as { data: Array<{ id: string; name: string }> };
  const party = body.data.find((row) => row.name === name);
  expect(party).toBeDefined();
  return party!;
}

async function openRuns(page: Page) {
  await page.getByRole('link', { name: 'Settings' }).click();
  return page.getByRole('region', { name: 'Delegated runs' });
}

async function startScopedRun(runs: Locator, task: string, partyName: string) {
  await runs.getByRole('textbox', { name: 'Task for delegated run' }).fill(task);
  await runs.getByRole('checkbox', { name: `Party: ${partyName}` }).check();
  await runs.getByRole('checkbox', { name: 'Acknowledge scoped read grant' }).check();
  await runs.getByRole('button', { name: 'Start scoped run' }).click();
  const row = runs.locator('[data-run-id]').first();
  await expect(row).toBeVisible();
  return { id: (await row.getAttribute('data-run-id'))!, row };
}

test('ar05_browser_follows_a_delegated_brief_to_review', async ({ page }, testInfo) => {
  const session = await signIn(page);
  const name = `AR05 brief ${randomUUID()}`;
  await grantedParty(page, session, name);
  const runs = await openRuns(page);
  const { id, row } = await startScopedRun(runs, `Review ${name} with granted reads`, name);

  if (testInfo.project.name === 'rust') {
    // No qualified Pi/Ollama pairing is provisioned by the browser lane. The
    // real service must fail closed with a typed, owner-safe capability code.
    await expect(row).toHaveAttribute('data-state', 'failed', { timeout: 20_000 });
    const status = await page.request.get(`/api/v1/agent/runs/${id}`, {
      headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
    });
    expect(status.ok()).toBe(true);
    const body = await status.json() as { data?: { wake_condition: string }; wake_condition?: string };
    const reason = body.data?.wake_condition ?? body.wake_condition ?? '';
    expect(['CAPABILITY_UNQUALIFIED', 'RUNNER_INSTALL_INVALID', 'ISOLATION_UNAVAILABLE'])
      .toContain(reason);
    return;
  }

  await expect(row).toHaveAttribute('data-state', 'succeeded', { timeout: 20_000 });
  await runs.getByRole('button', { name: `View transcript ${id}` }).click();
  const transcript = runs.getByRole('region', { name: `Transcript for ${id}` });
  await expect(transcript).toBeVisible();
  expect(await transcript.locator('script, img').count()).toBe(0);
  await page.reload();
  await openRuns(page);
  await expect(runs.locator(`[data-run-id="${id}"]`)).toHaveAttribute('data-state', 'succeeded');
});

test('ar05_browser_cancel_persists_across_reload', async ({ page }, testInfo) => {
  const session = await signIn(page);
  const name = `AR05 cancel ${randomUUID()}`;
  await grantedParty(page, session, name);
  const runs = await openRuns(page);
  const { id, row } = await startScopedRun(runs, `Hold a run for ${name}`, name);

  if (testInfo.project.name === 'rust') {
    await expect(row).toHaveAttribute('data-state', 'failed', { timeout: 20_000 });
    return;
  }

  const cancel = runs.getByRole('button', { name: `Cancel run ${id}` });
  await cancel.click();
  await expect(row).toHaveAttribute('data-state', 'cancelled', { timeout: 10_000 });
  await page.reload();
  await openRuns(page);
  await expect(runs.locator(`[data-run-id="${id}"]`)).toHaveAttribute('data-state', 'cancelled');
});
