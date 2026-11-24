import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { addParty, openParties } from './parties';

test('s03_reviewed_manual_message_copy_is_recipient_and_version_bound', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto('/');
  await openParties(page);
  const recipient = 'Dana Lee';
  const title = 'Onboarding checklist';
  await addParty(page, recipient);
  await page.getByRole('link', { name: 'Queue' }).click();
  await page.getByRole('button', { name: 'New opportunity' }).click();
  await page.getByLabel('Opportunity title').fill(title);
  await page.getByLabel('Participant').selectOption({ label: recipient + ' · person' });
  await page.getByRole('button', { name: 'Create opportunity', exact: true }).click();
  const scenario = page.getByRole('complementary', { name: 'Scenario details' });
  await expect(scenario.getByRole('heading', { name: title })).toBeVisible();

  const session = await (await page.request.get('/api/v1/session')).json();
  const auth = { 'X-Parley-Token': String(session.mutation_token), 'X-Parley-Epoch': String(session.epoch) };
  async function post(path: string, data: object, status: number) {
    const response = await page.request.post('/api/v1' + path, {
      headers: { ...auth, 'Idempotency-Key': randomUUID() },
      data,
    });
    expect(response.status(), await response.text()).toBe(status);
    if (status !== 204) return response.json();
  }
  const inventory: Array<{ id: string; title: string; participants: Array<{ id: string; name: string }> }> =
    await (await page.request.get('/api/v1/opportunities', { headers: auth })).json();
  const opportunity = inventory.find(item => item.title === title)!;
  const recipientId = opportunity.participants.find(item => item.name === recipient)!.id;
  const path = '/opportunities/' + opportunity.id;
  const theirInterest = await post(path + '/interests', {
    recipient_id: recipientId, side: 'them', statement: 'A smoother handoff', knowledge_kind: 'hypothesis',
    check_suggestion: 'Ask the recipient whether the handoff is a priority.',
  }, 201);
  const ownerInterest = await post(path + '/interests', {
    recipient_id: recipientId, side: 'me', statement: 'Learn from their onboarding process', knowledge_kind: 'owner_report',
  }, 201);
  const exchange = await post(path + '/exchange', {
    recipient_id: recipientId, they_get: 'A checklist', i_get: 'Feedback',
    interest_ids: [theirInterest.id, ownerInterest.id],
  }, 201);
  await post(path + '/exchange/' + recipientId + '/attest', {}, 200);
  const template = await post('/templates', { name: 'Short introduction', body: 'Hello.' }, 201);
  await post('/templates/' + template.id + '/approve', {}, 204);
  await post('/templates/' + template.id + '/activate', {}, 204);

  const body = 'Hello.';
  const material = await post(path + '/materials', {
    recipient_id: recipientId, kind: 'outreach', template_version_id: template.id, body,
    exchange_version: exchange.version, slot_fills: [], sentence_bindings: [{ sentence_index: 0, claim_type: 'generic' }],
  }, 201);
  await post('/materials/' + material.id + '/approve', {
    override_reasons: {
      'S1 GIVE_FIRST': 'The reviewer confirmed that the recipient requested a short introduction.',
      'S2 ANCHOR': 'The reviewer checked the wording of this specific version.',
      'S7 CADENCE': 'The introduction is scheduled at the approved cadence.',
    },
  }, 200);

  await page.getByRole('link', { name: 'Output' }).click();
  const workspace = page.getByRole('region', { name: 'Output workspace' });
  await workspace.getByRole('combobox', { name: 'Opportunity', exact: true }).selectOption({ label: title });
  await workspace.getByRole('combobox', { name: 'Recipient', exact: true }).selectOption({ label: recipient });
  await workspace.getByRole('button', { name: /^Drafts/ }).click();
  await workspace.getByRole('button', { name: /Outreach/ }).first().click();
  await workspace.getByRole('button', { name: 'Review', exact: true }).click();
  await workspace.getByRole('button', { name: 'Copy or download approved text' }).click();
  const review = page.getByRole('dialog');
  await review.getByRole('button', { name: 'Copy approved text' }).click();
  await expect(review.getByRole('status')).toContainText('Copy confirmed for Dana Lee, version 1');
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(body);
  await expect(review.getByText('No send has been recorded', { exact: false })).toBeVisible();
  await review.getByRole('button', { name: 'Close', exact: true }).click();

  await page.getByRole('link', { name: 'Queue' }).click();
  await scenario.getByText('Linked material (1)', { exact: true }).click();
  await scenario.getByRole('button', { name: 'Review outreach v1 in Output' }).click();
  await expect(page).toHaveURL('/output/' + material.id);
  await expect(page.getByRole('dialog')).toContainText('Version history');
  await expect(page.getByRole('dialog')).toContainText('Outreach · v1');
});
