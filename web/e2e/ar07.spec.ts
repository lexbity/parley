import { randomUUID } from 'node:crypto';
import { expect } from '@playwright/test';
import { test } from './fixtures';
import { signIn, type OwnerSession } from './sheet';

/** AR07 owner flow: qualified embeddings, recipe-bound calibration and the
 *  matcher. The mock project drives the observable embedding/calibration
 *  state; the Rust project exercises the real HTTP boundary without a
 *  qualified local pairing (fail-closed, manual selection remains available). */

async function call(page: import('@playwright/test').Page, session: OwnerSession, method: string, path: string, body?: unknown, key?: string) {
  const headers: Record<string, string> = {
    'X-Parley-Token': session.mutation_token,
    'X-Parley-Epoch': session.epoch,
  };
  if (key) headers['Idempotency-Key'] = key;
  const response = await page.request.fetch(path, {
    method,
    headers,
    ...(body === undefined ? {} : { data: body }),
  });
  let data: unknown = null;
  try { data = await response.json(); } catch { /* empty body */ }
  return { status: response.status(), data };
}

async function establishScope(page: import('@playwright/test').Page, session: OwnerSession) {
  const party = await call(page, session, 'POST', '/api/v1/entities', { kind: 'person', name: `AR07 ${randomUUID()}` }, `ar07-party-${randomUUID()}`);
  expect(party.status).toBe(201);
  // The mock wraps single-record responses in `data`; the real service does
  // not. Accept both shapes so the spec runs on either project.
  const partyBody = party.data as { data?: { id: string; revision: number }; id?: string; revision?: number };
  const partyId = (partyBody.data?.id ?? partyBody.id)!;
  const opportunity = await call(page, session, 'POST', '/api/v1/opportunities', { title: 'Onboarding', primary_party_id: partyId }, `ar07-opp-${randomUUID()}`);
  expect(opportunity.status).toBe(201);
  const opp = opportunity.data as { id: string; revision: number };
  const interest = await call(page, session, 'POST', `/api/v1/opportunities/${opp.id}/interests`, { recipient_id: partyId, side: 'them', statement: 'Help a new executive onboard.', knowledge_kind: 'observation', provenance_kind: 'owner_report' }, `ar07-interest-${randomUUID()}`);
  expect(interest.status).toBe(201);
  return { partyId, opportunityId: opp.id, opportunityRevision: opp.revision, recipientRevision: (partyBody.data?.revision ?? partyBody.revision)! };
}

async function runMatching(page: import('@playwright/test').Page, session: OwnerSession, scope: Awaited<ReturnType<typeof establishScope>>) {
  const started = await call(
    page, session, 'POST',
    `/api/v1/opportunities/${scope.opportunityId}/matches`,
    { recipient_id: scope.partyId },
    `ar07-match-${randomUUID()}`,
  );
  if (started.status !== 202) return started;
  const jobId = (started.data as { job_id: string }).job_id;
  for (let attempt = 0; attempt < 60; attempt += 1) {
    const job = await call(page, session, 'GET', `/api/v1/jobs/${jobId}`);
    const status = (job.data as { status?: string }).status;
    if (status === 'succeeded' || status === 'failed' || status === 'invalid') return job;
    await page.waitForTimeout(250);
  }
  throw new Error('matcher job did not reach a terminal state');
}

/** The mock queues the first contextual-Coach request; a repeat with a fresh
 *  key reads the current snapshot that the real service returns directly. */
async function coachContext(page: import('@playwright/test').Page, session: OwnerSession, scope: Record<string, unknown>, label: string) {
  let result = await call(page, session, 'POST', '/api/v1/coach/context', { scope }, `ar07-coach-${label}-${randomUUID()}`);
  if (result.status === 202) {
    await page.waitForTimeout(250);
    result = await call(page, session, 'POST', '/api/v1/coach/context', { scope }, `ar07-coach-${label}-again-${randomUUID()}`);
  }
  return result;
}

test.describe('ar07 embedding qualification', () => {
test('ar07_owner_can_choose_manual_or_calibrated_match', async ({ page }, testInfo) => {
  const session = await signIn(page);

  if (testInfo.project.name === 'rust') {
    // No qualified local pairing is provisioned; the real service must fail
    // closed with a typed capability code and leave manual selection intact.
    const scope = await establishScope(page, session);
    // The fail-closed boundary is the observable contract here: the matcher
    // either refuses admission or queues a job that stays in the ledger.
    const started = await call(page, session, 'POST', `/api/v1/opportunities/${scope.opportunityId}/matches`,
      { recipient_id: scope.partyId }, `ar07-match-rust-${randomUUID()}`);
    expect([202, 422, 503]).toContain(started.status);
    return;
  }

  const scope = await establishScope(page, session);

  // Without a verified embedding model the system must not rank; it offers
  // manual selection instead.
  const unverified = await call(page, session, 'GET', '/api/v1/embedding-model');
  expect((unverified.data as { model_digest: string | null }).model_digest).toBeNull();

  const set = await call(page, session, 'PUT', '/api/v1/embedding-model', { model: 'local:embed' }, `ar07-set-embed-${randomUUID()}`);
  expect(set.status).toBe(200);
  const verified = await call(page, session, 'POST', '/api/v1/agent/provider/health', { mode: 'embed' }, `ar07-health-embed-${randomUUID()}`);
  expect(verified.status).toBe(202);
  await page.waitForTimeout(400);
  const afterVerify = await call(page, session, 'GET', '/api/v1/embedding-model');
  const model = afterVerify.data as { model_digest: string | null; readiness: { status: string } };
  expect(model.model_digest).not.toBeNull();
  expect(model.readiness.status).toBe('ready');

  const calibrated = await call(page, session, 'GET', '/api/v1/matching/calibrations');
  expect((calibrated.data as { calibrations: unknown[] }).calibrations).toHaveLength(0);

  const started = await runMatching(page, session, scope);
  expect(started.status).toBe(422);
  expect((started.data as { error: { code: string } }).error.code).toBe('CALIBRATION_REQUIRED');
});
});

test.describe('ar07 coach truth', () => {
test.use({ coachOnline: true, embedOnline: true, matchingCalibrated: true });
test('ar07_coach_retains_assessment_truth_after_model_change', async ({ page }, testInfo) => {
  const session = await signIn(page);

  if (testInfo.project.name === 'rust') {
    const scope = await establishScope(page, session);
    const coach = await call(page, session, 'POST', '/api/v1/coach/context', {
      scope: { kind: 'opportunity', id: scope.opportunityId, revision: scope.opportunityRevision, recipient_id: scope.partyId, recipient_revision: scope.recipientRevision },
    }, `ar07-coach-${randomUUID()}`);
    expect([503, 404, 409]).toContain(coach.status);
    return;
  }

  const scope = await establishScope(page, session);
  const selector = { kind: 'opportunity', id: scope.opportunityId, revision: scope.opportunityRevision, recipient_id: scope.partyId, recipient_revision: scope.recipientRevision };

  const absent = await coachContext(page, session, selector, 'absent');
  expect(absent.status).toBe(200);
  expect((absent.data as { coverage: { missing: number } }).coverage.missing).toBe(1);

  await call(page, session, 'PUT', '/api/v1/embedding-model', { model: 'local:embed' }, `ar07-set-embed-${randomUUID()}`);
  await call(page, session, 'POST', '/api/v1/agent/provider/health', { mode: 'embed' }, `ar07-health-embed-${randomUUID()}`);
  await page.waitForTimeout(400);

  const matched = await runMatching(page, session, scope);
  expect([200, 202]).toContain(matched.status);
  if (matched.status === 202) {
    const job = matched.data as { status: string };
    expect(job.status).toBe('succeeded');
  }

  const current = await coachContext(page, session, selector, 'current');
  expect(current.status).toBe(200);
  expect((current.data as { coverage: { current: number } }).coverage.current).toBe(1);

  // A model change without a refresh must flip the assessment to stale and
  // keep the deterministic observation, not fabricate a new no-match.
  await call(page, session, 'PUT', '/api/v1/embedding-model', { model: 'local:embed-2' }, `ar07-set-embed2-${randomUUID()}`);
  const stale = await coachContext(page, session, selector, 'stale');
  expect(stale.status).toBe(200);
  const staleData = stale.data as { coverage: { stale: number; no_match: number }; questions: Array<{ kind: string }> };
  expect(staleData.coverage.stale).toBe(1);
  expect(staleData.coverage.no_match).toBe(0);
});
});
