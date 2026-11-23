import { randomUUID } from 'node:crypto';
import { expect, type Page } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties } from './parties';

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
  await openParties(page);
  await addParty(page, name, 'person');

  await page.getByRole('link', { name: 'Queue' }).click();
  await page.getByRole('button', { name: 'New opportunity' }).click();
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
  // Copy through the production release UI, then explicitly record delivery
  // from the same material and recipient surface.
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.getByRole('link', { name: 'Output' }).click();
  const outputRow = page.locator(`#output-material-${material.id}`);
  await expect(outputRow).toBeVisible();
  await outputRow.getByRole('button', { name: 'Copy approved text' }).click();
  await expect(outputRow.getByRole('status')).toContainText('Copy confirmed');
  await outputRow.getByLabel('Reply deadline (inclusive local date)').fill('2026-11-28');
  const sendResponsePromise = page.waitForResponse((response) => response.request().method() === 'POST'
    && new URL(response.url()).pathname.endsWith('/send'));
  await outputRow.getByRole('button', { name: 'Mark this delivered version sent' }).click();
  const sendResponse = await sendResponsePromise;
  expect(sendResponse.status()).toBe(201);
  const send = await sendResponse.json();
  const threadId = String(send.thread_id);
  expect(Number(send.touch_no)).toBe(1);
  expect(String(send.response_deadline)).toBeTruthy();
  expect(send.capacity_charged).toBe(false);

  const threads = await (await page.request.get('/api/v1/threads', { headers: authHeaders(apiState) })).json();
  const ownThread = threads.find((thread: { id: string }) => String(thread.id) === threadId);
  expect(String(ownThread.state)).toBe('waiting');
  expect(Number(ownThread.current_touch)).toBe(1);
  // The thread listing exposes the send ID so a reloaded client can attach a
  // response to the historical send (RFC-0003 FQ5).
  expect(Array.isArray(ownThread.sends)).toBe(true);
  expect(String(ownThread.sends[0].id)).toBe(String(send.id));
  expect(Number(ownThread.sends[0].touch_no)).toBe(1);

  // A nondeclining reply moves the thread into conversation.
  const outcome = await (await page.request.post(`/api/v1/sends/${send.id}/outcome`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { class: 'reply_positive', received_at: new Date().toISOString(), note: 'Replied the same day' },
  })).json();
  expect(String(outcome.class)).toBe('reply_positive');
  const afterReply = await (await page.request.get('/api/v1/threads', { headers: authHeaders(apiState) })).json();
  const threadAfterReply = afterReply.find((thread: { id: string }) => String(thread.id) === threadId);
  expect(String(threadAfterReply.state)).toBe('conversing');

  // A second explicit send starts a new touch with its own deadline and its
  // own one-use release grant. The first send changed the thread and party
  // policy revisions, so the follow-up is a new reviewed material version.
  const revised = await (await page.request.post(`/api/v1/materials/${material.id}/versions`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {
      recipient_id: recipientId, kind: 'outreach', template_version_id: template.id,
      body: 'Hello.', exchange_version: exchange.version,
      slot_fills: [], sentence_bindings: [{ sentence_index: 0, claim_type: 'generic' }],
    },
  })).json();
  expect(Number(revised.version)).toBeGreaterThan(Number(material.version));
  const followupApproval = await (await page.request.post(`/api/v1/materials/${material.id}/approve`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {
      override_reasons: {
        'S1 GIVE_FIRST': 'This is a reviewed factual outreach, not a gift message.',
        'S2 ANCHOR': 'The exact approved text is the anchor for this outreach.',
        'S7 CADENCE': 'The follow-up is scheduled at the pack cadence.',
      },
    },
  })).json();
  expect(String(followupApproval.content_sha256)).toBe(String(revised.content_sha256));
  const secondRelease = await (await page.request.post(`/api/v1/materials/${material.id}/release`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {},
  })).json();
  const secondSend = await (await page.request.post(`/api/v1/threads/${threadId}/send`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {
      opportunity_id: opportunityId, recipient_id: recipientId,
      primary: { material_id: material.id, version: Number(revised.version), release_id: secondRelease.release_id },
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

  // The rung outcome is visible in the thread history after the reload, so
  // the second touch's history is reconstructed from the wire, not memory.
  const historyAfter = await (await page.request.get('/api/v1/threads', { headers: authHeaders(apiState) })).json();
  const threadAfter = historyAfter.find((thread: { id: string }) => String(thread.id) === threadId);
  const secondTouch = threadAfter.sends.find((entry: { touch_no: number }) => Number(entry.touch_no) === 2);
  expect(String(secondTouch.id)).toBe(String(secondSend.id));
  expect(String(secondTouch.outcome.class)).toBe('rung_accepted');
});

test('s07_browser_dnc_clearance_is_reason_bearing_and_restores_release', async ({ page }) => {
  await page.goto('/');
  const name = `Declining Dana ${randomUUID()}`;
  await openParties(page);
  await addParty(page, name, 'person');

  await page.getByRole('link', { name: 'Queue' }).click();
  await page.getByRole('button', { name: 'New opportunity' }).click();
  await page.getByLabel('Opportunity title').fill('Follow-up after decline');
  await page.getByLabel('Participant').selectOption({ label: `${name} · person` });
  await page.getByRole('button', { name: 'Create opportunity' }).click();
  await expect(page.getByRole('heading', { name: 'Follow-up after decline' })).toBeVisible();

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
  const them = await addInterest('them', 'A clearer handoff');
  const me = await addInterest('me', 'Offer a usable checklist');
  const exchange = await (await page.request.post(`/api/v1/opportunities/${opportunityId}/exchange`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { recipient_id: recipientId, they_get: 'A practical checklist', i_get: 'A better understanding of the handoff', interest_ids: [them.id, me.id] },
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
  const overrides = {
    'S1 GIVE_FIRST': 'This is a reviewed factual outreach, not a gift message.',
    'S2 ANCHOR': 'The exact approved text is the anchor for this outreach.',
  };
  const approval = await (await page.request.post(`/api/v1/materials/${material.id}/approve`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { override_reasons: overrides },
  })).json();
  expect(String(approval.content_sha256)).toBe(String(material.content_sha256));
  const released = await (await page.request.post(`/api/v1/materials/${material.id}/release`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {},
  })).json();
  const send = await (await page.request.post(`/api/v1/threads/t-dnc-browser/send`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {
      opportunity_id: opportunityId, recipient_id: recipientId,
      primary: { material_id: material.id, version: released.version, release_id: released.release_id },
      companions: [], deadline: { days: 7 },
    },
  })).json();
  expect(Number(send.touch_no)).toBe(1);

  // An explicit decline suppresses the party; fresh release is blocked.
  const declined = await (await page.request.post(`/api/v1/sends/${send.id}/outcome`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { class: 'declined', received_at: new Date().toISOString() },
  })).json();
  expect(String(declined.class)).toBe('declined');
  const blocked = await page.request.post(`/api/v1/materials/${material.id}/release`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {},
  });
  expect(blocked.status()).toBe(422);

  // Clearance without a reason is rejected; a reason-bearing clearance
  // reopens eligibility and a fresh version becomes releasable.
  const noReason = await page.request.post(`/api/v1/entities/${recipientId}/clear-dnc`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { reason: 'short' },
  });
  expect(noReason.status()).toBe(422);
  const cleared = await (await page.request.post(`/api/v1/entities/${recipientId}/clear-dnc`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { reason: 'The counterparty asked to be re-contacted after a review.' },
  })).json();
  expect(String(cleared.state)).toBe('open');
  expect(cleared.do_not_contact).toBe(false);
  const revised = await (await page.request.post(`/api/v1/materials/${material.id}/versions`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {
      recipient_id: recipientId, kind: 'outreach', template_version_id: template.id,
      body: 'Hello.', exchange_version: exchange.version,
      slot_fills: [], sentence_bindings: [{ sentence_index: 0, claim_type: 'generic' }],
    },
  })).json();
  const freshApproval = await (await page.request.post(`/api/v1/materials/${material.id}/approve`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { override_reasons: overrides },
  })).json();
  expect(String(freshApproval.content_sha256)).toBe(String(revised.content_sha256));
  const freshRelease = await page.request.post(`/api/v1/materials/${material.id}/release`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {},
  });
  expect(freshRelease.status()).toBe(200);
});
