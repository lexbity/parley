import { randomUUID } from 'node:crypto';
import { createServer, type Server } from 'node:http';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties } from './parties';
import { invoke, openPalette, palette, readProviderRevision, signIn } from './sheet';

type ScriptedProvider = { endpoint: string; close(): Promise<void> };

/** A loopback tool-capable provider for the real Rust runtime. */
async function startProvider(replies: unknown[], hold = false): Promise<ScriptedProvider> {
  let index = 0;
  const sockets = new Set<import('node:net').Socket>();
  const server: Server = createServer((request, response) => {
    if (hold) return;
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

async function configureProvider(page: import('@playwright/test').Page, session: { mutation_token: string; epoch: string }, endpoint: string) {
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

test('u03_palette_creates_opportunity_with_authoritative_party_picker', async ({ page }) => {
  await signIn(page);
  const name = `U03 palette ${randomUUID()}`;
  await openParties(page);
  await addParty(page, name, 'person');
  await page.getByRole('link', { name: 'Queue' }).click();

  await openPalette(page);
  const combobox = palette(page).getByRole('combobox');
  await combobox.fill('create opportunity');
  await expect(palette(page).getByRole('option').first()).toContainText('Create opportunity');
  await combobox.press('Enter');
  await expect(palette(page)).toContainText('Opportunity title');

  const title = `Palette created ${randomUUID()}`;
  await palette(page).getByLabel('Opportunity title').fill(title);
  await palette(page).getByLabel('Primary party').selectOption({ label: name });
  await palette(page).getByRole('button', { name: 'Continue' }).click();
  await expect(palette(page)).toContainText('Create this open opportunity');
  await palette(page).getByRole('button', { name: 'Confirm' }).click();
  await expect(page.locator('tbody tr[data-opportunity-id]').filter({ hasText: title })).toBeVisible();
  await palette(page).getByRole('button', { name: 'Done' }).click();
  await expect(page.getByRole('heading', { name: 'Queue', exact: true })).toBeVisible();
});

test('u03_command_create_replays_one_fact', async ({ page }) => {
  const session = await signIn(page);
  const partyName = `U03 replay ${randomUUID()}`;
  const partyResponse = await page.request.post('/api/v1/entities', {
    headers: {
      'content-type': 'application/json',
      'X-Parley-Token': session.mutation_token,
      'X-Parley-Epoch': session.epoch,
      'Idempotency-Key': randomUUID(),
    },
    data: { kind: 'person', name: partyName },
  });
  expect(partyResponse.ok()).toBe(true);
  const partyBody = (await partyResponse.json()) as { data: { id: string } };
  const party = partyBody.data;
  const args = { title: `Replayed create ${randomUUID()}`, primary_party_id: party.id };
  const key = randomUUID();
  const first = await invoke(page, session, 'opportunity.create', args, key);
  expect(first.status).toBe(200);
  expect(first.body.ok).toBe(true);
  const original = first.body.result!.opportunity_id;
  const replay = await invoke(page, session, 'opportunity.create', args, key);
  expect(replay.status).toBe(200);
  expect(replay.body.replayed).toBe(true);
  expect(replay.body.result!.opportunity_id).toBe(original);
  const fresh = await invoke(page, session, 'opportunity.create', args, randomUUID());
  expect(fresh.status).toBe(200);
  expect(fresh.body.result!.opportunity_id).not.toBe(original);
});

test('u03_delegated_create_consent_and_revocation_keep_queue_current', async ({ page }, testInfo) => {
  const session = await signIn(page);
  const name = `U03 delegate ${randomUUID()}`;
  await openParties(page);
  await addParty(page, name, 'person');
  let provider: ScriptedProvider | null = null;
  if (testInfo.project.name === 'rust') {
    // An unanswered probe keeps the claimed run active until the owner
    // cancellation, which must win at the writer boundary.
    provider = await startProvider([], true);
    await configureProvider(page, session, provider.endpoint);
  }
  try {
    await page.getByRole('link', { name: 'Settings' }).click();
    const runs = page.getByRole('region', { name: 'Delegated runs' });
    await runs.getByRole('checkbox', { name: 'Grant opportunity.create' }).check();
    await expect(runs.getByRole('checkbox', { name: 'Grant created by this run opportunities' })).toBeVisible();
    await runs.getByRole('checkbox', { name: 'Grant created by this run opportunities' }).check();
    await runs.getByRole('textbox', { name: 'Task for delegated run' }).fill(`Create a scoped opportunity for ${name}`);
    await runs.getByRole('checkbox', { name: `Party: ${name}` }).check();
    await runs.getByRole('checkbox', { name: 'Acknowledge scoped read grant' }).check();
    await runs.getByRole('button', { name: 'Start scoped run' }).click();
    const row = runs.locator('[data-run-id]').first();
    await expect(row).toBeVisible();
    const id = (await row.getAttribute('data-run-id'))!;
    await runs.getByRole('button', { name: `Cancel run ${id}` }).click();
    await expect(row).toHaveAttribute('data-state', 'cancelled');

    await page.getByRole('link', { name: 'Queue' }).click();
    await expect(page.getByRole('heading', { name: 'Queue', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'New opportunity' })).toBeVisible();
  } finally {
    await provider?.close();
  }
});
