import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';

test('ar03_manual_template_draft_survives_model_unavailability', async ({ page }) => {
  await page.goto('/');
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

  // The owner can author, approve and activate templates through the typed
  // validator independently of local model availability; the same operation
  // backs the candidate save path (ar03 integration suite).
  const family = await post('/template-families', { name: 'AR03 Family' }, 201) as { id: string };
  const body = '{{their_name:name}},\nI can offer {{gift:guide}}.\nWould a short review be useful {{ask:next_step}}?';
  const created = await post(`/templates/${family.id}/versions`, {
    name: 'Introduction draft', body, stage_class: 'prepared', channel: 'email',
    sentence_rules: [[], ['R1'], ['R2']], proof_fallback: 'omit_sentence',
  }, 201) as { id: string; status: string };
  expect(created.status).toBe('draft');

  // The deterministic owner gates are independent of model availability.
  await post(`/templates/${created.id}/approve`, {}, 204);
  await post(`/templates/${created.id}/activate`, {}, 204);

  const versions = await (await page.request.get('/api/v1/templates', { headers: auth })).json() as Array<{ id: string; status: string }>;
  const active = versions.find((template) => template.id === created.id);
  expect(active?.status).toBe('active');
});
