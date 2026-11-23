import { afterEach, describe, expect, it, vi } from 'vitest';
import { ParleyClient } from './client';

/** Exhaustive, meaningful decoding coverage for the singleton API client. */
const session = {
  workspace_id: 'workspace-1', epoch: 'epoch-1', wire_major: 1, wire_minor: 0,
  capabilities: ['resources.read'], mutation_token: 'token-1', cursor: '7',
};

function response(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
}

function clientWith(...bodies: unknown[]): ParleyClient {
  const responses = bodies.map((body) => response(body));
  vi.spyOn(globalThis, 'fetch')
    .mockResolvedValueOnce(new Response(JSON.stringify(session), { status: 200 }))
    .mockImplementation(() => Promise.resolve(responses.shift() ?? response({})));
  return new ParleyClient();
}

afterEach(() => vi.restoreAllMocks());

describe('ParleyClient Queue, insights and board decoders', () => {
  it('decodes the queue sheet, phase receipts, daily queue and insights', async () => {
    const phase = { stage_id: 'preparing', label: 'Preparing', class: 'prepared', kind: 'planning', pack_id: 'job_search', pack_version: '1.0.0', revision: 3 };
    const row = {
      opportunity_id: 'o1', title: 'Onboarding', lifecycle: 'open', close_reason: null, pack_id: phase.pack_id, pack_version: phase.pack_version, phase_revision: phase.revision, phase,
      participants: [{ id: 'p1', name: 'Dana Lee', kind: 'person', role: 'primary' }],
      next_action: { kind: 'task', label: 'Confirm', reason: 'Ready', date: '2026-11-23', thread_id: 't1', recipient_id: 'p1', recipient_name: 'Dana Lee' },
      other_action_count: 2,
    };
    const sheet = {
      generation: '7', total: 1, rows: [row],
      phases: [{ ...phase, count: 1 }], cursor: '0', next_cursor: null,
    };
    const daily = {
      as_of: '2026-11-23T00:00:00Z', pack_id: 'job_search', pack_version: '1.0.0', daily_cap: 12,
      actionable_count: 1, deferred_count: 0,
      items: [{ id: 'd1', opportunity_id: 'o1', title: 'Onboarding', type: 'followup_due', party_id: 'p1', party_name: 'Dana Lee', due_at: '2026-11-24', reason: 'Due', blocking_reason: null, remediation_target: { kind: 'opportunity', id: 'o1' } }],
    };
    const metric = { id: 'm1', label: 'Response rate', numerator: 3, denominator: 4, rate: 0.75, lower: 0.2, upper: 0.9, state: 'measured' };
    const insights = {
      as_of: '2026-11-23T00:00:00Z', period_start: '2026-11-16', period_end: '2026-11-23',
      pack_id: 'job_search', pack_version: '1.0.0', sample_size: 4,
      metrics: [metric],
      comparisons: [{ label: 'A vs B', left: metric, right: { ...metric, state: 'insufficient_observations' }, state: 'inconclusive' }],
      gaps: [{ id: 'g1', label: 'Proof', covered: 1, total: 4, blocked_count: 1, suggested_kind: 'proof' }],
      questions: [{ id: 'q1', kind: 'proof', text: 'What changed?', target_kind: 'party', target_id: 'p1', impact: 2, state: 'open' }],
    };
    const client = clientWith(
      sheet,
      { row, event_id: 'ev-1', phase_revision: 3, reverted: false },
      daily,
      insights,
    );
    await client.connect();
    expect((await client.getQueueSheet({ search: 'Onboarding', phase: 'preparing', sort: 'name' })).rows[0]?.phase?.label).toBe('Preparing');
    expect((await client.setOpportunityPhase('o1', { target_stage_id: 'preparing', pack_version: '1.0.0', expected_phase_revision: 2 })).event_id).toBe('ev-1');
    expect((await client.getDailyQueue({ missing_material: true })).actionable_count).toBe(1);
    expect((await client.getInsights()).comparisons[0]?.state).toBe('inconclusive');
  });

  it('rejects malformed sheet, phase, daily and insight payloads', async () => {
    const client = clientWith(
      { generation: '1', total: 0, rows: [], phases: [{ kind: 'planning', count: 'bad' }], cursor: null, next_cursor: null },
      { row: {}, event_id: 'ev', phase_revision: 1, reverted: false },
      { as_of: 'now', pack_id: 'p', pack_version: '1', daily_cap: 1, actionable_count: 0, deferred_count: 0, items: [{ id: 'd', opportunity_id: 'o', title: 'x', type: 'unknown' }] },
      { as_of: 'now', period_start: 'a', period_end: 'b', pack_id: 'p', pack_version: '1', sample_size: 0, metrics: [{ id: 'm', label: 'x', numerator: 0, denominator: 0, rate: 'bad', lower: null, upper: null, state: 'measured' }], comparisons: [], gaps: [], questions: [] },
    );
    await client.connect();
    await expect(client.getQueueSheet()).rejects.toThrow(/phase kind is malformed/);
    await expect(client.setOpportunityPhase('o1', { target_stage_id: 'x', pack_version: '1', expected_phase_revision: 1 })).rejects.toThrow(/sheet row is malformed/);
    await expect(client.getDailyQueue()).rejects.toThrow(/Daily queue item is malformed/);
    await expect(client.getInsights()).rejects.toThrow(/insight rate is malformed/);
  });

  it('decodes board snapshots and guarded list/placement receipts', async () => {
    const list = { id: 'l1', title: 'Inbox', position: 0, archived: false, revision: 1 };
    const board = {
      lists: [list],
      cards: [{ opportunity: { id: 'o1', title: 'Onboarding', state: 'open', revision: 1, participant_ids: [], source_variant_ids: [], material_ids: [], tags: [] }, placement: { opportunity_id: 'o1', list_id: 'l1', position: 0, revision: 1, next_task: 'Call' } }],
    };
    const notes = { opportunity_id: 'o1', markdown: '- [ ] Call', revision: 2, next_task: 'Call', tasks: [{ byte_start: 0, byte_end: 10, line_sha256: 'a'.repeat(64), checked: false, text: 'Call' }] };
    const client = clientWith(
      board,
      { list, board_revision: 1 },
      { archived_list: list, destination_list: { ...list, id: 'l2' }, moved: [{ opportunity_id: 'o1', position: 0, placement_revision: 2 }] },
      { opportunity_id: 'o1', list_id: 'l2', position: 1, placement_revision: 2, source_list_revision: 1, destination_list_revision: 1 },
      notes,
    );
    await client.connect();
    expect((await client.getBoard()).cards[0]?.next_task).toBe('Call');
    expect((await client.createBoardList({ title: 'Next' })).list.id).toBe('l1');
    expect((await client.archiveBoardList('l1', { destination_list_id: 'l2', expected_list_revision: 1, expected_destination_revision: 1 })).moved[0]?.placement_revision).toBe(2);
    expect((await client.moveOpportunityPlacement('o1', { list_id: 'l1', expected_source_list_revision: 1, expected_list_revision: 1, expected_placement_revision: 1 })).destination_list_revision).toBe(1);
    expect((await client.getOpportunityNotes('o1')).tasks[0]?.text).toBe('Call');
  });
});

describe('ParleyClient administration decoders', () => {
  it('decodes maintenance, packs, backups and admin operations', async () => {
    const backup = { id: 'b1', created_at: '2026-11-23T00:00:00Z', schema_version: 20, workspace_epoch: 'epoch-1', size_bytes: 1024, integrity: 'verified' };
    const overview = {
      as_of: '2026-11-23T00:00:00Z', workspace_epoch: 'epoch-1', event_cursor: '7', last_verified_backup: backup,
      backups: [backup], parties: [{ id: 'p1', kind: 'person', name: 'Dana Lee', descriptor: null }],
    };
    const preview = {
      id: 'pv1', operation: 'restore', digest: 'a'.repeat(64), workspace_epoch: 'epoch-1', event_cursor: '7',
      expires_at: '2026-11-23T01:00:00Z', backup, party: { id: 'p1', name: 'Dana Lee' },
      impact: { parties: 1 }, preserved_shared: { captures: 2 },
    };
    const job = { id: 'j1', operation: 'restore', status: 'queued', summary: null, new_epoch: null, last_error: null };
    const client = clientWith(
      { backup_directory: '/backups', interval_hours: 24, retention_count: 14, revision: 1, last_successful_at: null },
      { backup_directory: '/b', interval_hours: 1, retention_count: 2, revision: 2, last_successful_at: null },
      { items: [backup], last_successful_at: null },
      overview, preview, job,
      { revision: 1, active_pack_id: 'job_search', active_pack_version: '1.0.0', active_document: 'pack', available_packs: [{ id: 'job_search', name: 'Job search', version: '1.0.0', document: 'pack' }] },
      { valid: true, pack_id: 'job_search', version: '1.0.0', normalized_document: 'pack', errors: [{ line: 1, field: 'stage', message: 'fix' }] },
      { job_id: 'run-1' },
      { items: [{ id: 'b1' }] },
    );
    await client.connect();
    expect((await client.getMaintenanceSettings()).retention_count).toBe(14);
    expect((await client.updateMaintenanceSettings({ backup_directory: '/b', interval_hours: 1, retention_count: 2, expected_revision: 1 })).revision).toBe(2);
    expect((await client.listBackups()).items[0]?.id).toBe('b1');
    expect((await client.getAdminOverview()).last_verified_backup?.id).toBe('b1');
    expect((await client.createAdminPreview({ operation: 'restore', backup_id: 'b1' })).party?.name).toBe('Dana Lee');
    expect((await client.getAdminJob('j1')).status).toBe('queued');
    expect((await client.getPackSettings()).available_packs[0]?.id).toBe('job_search');
    expect((await client.validatePack('pack')).errors[0]?.message).toBe('fix');
    expect((await client.runCoach()).job_id).toBe('run-1');
    expect((await client.listBackups()).items).toHaveLength(1);
  });

  it('rejects malformed admin and pack responses', async () => {
    const client = clientWith(
      { id: 'b', created_at: 'now', schema_version: 'bad', workspace_epoch: 'e', size_bytes: 1, integrity: 'unknown' },
      { as_of: 'now', workspace_epoch: 'e', event_cursor: '0', last_verified_backup: null, backups: [], parties: [{ id: 'p', kind: 'alien', name: 'x' }] },
      { id: 'j', operation: 'restore', status: 'unknown', summary: null, new_epoch: null, last_error: null },
      { valid: false, errors: [{ line: null, field: null, message: 7 }] },
    );
    await client.connect();
    await expect(client.createAdminBackup()).rejects.toThrow(/backup record is malformed/);
    await expect(client.getAdminOverview()).rejects.toThrow(/admin party is malformed/);
    await expect(client.getAdminJob('j')).rejects.toThrow(/admin job is malformed/);
    await expect(client.validatePack('bad')).rejects.toThrow(/pack_error.message/);
  });
});

describe('ParleyClient resource board decoders', () => {
  const counts = { files: 1, connections: 2, proposed: 3, deliveries: 4 };
  const card = {
    resource_id: 'r1', subtype: 'proof', title: 'Case study', readiness: 'ready',
    placement: { column_id: 'c1', position: 0, revision: 1 }, counts, gaps: [], etag: '"r1"',
  };
  const entity = {
    id: 'r1', kind: 'resource', title: 'Case study', subtype: 'proof', body: { detail: 'x' }, readiness: 'ready',
    revision: 2, notes: { markdown: 'notes', revision: 1 }, etag: '"r1"',
  };
  const attachment = {
    id: 'a1', entity_id: 'r1', upload_id: 'u1', filename: 'proof.pdf', content_type: 'application/pdf',
    size: 10, sha256: 'a'.repeat(64), share_permission: true, reviewed_by_owner: true, created_at: '2026-11-23T00:00:00Z',
  };

  it('decodes the board snapshot, cards, deliveries, moves and attachments', async () => {
    const client = clientWith(
      { columns: [{ id: 'c1', title: 'Inbox', position: 0, revision: 1 }], cards: [card], off_board: [{ resource_id: 'r2', subtype: 'gift', title: 'Offer', readiness: 'draft', legacy: true, counts, gaps: ['detail'], etag: '"r2"' }], next_cursor: '1', event_cursor: 9 },
      { data: entity },
      { entity, placement: { column_id: 'c1', position: 0, revision: 1 }, created_related: { edge_id: 'e1', to_id: 'p1' }, affected_versions: [{ material_id: 'm1', version_no: 2 }], event_cursor: 9 },
      { data: [{ send_id: 's1', sent_at: '2026-11-23T00:00:00Z', recipient_id: 'p1', recipient_name: 'Dana Lee', opportunity_id: 'o1', opportunity_title: 'Onboarding', material_id: 'm1', material_version: 2 }] },
      { placement: { column_id: 'c1', position: 1, revision: 2 }, source_column_revision: 1, destination_column_revision: null },
      { data: [attachment] },
      { attachment },
      { resource_title: 'Case study', resource_revision: 2, body: 'proof', filename: 'proof.pdf' },
    );
    await client.connect();
    expect((await client.getResourceBoard({ limit: 10, cursor: '0', includeOffboard: true })).event_cursor).toBe(9);
    expect((await client.getEntityCard('r1')).notes.markdown).toBe('notes');
    expect((await client.commitResourceCard({}, 'key-1')).created_related?.edge_id).toBe('e1');
    expect((await client.getEntityCardDeliveries('r1'))[0]?.material_version).toBe(2);
    expect((await client.moveResourceCard({ resource_id: 'r1', destination_column_id: 'c1' })).placement?.position).toBe(1);
    expect((await client.listAttachments('r1'))[0]?.sha256).toBe('a'.repeat(64));
    expect((await client.bindAttachment('r1', 'u1')).filename).toBe('proof.pdf');
    expect((await client.getDeliveredResourceVersion('r1', 's1', 'm1', 2)).resourceTitle).toBe('Case study');
  });

  it('rejects malformed board, delivery and attachment payloads', async () => {
    const client = clientWith(
      { columns: [{ id: 'c', title: 'x', position: 'bad', revision: 1 }], cards: [], off_board: [], event_cursor: 0 },
      { columns: [], cards: [], off_board: [{ resource_id: 'r', subtype: 'gift', title: 'x', readiness: 'draft', legacy: true, counts: { files: 1 }, gaps: [], etag: '"r"' }], event_cursor: 0 },
      { data: [{ send_id: 's', sent_at: 'now', recipient_id: 'p', recipient_name: 'n', opportunity_id: 'o', opportunity_title: 't', material_id: 'm', material_version: 'two' }] },
      { data: [{ ...attachment, sha256: 'not-a-hash' }] },
    );
    await client.connect();
    await expect(client.getResourceBoard()).rejects.toThrow(/resource column is malformed/);
    await expect(client.getResourceBoard()).rejects.toThrow(/resource counts are malformed/);
    await expect(client.getEntityCardDeliveries('r')).rejects.toThrow(/delivery row is malformed/);
    await expect(client.listAttachments('r')).rejects.toThrow(/entity attachment is malformed/);
  });
});

describe('ParleyClient agent and writing decoders', () => {
  it('decodes agent chats, briefs, fluency, segments and templates', async () => {
    const chat = {
      id: 'chat1', role: 'interviewer', scope_kind: 'owner', scope_id: null, asset_kind: null, revision: 2, active_job_id: null,
      turns: [{ id: 't1', turn_no: 1, speaker: 'owner', text: 'Hello', job_id: null, created_at: '2026-11-23T00:00:00Z' }],
      questions: [{ question_id: 'q1', state: 'open' }],
      candidates: [{ id: 'c1', asset_kind: 'gift', fields: {}, owner_quotes: {}, missing_fields: ['detail'], state: 'pending', resource_id: null }],
    };
    const brief = { id: 'b1', scope_kind: 'party', scope_id: 'p1', kind: 'proof', text: 'A case study', detail: {}, tier: 1, quote: null, check_suggestion: null, speaker_kind: 'owner', status: 'active', revision: 1, supersedes_id: null };
    const template = { id: 't1', family_id: 'f1', name: 'Intro', version: 1, status: 'active', body: 'Hello', sentence_rules: ['short'], stage_class: 'identified', channel: 'email', parent_version_id: null };
    const client = clientWith(
      chat,
      [{ id: 'chat1', role: 'interviewer', scope_kind: 'owner', scope_id: null, asset_kind: null, revision: 2, last_turn: 'Hello', updated_at: 'now', active_job_id: null }],
      chat,
      { id: 'j1', role: 'briefer', status: 'succeeded', result: {}, last_error: null },
      [brief],
      [{ term: 'proof', status: 'learning', revision: 1 }],
      { term: 'proof', status: 'fluent', revision: 2 },
      [{ id: 's1', name: 'Segment', description: null, revision: 1 }],
      { id: 's2', name: 'New', description: 'x', revision: 1 },
      { role: 'interviewer', version_id: 'v1', version: 1, content: 'policy', sha256: 'a'.repeat(64) },
      { version_id: 'qb1', content: { questions: {} } },
      [template],
      { items: [{ id: 'f1', name: 'Introductions' }] },
      { model: 'embed', model_digest: null, calibrated_kinds: ['proof'] },
      { items: [{ id: 'vs1', text: 'Voice', channel: null, revision: 1 }] },
    );
    await client.connect();
    expect((await client.createAgentChat({ role: 'interviewer', scope_kind: 'owner' })).turns[0]?.text).toBe('Hello');
    expect((await client.listAgentChats())[0]?.last_turn).toBe('Hello');
    expect((await client.getAgentChat('chat1')).candidates[0]?.missing_fields).toEqual(['detail']);
    expect((await client.getRoleJob('j1')).status).toBe('succeeded');
    expect((await client.getBriefItems('party', 'p1'))[0]?.text).toBe('A case study');
    expect((await client.listFluency())[0]?.status).toBe('learning');
    expect((await client.updateFluency('proof', 'fluent', 1)).status).toBe('fluent');
    expect((await client.listSegments())[0]?.name).toBe('Segment');
    expect((await client.createSegment('New', 'x')).id).toBe('s2');
    expect((await client.getAgentPrompt('interviewer')).sha256).toBe('a'.repeat(64));
    expect((await client.getQuestionBank()).version_id).toBe('qb1');
    expect((await client.listWritingTemplates())[0]?.name).toBe('Intro');
    expect((await client.listTemplateFamilies())[0]?.id).toBe('f1');
    expect((await client.getEmbeddingModel()).calibrated_kinds).toEqual(['proof']);
    expect((await client.listVoiceSamples())[0]?.text).toBe('Voice');
  });
});
