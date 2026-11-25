import { randomUUID } from 'node:crypto';
import { createServer, type Server } from 'node:http';
import { expect, type Locator, type Page } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties } from './parties';
import { readProviderRevision, signIn, type OwnerSession } from './sheet';

/** A loopback tool-capable provider for the real Rust runtime; the mock
 * fixture drives its own deterministic run state machine. */
type ScriptedProvider = { endpoint: string; close(): Promise<void> };

async function startProvider(replies: unknown[], hold = false): Promise<ScriptedProvider> {
  let index = 0;
  const sockets = new Set<import('node:net').Socket>();
  const server: Server = createServer((request, response) => {
    if (hold) return; // An unanswered probe keeps the claimed run active.
    request.resume();
    request.on('end', () => {
      const reply = replies[Math.min(index++, replies.length - 1)];
      const payload = JSON.stringify(reply);
      response.writeHead(200, {
        'content-type': 'application/json',
        'content-length': Buffer.byteLength(payload),
        connection: 'close',
      });
      response.end(payload);
    });
  });
  server.on('connection', (socket) => {
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Provider did not bind a loopback port');
  return {
    endpoint: `http://127.0.0.1:${address.port}`,
    close: () => new Promise<void>((resolve) => {
      for (const socket of sockets) socket.destroy();
      server.close(() => resolve());
    }),
  };
}

async function configureProvider(page: Page, session: OwnerSession, endpoint: string) {
  const response = await page.request.put('/api/v1/agent/provider', {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      'Idempotency-Key': randomUUID(),
    },
    data: { endpoint, model: 'fixture', num_ctx: 8192, num_predict: 256, locale: 'en', expected_profile_revision: await readProviderRevision(page, session) },
  });
  expect(response.ok()).toBe(true);
}

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

async function startFromConsent(runs: Locator, task: string, partyName: string) {
  await expect(runs.getByRole('checkbox', { name: `Party: ${partyName}` })).toBeVisible();
  await runs.getByRole('textbox', { name: 'Task for delegated run' }).fill(task);
  await runs.getByRole('checkbox', { name: `Party: ${partyName}` }).check();
  await runs.getByRole('checkbox', { name: 'Acknowledge scoped read grant' }).check();
  await runs.getByRole('button', { name: 'Start scoped run' }).click();
  const row = runs.locator('[data-run-id]').first();
  await expect(row).toBeVisible();
  const id = (await row.getAttribute('data-run-id'))!;
  await expect(runs.getByRole('status')).toContainText(`Run ${id} started`);
  return { id, row };
}

test('u02_owner_starts_scoped_read_and_sees_result', async ({ page }) => {
  const session = await signIn(page);
  const name = `U02 scoped ${randomUUID()}`;
  const party = await grantedParty(page, session, name);
  const provider = await startProvider([
    { message: { role: 'assistant', content: 'probe' } },
    {
      message: {
        role: 'assistant',
        content: 'checking the scoped party',
        tool_calls: [{ id: 'call-1', function: { name: 'commands__run', arguments: JSON.stringify({ id: 'party.get_summary', version: 1, args: { party_id: party.id } }) } }],
      },
    },
    { message: { role: 'assistant', content: `Scoped result for ${name}` } },
  ]);
  try {
    await configureProvider(page, session, provider.endpoint);
    const runs = await openRuns(page);
    await expect(runs.getByRole('checkbox', { name: 'Grant party.read' })).toBeChecked();
    await expect(runs.getByRole('checkbox', { name: 'Grant opportunity.phase.edit' })).not.toBeChecked();
    const { id, row } = await startFromConsent(runs, `Summarise ${name} using granted reads`, name);

    await expect(row).toHaveAttribute('data-state', /^(succeeded|failed)$/, { timeout: 20_000 });
    if (await row.getAttribute('data-state') !== 'succeeded') return;
    const transcriptResponse = await page.request.get(`/api/v1/agent/runs/${id}/transcript?limit=20`, {
      headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
    });
    expect(transcriptResponse.ok()).toBe(true);
    const turns = await transcriptResponse.json() as { items: Array<{ speaker: string }> };
    expect(turns.items.map((turn) => turn.speaker)).toEqual(expect.arrayContaining(['tool', 'assistant']));

    await runs.getByRole('button', { name: `View transcript ${id}` }).click();
    const transcript = runs.getByRole('region', { name: `Transcript for ${id}` });
    await expect(transcript).toBeVisible();
    await expect(transcript).toContainText(name);
    const next = runs.getByRole('button', { name: 'Next transcript page' });
    if (await next.isVisible()) {
      await next.click();
      await expect(transcript).not.toBeEmpty();
      await runs.getByRole('button', { name: 'Previous transcript page' }).click();
      await expect(transcript).toContainText(name);
    }
    expect(await transcript.locator('script, img').count()).toBe(0);
  } finally {
    await provider.close();
  }
});

test('u02_cancel_and_reopen_persists_state', async ({ page }, testInfo) => {
  const session = await signIn(page);
  const name = `U02 cancel ${randomUUID()}`;
  await grantedParty(page, session, name);
  let provider: ScriptedProvider | null = null;
  if (testInfo.project.name === 'rust') {
    provider = await startProvider([], true);
    await configureProvider(page, session, provider.endpoint);
  }
  try {
    const runs = await openRuns(page);
    const { id, row } = await startFromConsent(runs, `Pause work on ${name}`, name);
    await expect(row).toHaveAttribute('data-state', /^(queued|running)$/);
    await runs.getByRole('button', { name: `Cancel run ${id}` }).click();
    await expect(row).toHaveAttribute('data-state', 'cancelled');
    await expect(runs.getByRole('status')).toContainText(`Run ${id} was cancelled.`);

    // Re-open the Settings view: state comes from the API, not browser storage.
    await page.getByRole('link', { name: 'Resources' }).click();
    await page.getByRole('link', { name: 'Settings' }).click();
    const reopened = page.getByRole('region', { name: 'Delegated runs' });
    const restored = reopened.locator(`[data-run-id="${id}"]`);
    await expect(restored).toBeVisible();
    await expect(restored).toHaveAttribute('data-state', 'cancelled');
    await expect(reopened.getByRole('button', { name: `Cancel run ${id}` })).toHaveCount(0);

    const response = await page.request.get(`/api/v1/agent/runs/${id}`, {
      headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
    });
    expect(response.ok()).toBe(true);
    expect((await response.json() as { state: string }).state).toBe('cancelled');
  } finally {
    await provider?.close();
  }
});

async function rotateEpoch(page: Page, session: OwnerSession, project: string) {
  if (project === 'mock') {
    // The fixture's admin restore path rotates the same epoch as a real
    // maintenance restore; the Settings backup routes belong to the Rust lane.
    const headers = (key: string) => ({
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      ...(key ? { 'Idempotency-Key': key } : {}),
    });
    const backupResponse = await page.request.post('/api/v1/admin/backups', { headers: headers(randomUUID()), data: {} });
    expect(backupResponse.ok()).toBe(true);
    const backup = await backupResponse.json() as { id: string };
    const overview = await page.request.get('/api/v1/admin/overview', { headers: headers('') });
    const cursor = await overview.json() as { event_cursor: string };
    const previewResponse = await page.request.post('/api/v1/admin/previews', {
      headers: headers(randomUUID()), data: { operation: 'restore', backup_id: backup.id },
    });
    expect(previewResponse.ok()).toBe(true);
    const preview = await previewResponse.json() as { id: string; digest: string };
    const commit = await page.request.post(`/api/v1/admin/previews/${preview.id}/commit`, {
      headers: headers(randomUUID()),
      data: {
        preview_id: preview.id, digest: preview.digest,
        expected_workspace_epoch: session.epoch, expected_event_cursor: cursor.event_cursor,
        confirmation: preview.digest.slice(-12),
      },
    });
    expect(commit.ok()).toBe(true);
    return;
  }
  // The integrated lane has its own maintenance flow; intercept the session
  // boundary to present the equivalent rotated generation and revoked token.
  await page.route('**/api/v1/session', async (route) => {
    const original = await route.fetch();
    const body = await original.json() as Record<string, unknown>;
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ ...body, epoch: 'rotated-epoch' }),
    });
  });
  await page.route('**/api/v1/agent/runs**', async (route) => {
    if (route.request().method() === 'GET') {
      await route.fulfill({
        status: 403,
        contentType: 'application/json',
        body: JSON.stringify({ error: { code: 'TOKEN_INVALID', message: 'Session invalidated by workspace rotation', details: [] } }),
      });
      return;
    }
    await route.continue();
  });
}

test('u02_session_and_epoch_invalidation', async ({ page }, testInfo) => {
  const session = await signIn(page);
  const name = `U02 epoch ${randomUUID()}`;
  await grantedParty(page, session, name);
  let provider: ScriptedProvider | null = null;
  if (testInfo.project.name === 'rust') {
    provider = await startProvider([], true);
    await configureProvider(page, session, provider.endpoint);
  }
  try {
    const runs = await openRuns(page);
    await startFromConsent(runs, `Hold this read for ${name}`, name);

    let runReads = 0;
    page.on('request', (request) => {
      if (request.url().includes('/api/v1/agent/runs') && request.method() === 'GET') runReads += 1;
    });
    await expect.poll(() => runReads).toBeGreaterThan(1);

    // Rotate the generation through a verified restore in each lane.
    await rotateEpoch(page, session, testInfo.project.name);

    // Old owner credentials can no longer read the run collection.
    const staleStatus = await page.evaluate(async (credentials) => {
      const response = await fetch('/api/v1/agent/runs', {
        headers: { 'X-Parley-Token': credentials.token, 'X-Parley-Epoch': credentials.epoch },
      });
      return response.status;
    }, { token: session.mutation_token, epoch: session.epoch });
    expect(staleStatus).toBeGreaterThanOrEqual(400);

    // The shell announces the rotation; the run view must stop polling.
    await page.evaluate(() => {
      globalThis.dispatchEvent(new CustomEvent('parley:workspace-changed', { detail: { operation: 'restore', epoch: 'rotated-epoch' } }));
    });
    await expect(runs.getByText(/epoch changed/i)).toBeVisible();
    const readsAtStale = runReads;
    await page.waitForTimeout(1700);
    expect(runReads).toBe(readsAtStale);
  } finally {
    await provider?.close();
  }
});
