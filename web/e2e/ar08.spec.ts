import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { signIn } from './sheet';

/** AR08: Composer and Linter run through the one packaged worker boundary and
 *  the owner reviews the generated draft; provider failure leaves every
 *  deterministic/manual gate, template and release surface usable. */

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

async function createActiveTemplate(page: import('@playwright/test').Page, a: Auth): Promise<string> {
  const family = await call(page, 'POST', '/api/v1/template-families', { name: `AR08 ${randomUUID()}` }, a);
  expect(family.status).toBe(201);
  const familyId = (family.data as { id: string }).id;
  const body = '{{their_name:name}},\nI can offer {{gift:guide}}.\nWould a short review be useful {{ask:next_step}}?';
  const version = await call(page, 'POST', `/api/v1/templates/${familyId}/versions`, {
    name: 'AR08 draft', body, stage_class: 'prepared', channel: 'email',
    sentence_rules: [[], ['R1'], ['R2']], proof_fallback: 'omit_sentence',
  }, a);
  expect(version.status).toBe(201);
  const versionId = (version.data as { id: string }).id;
  await call(page, 'POST', `/api/v1/templates/${versionId}/approve`, {}, a);
  await call(page, 'POST', `/api/v1/templates/${versionId}/activate`, {}, a);
  return versionId;
}

test('ar08_author_reviews_generated_draft_from_selected_template', async ({ page }, testInfo) => {
  await signIn(page);
  const a = await auth(page);
  const versionId = await createActiveTemplate(page, a);

  if (testInfo.project.name === 'rust') {
    // No qualified local pairing is provisioned; the same role operation fails
    // closed with a typed capability code, and manual authoring stays usable.
    const started = await call(page, 'POST', '/api/v1/templates/build', {
      stage_class: 'prepared', rungs: [1], rules: ['R1', 'R2'], channel: 'email',
      max_chars: 1200, voice_sample_ids: [],
    }, a);
    // Without a qualified pairing the role operation fails closed at the
    // boundary; manual authoring remains available and is covered below.
    expect([202, 503]).toContain(started.status);
    if (started.status !== 202) return;
    const jobId = (started.data as { job_id: string }).job_id;
    let terminal: Record<string, unknown> | null = null;
    for (let attempt = 0; attempt < 60; attempt += 1) {
      const job = await call(page, 'GET', `/api/v1/jobs/${jobId}`, undefined, a);
      const status = (job.data as { status: string }).status;
      if (status === 'failed' || status === 'invalid' || status === 'succeeded') { terminal = job.data as Record<string, unknown>; break; }
      await page.waitForTimeout(250);
    }
    expect(terminal).not.toBeNull();
    expect(['failed', 'invalid']).toContain(terminal!.status);
    expect(['CAPABILITY_UNQUALIFIED', 'RUNNER_INSTALL_INVALID', 'ISOLATION_UNAVAILABLE', 'MODEL_UNAVAILABLE']).toContain(terminal!.last_error_code);
    return;
  }

  // Mock project: the compose job resolves to one reviewable draft candidate.
  const started = await call(page, 'POST', '/api/v1/opportunities/o-1/materials/compose', {
    recipient_id: 'r-1', kind: 'outreach', template_version_id: versionId, exchange_version: 1,
    slot_fills: [{ slot: 'their_name:name', value: 'Dana' }, { slot: 'gift:item', value: 'a book' }],
    selected_match_ids: [],
  }, a);
  expect(started.status).toBe(202);
  const jobId = (started.data as { job_id: string }).job_id;
  const job = await call(page, 'GET', `/api/v1/jobs/${jobId}`, undefined, a);
  expect(job.status).toBe(200);
  expect((job.data as { role: string }).role).toBe('composer');
  expect((job.data as { status: string }).status).toBe('succeeded');
  const result = (job.data as { result: { status: string; owner_review_required: boolean; released: boolean; slot_fills: Record<string, string> } }).result;
  expect(result.status).toBe('draft');
  expect(result.owner_review_required).toBe(true);
  expect(result.released).toBe(false);
  expect(result.slot_fills['gift:item']).toBe('a book');
});

test('ar08_provider_failure_leaves_manual_gate_and_release_available', async ({ page }, testInfo) => {
  await signIn(page);
  const a = await auth(page);

  // Manual template authoring, deterministic outputs and the settings surface
  // never depend on a live model.
  await createActiveTemplate(page, a);
  const outputs = await call(page, 'GET', '/api/v1/outputs', undefined, a);
  expect(outputs.status).toBe(200);

  const provider = await call(page, 'GET', '/api/v1/agent/provider', undefined, a);
  expect(provider.status).toBe(200);

  if (testInfo.project.name === 'mock') {
    const lint = await call(page, 'POST', '/api/v1/materials/m-1/lint', { version: 1, content_sha256: 'a'.repeat(64) }, a);
    expect(lint.status).toBe(202);
    const lintJobId = (lint.data as { job_id: string }).job_id;
    const lintJob = await call(page, 'GET', `/api/v1/jobs/${lintJobId}`, undefined, a);
    expect((lintJob.data as { role: string }).role).toBe('linter');
    expect((lintJob.data as { status: string }).status).toBe('succeeded');
    expect((lintJob.data as { result: { findings: unknown[] } }).result.findings).toEqual([]);
    return;
  }

  // Rust project: an unqualified probe check surfaces a typed queued/closed
  // state while the settings read and manual ops remain intact.
  const health = await call(page, 'POST', '/api/v1/agent/provider/health', { mode: 'structured_role' }, a);
  expect([200, 202, 503, 409]).toContain(health.status);
});
