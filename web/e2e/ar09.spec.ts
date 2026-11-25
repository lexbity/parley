import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { signIn } from './sheet';

/** AR09: one owner Activity surface finds, cancels and diagnoses durable work.
 *  Navigation away and back keeps the list current from the server, manual
 *  drafts survive, and cancellation uses the real job/run cancel operations. */

type Auth = { 'X-Parley-Token': string; 'X-Parley-Epoch': string };

async function auth(page: import('@playwright/test').Page): Promise<Auth> {
  const session = await (await page.request.get('/api/v1/session')).json() as { mutation_token: string; epoch: string };
  return { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch };
}

async function call(
  page: import('@playwright/test').Page,
  method: string,
  path: string,
  body?: unknown,
  headers?: Record<string, string>,
) {
  const response = await page.request.fetch(path, {
    method,
    headers: {
      ...(method === 'GET' ? {} : { 'Idempotency-Key': randomUUID() }),
      ...headers,
    },
    ...(body === undefined ? {} : { data: body }),
  });
  let data: unknown = null;
  try { data = await response.json(); } catch { /* empty */ }
  return { status: response.status(), data };
}

async function startRun(page: import('@playwright/test').Page, a: Auth, task: string): Promise<string> {
  const started = await call(page, 'POST', '/api/v1/agent/runs', {
    task,
    targets: [{ kind: 'party', id: 'r-1' }],
    permissions: ['party.read'],
    allow_created_opportunities: false,
  }, a);
  expect(started.status).toBe(201);
  return (started.data as { run_id: string }).run_id;
}

test('ar09_owner_reopens_and_cancels_after_navigation', async ({ page }, testInfo) => {
  await signIn(page);
  const a = await auth(page);
  const runId = await startRun(page, a, testInfo.project.name === 'mock' ? 'hang on summary' : 'Summarise the granted party');

  await page.goto('/activity');
  const row = page.locator(`[data-kind="run"][data-id="${runId}"]`);
  await expect(row).toBeVisible({ timeout: 15_000 });

  // Navigation away and back keeps the durable row (server-backed, not
  // view-local browser state).
  await page.getByRole('link', { name: 'Output', exact: true }).click();
  await page.getByRole('link', { name: 'Activity', exact: true }).click();
  await expect(row).toBeVisible();

  if (testInfo.project.name === 'mock') {
    // The hung run stays active; real cancel moves it to a terminal state.
    await expect(row).toHaveAttribute('data-state', /^(queued|running)$/);
    await row.getByRole('button', { name: `Cancel ${runId}` }).click();
    await expect(row).toHaveAttribute('data-state', 'cancelled', { timeout: 15_000 });
    await expect(row.getByRole('button', { name: `Cancel ${runId}` })).toHaveCount(0);
  } else {
    // The rust project run may fail closed (no qualified pairing). It must
    // still appear and remain listed after navigation.
    await expect(row).toHaveAttribute('data-state', /^(queued|running|succeeded|failed|cancelled|expired)$/);
  }
});

test('ar09_recovery_displays_diagnosis_and_preserves_draft', async ({ page }, testInfo) => {
  await signIn(page);
  const a = await auth(page);
  const task = testInfo.project.name === 'mock' ? 'fail on summary' : 'Fail this run deliberately';
  const runId = await startRun(page, a, task);

  await page.goto('/activity');
  const row = page.locator(`[data-kind="run"][data-id="${runId}"]`);
  await expect(row).toBeVisible({ timeout: 15_000 });

  if (testInfo.project.name === 'mock') {
    // A typed closed diagnosis is rendered from the row directly.
    await expect(row.getByRole('alert')).toContainText('PROVIDER_FAILED', { timeout: 15_000 });
  } else {
    // The real service fails closed with one of the stable diagnostic codes.
    await expect(row.getByRole('alert')).toContainText(/RUNNER_INSTALL_INVALID|PROTOCOL_MISMATCH|ISOLATION_UNAVAILABLE|MODEL_UNAVAILABLE|PROFILE_CHANGED|CAPABILITY_UNQUALIFIED|INFERENCE_BUSY|WORKER_PROTOCOL|RESULT_INVALID|PROVIDER_FAILED|QUEUE_EXPIRED|CANCELLED|INTERRUPTED/, { timeout: 20_000 });
  }
  await expect(row).toHaveAttribute('data-state', /^(failed|cancelled|invalid)$/);

  // The owner's manual draft survives visiting Activity and coming back.
  await page.getByRole('link', { name: 'Output', exact: true }).click();
  const editor = page.getByLabel('Email draft');
  await editor.fill(`Draft preserved ${runId}`);
  await page.getByRole('link', { name: 'Activity', exact: true }).click();
  await expect(page.locator(`[data-kind="run"][data-id="${runId}"]`)).toBeVisible();
  await page.getByRole('link', { name: 'Output', exact: true }).click();
  await expect(editor).toHaveValue(`Draft preserved ${runId}`);
});
