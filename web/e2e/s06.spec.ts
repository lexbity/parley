import { randomUUID } from 'node:crypto';
import { expect, type Page } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties } from './parties';

/**
 * S06 verified file issuance (RFC-0002 BQ4/BE-11). The browser session drives
 * the real local service and the in-process fixture API over the same HTTP
 * contract: upload bytes, review them for the named contact, bind them to an
 * approved material version, release and download the exact bytes, and prove
 * the grant is one-use. The same record appears in Queue and Output.
 */

const BODY = Buffer.from('S06 handoff payload — æøå ✓\n', 'utf8');

type Api = { token: string; epoch: string; key: () => string };

async function api(page: Page): Promise<Api> {
  const session = await (await page.request.get('/api/v1/session')).json();
  return {
    token: String(session.mutation_token),
    epoch: String(session.epoch),
    key: () => `s06-${randomUUID()}`,
  };
}

function authHeaders(apiState: Api, extra: Record<string, string> = {}) {
  return {
    'X-Parley-Token': apiState.token,
    'X-Parley-Epoch': apiState.epoch,
    ...extra,
  };
}

test('s06_browser_verified_file_handoff_downloads_exact_bytes_once', async ({ page }) => {
  await page.goto('/');
  const name = `Dana Lee ${randomUUID()}`;
  await openParties(page);
  await addParty(page, name, 'person');

  await page.getByRole('link', { name: 'Queue' }).click();
  await page.getByLabel('Opportunity title').fill('Handoff exchange');
  await page.getByLabel('Participant').selectOption({ label: `${name} · person` });
  await page.getByRole('button', { name: 'Create opportunity' }).click();
  await expect(page.getByRole('heading', { name: 'Handoff exchange' })).toBeVisible();

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
        recipient_id: recipientId,
        side,
        statement,
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
    data: { name: 'Handoff note', body: 'Hello.' },
  })).json();
  const approveTemplate = await page.request.post(`/api/v1/templates/${template.id}/approve`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {},
  });
  expect(approveTemplate.status()).toBe(204);
  const activateTemplate = await page.request.post(`/api/v1/templates/${template.id}/activate`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {},
  });
  expect(activateTemplate.status()).toBe(204);

  // An unreviewed upload cannot bind to a material.
  const upload = await (await page.request.post('/api/v1/uploads', {
    headers: authHeaders(apiState, { 'Content-Type': 'application/octet-stream', 'X-Parley-Filename': encodeURIComponent('handoff.pdf') }),
    data: BODY,
  })).json();
  const unreviewed = await page.request.post(`/api/v1/opportunities/${opportunityId}/materials`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {
      recipient_id: recipientId, kind: 'handoff', template_version_id: template.id,
      body: 'Please find the attached handoff.', exchange_version: exchange.version,
      slot_fills: [], sentence_bindings: [{ sentence_index: 0, claim_type: 'generic' }],
      attachment: { upload_id: upload.id },
    },
  });
  expect(unreviewed.status()).toBe(422);

  const review = await page.request.post(`/api/v1/uploads/${upload.id}/review`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: { share_permission: true },
  });
  expect(review.status()).toBe(200);

  const material = await (await page.request.post(`/api/v1/opportunities/${opportunityId}/materials`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {
      recipient_id: recipientId, kind: 'handoff', template_version_id: template.id,
      body: 'Please find the attached handoff.', exchange_version: exchange.version,
      slot_fills: [], sentence_bindings: [{ sentence_index: 0, claim_type: 'generic' }],
      attachment: { upload_id: upload.id },
    },
  })).json();
  expect(String(material.content_sha256)).toBe(String(upload.sha256));

  const approval = await (await page.request.post(`/api/v1/materials/${material.id}/approve`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {
      override_reasons: {
        'S1 GIVE_FIRST': 'This is a reviewed file handoff, not a gift message.',
        'S2 ANCHOR': 'The exact approved attachment bytes are the anchor for this handoff.',
      },
    },
  })).json();
  expect(String(approval.content_sha256)).toBe(String(upload.sha256));

  const released = await (await page.request.post(`/api/v1/materials/${material.id}/release`, {
    headers: authHeaders(apiState, { 'Idempotency-Key': apiState.key() }),
    data: {},
  })).json();
  expect(String(released.attachment.sha256)).toBe(String(upload.sha256));

  // Queue and Output address the same record.
  const queue = await (await page.request.get(`/api/v1/opportunities/${opportunityId}/materials`, { headers: authHeaders(apiState) })).json();
  expect(String(queue[0].id)).toBe(String(material.id));
  const outputList = await (await page.request.get('/api/v1/outputs', { headers: authHeaders(apiState) })).json();
  expect(outputList.some((item: { id: string }) => String(item.id) === String(material.id))).toBe(true);

  const first = await page.request.get(`/api/v1/materials/${material.id}/download?release_id=${released.release_id}`, { headers: authHeaders(apiState) });
  expect(first.status()).toBe(200);
  const downloaded = Buffer.from(await first.body());
  expect(downloaded.equals(BODY)).toBe(true);

  const second = await page.request.get(`/api/v1/materials/${material.id}/download?release_id=${released.release_id}`, { headers: authHeaders(apiState) });
  expect(second.ok()).toBe(false);
  expect([409, 422]).toContain(second.status());

  // The shell still renders the Output workspace with the released material.
  await page.getByRole('link', { name: 'Output' }).click();
  await expect(page.getByRole('region', { name: 'Composer, evidence and templates' })).toBeVisible();
});
