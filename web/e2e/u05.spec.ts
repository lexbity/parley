import { randomUUID } from 'node:crypto';
import { createServer, type Server } from 'node:http';
import { expect, type Locator, type Page } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties } from './parties';
import { readProviderRevision, signIn, type OwnerSession } from './sheet';

/** A loopback scripted provider for the real Rust runtime. Responses are
 *  chosen from the request body so connection order stays deterministic even
 *  when the worker and the resumed run interleave. */
type ScriptedProvider = { endpoint: string; close(): Promise<void> };

const TAGS = { models: [{ name: 'fixture', digest: 'sha256:u05' }] };

function brieferOutput() {
  return {
    message: {
      role: 'assistant',
      content: JSON.stringify({
        response: 'A brief for the party is ready for owner review.',
        items: [{
          kind: 'jargon',
          text: 'Onboarding is a term used for helping a new user get started.',
          detail: {
            term: 'onboarding',
            plain_meaning: 'Helping a new user get started.',
            use_or_avoid: 'owner_decides',
            applies_to: 'role',
          },
          tier: 2,
          quote: null,
          check_suggestion: 'Check the party own wording in the supplied professional material.',
        }],
      }),
    },
    prompt_eval_count: 20,
    eval_count: 15,
  };
}

async function startProvider(partyId: string) {
  const sockets = new Set<import('node:net').Socket>();
  const server: Server = createServer((request, response) => {
    sockets.add(request.socket);
    request.socket.on('close', () => sockets.delete(request.socket));
    void (async () => {
      let body = '';
      for await (const chunk of request) body += String(chunk);
      const reply = replyFor(request.method ?? 'GET', body);
      const payload = JSON.stringify(reply);
      response.writeHead(200, {
        'content-type': 'application/json',
        'content-length': Buffer.byteLength(payload),
        connection: 'close',
      });
      response.end(payload);
    })();
  });
  server.on('connection', (socket) => sockets.add(socket));
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

  function replyFor(method: string, body: string): unknown {
    if (method === 'GET') return TAGS;
    let value: Record<string, unknown>;
    try { value = JSON.parse(body) as Record<string, unknown>; } catch { return TAGS; }
    if (!Array.isArray(value.messages)) return TAGS;
    const messages = value.messages as Array<Record<string, unknown>>;
    const content = messages
      .map((message) => message.content)
      .filter((item): item is string => typeof item === 'string');
    if (content.some((text) => text.includes('Tool support probe'))) {
      return { message: { role: 'assistant', content: 'probe' } };
    }
    if (value.format) return brieferOutput();
    if (content.some((text) => text.includes('review_reference'))) {
      return { message: { role: 'assistant', content: 'The brief proposal is ready for owner review.' } };
    }
    const tool = messages
      .filter((message) => message.role === 'tool')
      .map((message) => String(message.content ?? ''))
      .reverse()[0];
    if (tool) {
      let jobId = '';
      try {
        const envelope = JSON.parse(tool) as { result?: { job_id?: string } };
        jobId = envelope.result?.job_id ?? '';
      } catch { /* malformed tool body */ }
      return {
        message: {
          role: 'assistant',
          content: 'check the brief job',
          tool_calls: [{
            id: 'call-1',
            function: {
              name: 'commands__run',
              arguments: JSON.stringify({ id: 'job.get_status', version: 1, args: { job_id: jobId } }),
            },
          }],
        },
      };
    }
    return {
      message: {
        role: 'assistant',
        content: 'start a brief',
        tool_calls: [{
          id: 'call-1',
          function: {
            name: 'commands__run',
            arguments: JSON.stringify({
              id: 'brief.start',
              version: 1,
              args: { party_id: partyId, expected_revision: 1, purpose: 'Prepare a reviewable brief for the party' },
            }),
          },
        }],
      },
    };
  }
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
  const health = await page.request.post('/api/v1/agent/provider/health', {
    headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
  });
  expect(health.ok()).toBe(true);
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

async function startBriefRun(runs: Locator, task: string, partyName: string) {
  await expect(runs.getByRole('checkbox', { name: `Party: ${partyName}` })).toBeVisible();
  await runs.getByRole('textbox', { name: 'Task for delegated run' }).fill(task);
  await runs.getByRole('checkbox', { name: `Party: ${partyName}` }).check();
  await runs.getByRole('checkbox', { name: 'Grant brief.start' }).check();
  await runs.getByRole('checkbox', { name: 'Grant job.read' }).check();
  await runs.getByRole('checkbox', { name: 'Acknowledge scoped read grant' }).check();
  await runs.getByRole('button', { name: 'Start scoped run' }).click();
  const row = runs.locator('[data-run-id]').first();
  await expect(row).toBeVisible();
  const id = (await row.getAttribute('data-run-id'))!;
  await expect(runs.getByRole('status')).toContainText(`Run ${id} started`);
  return { id, row };
}

test('u05_delegated_brief_reports_waiting_and_opens_review', async ({ page }, testInfo) => {
  const session = await signIn(page);
  const name = `U05 brief ${randomUUID()}`;
  const party = await grantedParty(page, session, name);
  let provider: ScriptedProvider | null = null;
  if (testInfo.project.name === 'rust') {
    provider = await startProvider(party.id);
    await configureProvider(page, session, provider.endpoint);
  }
  try {
    const runs = await openRuns(page);
    const { id, row } = await startBriefRun(runs, `Prepare a brief for ${name}`, name);

    // The run reports its waiting state distinctly, then a review reference.
    if (testInfo.project.name === 'mock') {
      await expect(row).toHaveAttribute('data-state', 'waiting_job', { timeout: 15_000 });
      await expect(runs.getByText(/Waiting for its background job/i)).toBeVisible();
    }
    await expect(row).toHaveAttribute('data-state', 'succeeded', { timeout: 30_000 });
    const statusResponse = await page.request.get(`/api/v1/agent/runs/${id}`, {
      headers: { 'X-Parley-Token': session.mutation_token, 'X-Parley-Epoch': session.epoch },
    });
    expect(statusResponse.ok()).toBe(true);
    const status = await statusResponse.json() as {
      state: string;
      review: { job_id: string; scope_kind: string; scope_id: string } | null;
    };
    expect(status.state).toBe('succeeded');
    expect(status.review).toEqual({ job_id: expect.any(String), scope_kind: 'party', scope_id: party.id });

    const reviewButton = runs.getByRole('button', { name: `Open reviewed candidate for run ${id}` });
    await expect(reviewButton).toBeVisible();
    await reviewButton.click();

    // The reviewed brief opens on the party desk with the chat pane active.
    await expect(page).toHaveURL(new RegExp(`/parties/${party.id}$`));
    await expect(page.getByRole('heading', { name: 'Lead chat' })).toBeVisible();
    await page.getByText('Research scope and brief', { exact: true }).click();
    await expect(page.getByRole('button', { name: 'Generate brief proposals' })).toBeVisible();

    // The palette exposes the local open-brief command and opens the same pane.
    await page.keyboard.press('Control+Shift+P');
    const palette = page.locator('.command-palette');
    await expect(palette).toBeVisible();
    await palette.getByRole('combobox', { name: 'Search commands' }).fill('brief');
    const openBrief = palette.getByRole('option', { name: /Open brief/i });
    await expect(openBrief).toBeVisible();
    await openBrief.click();
    await expect(page.getByRole('heading', { name: 'Lead chat' })).toBeVisible();
  } finally {
    await provider?.close();
  }
});

test('u05_hung_provider_leaves_queue_and_brief_usable', async ({ page }, testInfo) => {
  const session = await signIn(page);
  const name = `U05 hang ${randomUUID()}`;
  const party = await grantedParty(page, session, name);
  let provider: ScriptedProvider | null = null;
  if (testInfo.project.name === 'rust') {
    // A hold provider accepts connections but never replies, so the claimed
    // run stays running and the rest of the app must stay usable.
    const sockets = new Set<import('node:net').Socket>();
    const holdServer: Server = createServer(() => { /* never respond */ });
    holdServer.on('connection', (socket) => sockets.add(socket));
    await new Promise<void>((resolve) => holdServer.listen(0, '127.0.0.1', resolve));
    const address = holdServer.address();
    if (!address || typeof address === 'string') throw new Error('hold provider bind failed');
    const endpoint = `http://127.0.0.1:${address.port}`;
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
    provider = {
      endpoint,
      close: () => new Promise<void>((resolve) => {
        for (const socket of sockets) socket.destroy();
        holdServer.close(() => resolve());
      }),
    };
  }
  try {
    const runs = await openRuns(page);
    const { id, row } = await startBriefRun(runs, `Hang the provider for ${name}`, name);
    await expect(row).toHaveAttribute('data-state', /^(queued|running)$/, { timeout: 10_000 });

    // The Queue tab keeps working while the run is stuck.
    await page.getByRole('link', { name: 'Queue' }).click();
    await expect(page).toHaveURL(/\/queue$/);
    await expect(page.locator('main')).toBeVisible();

    // The Brief surface stays reachable and usable on the parties desk.
    await page.goto(`/parties/${party.id}`);
    await page.getByRole('button', { name: 'Research chat' }).click();
    await expect(page.getByRole('heading', { name: 'Lead chat' })).toBeVisible();
    await page.getByText('Research scope and brief', { exact: true }).click();
    await expect(page.getByRole('button', { name: 'Generate brief proposals' })).toBeVisible();

    // The stuck run is still observed without breaking the view.
    await page.getByRole('link', { name: 'Settings' }).click();
    const runsAgain = page.getByRole('region', { name: 'Delegated runs' });
    const stuck = runsAgain.locator(`[data-run-id="${id}"]`);
    await expect(stuck).toBeVisible();
    await expect(stuck).toHaveAttribute('data-state', /^(queued|running)$/);
  } finally {
    await provider?.close();
  }
});
