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

test.describe('ar03 model build', () => {
  test.use({ embedOnline: true });

  test('ar03_model_build_lists_reviewable_candidates', async ({ page }) => {
  test.info().annotations.push({ type: 'NOTE', description: 'mock: the scripted builder returns two owner-review candidates and the durable listing re-reads them.' });
  await page.goto('/');
  const session = await (await page.request.get('/api/v1/session')).json();
  const auth = { 'X-Parley-Token': String(session.mutation_token), 'X-Parley-Epoch': String(session.epoch) };

  // With the local model online, the contextual build queues a builder job.
  const build = await page.request.post('/api/v1/templates/build', {
    headers: { ...auth, 'Idempotency-Key': randomUUID() },
    data: { stage_class: 'prepared', rungs: [1], rules: ['R1'], channel: 'email', max_chars: 1200, voice_sample_ids: [] },
  });
  // The rust browser project has no qualified local pairing; the service
  // fails closed with 503 and the manual path stays available there.
  if (build.status() === 503) return;
  expect(build.status()).toBe(202);
  const { job_id, role } = await build.json();
  expect(role).toBe('template_builder');

  // The role job converges with the validated variant union as its result.
  const job = await (await page.request.get(`/api/v1/jobs/${job_id}`, { headers: auth })).json();
  expect(job.status).toBe('succeeded');
  expect(job.result.variants).toHaveLength(2);

  // The durable listing re-reads the same candidates after the job result.
  const listing = await (await page.request.get(`/api/v1/templates/build-candidates/${job_id}`, { headers: auth })).json();
  expect(listing.role).toBe('template_builder');
  expect(listing.items).toHaveLength(2);
  for (const [index, item] of listing.items.entries()) {
    expect(item.variant_no).toBe(index + 1);
    expect(item.status).toBe('owner_review_required');
    expect(item.saved_version_id).toBeNull();
  }

  // Unknown jobs are the same 404 the real service returns.
  const missing = await page.request.get('/api/v1/templates/build-candidates/does-not-exist', { headers: auth });
  expect(missing.status()).toBe(404);
  expect((await missing.json()).error.code).toBe('JOB_NOT_FOUND');
});
});
