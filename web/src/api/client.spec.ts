import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError, newOperationKey, ParleyClient, type ExtractorJob, type Resource, type SavedSource } from './client';

const session = {
  workspace_id: 'workspace-1', epoch: 'epoch-1', wire_major: 1, wire_minor: 0,
  capabilities: ['resources.read', 'resources.write'], mutation_token: 'token-1', cursor: '0',
};
const resource: Resource = {
  id: 'resource 1', kind: 'gift', title: 'Offer', body: { description: 'A useful offer', delivery: 'on_request' },
  readiness: 'draft', revision: 1, etag: '"resource-1"',
};
const party = { id: 'party 1', kind: 'person' as const, name: 'Dana Lee', focus: '', revision: 1 };
const source: SavedSource = {
  content_id: 'content-1', variant_id: 'variant-1', occurrence_id: 'occurrence-1',
  normalized_sha256: 'a'.repeat(64), raw_sha256: 'b'.repeat(64),
};

function response(body: unknown, status = 200, headers: HeadersInit = {}): Response {
  return new Response(JSON.stringify(body), { status, headers });
}

function sessionResponse(): Response {
  return response(session, 200, { 'X-Parley-Epoch': session.epoch });
}

afterEach(() => {
  vi.restoreAllMocks();
  history.replaceState(null, '', '/');
});

describe('ParleyClient S03 opportunity and release commands', () => {
  it('rejects malformed opportunity, material, and release responses', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response([{}]))
      .mockResolvedValueOnce(response([{ id: 'op', title: 'x', state: 'open', revision: 1, participant_ids: [7], source_variant_ids: [], material_ids: [] }]))
      .mockResolvedValueOnce(response([{}]))
      .mockResolvedValueOnce(response({}));
    const client = new ParleyClient();
    await client.connect();
    await expect(client.listOpportunities()).rejects.toThrow('opportunity is malformed');
    await expect(client.listOpportunities()).rejects.toThrow('participant_ids must be a text list');
    await expect(client.listOutputs()).rejects.toThrow('material is malformed');
    await expect(client.releaseMaterial('bad')).rejects.toThrow('release is malformed');
  });

  it('uses the authenticated workspace session for opportunity and Output operations', async () => {
    const opportunity = { id: 'op-1', title: 'Onboarding', state: 'open', revision: 1, participant_ids: ['party 1'], source_variant_ids: [], material_ids: [] };
    const material = { id: 'mat-1', opportunity_id: 'op-1', recipient_id: 'party 1', recipient_name: 'Dana Lee', kind: 'outreach', version: 1, body: 'Hello Dana.', content_sha256: 'a'.repeat(64), status: 'approved', gate: null, attachment: null };
    const released = { release_id: 'release-1', material_id: 'mat-1', version: 1, recipient_id: 'party 1', body: 'Hello Dana.', content_sha256: 'a'.repeat(64), issued_at: '2026-11-21T12:00:00Z', attachment: null };
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response(opportunity, 201))
      .mockResolvedValueOnce(response([opportunity]))
      .mockResolvedValueOnce(response([material]))
      .mockResolvedValueOnce(response(released))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    const client = new ParleyClient();
    await client.connect();
    expect(await client.createOpportunity('Onboarding', 'party 1')).toEqual(opportunity);
    expect(await client.listOpportunities()).toEqual([opportunity]);
    expect(await client.listOutputs()).toEqual([material]);
    expect(await client.releaseMaterial('mat-1')).toEqual(released);
    await client.acknowledgeRelease('release-1', true);
    expect(fetchMock.mock.calls.slice(1).map(([url]) => url)).toEqual([
      '/api/v1/opportunities', '/api/v1/opportunities', '/api/v1/outputs',
      '/api/v1/materials/mat-1/release', '/api/v1/releases/release-1/ack',
    ]);
    for (const [, init] of fetchMock.mock.calls.slice(1)) {
      expect(new Headers(init?.headers).get('X-Parley-Token')).toBe(session.mutation_token);
      expect(new Headers(init?.headers).get('X-Parley-Epoch')).toBe(session.epoch);
    }
  });

  it('reads and approves a material with its gate report', async () => {
    const gate = { checks: [{ id: 'H3 GROUNDED', severity: 'hard', pass: true, detail: '', overridden: false }], copy_enabled: true };
    const material = { id: 'mat-1', opportunity_id: 'op-1', recipient_id: 'party 1', recipient_name: 'Dana Lee', kind: 'outreach', version: 1, body: 'Hello Dana.', content_sha256: 'a'.repeat(64), status: 'approved', gate, attachment: null };
    const approval = { id: 'approval-1', material_id: 'mat-1', version: 1, content_sha256: 'a'.repeat(64), gate: { checks: [], copy_enabled: true }, approved_at: '2026-11-21T12:00:00Z' };
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response(material))
      .mockResolvedValueOnce(response(approval))
      .mockResolvedValueOnce(response(material));
    const client = new ParleyClient(); await client.connect();
    expect(await client.getMaterial('mat-1')).toEqual(material);
    expect(await client.approveMaterial('mat-1', {})).toEqual({ ...material, gate: { checks: [], copy_enabled: true } });
    expect(fetchMock.mock.calls.slice(1).map(([url]) => url)).toEqual([
      '/api/v1/materials/mat-1', '/api/v1/materials/mat-1/approve', '/api/v1/materials/mat-1',
    ]);
  });

  it('rejects a malformed thread send history and party policy receipt', async () => {
    const thread = { id: 'thread-1', opportunity_id: 'opp-1', recipient_id: 'party 1', state: 'waiting', current_touch: 0, expires_at: null, next_action_at: null, sent_at: null, updated_at: '2026-11-21T08:00:00Z', sends: 'not-an-array' };
    const badSend = { ...thread, sends: [{ id: 'send-1', touch_no: 1, primary_material_id: 'mat-1', primary_version_no: 1, recipient_id: 'party 1', content_sha256: 'x', sent_at: '2026-11-21T08:00:00Z', response_deadline: '2026-11-28T08:00:00Z', deadline_zone: 'UTC', capacity_charged: false, outcome: null }] };
    const badState = { ...thread, state: 'weird', sends: [] };
    const badTouch = { ...thread, current_touch: -1, sends: [] };
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response([thread]))
      .mockResolvedValueOnce(response([badSend]))
      .mockResolvedValueOnce(response([badState]))
      .mockResolvedValueOnce(response([badTouch]))
      .mockResolvedValueOnce(response({ party_id: 'party 1', state: 'open', do_not_contact: 'no', highest_rung: 0, touch_count: 0, revision: 2 }))
      .mockResolvedValueOnce(response({ state: 'open', do_not_contact: false, highest_rung: 0, touch_count: 0, revision: 2 }));
    const client = new ParleyClient(); await client.connect();
    await expect(client.listThreads({})).rejects.toThrow('thread send history is malformed');
    await expect(client.listThreads({})).rejects.toThrow('thread send history is malformed');
    await expect(client.listThreads({})).rejects.toThrow('contact thread is malformed');
    await expect(client.listThreads({})).rejects.toThrow('contact thread is malformed');
    await expect(client.clearPartyDnc('party 1', { reason: 'A recorded clearance reason for the test.' })).rejects.toThrow('party policy receipt is malformed');
    await expect(client.clearPartyDnc('party 2', { reason: 'A recorded clearance reason for the test.' })).rejects.toThrow('policy.party_id must be text');
    expect(fetchMock.mock.calls.slice(1).map(([url]) => url)).toEqual([
      '/api/v1/threads', '/api/v1/threads', '/api/v1/threads', '/api/v1/threads', '/api/v1/entities/party%201/clear-dnc', '/api/v1/entities/party%202/clear-dnc',
    ]);
  });

  it('decodes and rejects graph snapshots with revision checks', async () => {
    const node = { id: 'e1', kind: 'person', name: 'Dana Lee', focus: '', revision: 1, x: 0, y: 0, position_revision: 0 };
    const edge = { id: 'ed1', from_id: 'e2', to_id: 'e1', verb: 'member_of', revision: 1, occurrence_id: 'occ-1', quote: 'works with Dana', tier: 1, speaker_kind: 'unknown', captured_at: '2026-11-21T08:00:00Z', source_title: null, source_url: null };
    const snapshot = { nodes: [node], edges: [edge], omitted_nodes: 0, omitted_edges: 0 };
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response(snapshot))
      .mockResolvedValueOnce(response({ ...snapshot, omitted_nodes: -1 }))
      .mockResolvedValueOnce(response({ ...snapshot, nodes: [{ ...node, x: 'wide' }] }))
      .mockResolvedValueOnce(response({ ...snapshot, edges: [{ ...edge, tier: 9 }] }));
    const client = new ParleyClient(); await client.connect();
    expect(await client.getGraph({ center_id: 'p1', view_scope: 'leads', view_id: 'p1' })).toEqual(snapshot);
    await expect(client.getGraph({})).rejects.toThrow('graph snapshot is malformed');
    await expect(client.getGraph({})).rejects.toThrow('graph node is malformed');
    await expect(client.getGraph({})).rejects.toThrow('graph edge is malformed');
    expect(fetchMock.mock.calls.slice(1).map(([url]) => url)).toEqual([
      '/api/v1/graph?view_scope=leads&view_id=p1&center_id=p1', '/api/v1/graph?view_scope=leads&view_id=all',
      '/api/v1/graph?view_scope=leads&view_id=all', '/api/v1/graph?view_scope=leads&view_id=all',
    ]);
  });
});

describe('ParleyClient S04 local Extractor commands', () => {
  const job: ExtractorJob = {
    id: 'job/1', role: 'extractor', status: 'succeeded', current_attempt: 1, max_attempts: 3,
    progress_current: 1, progress_total: 1, injection_suspected: true,
    last_error_code: null, last_error: null, source_occurrence_id: 'occ-1', proposal_batch_id: 'batch/1',
    proposals: [{ id: 'proposal-1', kind: 'signal', status: 'pending', candidate: {
      statement: 'The next engineer will work with Dana.', quote: 'The next engineer will work with Dana.', mention_name: 'Dana', mention_kind: 'person',
    } }],
  };

  it('calls provider, capture review, cancellation, and proposal commands with session authority', async () => {
    const provider = { endpoint: 'http://127.0.0.1:11434', model: 'local:latest', num_ctx: 8192, num_predict: 512, locale: 'en' };
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ ...source, job_id: 'job-1', job_status: 'queued' }, 201))
      .mockResolvedValueOnce(response(job))
      .mockResolvedValueOnce(response(job))
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
      .mockResolvedValueOnce(response({ status: 'partially_applied' }))
      .mockResolvedValueOnce(response(provider))
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
      .mockResolvedValueOnce(response({ available: true, model: 'local:latest', models: ['local:latest'], message: 'Available' }));
    const client = new ParleyClient(); await client.connect();
    expect(await client.saveSource({ text: 'Hiring update.', title: null, url: null })).toMatchObject({ job_id: 'job-1', job_status: 'queued' });
    expect(await client.getExtractorJob('job/1')).toEqual(job);
    expect(await client.getCaptureProposals('occ/1')).toEqual(job);
    await client.cancelExtractorJob('job/1');
    expect(await client.resolveProposals('batch/1', [{ item_id: 'proposal-1', action: 'unresolved' }], 'op-1')).toEqual({ status: 'partially_applied' });
    expect(await client.getProvider()).toEqual(provider);
    await client.updateProvider(provider);
    expect(await client.checkProvider()).toMatchObject({ available: true, message: 'Available' });
    expect(fetchMock.mock.calls.map(([url]) => url).slice(1)).toEqual([
      '/api/v1/captures', '/api/v1/jobs/job%2F1', '/api/v1/captures/occ%2F1/proposals', '/api/v1/jobs/job%2F1',
      '/api/v1/proposals/batch%2F1/resolve', '/api/v1/agent/provider', '/api/v1/agent/provider', '/api/v1/agent/provider/health',
    ]);
    expect(JSON.parse(String(fetchMock.mock.calls[5]?.[1]?.body))).toEqual({ decisions: [{ item_id: 'proposal-1', action: 'unresolved' }], opportunity_id: 'op-1' });
    expect(new Headers(fetchMock.mock.calls[7]?.[1]?.headers).get('Idempotency-Key')).toBeTruthy();
  });

  it('rejects malformed provider, proposal, and health responses', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ endpoint: 'http://127.0.0.1', model: '', num_ctx: 'large', num_predict: 512, locale: 'en' }))
      .mockResolvedValueOnce(response({ id: 'job', proposals: [{}] }))
      .mockResolvedValueOnce(response({ status: 4 }))
      .mockResolvedValueOnce(response({ available: 'yes', model: 'local', models: [], message: 'bad' }))
      .mockResolvedValueOnce(response({ available: true, model: 'local', models: [4], message: 'bad' }));
    const client = new ParleyClient(); await client.connect();
    await expect(client.getProvider()).rejects.toThrow('provider settings are malformed');
    await expect(client.getExtractorJob('job')).rejects.toThrow('proposal is malformed');
    await expect(client.resolveProposals('batch', [{ item_id: 'x', action: 'dismiss' }])).rejects.toThrow('review result is malformed');
    await expect(client.checkProvider()).rejects.toThrow('provider health is malformed');
    await expect(client.checkProvider()).rejects.toThrow('health.models must be a text list');
    expect(fetchMock).toHaveBeenCalledTimes(6);
  });
});

describe('ParleyClient S07 contact lifecycle commands', () => {
  it('uses authenticated version-bound send, thread, outcome, reopen, resolution and capacity operations', async () => {
    const sendHistory = { id: 'send-1', touch_no: 1, primary_material_id: 'mat-1', primary_version_no: 3, recipient_id: 'party 1', content_sha256: 'a'.repeat(64), sent_at: '2026-11-21T08:00:00Z', response_deadline: '2026-11-28T08:00:00Z', deadline_zone: 'America/Vancouver', next_action_at: null, capacity_charged: false, outcome: { id: 'outcome-1', send_id: 'send-1', class: 'reply_positive', rung_accepted: null, received_at: null, recorded_at: '2026-11-22T08:00:00Z', note: null } };
    const thread = { id: 'thread-1', opportunity_id: 'opp-1', recipient_id: 'party 1', state: 'waiting', current_touch: 1, expires_at: '2026-11-28T08:00:00Z', next_action_at: null, sent_at: '2026-11-21T08:00:00Z', updated_at: '2026-11-21T08:00:00Z', sends: [sendHistory] };
    const sent = { id: 'send-1', thread_id: 'thread-1', touch_no: 1, primary_material_id: 'mat-1', primary_version_no: 3, recipient_id: 'party 1', content_sha256: 'a'.repeat(64), sent_at: '2026-11-21T08:00:00Z', response_deadline: '2026-11-28T08:00:00Z', deadline_zone: 'America/Vancouver', next_action_at: null, capacity_charged: false };
    const outcome = { id: 'outcome-1', send_id: 'send-1', class: 'reply_positive', rung_accepted: null, received_at: null, recorded_at: '2026-11-22T08:00:00Z', note: null };
    const resolution = { id: 'resolution-1', opportunity_id: 'opp-1', thread_id: null, result: 'no_deal', they_benefited: 'unknown', i_benefited: 'unknown', noted_at: '2026-11-23T08:00:00Z' };
    const capacity = { period: 'quarter', unit: 'project slots', total: 8, committed: 1, remaining: 7, updated_at: '2026-11-23T08:00:00Z' };
    const policy = { party_id: 'party 1', state: 'open', do_not_contact: false, highest_rung: 0, touch_count: 1, revision: 2 };
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response([thread]))
      .mockResolvedValueOnce(response(sent, 201))
      .mockResolvedValueOnce(response(outcome))
      .mockResolvedValueOnce(response(thread))
      .mockResolvedValueOnce(response(resolution, 201))
      .mockResolvedValueOnce(response([capacity]))
      .mockResolvedValueOnce(response(capacity))
      .mockResolvedValueOnce(response(policy));
    const client = new ParleyClient(); await client.connect();
    expect(await client.listThreads({ opportunity_id: 'opp-1', recipient_id: 'party 1' })).toEqual([thread]);
    expect(await client.sendOnThread('thread-1', { opportunity_id: 'opp-1', recipient_id: 'party 1', primary: { material_id: 'mat-1', version: 3, release_id: 'release-1' }, deadline: { date: '2026-11-28', zone: 'America/Vancouver' } }, 'send-key')).toEqual(sent);
    expect(await client.recordOutcome('send-1', { class: 'reply_positive' }, 'outcome-key')).toEqual(outcome);
    expect(await client.reopenThread('thread-1')).toEqual(thread);
    expect(await client.recordResolution('opp-1', { result: 'no_deal', they_benefited: 'unknown', i_benefited: 'unknown' })).toEqual(resolution);
    expect(await client.listCapacity()).toEqual([capacity]);
    expect(await client.updateCapacity({ period: 'quarter', unit: 'project slots', total: 8 })).toEqual(capacity);
    expect(await client.clearPartyDnc('party 1', { reason: 'Recipient asked for a fresh start with a concrete proposal.' }, 'dnc-key')).toEqual(policy);
    expect(fetchMock.mock.calls.slice(1).map(([url]) => url)).toEqual([
      '/api/v1/threads?opportunity_id=opp-1&recipient_id=party+1', '/api/v1/threads/thread-1/send',
      '/api/v1/sends/send-1/outcome', '/api/v1/threads/thread-1/reopen', '/api/v1/opportunities/opp-1/resolution',
      '/api/v1/capacity', '/api/v1/capacity', '/api/v1/entities/party%201/clear-dnc',
    ]);
    expect(new Headers(fetchMock.mock.calls[2]![1]?.headers).get('Idempotency-Key')).toBe('send-key');
    expect(new Headers(fetchMock.mock.calls[2]![1]?.headers).get('X-Parley-Epoch')).toBe(session.epoch);
    expect(new Headers(fetchMock.mock.calls[8]![1]?.headers).get('Idempotency-Key')).toBe('dnc-key');
  });
});

describe('ParleyClient S09 recipient writing commands', () => {
  it('binds exchange, templates, matching, composition, lint and voice versions to one session', async () => {
    const interest = { id: 'interest-1', side: 'them', statement: 'Improve onboarding', knowledge_kind: 'hypothesis', check_suggestion: 'Verify with a source', claim_id: null, claim_revision: null, claim: null, revision: 1, status: 'active' };
    const template = { id: 'template-1', family_id: 'family-1', name: 'One ask', version: 1, status: 'draft', body: '{{their_name:name}}', sentence_rules: [[]], stage_class: 'identified', channel: 'email', parent_version_id: null };
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ opportunity_id: 'op-1', recipient_id: 'party-1', items: [interest] }))
      .mockResolvedValueOnce(response({ current: { version: 1, they_get: 'A map', i_get: 'A conversation', fairness_attested: false }, versions: [] }))
      .mockResolvedValueOnce(response(interest, 201))
      .mockResolvedValueOnce(response({ version: 1 }))
      .mockResolvedValueOnce(response({ version: 1, fairness_attested: true }))
      .mockResolvedValueOnce(response([template]))
      .mockResolvedValueOnce(response({ items: [{ id: 'family-1', name: 'One ask' }] }))
      .mockResolvedValueOnce(response({ id: 'family-1', name: 'One ask' }, 201))
      .mockResolvedValueOnce(response({ id: 'template-1', family_id: 'family-1', version: 1, status: 'draft', body: '{{their_name:name}}', rules: [[]], stage_class: 'identified', channel: 'email', parent_version_id: null }, 201))
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }))
      .mockResolvedValueOnce(response({ model: 'embed-local', model_digest: 'digest-1', calibrated_kinds: ['gift'] }))
      .mockResolvedValueOnce(response({ model: 'embed-local', model_digest: 'digest-1', calibration_required: true }))
      .mockResolvedValueOnce(response({ job_id: 'match-job', role: 'matcher' }, 202))
      .mockResolvedValueOnce(response({ id: 'match-1', opportunity_id: 'op-1', recipient_id: 'party-1', embed_model_digest: 'digest-1', calibration_json: '{}', shortlist_json: '[]', why_job_id: null }))
      .mockResolvedValueOnce(response({ job_id: 'builder-job', role: 'template_builder' }, 202))
      .mockResolvedValueOnce(response({ job_id: 'composer-job', role: 'composer' }, 202))
      .mockResolvedValueOnce(response({ material_id: 'material-1', version: 1, status: 'draft' }, 201))
      .mockResolvedValueOnce(response({ job_id: 'lint-job', role: 'linter' }, 202))
      .mockResolvedValueOnce(response({ items: [{ id: 'voice-1', text: 'A sample', channel: 'email', revision: 1 }] }))
      .mockResolvedValueOnce(response({ id: 'voice-2', text: 'Another sample', channel: null, revision: 1 }, 201))
      .mockResolvedValueOnce(response({ id: 'voice-2', status: 'retired', revision: 2 }));
    const client = new ParleyClient(); await client.connect();
    expect(await client.getWritingInterests('op-1', 'party-1')).toEqual([interest]);
    expect((await client.getWritingExchange('op-1', 'party-1')).current).toMatchObject({ version: 1 });
    expect(await client.addWritingInterest('op-1', { recipient_id: 'party-1', side: 'them', statement: 'Improve onboarding', knowledge_kind: 'hypothesis', check_suggestion: 'Verify' }, 'interest-key')).toMatchObject({ id: 'interest-1' });
    await client.saveWritingExchange('op-1', { recipient_id: 'party-1', they_get: 'A map', i_get: 'A conversation', interest_ids: ['interest-1'] }, 'exchange-key');
    await client.attestWritingExchange('op-1', 'party-1', 'attest-key');
    expect(await client.listWritingTemplates()).toEqual([expect.objectContaining({ id: 'template-1', name: 'One ask' })]);
    expect(await client.listTemplateFamilies()).toEqual([{ id: 'family-1', name: 'One ask' }]);
    await client.createTemplateFamily('One ask', 'family-key');
    expect(await client.createWritingTemplateVersion('family-1', { name: 'One ask', body: '{{their_name:name}}', stage_class: 'identified', channel: 'email', sentence_rules: [[]] }, 'version-key')).toMatchObject({ id: 'template-1', name: 'One ask' });
    await client.approveWritingTemplate('template-1', 'approve-key'); await client.activateWritingTemplate('template-1', 'activate-key');
    expect(await client.getEmbeddingModel()).toMatchObject({ model_digest: 'digest-1', calibrated_kinds: ['gift'] }); await client.setEmbeddingModel('embed-local', 'model-key');
    expect(await client.startMatching('op-1', 'party-1', 'match-key')).toEqual({ job_id: 'match-job', role: 'matcher' });
    expect(await client.getWritingMatch('op-1', 'party-1')).toMatchObject({ id: 'match-1', shortlist_json: '[]' });
    expect(await client.buildWritingTemplates({ stage_class: 'identified', rungs: [1], rules: ['R2'], channel: 'email', max_chars: 1200 }, 'builder-key')).toEqual({ job_id: 'builder-job', role: 'template_builder' });
    expect(await client.composeWritingMaterial('op-1', { recipient_id: 'party-1', kind: 'outreach', template_version_id: 'template-1', exchange_version: 1, slot_fills: [] }, 'composer-key')).toEqual({ job_id: 'composer-job', role: 'composer' });
    expect(await client.saveWritingDraft('op-1', 'composer-job', 'Hi Dana.', 'save-key')).toMatchObject({ material_id: 'material-1' });
    expect(await client.lintWritingMaterial('material-1', 1, 'a'.repeat(64), 'lint-key')).toMatchObject({ job_id: 'lint-job' });
    expect(await client.listVoiceSamples()).toHaveLength(1); await client.createVoiceSample('Another sample', undefined, 'voice-key'); await client.retireVoiceSample('voice-2', 'retire-key');
    const calls = fetchMock.mock.calls.slice(1);
    expect(calls.map(([url]) => url)).toEqual([
      '/api/v1/opportunities/op-1/interests?recipient_id=party-1', '/api/v1/opportunities/op-1/exchange?recipient_id=party-1',
      '/api/v1/opportunities/op-1/interests', '/api/v1/opportunities/op-1/exchange', '/api/v1/opportunities/op-1/exchange/party-1/attest',
      '/api/v1/templates', '/api/v1/template-families', '/api/v1/template-families', '/api/v1/templates/family-1/versions',
      '/api/v1/templates/template-1/approve', '/api/v1/templates/template-1/activate', '/api/v1/embedding-model', '/api/v1/embedding-model',
      '/api/v1/opportunities/op-1/matches', '/api/v1/opportunities/op-1/matches?recipient_id=party-1', '/api/v1/templates/build',
      '/api/v1/opportunities/op-1/materials/compose', '/api/v1/opportunities/op-1/materials/compose/composer-job/save',
      '/api/v1/materials/material-1/lint', '/api/v1/voice-samples', '/api/v1/voice-samples', '/api/v1/voice-samples/voice-2/retire',
    ]);
    expect(new Headers(calls[2]?.[1]?.headers).get('Idempotency-Key')).toBe('interest-key');
    expect(new Headers(calls[8]?.[1]?.headers).get('Idempotency-Key')).toBe('version-key');
  });
});

describe('ParleyClient S05 board and canonical notes commands', () => {
  const board = {
    lists: [{ id: 'list-a', title: 'Today', position: 0, archived: false, revision: 2 }],
    cards: [{ opportunity: { id: 'op-1', title: 'Onboarding', state: 'open', revision: 1, participant_ids: ['party 1'], source_variant_ids: [], material_ids: [], tags: [] }, placement: { opportunity_id: 'op-1', list_id: 'list-a', position: 0, revision: 3, next_task: 'Review checklist' } }],
  };
  const notes = {
    opportunity_id: 'op-1', markdown: '🧭\n- [ ] Review checklist\n', revision: 2, next_task: 'Review checklist',
    tasks: [{ byte_start: 5, byte_end: 29, line_sha256: 'c'.repeat(64), checked: false, text: 'Review checklist' }],
  };

  it('uses session authority and idempotency keys for list, placement, and Markdown changes', async () => {
    const movedList = { id: 'list-a', title: 'Today', position: 0, archived: false, revision: 3 };
    const archive = { archived_list: { ...movedList, archived: true }, destination_list: { id: 'list-b', title: 'Later', position: 1, archived: false, revision: 4 }, moved: [] };
    const placement = { opportunity_id: 'op-1', list_id: 'list-a', position: 0, placement_revision: 4, source_list_revision: 3, destination_list_revision: 3 };
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response(board))
      .mockResolvedValueOnce(response({ list: movedList, board_revision: 2 }, 201))
      .mockResolvedValueOnce(response({ lists: [movedList] }))
      .mockResolvedValueOnce(response({ list: movedList }))
      .mockResolvedValueOnce(response(archive))
      .mockResolvedValueOnce(response(placement))
      .mockResolvedValueOnce(response(notes))
      .mockResolvedValueOnce(response(notes))
      .mockResolvedValueOnce(response({ ...notes, markdown: '🧭\n- [x] Review checklist\n', revision: 3, next_task: null, tasks: [{ ...notes.tasks[0], checked: true }] }));
    const client = new ParleyClient(); await client.connect();
    expect(await client.getBoard()).toEqual({ lists: board.lists, cards: [{ opportunity: board.cards[0]!.opportunity, opportunity_id: 'op-1', list_id: 'list-a', position: 0, placement_revision: 3, next_task: 'Review checklist' }] });
    expect(await client.createBoardList({ title: 'Today', before_list_id: null }, 'key-create')).toMatchObject({ list: movedList, board_revision: 2 });
    expect(await client.reorderBoardLists({ ordered_ids: ['list-a'], expected_revisions: { 'list-a': 2 } }, 'key-order')).toEqual([movedList]);
    expect(await client.renameBoardList('list-a', { title: 'Today', expected_revision: 2 }, 'key-rename')).toEqual(movedList);
    expect(await client.archiveBoardList('list-a', { destination_list_id: 'list-b', expected_list_revision: 2, expected_destination_revision: 3 }, 'key-archive')).toEqual(archive);
    expect(await client.moveOpportunityPlacement('op-1', { list_id: 'list-a', before_opportunity_id: null, expected_source_list_revision: 2, expected_list_revision: 2, expected_placement_revision: 3 }, 'key-move')).toEqual(placement);
    expect(await client.getOpportunityNotes('op-1')).toEqual(notes);
    expect(await client.updateOpportunityNotes('op-1', notes.markdown, 2, 'key-notes')).toEqual(notes);
    expect((await client.toggleOpportunityTask('op-1', { notes_revision: 2, byte_start: 5, byte_end: 29, line_sha256: 'c'.repeat(64), checked: true }, 'key-toggle')).tasks[0]?.checked).toBe(true);
    const urls = fetchMock.mock.calls.slice(1).map(([url]) => String(url));
    expect(urls).toEqual([
      '/api/v1/board', '/api/v1/board/lists', '/api/v1/board/lists/reorder', '/api/v1/board/lists/list-a',
      '/api/v1/board/lists/list-a/archive', '/api/v1/opportunities/op-1/placement', '/api/v1/opportunities/op-1/notes',
      '/api/v1/opportunities/op-1/notes', '/api/v1/opportunities/op-1/notes/tasks/toggle',
    ]);
    for (const [, init] of fetchMock.mock.calls.slice(1)) {
      const headers = new Headers(init?.headers);
      expect(headers.get('X-Parley-Token')).toBe(session.mutation_token);
      expect(headers.get('X-Parley-Epoch')).toBe(session.epoch);
    }
    expect(new Headers(fetchMock.mock.calls[2]?.[1]?.headers).get('Idempotency-Key')).toBe('key-create');
  });

  it('rejects malformed board positions, task offsets, and hash spans', async () => {
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ lists: [], cards: [{}] }))
      .mockResolvedValueOnce(response({ ...notes, tasks: [{ ...notes.tasks[0], byte_end: -1 }] }))
      .mockResolvedValueOnce(response({ ...notes, tasks: [{ ...notes.tasks[0], line_sha256: 'bad' }] }));
    const client = new ParleyClient(); await client.connect();
    await expect(client.getBoard()).rejects.toThrow('board placement is malformed');
    await expect(client.getOpportunityNotes('op-1')).rejects.toThrow('opportunity task is malformed');
    await expect(client.getOpportunityNotes('op-1')).rejects.toThrow('opportunity task is malformed');
  });
});

describe('ParleyClient', () => {
  it('redeems and removes a bootstrap fragment before loading session', async () => {
    history.replaceState(null, '', '/#bootstrap=secret-1');
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(response({ ok: true }))
      .mockResolvedValueOnce(sessionResponse());

    const client = new ParleyClient();
    await client.connect();

    expect(location.hash).toBe('');
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(fetchMock.mock.calls[0]?.[0]).toBe('/api/v1/session/bootstrap');
    expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toEqual({ secret: 'secret-1' });
    expect(fetchMock.mock.calls[1]?.[0]).toBe('/api/v1/session');
    expect(fetchMock.mock.calls[1]?.[1]).toMatchObject({ credentials: 'same-origin', cache: 'no-store' });
    expect(new Headers(fetchMock.mock.calls[0]?.[1]?.headers).has('X-Parley-Token')).toBe(false);
  });

  it('loads resources, one resource, and ordered events with decoded data', async () => {
    const client = new ParleyClient();
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ data: [resource], cursor: '1' }))
      .mockResolvedValueOnce(response({ data: resource }))
      .mockResolvedValueOnce(response({
        items: [{ cursor: '1', event_id: 'event-1', entity_type: 'resource', entity_id: resource.id,
          revision: 1, event_type: 'resource.created', occurred_at: '2026-11-22T09:00:00Z', payload: { kind: 'gift' } }],
        next_cursor: '1', observed_cursor: '1',
      }))
      .mockResolvedValueOnce(response({
        items: [{ cursor: '1', event_id: 'event-1', entity_type: 'opportunity', entity_id: 'o1',
          revision: 1, event_type: 'opportunity.created', occurred_at: '2026-11-22T09:00:00Z', payload: {} }],
        next_cursor: '1', observed_cursor: '1',
      }));

    await client.connect();
    expect(await client.listResources()).toEqual([resource]);
    expect(await client.getResource(resource.id)).toEqual(resource);
    expect(await client.getEvents('7/8')).toMatchObject({ next_cursor: '1', items: [{ entity_id: resource.id, occurred_at: '2026-11-22T09:00:00Z' }] });
    expect(await client.getEvents('7/8', { entity_type: 'opportunity', entity_id: 'o1' })).toMatchObject({ next_cursor: '1' });
    expect(fetchMock.mock.calls.slice(1).map(([url]) => url)).toEqual([
      '/api/v1/entities?kind=resource',
      '/api/v1/entities/resource%201',
      '/api/v1/events?after=7%2F8',
      '/api/v1/events?after=7%2F8&entity_type=opportunity&entity_id=o1',
    ]);
    for (const [, init] of fetchMock.mock.calls.slice(1)) {
      const headers = new Headers(init?.headers);
      expect(headers.get('X-Parley-Token')).toBe(session.mutation_token);
      expect(headers.get('X-Parley-Epoch')).toBe(session.epoch);
    }
  });

  it('lists directory parties and saved sources from the scoped reads', async () => {
    const client = new ParleyClient();
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ data: [party], cursor: '2' }))
      .mockResolvedValueOnce(response([source]));

    await client.connect();
    expect(await client.listParties()).toEqual([party]);
    expect(await client.listSources()).toEqual([source]);
    expect(fetchMock.mock.calls.slice(1).map(([url]) => url)).toEqual([
      '/api/v1/entities?kind=person&kind=org',
      '/api/v1/captures',
    ]);
  });

  it('saves a source without inferring identity and creates a directory entity', async () => {
    const client = new ParleyClient();
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response(source, 201))
      .mockResolvedValueOnce(response({ data: party, cursor: '3' }, 201));

    await client.connect();
    const saved = await client.saveSource({ text: 'Hiring update quoting Dana Lee.', title: null, url: null });
    expect(saved).toEqual(source);
    const capture = new Headers(fetchMock.mock.calls[1]?.[1]?.headers);
    expect(JSON.parse(String(fetchMock.mock.calls[1]?.[1]?.body))).toEqual({
      text: 'Hiring update quoting Dana Lee.', title: null, url: null,
    });
    expect(capture.get('X-Parley-Token')).toBe(session.mutation_token);

    const key = newOperationKey();
    const created = await client.createEntity({ kind: 'person', name: 'Dana Lee' }, key);
    expect(created).toEqual(party);
    expect(fetchMock.mock.calls[2]?.[0]).toBe('/api/v1/entities');
    const entity = new Headers(fetchMock.mock.calls[2]?.[1]?.headers);
    expect(entity.get('Idempotency-Key')).toBe(key);
    expect(JSON.parse(String(fetchMock.mock.calls[2]?.[1]?.body))).toEqual({ kind: 'person', name: 'Dana Lee' });
  });

  it('sends create and conditional update with stable operation headers', async () => {
    const client = new ParleyClient();
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ data: resource }, 201))
      .mockResolvedValueOnce(response({ data: { ...resource, revision: 2, etag: '"resource-2"' } }));
    await client.connect();

    await client.createResource({ kind: 'gift', title: 'Offer', body: resource.body }, 'key-create');
    await client.updateResource(resource, { title: 'Offer revised', body: resource.body }, 'key-update');

    const create = new Headers(fetchMock.mock.calls[1]?.[1]?.headers);
    expect(create.get('Idempotency-Key')).toBe('key-create');
    expect(JSON.parse(String(fetchMock.mock.calls[1]?.[1]?.body))).toEqual({ kind: 'gift', title: 'Offer', body: resource.body });
    const update = new Headers(fetchMock.mock.calls[2]?.[1]?.headers);
    expect(update.get('If-Match')).toBe(resource.etag);
    expect(update.get('Idempotency-Key')).toBe('key-update');
    expect(fetchMock.mock.calls[2]?.[0]).toBe('/api/v1/entities/resource%201');
  });

  it('validates operation keys as version 7 UUIDs', () => {
    expect(newOperationKey()).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  });

  it('decodes structured API failures and uses safe fallback errors', async () => {
    const client = new ParleyClient();
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ error: {
        message: 'Stale resource', code: 'STALE_REVISION', details: [
          { field: 'title', message: 'Current value changed' }, null, { field: 2, message: 'General detail' },
        ],
      } }, 412));
    await client.connect();

    await expect(client.listResources()).rejects.toMatchObject({
      name: 'Error', message: 'Stale resource', status: 412, code: 'STALE_REVISION',
      details: [{ field: 'title', message: 'Current value changed' }, { message: 'General detail' }],
    });

    const fallbackClient = new ParleyClient();
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ unknown: true }, 503));
    await fallbackClient.connect();
    await expect(fallbackClient.listResources()).rejects.toMatchObject({
      message: 'Request failed (503)', code: 'INVALID_ERROR_RESPONSE',
    });
    expect(ApiError).toBeTypeOf('function');
  });

  it('rejects malformed sessions, resources, lists and event rows', async () => {
    const malformedSessions: unknown[] = [null, { ...session, capabilities: ['ok', 1] }, { ...session, wire_major: '1' }];
    for (const malformed of malformedSessions) {
      const client = new ParleyClient();
      vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(response(malformed));
      await expect(client.connect()).rejects.toThrow(/workspace session is malformed/);
      vi.restoreAllMocks();
    }

    const client = new ParleyClient();
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ data: [{ ...resource, revision: 0 }], cursor: '0' }));
    await client.connect();
    await expect(client.listResources()).rejects.toThrow(/resource is malformed/);

    const listClient = new ParleyClient();
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ data: {}, cursor: '0' }));
    await listClient.connect();
    await expect(listClient.listResources()).rejects.toThrow(/resource list is malformed/);

    const partyClient = new ParleyClient();
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ data: [{ ...party, revision: 0 }], cursor: '0' }));
    await partyClient.connect();
    await expect(partyClient.listParties()).rejects.toThrow(/directory entity is malformed/);

    const sourceClient = new ParleyClient();
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response([2]));
    await sourceClient.connect();
    await expect(sourceClient.listSources()).rejects.toThrow(/saved source is malformed/);

    const eventClient = new ParleyClient();
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ items: [{ ...resource, payload: null }], next_cursor: '0', observed_cursor: '0' }));
    await eventClient.connect();
    await expect(eventClient.getEvents()).rejects.toThrow(/event is malformed/);
  });

  it('reports unreadable payloads and preserves transport errors', async () => {
    const client = new ParleyClient();
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(new Response('not-json', { status: 200 }));
    await client.connect();
    await expect(client.listResources()).rejects.toMatchObject({ status: 200, code: 'INVALID_RESPONSE' });

    const offline = new ParleyClient();
    vi.spyOn(globalThis, 'fetch').mockRejectedValueOnce(new Error('offline'));
    await expect(offline.connect()).rejects.toThrow('offline');
    const unknown = new ParleyClient();
    vi.spyOn(globalThis, 'fetch').mockRejectedValueOnce('offline');
    await expect(unknown.connect()).rejects.toThrow('Connection failed');
  });
});

describe('ParleyClient P01 party notes and dossier commands', () => {
  it('round-trips entity notes under revision and ETag preconditions', async () => {
    const notes = { entity_id: 'p1', markdown: 'Welcome\n', revision: 2, etag: '"p1.e1.notes.p1.2"' };
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ data: notes }))
      .mockResolvedValueOnce(response({ data: notes }));
    const client = new ParleyClient();
    await client.connect();
    expect(await client.getEntityNotes('p1')).toEqual(notes);
    expect(await client.updateEntityNotes('p1', 'Welcome\n', 1, '"p1.e1.notes.p1.1"', 'notes-key')).toEqual(notes);
    expect(fetchMock.mock.calls.slice(1).map(([url]) => url)).toEqual([
      '/api/v1/entities/p1/notes', '/api/v1/entities/p1/notes',
    ]);
    const put = fetchMock.mock.calls[2]![1]?.headers;
    expect(new Headers(put).get('If-Match')).toBe('"p1.e1.notes.p1.1"');
    expect(new Headers(put).get('Idempotency-Key')).toBe('notes-key');
    expect(JSON.parse(String(fetchMock.mock.calls[2]![1]?.body))).toEqual({ markdown: 'Welcome\n', expected_notes_rev: 1 });
  });

  it('rejects malformed notes responses', async () => {
    const client = new ParleyClient();
    vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ data: { entity_id: 'p1', markdown: 'x', revision: 0, etag: '"e"' } }))
      .mockResolvedValueOnce(response({ data: { entity_id: 'p1', markdown: 'x', revision: 0, etag: '"e"' } }));
    await client.connect();
    await expect(client.getEntityNotes('p1')).rejects.toThrow('entity notes are malformed');
    await expect(client.updateEntityNotes('p1', 'x', 1, '"e"', 'k')).rejects.toThrow('updated entity notes are malformed');
  });

  it('decodes the dossier and rejects malformed association hops', async () => {
    const entity = { id: 'p1', kind: 'org', name: 'Fieldwork', focus: 'CSV tools', revision: 2, etag: '"p1.e1.entity.p1.2"' };
    const dossier = {
      entity,
      direct_links: [{ id: 'e1', from_id: 'p2', to_id: 'p1', verb: 'member_of', revision: 1, tier: 1, wording: 'Mira maintains the CSV tools.' }],
      files: [{ id: 'a1', filename: 'guide.pdf', content_type: 'application/pdf', size: 1024, sha256: 'a'.repeat(64), share_permission: false, reviewed_by_owner: true, created_at: '2026-11-22T00:00:00Z' }],
      opportunities: [{ id: 'o1', title: 'Onboarding exchange', role: 'primary' }],
      associated_resources: [{ id: 'r1', title: 'Checklist', kind: 'proof', opportunity_id: 'o1', opportunity_title: 'Onboarding exchange' }],
    };
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ data: dossier }))
      .mockResolvedValueOnce(response({ data: { ...dossier, direct_links: [{}] } }));
    const client = new ParleyClient();
    await client.connect();
    expect(await client.getDossier('p1')).toEqual(dossier);
    await expect(client.getDossier('p1')).rejects.toThrow('dossier link is malformed');
    expect(fetchMock.mock.calls.slice(1).map(([url]) => url)).toEqual([
      '/api/v1/entities/p1/dossier', '/api/v1/entities/p1/dossier',
    ]);
  });
});
