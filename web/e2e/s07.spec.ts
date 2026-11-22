import { randomUUID } from 'node:crypto';
import { expect, type Page } from '@playwright/test';
import { test } from './fixtures';

/**
 * S07 deadline equality and party-wide suppression (RFC-0002 BE-08). The
 * browser drives the same HTTP contract on the mock and real Rust service:
 * an explicit Mark sent records a send with a response deadline, a reply
 * moves the thread into conversation, a rung acceptance advances the ladder,
 * and a resolution closes active timers. Capacity provisioning defines the
 * ledger charged once per capacity-carrying send.
 */

type Api = { token: string; epoch: string; key: () => string };

async function api(page: Page): Promise<Api> {
  const session = await (await page.request.get('/api/v1/session')).json();
  return {
    token: String(session.mutation_token),
    epoch: String(session.epoch),
    key: () => `s07-${randomUUID()}`,
  };
}

function authHeaders(apiState: Api, extra: Record<string, string> = {}) {
  return {
    'X-Parley-Token': apiState.token,
    'X-Parley-Epoch': apiState.epoch,
    ...extra,
  };
}

test('s07_browser_explicit_mark_sent_deadline_outcome_and_resolution', async ({ page }) => {
  await page.goto('/');
  const name = `Dana Lee ${randomUUID()}`;
  await page.getByRole('button', { name: 'Leads' }).click();
  await page.locator('section[aria-labelledby="leads-heading"] input').fill(name);
  await page.getByRole('button', { name: 'Add lead' }).click();

  await page.getByRole('button', { name: 'Queue' }).click();
  await page.getByLabel('Opportunity title').fill('Onboarding follow-up');
  await page.getByLabel('Participant').selectOption({ label: `${name} · person` });
  await page.getByRole('button', { name: 'Create opportunity' }).click();
  await expect(page.getByRole('heading', { name: 'Onboarding follow-up' })).toBeVisible();

  const apiState = await api(page);
  const opportunity = await (await page.request.get('/api/v1/opportunities', { headers: authHeaders(apiState) })).json();
  const opportunityId = String(opportunity[0].id);
  const partyPayload = await (await page.request.get('/api/v1/entities?kind=person', { headers: authHeaders(apiState) })).json();
  const partyList = (Array.isArray(partyPayload) ? partyPayload : partyPayload.data) as Array<{ id: string; name: string }>;
  const recipientId = String(partyList.find((entity) => entity.name === name)?.id);

  const addInterest = async (side: string, statement: string) => {
    const response = await page.request.post(`/api/v1/opportunities/${opportunityId}/interests`, {
      headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
      data: {
        recipient_id: recipientId, side, statement,
        knowledge_kind: side === 'them' ? 'hypothesis' : 'owner_report',
        ...(side === 'them' ? { check_suggestion: 'Ask the owner whether this is current.' } : {}),
      },
    });
    expect(response.status()).toBe(201);
    return response.json();
  };
  const them = await addInterest('them', 'A clearer onboarding handoff');
  const me = await addInterest('me', 'Offer a usable checklist');
  const exchange = await (await page.request.post(`/api/v1/opportunities/${opportunityId}/exchange`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { recipient_id: recipientId, they_get: 'A practical onboarding checklist', i_get: 'A better understanding of the handoff', interest_ids: [them.id, me.id] },
  })).json();
  const attest = await page.request.post(`/api/v1/opportunities/${opportunityId}/exchange/${recipientId}/attest`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {},
  });
  expect(attest.status()).toBe(200);

  const template = await (await page.request.post('/api/v1/templates', {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { name: 'Brief hello', body: 'Hello.' },
  })).json();
  for (const action of ['approve', 'activate']) {
    const transition = await page.request.post(`/api/v1/templates/${template.id}/${action}`, {
      headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
      data: {},
    });
    expect(transition.status()).toBe(204);
  }

  const material = await (await page.request.post(`/api/v1/opportunities/${opportunityId}/materials`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {
      recipient_id: recipientId, kind: 'outreach', template_version_id: template.id,
      body: 'Hello.', exchange_version: exchange.version,
      slot_fills: [], sentence_bindings: [{ sentence_index: 0, claim_type: 'generic' }],
    },
  })).json();
  const approval = await (await page.request.post(`/api/v1/materials/${material.id}/approve`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {
      override_reasons: {
        'S1 GIVE_FIRST': 'This is a reviewed factual outreach, not a gift message.',
        'S2 ANCHOR': 'The exact approved text is the anchor for this outreach.',
        'S7 CADENCE': 'The follow-up is scheduled at the pack cadence.',
      },
    },
  })).json();
  expect(String(approval.content_sha256)).toBe(String(material.content_sha256));
  const released = await (await page.request.post(`/api/v1/materials/${material.id}/release`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {},
  })).json();

  // A follow-up touch needs its own one-use release grant; both grants are
  // prepared before any send so their approval dependency snapshots match.
  const secondRelease = await (await page.request.post(`/api/v1/materials/${material.id}/release`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {},
  })).json();
  expect(String(secondRelease.release_id)).not.toBe(String(released.release_id));

  // Explicit Mark sent records a send with a response deadline.
  const threadId = `browser-thread-${randomUUID()}`;
  const send = await (await page.request.post(`/api/v1/threads/${threadId}/send`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {
      opportunity_id: opportunityId, recipient_id: recipientId,
      primary: { material_id: material.id, version: material.version, release_id: released.release_id },
      companions: [], deadline: { days: 7 },
    },
  })).json();
  expect(String(send.thread_id)).toBe(threadId);
  expect(Number(send.touch_no)).toBe(1);
  expect(String(send.response_deadline)).toBeTruthy();
  expect(send.capacity_charged).toBe(false);

  const threads = await (await page.request.get('/api/v1/threads', { headers: authHeaders(apiState) })).json();
  const ownThread = threads.find((thread: { id: string }) => String(thread.id) === threadId);
  expect(String(ownThread.state)).toBe('waiting');
  expect(Number(ownThread.current_touch)).toBe(1);

  // A nondeclining reply moves the thread into conversation.
  const outcome = await (await page.request.post(`/api/v1/sends/${send.id}/outcome`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { class: 'reply_positive', received_at: new Date().toISOString(), note: 'Replied the same day' },
  })).json();
  expect(String(outcome.class)).toBe('reply_positive');
  const afterReply = await (await page.request.get('/api/v1/threads', { headers: authHeaders(apiState) })).json();
  const threadAfterReply = afterReply.find((thread: { id: string }) => String(thread.id) === threadId);
  expect(String(threadAfterReply.state)).toBe('conversing');

  // A second explicit send starts a new touch with its own deadline.
  // A second explicit send starts a new touch with its own deadline and its
  // own one-use release grant.
  const secondSend = await (await page.request.post(`/api/v1/threads/${threadId}/send`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {
      opportunity_id: opportunityId, recipient_id: recipientId,
      primary: { material_id: material.id, version: material.version, release_id: secondRelease.release_id },
      companions: [], deadline: { days: 7 },
    },
  })).json();
  expect(Number(secondSend.touch_no)).toBe(2);
  expect(secondSend.capacity_charged).toBe(false);

  // A rung acceptance advances the ladder once per send.
  const rung = await (await page.request.post(`/api/v1/sends/${secondSend.id}/outcome`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { class: 'rung_accepted', rung_accepted: 2 },
  })).json();
  expect(String(rung.class)).toBe('rung_accepted');
  const replay = await (await page.request.post(`/api/v1/sends/${secondSend.id}/outcome`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { class: 'rung_accepted', rung_accepted: 2 },
  })).json();
  expect(String(replay.id)).toBe(String(rung.id));

  // Resolution closes active timers and marks the opportunity resolved.
  const resolution = await (await page.request.post(`/api/v1/opportunities/${opportunityId}/resolution`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { result: 'mutual_win', they_benefited: 'yes', i_benefited: 'yes' },
  })).json();
  expect(String(resolution.result)).toBe('mutual_win');
  const opportunityAfter = await (await page.request.get(`/api/v1/opportunities/${opportunityId}`, { headers: authHeaders(apiState) })).json();
  expect(String(opportunityAfter.state)).toBe('resolved');

  // Capacity provisioning defines the ledger.
  const capacity = await (await page.request.post('/api/v1/capacity', {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { period: 'quarter', unit: 'project slots', total: 20 },
  })).json();
  expect(Number(capacity.total)).toBe(20);
  expect(Number(capacity.remaining)).toBe(20);
});
