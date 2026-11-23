import type { components } from './generated';

export type Resource = components['schemas']['Resource'];
export type GraphEntity = components['schemas']['GraphEntity'];
export type GraphSnapshot = components['schemas']['GraphSnapshot'];
export type Entity = components['schemas']['Entity'];
export type EntityNotes = components['schemas']['EntityNotes'];
export type Dossier = components['schemas']['Dossier'];
export type SavedSource = components['schemas']['SavedSource'];
export type EntityInput = components['schemas']['EntityInput'];
export type CaptureInput = components['schemas']['CaptureInput'];
export type Opportunity = components['schemas']['Opportunity'];
export type GateCheck = { id: string; severity: 'hard' | 'soft'; pass: boolean; detail: string; overridden: boolean };
export type GateReport = { checks: GateCheck[]; copy_enabled: boolean };
export type Material = Omit<components['schemas']['MaterialRecord'], 'gate'> & { gate: GateReport | null };
export type ReleasedMaterial = components['schemas']['ReleasedMaterial'];
export type ContactThread = components['schemas']['ThreadRecord'];
export type ThreadSend = components['schemas']['ThreadSend'];
export type SendRecord = components['schemas']['SendRecord'];
export type ContactOutcome = components['schemas']['OutcomeRecord'];
export type ContactResolution = components['schemas']['ResolutionRecord'];
export type PartyPolicyRecord = components['schemas']['PartyPolicyRecord'];
export type CapacityRecord = components['schemas']['CapacityRecord'];
export type DailyQueueItem = {
  id: string; opportunity_id: string; title: string; type: 'followup_due' | 'ready_to_send' | 'blocked' | 'needs_input' | 'review_stale';
  party_id: string | null; party_name: string | null; due_at: string | null; reason: string; blocking_reason: string | null;
  remediation_target: { kind: 'opportunity' | 'question' | 'material'; id: string } | null;
};
export type DailyQueueSnapshot = { as_of: string; pack_id: string; pack_version: string; daily_cap: number; actionable_count: number; deferred_count: number; items: DailyQueueItem[] };
export type InsightMetric = { id: string; label: string; numerator: number; denominator: number; rate: number | null; lower: number | null; upper: number | null; state: 'measured' | 'insufficient_observations' };
export type InsightComparison = { label: string; left: InsightMetric; right: InsightMetric; state: 'distinguishable' | 'inconclusive' | 'exploratory' };
export type CoachQuestion = { id: string; kind: string; text: string; target_kind: string; target_id: string; impact: number; state: 'open' | 'answered' | 'dismissed' };
export type InsightsReport = { as_of: string; period_start: string; period_end: string; pack_id: string; pack_version: string; sample_size: number; metrics: InsightMetric[]; comparisons: InsightComparison[]; gaps: Array<{ id: string; label: string; covered: number; total: number; blocked_count: number; suggested_kind: string }>; questions: CoachQuestion[] };
export type PackSettings = { revision: number; active_pack_id: string; active_pack_version: string; active_document: string; available_packs: Array<{ id: string; name: string; version: string; document: string }> };
export type PackValidation = { valid: boolean; pack_id: string | null; version: string | null; normalized_document: string | null; errors: Array<{ line: number | null; field: string | null; message: string }> };
export type AdminBackup = { id: string; created_at: string; schema_version: number; workspace_epoch: string; size_bytes: number; integrity: 'verified' | 'unknown' };
export type AdminOverview = { as_of: string; workspace_epoch: string; event_cursor: string; last_verified_backup: AdminBackup | null; backups: AdminBackup[]; parties: Array<{ id: string; kind: 'person' | 'org'; name: string; descriptor: string | null }> };
export type AdminOperation = 'restore' | 'erase_party' | 'erase_workspace';
export type AdminPreview = { id: string; operation: AdminOperation; digest: string; workspace_epoch: string; event_cursor: string; expires_at: string; backup: AdminBackup | null; party: { id: string; name: string } | null; impact: Record<string, number>; preserved_shared: Record<string, number> };
export type AdminJob = { id: string; operation: AdminOperation; status: 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled'; summary: string | null; new_epoch: string | null; last_error: string | null };
export type ResourceColumn = components['schemas']['ResourceColumn'];
export type PlacementView = components['schemas']['PlacementView'];
export type ResourceCounts = components['schemas']['ResourceCounts'];
export type ResourceCardSummary = components['schemas']['ResourceCardSummary'];
export type OffBoardResource = components['schemas']['OffBoardResource'];
export type ResourceBoardSnapshot = components['schemas']['ResourceBoardSnapshot'];
export type EntityCard = components['schemas']['EntityCard'];
export type CreatedRelated = components['schemas']['CreatedRelated'];
export type ResourceCardCommitResult = components['schemas']['ResourceCardCommitResult'];
export type MoveResourceCardResult = components['schemas']['MoveResourceCardResult'];
export type ResourcePlacementInput = components['schemas']['ResourcePlacementInput'];
export type EdgeView = components['schemas']['EdgeView'];
export type EntityAttachment = components['schemas']['EntityAttachment'];
export type DeliveryRow = components['schemas']['DeliveryRow'];
export type BoardList = components['schemas']['BoardList'];
export type BoardOpportunity = Opportunity & { tags: string[] };
export type BoardPlacement = { opportunity: BoardOpportunity; opportunity_id: string; list_id: string; position: number; placement_revision: number; next_task: string | null };
export type BoardSnapshot = { lists: BoardList[]; cards: BoardPlacement[] };
export type OpportunityNotes = components['schemas']['OpportunityNotes'];
export type OpportunityTask = NonNullable<OpportunityNotes['tasks']>[number];
export type MovePlacementInput = components['schemas']['MoveOpportunityPlacement'];
type Session = components['schemas']['Session'];
type EventPage = components['schemas']['EventPage'];
type CreateResource = components['schemas']['CreateResource'];
type UpdateResource = components['schemas']['UpdateResource'];

export type ProviderConfig = { endpoint: string; model: string; num_ctx: number; num_predict: number; locale: string };
export type ExtractorProposal = { id: string; kind: string; candidate: { statement: string; quote: string; mention_name: string | null; mention_kind: string | null }; status: string };
export type ExtractorJob = { id: string; role: string; status: string; current_attempt: number; max_attempts: number; progress_current: number; progress_total: number; injection_suspected: boolean; last_error_code: string | null; last_error: string | null; source_occurrence_id: string; proposal_batch_id: string | null; proposals: ExtractorProposal[] };
export type ProposalDecision = { item_id: string; action: 'confirm' | 'correct' | 'dismiss' | 'source_only' | 'unresolved'; target_entity_id?: string; corrected_statement?: string };
export type AgentChat = {
  id: string; role: 'interviewer' | 'briefer'; scope_kind: 'owner' | 'party' | 'segment'; scope_id: string | null;
  asset_kind: 'gift' | 'proof' | 'artifact' | null; revision: number; active_job_id: string | null;
  turns: Array<{ id: string; turn_no: number; speaker: 'owner' | 'agent'; text: string; job_id: string | null; created_at: string }>;
  questions: Array<{ question_id: string; state: string }>;
  candidates: Array<{ id: string; asset_kind: string; fields: Record<string, unknown>; owner_quotes: Record<string, unknown>; missing_fields: string[]; state: string; resource_id: string | null }>;
};
export type AgentChatSummary = { id: string; role: AgentChat['role']; scope_kind: AgentChat['scope_kind']; scope_id: string | null; asset_kind: AgentChat['asset_kind']; revision: number; last_turn: string | null; updated_at: string; active_job_id: string | null };
export type BriefItem = { id: string; scope_kind: 'party' | 'segment'; scope_id: string; kind: string; text: string; detail: Record<string, unknown>; tier: 1 | 2 | 3; quote: string | null; check_suggestion: string | null; speaker_kind: string; status: string; revision: number; supersedes_id: string | null };
export type RoleJob = { id: string; role: string; status: string; result: Record<string, unknown> | null; last_error: string | null; scope_kind?: string; scope_id?: string };
export type VocabularyFluency = { term: string; status: 'unknown' | 'learning' | 'fluent'; revision: number };
export type WritingTemplate = { id: string; family_id: string; name: string; version: number; status: string; body: string; sentence_rules: string[]; stage_class: string; channel: string; parent_version_id: string | null };
export type WritingInterest = { id: string; side: 'them' | 'me'; statement: string; knowledge_kind: string; check_suggestion: string | null; claim_id: string | null; claim_revision: number | null; revision: number; status: string };
export type WritingMatch = { id: string; opportunity_id: string; recipient_id: string; embed_model_digest: string; calibration_json: string; shortlist_json: string; why_job_id: string | null };
export type WritingJob = { job_id: string; role: string };
export type BackupRecord = { id: string; created_at: string; status: 'verified' | 'damaged' | 'incomplete'; manifest_sha256: string; workspace_id: string; format_version: number };
export type RestorePreview = { id: string; digest: string; backup: BackupRecord; current_cursor: string; erasure_resurrections: number; required_bytes: number };
export type ErasePreview = { id: string; digest: string; domain_cursor: string; affected_counts: Record<string, number>; shared_sources: number; owner_managed_backups: number };
export type MaintenanceSettings = { backup_directory: string; interval_hours: number; retention_count: number; revision: number; last_successful_at: string | null };
export type MaintenanceOperation = { id: string; kind: 'restore' | 'import' | 'erase'; state: 'pending_restart' | 'cleaning' | 'complete' | 'failed'; epoch: string; external_copies_requiring_disposal: number; error_code?: string };

export function newOperationKey(): string {
  const timestamp = Date.now().toString(16).padStart(12, '0').slice(-12);
  const bytes = globalThis.crypto.getRandomValues(new Uint8Array(10));
  const entropy = Array.from(bytes, (value) => value.toString(16).padStart(2, '0')).join('');
  const variant = ((Number.parseInt(entropy[3] ?? '0', 16) & 0x3) | 0x8).toString(16);
  return `${timestamp.slice(0, 8)}-${timestamp.slice(8)}-7${entropy.slice(0, 3)}-${variant}${entropy.slice(4, 7)}-${entropy.slice(7, 19)}`;
}

function object(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function requiredString(value: unknown, field: string): string {
  if (typeof value !== 'string') throw new Error(`Invalid response: ${field} must be text`);
  return value;
}

function decodeSession(value: unknown): Session {
  const session = object(value);
  if (!session || !Array.isArray(session.capabilities) || !session.capabilities.every((item) => typeof item === 'string')
    || !Number.isInteger(session.wire_major) || !Number.isInteger(session.wire_minor)) {
    throw new Error('Invalid response: workspace session is malformed');
  }
  return {
    workspace_id: requiredString(session.workspace_id, 'workspace_id'),
    epoch: requiredString(session.epoch, 'epoch'),
    wire_major: session.wire_major as number,
    wire_minor: session.wire_minor as number,
    capabilities: session.capabilities,
    mutation_token: requiredString(session.mutation_token, 'mutation_token'),
    cursor: requiredString(session.cursor, 'cursor'),
  };
}

function decodeResource(value: unknown): Resource {
  const resource = object(value);
  const body = resource ? object(resource.body) : null;
  if (!resource || !body
    || !['gift', 'proof', 'artifact'].includes(String(resource.kind))
    || !['draft', 'ready', 'retired'].includes(String(resource.readiness))
    || !Number.isSafeInteger(resource.revision) || Number(resource.revision) < 1) {
    throw new Error('Invalid response: resource is malformed');
  }
  return {
    id: requiredString(resource.id, 'resource.id'),
    kind: resource.kind as Resource['kind'],
    title: requiredString(resource.title, 'resource.title'),
    body,
    readiness: resource.readiness as Resource['readiness'],
    revision: Number(resource.revision),
    etag: requiredString(resource.etag, 'resource.etag'),
  };
}

function decodeGraphEntity(value: unknown): GraphEntity {
  const entity = object(value);
  if (!entity
    || !['person', 'org', 'project', 'resource'].includes(String(entity.kind))
    || !Number.isSafeInteger(entity.revision) || Number(entity.revision) < 1) {
    throw new Error('Invalid response: directory entity is malformed');
  }
  return {
    id: requiredString(entity.id, 'entity.id'),
    kind: entity.kind as GraphEntity['kind'],
    name: requiredString(entity.name, 'entity.name'),
    focus: typeof entity.focus === 'string' ? entity.focus : '',
    revision: Number(entity.revision),
    etag: typeof entity.etag === 'string' ? entity.etag : undefined,
  };
}

function decodeGraphSnapshot(value: unknown): GraphSnapshot {
  const snapshot = object(value);
  if (!snapshot || !Array.isArray(snapshot.nodes) || !Array.isArray(snapshot.edges)
    || !Number.isSafeInteger(snapshot.omitted_nodes) || Number(snapshot.omitted_nodes) < 0
    || !Number.isSafeInteger(snapshot.omitted_edges) || Number(snapshot.omitted_edges) < 0
    || snapshot.nodes.length > 200 || snapshot.edges.length > 400) {
    throw new Error('Invalid response: graph snapshot is malformed');
  }
  const nodes = snapshot.nodes.map((raw) => {
    const node = object(raw);
    if (!node || !Number.isFinite(node.x) || !Number.isFinite(node.y)
      || !Number.isSafeInteger(node.position_revision) || Number(node.position_revision) < 0) throw new Error('Invalid response: graph node is malformed');
    return { ...decodeGraphEntity(node), x: Number(node.x), y: Number(node.y), position_revision: Number(node.position_revision) };
  });
  const edges = snapshot.edges.map((raw) => {
    const edge = object(raw);
    if (!edge || !Number.isSafeInteger(edge.revision) || ![1, 2, 3].includes(Number(edge.tier))
      || !['owner', 'party', 'unknown'].includes(String(edge.speaker_kind))) throw new Error('Invalid response: graph edge is malformed');
    return {
      id: requiredString(edge.id, 'edge.id'), from_id: requiredString(edge.from_id, 'edge.from_id'),
      to_id: requiredString(edge.to_id, 'edge.to_id'), verb: requiredString(edge.verb, 'edge.verb'), revision: Number(edge.revision),
      occurrence_id: requiredString(edge.occurrence_id, 'edge.occurrence_id'), quote: requiredString(edge.quote, 'edge.quote'), tier: Number(edge.tier) as 1 | 2 | 3,
      speaker_kind: requiredString(edge.speaker_kind, 'edge.speaker_kind') as 'owner' | 'party' | 'unknown', captured_at: requiredString(edge.captured_at, 'edge.captured_at'),
      source_title: typeof edge.source_title === 'string' ? edge.source_title : null,
      source_url: typeof edge.source_url === 'string' ? edge.source_url : null,
    };
  });
  return { nodes, edges, omitted_nodes: Number(snapshot.omitted_nodes), omitted_edges: Number(snapshot.omitted_edges) };
}

function decodeEnvelopeData(value: unknown, decode: (item: unknown) => Resource | GraphEntity | SavedSource): { data: Resource | GraphEntity | SavedSource } {
  const envelope = object(value);
  if (!envelope) throw new Error('Invalid response: entity envelope is malformed');
  return { data: decode(envelope.data) };
}

function decodeEnvelopeList(value: unknown, decode: (item: unknown) => Resource | GraphEntity | SavedSource): { data: Array<Resource | GraphEntity | SavedSource>; cursor: string } {
  const envelope = object(value);
  if (!envelope || !Array.isArray(envelope.data)) throw new Error('Invalid response: resource list is malformed');
  requiredString(envelope.cursor, 'cursor');
  return { data: envelope.data.map(decode), cursor: requiredString(envelope.cursor, 'cursor') };
}

function stringArray(value: unknown, field: string): string[] {
  if (!Array.isArray(value) || !value.every((item) => typeof item === 'string')) {
    throw new Error(`Invalid response: ${field} must be a text list`);
  }
  return value;
}

function decodeOpportunity(value: unknown): Opportunity {
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.revision) || Number(item.revision) < 1) {
    throw new Error('Invalid response: opportunity is malformed');
  }
  return {
    id: requiredString(item.id, 'opportunity.id'),
    title: requiredString(item.title, 'opportunity.title'),
    state: requiredString(item.state, 'opportunity.state'),
    revision: Number(item.revision),
    participant_ids: stringArray(item.participant_ids, 'opportunity.participant_ids'),
    source_variant_ids: stringArray(item.source_variant_ids, 'opportunity.source_variant_ids'),
    material_ids: stringArray(item.material_ids, 'opportunity.material_ids'),
  };
}

function decodeBoardList(value: unknown): BoardList {
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.position) || typeof item.archived !== 'boolean'
    || !Number.isSafeInteger(item.revision) || Number(item.revision) < 1) {
    throw new Error('Invalid response: board list is malformed');
  }
  return {
    id: requiredString(item.id, 'board_list.id'),
    title: requiredString(item.title, 'board_list.title'),
    position: Number(item.position),
    archived: item.archived,
    revision: Number(item.revision),
  };
}

function decodeBoardOpportunity(value: unknown): BoardOpportunity {
  const opportunity = decodeOpportunity(value);
  const item = object(value);
  const tags = Array.isArray(item?.tags)
    ? item.tags.flatMap((entry) => typeof entry === 'string' ? [entry] : [])
    : [];
  return { ...opportunity, tags };
}

function decodeBoardPlacement(value: unknown): BoardPlacement {
  const item = object(value);
  const placement = item ? object(item.placement) : null;
  if (!item || !placement || !Number.isSafeInteger(placement.position) || !Number.isSafeInteger(placement.revision)
    || Number(placement.revision) < 1) {
    throw new Error('Invalid response: board placement is malformed');
  }
  return {
    opportunity: decodeBoardOpportunity(item.opportunity),
    opportunity_id: requiredString(placement.opportunity_id, 'placement.opportunity_id'),
    list_id: requiredString(placement.list_id, 'placement.list_id'),
    position: Number(placement.position),
    placement_revision: Number(placement.revision),
    next_task: typeof placement.next_task === 'string' ? placement.next_task : null,
  };
}

function decodeBoardSnapshot(value: unknown): BoardSnapshot {
  const board = object(value);
  if (!board || !Array.isArray(board.lists) || !Array.isArray(board.cards)) {
    throw new Error('Invalid response: board snapshot is malformed');
  }
  return { lists: board.lists.map(decodeBoardList), cards: board.cards.map(decodeBoardPlacement) };
}

function decodeOpportunityNotes(value: unknown): OpportunityNotes {
  const notes = object(value);
  if (!notes || !Number.isSafeInteger(notes.revision) || Number(notes.revision) < 1 || !Array.isArray(notes.tasks)) {
    throw new Error('Invalid response: opportunity notes are malformed');
  }
  const tasks = notes.tasks.map((value): OpportunityTask => {
    const task = object(value);
    if (!task || !Number.isSafeInteger(task.byte_start) || !Number.isSafeInteger(task.byte_end)
      || Number(task.byte_start) < 0 || Number(task.byte_end) < Number(task.byte_start)
      || typeof task.checked !== 'boolean' || !/^[a-f0-9]{64}$/.test(String(task.line_sha256))) {
      throw new Error('Invalid response: opportunity task is malformed');
    }
    return {
      byte_start: Number(task.byte_start),
      byte_end: Number(task.byte_end),
      line_sha256: requiredString(task.line_sha256, 'task.line_sha256'),
      checked: task.checked,
      text: requiredString(task.text, 'task.text'),
    };
  });
  return {
    opportunity_id: requiredString(notes.opportunity_id, 'notes.opportunity_id'),
    markdown: requiredString(notes.markdown, 'notes.markdown'),
    revision: Number(notes.revision),
    next_task: typeof notes.next_task === 'string' ? notes.next_task : null,
    tasks,
  };
}

function decodeDailyQueue(value: unknown): DailyQueueSnapshot {
  const snapshot = object(value);
  if (!snapshot || !Array.isArray(snapshot.items) || !Number.isSafeInteger(snapshot.daily_cap) || !Number.isSafeInteger(snapshot.actionable_count) || !Number.isSafeInteger(snapshot.deferred_count)) throw new Error('Invalid response: Daily queue snapshot is malformed');
  const items = snapshot.items.map((raw): DailyQueueItem => {
    const item = object(raw);
    if (!item || !['followup_due', 'ready_to_send', 'blocked', 'needs_input', 'review_stale'].includes(String(item.type))) throw new Error('Invalid response: Daily queue item is malformed');
    const target = item.remediation_target === null ? null : object(item.remediation_target);
    if (item.remediation_target !== null && (!target || !['opportunity', 'question', 'material'].includes(String(target.kind)))) throw new Error('Invalid response: Daily remediation target is malformed');
    return {
      id: requiredString(item.id, 'daily_item.id'), opportunity_id: requiredString(item.opportunity_id, 'daily_item.opportunity_id'),
      title: requiredString(item.title, 'daily_item.title'), type: item.type as DailyQueueItem['type'],
      party_id: typeof item.party_id === 'string' ? item.party_id : null, party_name: typeof item.party_name === 'string' ? item.party_name : null,
      due_at: typeof item.due_at === 'string' ? item.due_at : null, reason: requiredString(item.reason, 'daily_item.reason'),
      blocking_reason: typeof item.blocking_reason === 'string' ? item.blocking_reason : null,
      remediation_target: target ? { kind: target.kind as NonNullable<DailyQueueItem['remediation_target']>['kind'], id: requiredString(target.id, 'remediation_target.id') } : null,
    };
  });
  return { as_of: requiredString(snapshot.as_of, 'daily.as_of'), pack_id: requiredString(snapshot.pack_id, 'daily.pack_id'), pack_version: requiredString(snapshot.pack_version, 'daily.pack_version'), daily_cap: Number(snapshot.daily_cap), actionable_count: Number(snapshot.actionable_count), deferred_count: Number(snapshot.deferred_count), items };
}

function decodeInsightMetric(value: unknown): InsightMetric {
  const metric = object(value);
  if (!metric || !['measured', 'insufficient_observations'].includes(String(metric.state)) || !Number.isSafeInteger(metric.numerator) || !Number.isSafeInteger(metric.denominator)) throw new Error('Invalid response: insight metric is malformed');
  for (const field of ['rate', 'lower', 'upper'] as const) if (metric[field] !== null && (typeof metric[field] !== 'number' || !Number.isFinite(metric[field]))) throw new Error(`Invalid response: insight ${field} is malformed`);
  return { id: requiredString(metric.id, 'metric.id'), label: requiredString(metric.label, 'metric.label'), numerator: Number(metric.numerator), denominator: Number(metric.denominator), rate: typeof metric.rate === 'number' ? metric.rate : null, lower: typeof metric.lower === 'number' ? metric.lower : null, upper: typeof metric.upper === 'number' ? metric.upper : null, state: metric.state as InsightMetric['state'] };
}

function decodeInsights(value: unknown): InsightsReport {
  const report = object(value);
  if (!report || !Array.isArray(report.metrics) || !Array.isArray(report.comparisons) || !Array.isArray(report.gaps) || !Array.isArray(report.questions) || !Number.isSafeInteger(report.sample_size)) throw new Error('Invalid response: insights report is malformed');
  const comparisons = report.comparisons.map((raw) => { const item = object(raw); if (!item || !['distinguishable', 'inconclusive', 'exploratory'].includes(String(item.state))) throw new Error('Invalid response: insight comparison is malformed'); return { label: requiredString(item.label, 'comparison.label'), left: decodeInsightMetric(item.left), right: decodeInsightMetric(item.right), state: item.state as InsightComparison['state'] }; });
  const gaps = report.gaps.map((raw) => { const item = object(raw); if (!item || !Number.isSafeInteger(item.covered) || !Number.isSafeInteger(item.total) || !Number.isSafeInteger(item.blocked_count)) throw new Error('Invalid response: Coach gap is malformed'); return { id: requiredString(item.id, 'gap.id'), label: requiredString(item.label, 'gap.label'), covered: Number(item.covered), total: Number(item.total), blocked_count: Number(item.blocked_count), suggested_kind: requiredString(item.suggested_kind, 'gap.suggested_kind') }; });
  const questions = report.questions.map((raw): CoachQuestion => { const item = object(raw); if (!item || !['open', 'answered', 'dismissed'].includes(String(item.state))) throw new Error('Invalid response: Coach question is malformed'); return { id: requiredString(item.id, 'question.id'), kind: requiredString(item.kind, 'question.kind'), text: requiredString(item.text, 'question.text'), target_kind: requiredString(item.target_kind, 'question.target_kind'), target_id: requiredString(item.target_id, 'question.target_id'), impact: Number(item.impact), state: item.state as CoachQuestion['state'] }; });
  return { as_of: requiredString(report.as_of, 'insights.as_of'), period_start: requiredString(report.period_start, 'insights.period_start'), period_end: requiredString(report.period_end, 'insights.period_end'), pack_id: requiredString(report.pack_id, 'insights.pack_id'), pack_version: requiredString(report.pack_version, 'insights.pack_version'), sample_size: Number(report.sample_size), metrics: report.metrics.map(decodeInsightMetric), comparisons, gaps, questions };
}

function decodeAdminBackup(value: unknown): AdminBackup {
  const backup = object(value);
  if (!backup || !Number.isSafeInteger(backup.schema_version) || !Number.isSafeInteger(backup.size_bytes) || !['verified', 'unknown'].includes(String(backup.integrity))) throw new Error('Invalid response: backup record is malformed');
  return { id: requiredString(backup.id, 'backup.id'), created_at: requiredString(backup.created_at, 'backup.created_at'), schema_version: Number(backup.schema_version), workspace_epoch: requiredString(backup.workspace_epoch, 'backup.workspace_epoch'), size_bytes: Number(backup.size_bytes), integrity: backup.integrity as AdminBackup['integrity'] };
}

function decodeAdminOverview(value: unknown): AdminOverview {
  const overview = object(value);
  if (!overview || !Array.isArray(overview.backups) || !Array.isArray(overview.parties)) throw new Error('Invalid response: admin overview is malformed');
  const parties = overview.parties.map((raw) => { const party = object(raw); if (!party || !['person', 'org'].includes(String(party.kind))) throw new Error('Invalid response: admin party is malformed'); return { id: requiredString(party.id, 'party.id'), kind: party.kind as 'person' | 'org', name: requiredString(party.name, 'party.name'), descriptor: typeof party.descriptor === 'string' ? party.descriptor : null }; });
  return { as_of: requiredString(overview.as_of, 'admin.as_of'), workspace_epoch: requiredString(overview.workspace_epoch, 'admin.workspace_epoch'), event_cursor: requiredString(overview.event_cursor, 'admin.event_cursor'), last_verified_backup: overview.last_verified_backup === null ? null : decodeAdminBackup(overview.last_verified_backup), backups: overview.backups.map(decodeAdminBackup), parties };
}

function decodeAdminPreview(value: unknown): AdminPreview {
  const preview = object(value);
  if (!preview || !['restore', 'erase_party', 'erase_workspace'].includes(String(preview.operation)) || !/^[a-f0-9]{64}$/.test(String(preview.digest))) throw new Error('Invalid response: admin preview is malformed');
  const party = preview.party === null ? null : object(preview.party);
  const impact = object(preview.impact); const preserved = object(preview.preserved_shared);
  if (!impact || !preserved || [...Object.values(impact), ...Object.values(preserved)].some((count) => !Number.isSafeInteger(count) || Number(count) < 0)) throw new Error('Invalid response: admin impact summary is malformed');
  return { id: requiredString(preview.id, 'preview.id'), operation: preview.operation as AdminOperation, digest: requiredString(preview.digest, 'preview.digest'), workspace_epoch: requiredString(preview.workspace_epoch, 'preview.workspace_epoch'), event_cursor: requiredString(preview.event_cursor, 'preview.event_cursor'), expires_at: requiredString(preview.expires_at, 'preview.expires_at'), backup: preview.backup === null ? null : decodeAdminBackup(preview.backup), party: party ? { id: requiredString(party.id, 'preview.party.id'), name: requiredString(party.name, 'preview.party.name') } : null, impact: impact as Record<string, number>, preserved_shared: preserved as Record<string, number> };
}

function decodeAdminJob(value: unknown): AdminJob {
  const job = object(value);
  if (!job || !['restore', 'erase_party', 'erase_workspace'].includes(String(job.operation)) || !['queued', 'running', 'succeeded', 'failed', 'cancelled'].includes(String(job.status))) throw new Error('Invalid response: admin job is malformed');
  return { id: requiredString(job.id, 'admin_job.id'), operation: job.operation as AdminOperation, status: job.status as AdminJob['status'], summary: typeof job.summary === 'string' ? job.summary : null, new_epoch: typeof job.new_epoch === 'string' ? job.new_epoch : null, last_error: typeof job.last_error === 'string' ? job.last_error : null };
}

function decodeWrappedBoardList(value: unknown): { list: BoardList } {
  const wrapper = object(value);
  if (!wrapper) throw new Error('Invalid response: board list result is malformed');
  return { list: decodeBoardList(wrapper.list) };
}

function decodeArchivedBoardList(value: unknown): components['schemas']['ArchivedBoardList'] {
  const result = object(value);
  if (!result || !Array.isArray(result.moved)) throw new Error('Invalid response: archived board list is malformed');
  const moved = result.moved.map((value) => {
    const item = object(value);
    if (!item || !Number.isSafeInteger(item.position) || !Number.isSafeInteger(item.placement_revision)) {
      throw new Error('Invalid response: archived placement is malformed');
    }
    return { opportunity_id: requiredString(item.opportunity_id, 'moved.opportunity_id'), position: Number(item.position), placement_revision: Number(item.placement_revision) };
  });
  return { archived_list: decodeBoardList(result.archived_list), destination_list: decodeBoardList(result.destination_list), moved };
}

function decodeMovedPlacement(value: unknown): components['schemas']['OpportunityPlacement'] {
  const result = object(value);
  if (!result || !Number.isSafeInteger(result.position) || !Number.isSafeInteger(result.placement_revision)
    || !Number.isSafeInteger(result.source_list_revision) || !Number.isSafeInteger(result.destination_list_revision)) {
    throw new Error('Invalid response: moved placement is malformed');
  }
  return {
    opportunity_id: requiredString(result.opportunity_id, 'placement.opportunity_id'),
    list_id: requiredString(result.list_id, 'placement.list_id'),
    position: Number(result.position),
    placement_revision: Number(result.placement_revision),
    source_list_revision: Number(result.source_list_revision),
    destination_list_revision: Number(result.destination_list_revision),
  };
}

function decodeAgentChat(value: unknown): AgentChat {
  const chat = object(value);
  if (!chat || !Array.isArray(chat.turns) || !Array.isArray(chat.questions) || !Array.isArray(chat.candidates)
    || !Number.isSafeInteger(chat.revision) || Number(chat.revision) < 1) throw new Error('Invalid response: agent chat is malformed');
  return {
    id: requiredString(chat.id, 'chat.id'), role: requiredString(chat.role, 'chat.role') as AgentChat['role'],
    scope_kind: requiredString(chat.scope_kind, 'chat.scope_kind') as AgentChat['scope_kind'],
    scope_id: typeof chat.scope_id === 'string' ? chat.scope_id : null,
    asset_kind: typeof chat.asset_kind === 'string' ? chat.asset_kind as AgentChat['asset_kind'] : null,
    revision: Number(chat.revision), active_job_id: typeof chat.active_job_id === 'string' ? chat.active_job_id : null,
    turns: chat.turns.map((raw) => { const turn = object(raw); if (!turn) throw new Error('Invalid response: agent turn is malformed'); return { id: requiredString(turn.id, 'turn.id'), turn_no: Number(turn.turn_no), speaker: requiredString(turn.speaker, 'turn.speaker') as 'owner' | 'agent', text: requiredString(turn.text, 'turn.text'), job_id: typeof turn.job_id === 'string' ? turn.job_id : null, created_at: requiredString(turn.created_at, 'turn.created_at') }; }),
    questions: chat.questions.map((raw) => { const question = object(raw); if (!question) throw new Error('Invalid response: interviewer question state is malformed'); return { question_id: requiredString(question.question_id, 'question.question_id'), state: requiredString(question.state, 'question.state') }; }),
    candidates: chat.candidates.map((raw) => { const candidate = object(raw); if (!candidate || !object(candidate.fields) || !object(candidate.owner_quotes) || !Array.isArray(candidate.missing_fields)) throw new Error('Invalid response: interviewer candidate is malformed'); return { id: requiredString(candidate.id, 'candidate.id'), asset_kind: requiredString(candidate.asset_kind, 'candidate.asset_kind'), fields: candidate.fields as Record<string, unknown>, owner_quotes: candidate.owner_quotes as Record<string, unknown>, missing_fields: stringArray(candidate.missing_fields, 'candidate.missing_fields'), state: requiredString(candidate.state, 'candidate.state'), resource_id: typeof candidate.resource_id === 'string' ? candidate.resource_id : null }; }),
  };
}

function decodeBriefItem(value: unknown): BriefItem {
  const item = object(value); if (!item || !object(item.detail) || !Number.isSafeInteger(item.revision) || ![1, 2, 3].includes(Number(item.tier))) throw new Error('Invalid response: brief item is malformed');
  return { id: requiredString(item.id, 'brief.id'), scope_kind: requiredString(item.scope_kind, 'brief.scope_kind') as BriefItem['scope_kind'], scope_id: requiredString(item.scope_id, 'brief.scope_id'), kind: requiredString(item.kind, 'brief.kind'), text: requiredString(item.text, 'brief.text'), detail: item.detail as Record<string, unknown>, tier: Number(item.tier) as BriefItem['tier'], quote: typeof item.quote === 'string' ? item.quote : null, check_suggestion: typeof item.check_suggestion === 'string' ? item.check_suggestion : null, speaker_kind: requiredString(item.speaker_kind, 'brief.speaker_kind'), status: requiredString(item.status, 'brief.status'), revision: Number(item.revision), supersedes_id: typeof item.supersedes_id === 'string' ? item.supersedes_id : null };
}

function decodeAttachment(value: unknown): components['schemas']['AttachmentInfo'] | null {
  if (value === null || value === undefined) return null;
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.size) || Number(item.size) < 1
    || !/^[a-f0-9]{64}$/.test(String(item.sha256))) {
    throw new Error('Invalid response: attachment info is malformed');
  }
  return {
    upload_id: requiredString(item.upload_id, 'attachment.upload_id'),
    sha256: String(item.sha256),
    filename: requiredString(item.filename, 'attachment.filename'),
    content_type: requiredString(item.content_type, 'attachment.content_type'),
    size: Number(item.size),
  };
}

function decodeMaterial(value: unknown): Material {
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.version) || Number(item.version) < 1
    || !/^[a-f0-9]{64}$/.test(String(item.content_sha256))
    || !['draft', 'approved', 'released'].includes(String(item.status))
    || !(item.gate === null || object(item.gate))) {
    throw new Error('Invalid response: material is malformed');
  }
  let gate: GateReport | null = null;
  if (item.gate !== null) {
    gate = decodeGateReport(item.gate);
  }
  return {
    id: requiredString(item.id, 'material.id'),
    opportunity_id: requiredString(item.opportunity_id, 'material.opportunity_id'),
    recipient_id: requiredString(item.recipient_id, 'material.recipient_id'),
    recipient_name: requiredString(item.recipient_name, 'material.recipient_name'),
    kind: requiredString(item.kind, 'material.kind'),
    version: Number(item.version),
    body: requiredString(item.body, 'material.body'),
    content_sha256: item.content_sha256 as string,
    status: item.status as Material['status'],
    gate,
    attachment: decodeAttachment(item.attachment),
  };
}

function decodeGateReport(value: unknown): GateReport {
  const report = object(value);
  if (!report || !Array.isArray(report.checks) || typeof report.copy_enabled !== 'boolean') throw new Error('Invalid response: gate report is malformed');
  return {
    checks: report.checks.map((raw) => {
      const check = object(raw);
      if (!check || !['hard', 'soft'].includes(String(check.severity)) || typeof check.pass !== 'boolean' || typeof check.overridden !== 'boolean') throw new Error('Invalid response: gate check is malformed');
      return { id: requiredString(check.id, 'gate.id'), severity: check.severity as GateCheck['severity'], pass: check.pass, detail: requiredString(check.detail, 'gate.detail'), overridden: check.overridden };
    }),
    copy_enabled: report.copy_enabled,
  };
}

function decodeReleasedMaterial(value: unknown): ReleasedMaterial {
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.version) || Number(item.version) < 1
    || !/^[a-f0-9]{64}$/.test(String(item.content_sha256))) {
    throw new Error('Invalid response: release is malformed');
  }
  return {
    release_id: requiredString(item.release_id, 'release.release_id'),
    material_id: requiredString(item.material_id, 'release.material_id'),
    version: Number(item.version),
    recipient_id: requiredString(item.recipient_id, 'release.recipient_id'),
    body: requiredString(item.body, 'release.body'),
    content_sha256: item.content_sha256 as string,
    issued_at: requiredString(item.issued_at, 'release.issued_at'),
    attachment: decodeAttachment(item.attachment),
  };
}

function decodeArray<T>(value: unknown, field: string, decode: (item: unknown) => T): T[] {
  if (!Array.isArray(value)) throw new Error(`Invalid response: ${field} must be a list`);
  return value.map(decode);
}

function decodeSavedSource(value: unknown): SavedSource {
  const source = object(value);
  if (!source) throw new Error('Invalid response: saved source is malformed');
  return {
    content_id: requiredString(source.content_id, 'source.content_id'),
    variant_id: requiredString(source.variant_id, 'source.variant_id'),
    occurrence_id: requiredString(source.occurrence_id, 'source.occurrence_id'),
    normalized_sha256: requiredString(source.normalized_sha256, 'source.normalized_sha256'),
    raw_sha256: requiredString(source.raw_sha256, 'source.raw_sha256'),
    ...(typeof source.job_id === 'string' ? { job_id: source.job_id } : {}),
    ...(typeof source.job_status === 'string' ? { job_status: source.job_status } : {}),
  };
}

function decodeProvider(value: unknown): ProviderConfig {
  const row = object(value);
  if (!row || !Number.isSafeInteger(row.num_ctx) || !Number.isSafeInteger(row.num_predict)) throw new Error('Invalid response: provider settings are malformed');
  return { endpoint: requiredString(row.endpoint, 'provider.endpoint'), model: requiredString(row.model, 'provider.model'), num_ctx: Number(row.num_ctx), num_predict: Number(row.num_predict), locale: requiredString(row.locale, 'provider.locale') };
}

function decodeExtractorJob(value: unknown): ExtractorJob {
  const row = object(value);
  if (!row || !Array.isArray(row.proposals)) throw new Error('Invalid response: Extractor job is malformed');
  const proposals = row.proposals.map((entry): ExtractorProposal => {
    const item = object(entry); const candidate = item ? object(item.candidate) : null;
    if (!item || !candidate) throw new Error('Invalid response: Extractor proposal is malformed');
    return { id: requiredString(item.id, 'proposal.id'), kind: requiredString(item.kind, 'proposal.kind'), status: requiredString(item.status, 'proposal.status'), candidate: { statement: requiredString(candidate.statement, 'candidate.statement'), quote: requiredString(candidate.quote, 'candidate.quote'), mention_name: typeof candidate.mention_name === 'string' ? candidate.mention_name : null, mention_kind: typeof candidate.mention_kind === 'string' ? candidate.mention_kind : null } };
  });
  return { id: requiredString(row.id, 'job.id'), role: requiredString(row.role, 'job.role'), status: requiredString(row.status, 'job.status'), current_attempt: Number(row.current_attempt), max_attempts: Number(row.max_attempts), progress_current: Number(row.progress_current), progress_total: Number(row.progress_total), injection_suspected: row.injection_suspected === true, last_error_code: typeof row.last_error_code === 'string' ? row.last_error_code : null, last_error: typeof row.last_error === 'string' ? row.last_error : null, source_occurrence_id: requiredString(row.source_occurrence_id, 'job.source_occurrence_id'), proposal_batch_id: typeof row.proposal_batch_id === 'string' ? row.proposal_batch_id : null, proposals };
}

function decodeProviderHealth(value: unknown): { available: boolean; model: string; models: string[]; message: string } {
  const row = object(value);
  if (!row || typeof row.available !== 'boolean') throw new Error('Invalid response: provider health is malformed');
  return { available: row.available, model: requiredString(row.model, 'health.model'), models: stringArray(row.models, 'health.models'), message: requiredString(row.message, 'health.message') };
}

function decodeSavedSourceList(value: unknown): SavedSource[] {
  if (!Array.isArray(value)) throw new Error('Invalid response: source list is malformed');
  return value.map(decodeSavedSource);
}

function decodeEventPage(value: unknown): EventPage {
  const page = object(value);
  if (!page || !Array.isArray(page.items)) throw new Error('Invalid response: event page is malformed');
  const items = page.items.map((value) => {
    const event = object(value);
    const payload = event ? object(event.payload) : null;
    if (!event || !payload || !Number.isInteger(event.revision)) throw new Error('Invalid response: event is malformed');
    return {
      cursor: requiredString(event.cursor, 'event.cursor'),
      event_id: requiredString(event.event_id, 'event.event_id'),
      entity_type: requiredString(event.entity_type, 'event.entity_type'),
      entity_id: requiredString(event.entity_id, 'event.entity_id'),
      revision: Number(event.revision),
      event_type: requiredString(event.event_type, 'event.event_type'),
      payload,
    };
  });
  return {
    items,
    next_cursor: requiredString(page.next_cursor, 'next_cursor'),
    observed_cursor: requiredString(page.observed_cursor, 'observed_cursor'),
  };
}

function decodeResourceColumn(value: unknown): ResourceColumn {
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.position) || !Number.isSafeInteger(item.revision)) throw new Error('Invalid response: resource column is malformed');
  return { id: requiredString(item.id, 'column.id'), title: requiredString(item.title, 'column.title'), position: Number(item.position), revision: Number(item.revision) };
}

function decodePlacementView(value: unknown): PlacementView | null {
  if (value === null || value === undefined) return null;
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.position) || !Number.isSafeInteger(item.revision)) throw new Error('Invalid response: placement is malformed');
  return { column_id: requiredString(item.column_id, 'placement.column_id'), position: Number(item.position), revision: Number(item.revision) };
}

function decodeResourceCounts(value: unknown): ResourceCounts {
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.files) || !Number.isSafeInteger(item.connections) || !Number.isSafeInteger(item.proposed) || !Number.isSafeInteger(item.deliveries)) throw new Error('Invalid response: resource counts are malformed');
  return { files: Number(item.files), connections: Number(item.connections), proposed: Number(item.proposed), deliveries: Number(item.deliveries) };
}

function decodeResourceCardSummary(value: unknown): ResourceCardSummary {
  const item = object(value);
  if (!item || !object(item.counts) || !Array.isArray(item.gaps)) throw new Error('Invalid response: card summary is malformed');
  return { resource_id: requiredString(item.resource_id, 'card.resource_id'), subtype: requiredString(item.subtype, 'card.subtype'), title: requiredString(item.title, 'card.title'), readiness: requiredString(item.readiness, 'card.readiness'), placement: decodePlacementView(item.placement), counts: decodeResourceCounts(item.counts), gaps: stringArray(item.gaps, 'card.gaps'), etag: requiredString(item.etag, 'card.etag') };
}

function decodeOffBoardResource(value: unknown): OffBoardResource {
  const item = object(value);
  if (!item || typeof item.legacy !== 'boolean' || !object(item.counts) || !Array.isArray(item.gaps)) throw new Error('Invalid response: off-board resource is malformed');
  return { resource_id: requiredString(item.resource_id, 'off.resource_id'), subtype: requiredString(item.subtype, 'off.subtype'), title: requiredString(item.title, 'off.title'), readiness: requiredString(item.readiness, 'off.readiness'), legacy: item.legacy, counts: decodeResourceCounts(item.counts), gaps: stringArray(item.gaps, 'off.gaps'), etag: requiredString(item.etag, 'off.etag') };
}

function decodeResourceBoardSnapshot(value: unknown): ResourceBoardSnapshot {
  const board = object(value);
  if (!board || !Array.isArray(board.columns) || !Array.isArray(board.cards) || !Array.isArray(board.off_board) || !Number.isSafeInteger(board.event_cursor)) throw new Error('Invalid response: resource board snapshot is malformed');
  return {
    columns: board.columns.map(decodeResourceColumn),
    cards: board.cards.map(decodeResourceCardSummary),
    off_board: board.off_board.map(decodeOffBoardResource),
    next_cursor: typeof board.next_cursor === 'string' ? board.next_cursor : null,
    event_cursor: Number(board.event_cursor),
  };
}

function decodeEntityCard(value: unknown): EntityCard {
  const item = object(value);
  const notes = object(item?.notes);
  if (!item || !object(item.body) || !notes || !Number.isSafeInteger(item.revision) || !Number.isSafeInteger(notes.revision)) throw new Error('Invalid response: card entity is malformed');
  return {
    id: requiredString(item.id, 'entity.id'), kind: requiredString(item.kind, 'entity.kind'), title: requiredString(item.title, 'entity.title'),
    subtype: requiredString(item.subtype, 'entity.subtype'), body: item.body as Record<string, unknown>, readiness: requiredString(item.readiness, 'entity.readiness'),
    revision: Number(item.revision), notes: { markdown: requiredString(notes.markdown, 'notes.markdown'), revision: Number(notes.revision) },
    etag: requiredString(item.etag, 'entity.etag'),
  };
}

function decodeCreatedRelated(value: unknown): CreatedRelated | null {
  if (value === null || value === undefined) return null;
  const item = object(value);
  if (!item) throw new Error('Invalid response: created relationship is malformed');
  return { edge_id: requiredString(item.edge_id, 'related.edge_id'), to_id: requiredString(item.to_id, 'related.to_id') };
}

function decodeResourceCardCommitResult(value: unknown): ResourceCardCommitResult {
  const result = object(value);
  if (!result || !object(result.entity) || !Number.isSafeInteger(result.event_cursor ?? 0)) throw new Error('Invalid response: card commit result is malformed');
  const affected = Array.isArray(result.affected_versions)
    ? result.affected_versions.flatMap((entry) => {
      const item = object(entry);
      if (!item || !Number.isSafeInteger(item.version_no)) return [];
      return [{ material_id: requiredString(item.material_id, 'affected.material_id'), version_no: Number(item.version_no) }];
    })
    : [];
  return { entity: decodeEntityCard(result.entity), placement: decodePlacementView(result.placement), created_related: decodeCreatedRelated(result.created_related), affected_versions: affected, event_cursor: Number(result.event_cursor) };
}

function decodeDeliveryRow(value: unknown): DeliveryRow {
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.material_version)) throw new Error('Invalid response: delivery row is malformed');
  return {
    send_id: requiredString(item.send_id, 'delivery.send_id'), sent_at: requiredString(item.sent_at, 'delivery.sent_at'),
    recipient_id: requiredString(item.recipient_id, 'delivery.recipient_id'), recipient_name: requiredString(item.recipient_name, 'delivery.recipient_name'),
    opportunity_id: requiredString(item.opportunity_id, 'delivery.opportunity_id'), opportunity_title: requiredString(item.opportunity_title, 'delivery.opportunity_title'),
    material_id: requiredString(item.material_id, 'delivery.material_id'), material_version: Number(item.material_version),
    material_kind: typeof item.material_kind === 'string' ? item.material_kind : 'outreach',
  };
}

function decodeMoveResourceCardResult(value: unknown): MoveResourceCardResult {
  const result = object(value);
  if (!result) throw new Error('Invalid response: move result is malformed');
  return {
    placement: decodePlacementView(result.placement),
    source_column_revision: Number.isSafeInteger(result.source_column_revision) ? Number(result.source_column_revision) : null,
    destination_column_revision: Number.isSafeInteger(result.destination_column_revision) ? Number(result.destination_column_revision) : null,
  };
}

function decodeEdgeView(value: unknown): EdgeView {
  const edge = object(value);
  if (!edge || !Number.isSafeInteger(edge.revision)) throw new Error('Invalid response: edge is malformed');
  return {
    id: requiredString(edge.id, 'edge.id'), from_id: requiredString(edge.from_id, 'edge.from_id'), verb: requiredString(edge.verb, 'edge.verb'),
    to_id: requiredString(edge.to_id, 'edge.to_id'), state: requiredString(edge.state, 'edge.state'), revision: Number(edge.revision),
    basis: requiredString(edge.basis, 'edge.basis'), author_kind: requiredString(edge.author_kind, 'edge.author_kind'), created_at: requiredString(edge.created_at, 'edge.created_at'),
  };
}

function decodeEntityAttachment(value: unknown): EntityAttachment {
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.size) || typeof item.share_permission !== 'boolean' || typeof item.reviewed_by_owner !== 'boolean' || !/^[a-f0-9]{64}$/.test(String(item.sha256))) throw new Error('Invalid response: entity attachment is malformed');
  return {
    id: requiredString(item.id, 'attachment.id'), entity_id: requiredString(item.entity_id, 'attachment.entity_id'), upload_id: requiredString(item.upload_id, 'attachment.upload_id'),
    filename: requiredString(item.filename, 'attachment.filename'), content_type: requiredString(item.content_type, 'attachment.content_type'), size: Number(item.size),
    sha256: String(item.sha256), share_permission: item.share_permission, reviewed_by_owner: item.reviewed_by_owner, created_at: requiredString(item.created_at, 'attachment.created_at'),
  };
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code: string,
    readonly details: Array<{ field?: string; message: string }> = [],
  ) {
    super(message);
  }
}

function decodeApiError(value: unknown, status: number): ApiError {
  const root = object(value);
  const error = root ? object(root.error) : null;
  const details = error && Array.isArray(error.details)
    ? error.details.flatMap((item) => {
      const detail = object(item);
      if (!detail || typeof detail.message !== 'string') return [];
      return [{ ...(typeof detail.field === 'string' ? { field: detail.field } : {}), message: detail.message }];
    })
    : [];
  return new ApiError(
    error && typeof error.message === 'string' ? error.message : `Request failed (${status})`,
    status,
    error && typeof error.code === 'string' ? error.code : 'INVALID_ERROR_RESPONSE',
    details,
  );
}

export class ParleyClient {
  private session: Session | null = null;
  private connection: Promise<void> | null = null;

  async refreshSession(expectedEpoch?: string): Promise<void> {
    this.session = null;
    this.connection = null;
    await this.connect();
    const refreshed = this.session as Session | null;
    if (expectedEpoch && refreshed?.epoch !== expectedEpoch) throw new Error('Maintenance operation epoch does not match the active workspace session.');
  }

  /**
   * Redeem the bootstrap fragment once and load the workspace session. Every
   * caller awaits the same promise so the one-time secret is spent exactly
   * once and no request is sent before the session is known (RFC-0003 FQ1).
   */
  connect(): Promise<void> {
    if (!this.connection) {
      this.connection = this.establishSession().catch((error) => {
        this.connection = null;
        throw error;
      });
    }
    return this.connection;
  }

  private async establishSession(): Promise<void> {
    const fragment = new URLSearchParams(window.location.hash.slice(1));
    const bootstrap = fragment.get('bootstrap');
    if (bootstrap) {
      history.replaceState(null, '', `${location.pathname}${location.search}`);
      await this.request('/session/bootstrap', {
        method: 'POST',
        body: JSON.stringify({ secret: bootstrap }),
      }, false);
    }
    this.session = decodeSession(await this.request('/session'));
  }

  async listResources(): Promise<Resource[]> {
    const { data } = decodeEnvelopeList(await this.request('/entities?kind=resource'), decodeResource);
    return data as Resource[];
  }

  async listParties(): Promise<GraphEntity[]> {
    const { data } = decodeEnvelopeList(
      await this.request('/entities?kind=person&kind=org'),
      decodeGraphEntity,
    );
    return data as GraphEntity[];
  }

  async getGraph(options: { center_id?: string; view_scope?: string; view_id?: string } = {}): Promise<GraphSnapshot> {
    const params = new URLSearchParams({ view_scope: options.view_scope ?? 'leads', view_id: options.view_id ?? 'all' });
    if (options.center_id) params.set('center_id', options.center_id);
    return decodeGraphSnapshot(await this.request(`/graph?${params.toString()}`));
  }

  async saveGraphLayout(input: components['schemas']['GraphLayoutInput'], key = newOperationKey()): Promise<components['schemas']['GraphLayoutResult']> {
    const result = object(await this.mutate('/graph/layout', 'PUT', input, { 'Idempotency-Key': key }));
    if (!result || !Array.isArray(result.positions)) throw new Error('Invalid response: graph layout receipt is malformed');
    return { positions: result.positions.map((raw) => {
      const position = object(raw);
      if (!position || !Number.isFinite(position.x) || !Number.isFinite(position.y) || !Number.isSafeInteger(position.revision)) throw new Error('Invalid response: graph position receipt is malformed');
      return { entity_id: requiredString(position.entity_id, 'position.entity_id'), x: Number(position.x), y: Number(position.y), revision: Number(position.revision) };
    }) };
  }

  async getResource(id: string): Promise<Resource> {
    const { data } = decodeEnvelopeData(await this.request(`/entities/${encodeURIComponent(id)}`), decodeResource);
    return data as Resource;
  }

  async getEvents(after = '0'): Promise<EventPage> {
    return decodeEventPage(await this.request(`/events?after=${encodeURIComponent(after)}`));
  }

  async createResource(input: CreateResource, key: string): Promise<Resource> {
    const { data } = decodeEnvelopeData(
      await this.mutate('/entities', 'POST', input, { 'Idempotency-Key': key }),
      decodeResource,
    );
    return data as Resource;
  }

  async createEntity(input: EntityInput, key: string): Promise<GraphEntity> {
    const { data } = decodeEnvelopeData(
      await this.mutate('/entities', 'POST', input, { 'Idempotency-Key': key }),
      decodeGraphEntity,
    );
    return data as GraphEntity;
  }

  async createRelatedEntity(input: components['schemas']['RelatedEntityInput'], key = newOperationKey()): Promise<GraphEntity> {
    return decodeGraphEntity(await this.mutate('/graph/related', 'POST', input, { 'Idempotency-Key': key }));
  }

  /**
   * Typed party edit (name/focus). Requires the strong entity ETag from the
   * directory row, node or detail read; a stale tag yields 412.
   */
  async updateParty(id: string, input: { name?: string; focus?: string }, etag: string, key = newOperationKey()): Promise<GraphEntity> {
    const { data } = decodeEnvelopeData(
      await this.mutate(`/entities/${encodeURIComponent(id)}`, 'PATCH', input, {
        'If-Match': etag,
        'Idempotency-Key': key,
      }),
      decodeGraphEntity,
    );
    return data as GraphEntity;
  }

  async getEntityNotes(id: string): Promise<EntityNotes> {
    const envelope = object(await this.request(`/entities/${encodeURIComponent(id)}/notes`));
    const data = envelope ? object(envelope.data) : null;
    if (!data || !Number.isSafeInteger(data.revision) || Number(data.revision) < 1) {
      throw new Error('Invalid response: entity notes are malformed');
    }
    return {
      entity_id: requiredString(data.entity_id, 'notes.entity_id'),
      markdown: requiredString(data.markdown, 'notes.markdown'),
      revision: Number(data.revision),
      etag: requiredString(data.etag, 'notes.etag'),
    };
  }

  async updateEntityNotes(id: string, markdown: string, expected_notes_rev: number, etag: string, key = newOperationKey()): Promise<EntityNotes> {
    const envelope = object(await this.mutate(`/entities/${encodeURIComponent(id)}/notes`, 'PUT', { markdown, expected_notes_rev }, { 'If-Match': etag, 'Idempotency-Key': key }));
    const data = envelope ? object(envelope.data) : null;
    if (!data || !Number.isSafeInteger(data.revision) || Number(data.revision) < 1) {
      throw new Error('Invalid response: updated entity notes are malformed');
    }
    return {
      entity_id: requiredString(data.entity_id, 'notes.entity_id'),
      markdown: requiredString(data.markdown, 'notes.markdown'),
      revision: Number(data.revision),
      etag: requiredString(data.etag, 'notes.etag'),
    };
  }

  async getDossier(id: string): Promise<Dossier> {
    const envelope = object(await this.request(`/entities/${encodeURIComponent(id)}/dossier`));
    const data = envelope ? object(envelope.data) : null;
    if (!data || !Array.isArray(data.direct_links) || !Array.isArray(data.files)
      || !Array.isArray(data.opportunities) || !Array.isArray(data.associated_resources)) {
      throw new Error('Invalid response: entity dossier is malformed');
    }
    const links = data.direct_links.map((raw) => {
      const item = object(raw);
      if (!item || !Number.isSafeInteger(item.revision) || ![1, 2, 3].includes(Number(item.tier ?? 1))) throw new Error('Invalid response: dossier link is malformed');
      return { id: requiredString(item.id, 'link.id'), from_id: requiredString(item.from_id, 'link.from_id'), to_id: requiredString(item.to_id, 'link.to_id'), verb: requiredString(item.verb, 'link.verb'), revision: Number(item.revision), tier: Number(item.tier) as 1 | 2 | 3 | null, wording: typeof item.wording === 'string' ? item.wording : null };
    });
    const files = data.files.map((raw) => {
      const item = object(raw);
      if (!item || !Number.isSafeInteger(item.size) || Number(item.size) < 1
        || !/^[a-f0-9]{64}$/.test(String(item.sha256)) || typeof item.share_permission !== 'boolean' || typeof item.reviewed_by_owner !== 'boolean') throw new Error('Invalid response: dossier file is malformed');
      return { id: requiredString(item.id, 'file.id'), filename: requiredString(item.filename, 'file.filename'), content_type: requiredString(item.content_type, 'file.content_type'), size: Number(item.size), sha256: String(item.sha256), share_permission: item.share_permission, reviewed_by_owner: item.reviewed_by_owner, created_at: requiredString(item.created_at, 'file.created_at') };
    });
    const opportunities = data.opportunities.map((raw) => {
      const item = object(raw);
      if (!item || !['primary', 'participant'].includes(String(item.role))) throw new Error('Invalid response: dossier opportunity is malformed');
      return { id: requiredString(item.id, 'opportunity.id'), title: requiredString(item.title, 'opportunity.title'), role: item.role as 'primary' | 'participant' };
    });
    const resources = data.associated_resources.map((raw) => {
      const item = object(raw);
      if (!item) throw new Error('Invalid response: dossier resource is malformed');
      return { id: requiredString(item.id, 'resource.id'), title: requiredString(item.title, 'resource.title'), kind: requiredString(item.kind, 'resource.kind'), opportunity_id: requiredString(item.opportunity_id, 'resource.opportunity_id'), opportunity_title: requiredString(item.opportunity_title, 'resource.opportunity_title') };
    });
    return { entity: decodeGraphEntity(data.entity), direct_links: links, files, opportunities, associated_resources: resources };
  }

  async updateResource(resource: Resource, input: UpdateResource, key: string): Promise<Resource> {
    const { data } = decodeEnvelopeData(
      await this.mutate(`/entities/${encodeURIComponent(resource.id)}`, 'PATCH', input, {
        'If-Match': resource.etag,
        'Idempotency-Key': key,
      }),
      decodeResource,
    );
    return data as Resource;
  }

  async saveSource(input: CaptureInput): Promise<SavedSource> {
    const response = await this.request('/captures', { method: 'POST', body: JSON.stringify(input) });
    return decodeSavedSource(response);
  }

  async listSources(): Promise<SavedSource[]> {
    return decodeSavedSourceList(await this.request('/captures'));
  }

  async getExtractorJob(id: string): Promise<ExtractorJob> {
    return decodeExtractorJob(await this.request(`/jobs/${encodeURIComponent(id)}`));
  }

  async cancelExtractorJob(id: string): Promise<void> {
    await this.request(`/jobs/${encodeURIComponent(id)}`, { method: 'POST', body: '{}' });
  }

  async getCaptureProposals(occurrenceId: string): Promise<ExtractorJob> {
    return decodeExtractorJob(await this.request(`/captures/${encodeURIComponent(occurrenceId)}/proposals`));
  }

  async resolveProposals(batchId: string, decisions: ProposalDecision[], opportunityId?: string): Promise<{ status: string }> {
    const value = object(await this.mutate(`/proposals/${encodeURIComponent(batchId)}/resolve`, 'POST', {
      decisions,
      ...(opportunityId ? { opportunity_id: opportunityId } : {}),
    }, { 'Idempotency-Key': newOperationKey() }));
    if (!value || typeof value.status !== 'string') throw new Error('Invalid response: review result is malformed');
    return { status: value.status };
  }

  async getProvider(): Promise<ProviderConfig> {
    return decodeProvider(await this.request('/agent/provider'));
  }

  async updateProvider(config: ProviderConfig): Promise<void> {
    await this.mutate('/agent/provider', 'PUT', config, { 'Idempotency-Key': newOperationKey() });
  }

  async checkProvider(): Promise<{ available: boolean; model: string; models: string[]; message: string }> {
    return decodeProviderHealth(await this.request('/agent/provider/health', { method: 'POST', body: '{}' }));
  }

  async createOpportunity(title: string, primary_party_id: string): Promise<Opportunity> {
    return decodeOpportunity(await this.request('/opportunities', { method: 'POST', body: JSON.stringify({ title, primary_party_id }) }));
  }

  async listOpportunities(): Promise<Opportunity[]> {
    return decodeArray(await this.request('/opportunities'), 'opportunities', decodeOpportunity);
  }

  async listOutputs(): Promise<Material[]> {
    return decodeArray(await this.request('/outputs'), 'outputs', decodeMaterial);
  }

  async getWritingInterests(opportunityId: string, recipientId: string): Promise<WritingInterest[]> {
    const value = object(await this.request(`/opportunities/${encodeURIComponent(opportunityId)}/interests?recipient_id=${encodeURIComponent(recipientId)}`));
    if (!value || !Array.isArray(value.items)) throw new Error('Invalid response: recipient interests are malformed');
    return value.items.map((raw) => { const item = object(raw); if (!item) throw new Error('Invalid response: recipient interest is malformed'); return { id: requiredString(item.id, 'interest.id'), side: requiredString(item.side, 'interest.side') as WritingInterest['side'], statement: requiredString(item.statement, 'interest.statement'), knowledge_kind: requiredString(item.knowledge_kind, 'interest.knowledge_kind'), check_suggestion: typeof item.check_suggestion === 'string' ? item.check_suggestion : null, claim_id: typeof item.claim_id === 'string' ? item.claim_id : null, claim_revision: Number.isSafeInteger(item.claim_revision) ? Number(item.claim_revision) : null, revision: Number(item.revision), status: requiredString(item.status, 'interest.status') }; });
  }

  async getWritingExchange(opportunityId: string, recipientId: string): Promise<{ current: Record<string, unknown> | null; versions: Array<Record<string, unknown>> }> {
    const value = object(await this.request(`/opportunities/${encodeURIComponent(opportunityId)}/exchange?recipient_id=${encodeURIComponent(recipientId)}`));
    if (!value || !Array.isArray(value.versions)) throw new Error('Invalid response: recipient exchange is malformed');
    return { current: object(value.current), versions: value.versions.map((entry) => { const version = object(entry); if (!version) throw new Error('Invalid response: exchange version is malformed'); return version; }) };
  }

  async addWritingInterest(opportunityId: string, input: { recipient_id: string; side: 'them' | 'me'; statement: string; knowledge_kind: 'observation' | 'hypothesis' | 'owner_report'; check_suggestion?: string }, key = newOperationKey()): Promise<WritingInterest> {
    const value = object(await this.mutate(`/opportunities/${encodeURIComponent(opportunityId)}/interests`, 'POST', input, { 'Idempotency-Key': key }));
    if (!value) throw new Error('Invalid response: created interest is malformed');
    return { id: requiredString(value.id, 'interest.id'), side: requiredString(value.side, 'interest.side') as WritingInterest['side'], statement: requiredString(value.statement, 'interest.statement'), knowledge_kind: requiredString(value.knowledge_kind, 'interest.knowledge_kind'), check_suggestion: typeof value.check_suggestion === 'string' ? value.check_suggestion : null, claim_id: null, claim_revision: null, revision: Number(value.revision ?? 1), status: requiredString(value.status ?? 'active', 'interest.status') };
  }

  async saveWritingExchange(opportunityId: string, input: { recipient_id: string; they_get: string; i_get: string; interest_ids: string[] }, key = newOperationKey()): Promise<Record<string, unknown>> {
    const value = object(await this.mutate(`/opportunities/${encodeURIComponent(opportunityId)}/exchange`, 'POST', input, { 'Idempotency-Key': key }));
    if (!value) throw new Error('Invalid response: saved exchange is malformed'); return value;
  }

  async attestWritingExchange(opportunityId: string, recipientId: string, key = newOperationKey()): Promise<void> {
    await this.mutate(`/opportunities/${encodeURIComponent(opportunityId)}/exchange/${encodeURIComponent(recipientId)}/attest`, 'POST', {}, { 'Idempotency-Key': key });
  }

  async listWritingTemplates(): Promise<WritingTemplate[]> {
    const value = await this.request('/templates');
    if (!Array.isArray(value)) throw new Error('Invalid response: templates are malformed');
    return value.map((raw) => { const item = object(raw); if (!item) throw new Error('Invalid response: template is malformed'); return { id: requiredString(item.id, 'template.id'), family_id: requiredString(item.family_id, 'template.family_id'), name: requiredString(item.name, 'template.name'), version: Number(item.version), status: requiredString(item.status, 'template.status'), body: requiredString(item.body, 'template.body'), sentence_rules: Array.isArray(item.sentence_rules) ? item.sentence_rules.flatMap((entry) => Array.isArray(entry) ? entry.filter((rule): rule is string => typeof rule === 'string') : []) : [], stage_class: requiredString(item.stage_class, 'template.stage_class'), channel: requiredString(item.channel, 'template.channel'), parent_version_id: typeof item.parent_version_id === 'string' ? item.parent_version_id : null }; });
  }

  async listTemplateFamilies(): Promise<Array<{ id: string; name: string }>> { const value = object(await this.request('/template-families')); if (!value || !Array.isArray(value.items)) throw new Error('Invalid response: template families are malformed'); return value.items.map((entry) => { const item = object(entry); if (!item) throw new Error('Invalid response: template family is malformed'); return { id: requiredString(item.id, 'family.id'), name: requiredString(item.name, 'family.name') }; }); }
  async createTemplateFamily(name: string, key = newOperationKey()): Promise<{ id: string; name: string }> { const value = object(await this.mutate('/template-families', 'POST', { name }, { 'Idempotency-Key': key })); if (!value) throw new Error('Invalid response: created family is malformed'); return { id: requiredString(value.id, 'family.id'), name: requiredString(value.name, 'family.name') }; }
  async createWritingTemplateVersion(familyId: string, input: { name: string; body: string; stage_class: string; channel: string; sentence_rules: string[][]; proof_fallback?: string; parent_version_id?: string | null }, key = newOperationKey()): Promise<WritingTemplate> { const value = object(await this.mutate(`/templates/${encodeURIComponent(familyId)}/versions`, 'POST', input, { 'Idempotency-Key': key })); if (!value) throw new Error('Invalid response: created template version is malformed'); return { id: requiredString(value.id, 'template.id'), family_id: requiredString(value.family_id, 'template.family_id'), name: typeof value.name === 'string' ? value.name : input.name, version: Number(value.version), status: requiredString(value.status, 'template.status'), body: requiredString(value.body, 'template.body'), sentence_rules: input.sentence_rules.flat(), stage_class: requiredString(value.stage_class, 'template.stage_class'), channel: requiredString(value.channel, 'template.channel'), parent_version_id: typeof value.parent_version_id === 'string' ? value.parent_version_id : null }; }
  async approveWritingTemplate(id: string, key = newOperationKey()): Promise<void> { await this.mutate(`/templates/${encodeURIComponent(id)}/approve`, 'POST', {}, { 'Idempotency-Key': key }); }
  async activateWritingTemplate(id: string, key = newOperationKey()): Promise<void> { await this.mutate(`/templates/${encodeURIComponent(id)}/activate`, 'POST', {}, { 'Idempotency-Key': key }); }

  async getEmbeddingModel(): Promise<{ model: string; model_digest: string | null; calibrated_kinds: string[] }> { const value = object(await this.request('/embedding-model')); if (!value || !Array.isArray(value.calibrated_kinds)) throw new Error('Invalid response: embedding model is malformed'); return { model: requiredString(value.model, 'embedding_model'), model_digest: typeof value.model_digest === 'string' ? value.model_digest : null, calibrated_kinds: value.calibrated_kinds.filter((kind): kind is string => typeof kind === 'string') }; }
  async setEmbeddingModel(model: string, key = newOperationKey()): Promise<void> { await this.mutate('/embedding-model', 'PUT', { model }, { 'Idempotency-Key': key }); }
  async startMatching(opportunityId: string, recipientId: string, key = newOperationKey()): Promise<WritingJob> { const value = object(await this.mutate(`/opportunities/${encodeURIComponent(opportunityId)}/matches`, 'POST', { recipient_id: recipientId }, { 'Idempotency-Key': key })); if (!value) throw new Error('Invalid response: matching job is malformed'); return { job_id: requiredString(value.job_id, 'job_id'), role: requiredString(value.role, 'role') }; }
  async getWritingMatch(opportunityId: string, recipientId: string): Promise<WritingMatch> { const value = object(await this.request(`/opportunities/${encodeURIComponent(opportunityId)}/matches?recipient_id=${encodeURIComponent(recipientId)}`)); if (!value) throw new Error('Invalid response: match result is malformed'); return { id: requiredString(value.id, 'match.id'), opportunity_id: requiredString(value.opportunity_id, 'match.opportunity_id'), recipient_id: requiredString(value.recipient_id, 'match.recipient_id'), embed_model_digest: requiredString(value.embed_model_digest, 'match.embed_model_digest'), calibration_json: requiredString(value.calibration_json, 'match.calibration_json'), shortlist_json: requiredString(value.shortlist_json, 'match.shortlist_json'), why_job_id: typeof value.why_job_id === 'string' ? value.why_job_id : null }; }
  async buildWritingTemplates(input: { segment_id?: string; stage_class: string; rungs: number[]; rules: string[]; channel: string; max_chars: number; voice_sample_ids?: string[] }, key = newOperationKey()): Promise<WritingJob> { const value = object(await this.mutate('/templates/build', 'POST', input, { 'Idempotency-Key': key })); if (!value) throw new Error('Invalid response: template builder job is malformed'); return { job_id: requiredString(value.job_id, 'job_id'), role: requiredString(value.role, 'role') }; }
  async composeWritingMaterial(opportunityId: string, input: { recipient_id: string; kind: string; template_version_id: string; exchange_version: number; slot_fills: Array<{ slot: string; value: string; resource_id?: string }>; selected_match_ids?: string[] }, key = newOperationKey()): Promise<WritingJob> { const value = object(await this.mutate(`/opportunities/${encodeURIComponent(opportunityId)}/materials/compose`, 'POST', input, { 'Idempotency-Key': key })); if (!value) throw new Error('Invalid response: Composer job is malformed'); return { job_id: requiredString(value.job_id, 'job_id'), role: requiredString(value.role, 'role') }; }
  async saveWritingDraft(opportunityId: string, jobId: string, body: string, key = newOperationKey()): Promise<Record<string, unknown>> { const value = object(await this.mutate(`/opportunities/${encodeURIComponent(opportunityId)}/materials/compose/${encodeURIComponent(jobId)}/save`, 'POST', { body }, { 'Idempotency-Key': key })); if (!value) throw new Error('Invalid response: saved draft is malformed'); return value; }
  async lintWritingMaterial(id: string, version: number, content_sha256: string, key = newOperationKey()): Promise<Record<string, unknown>> { const value = object(await this.mutate(`/materials/${encodeURIComponent(id)}/lint`, 'POST', { version, content_sha256 }, { 'Idempotency-Key': key })); if (!value) throw new Error('Invalid response: lint result is malformed'); return value; }
  async listVoiceSamples(): Promise<Array<{ id: string; text: string; channel: string | null; revision: number }>> { const value = object(await this.request('/voice-samples')); if (!value || !Array.isArray(value.items)) throw new Error('Invalid response: voice samples are malformed'); return value.items.map((entry) => { const item = object(entry); if (!item) throw new Error('Invalid response: voice sample is malformed'); return { id: requiredString(item.id, 'voice_sample.id'), text: requiredString(item.text, 'voice_sample.text'), channel: typeof item.channel === 'string' ? item.channel : null, revision: Number(item.revision) }; }); }
  async createVoiceSample(text: string, channel?: string, key = newOperationKey()): Promise<void> { await this.mutate('/voice-samples', 'POST', { text, ...(channel ? { channel } : {}) }, { 'Idempotency-Key': key }); }
  async retireVoiceSample(id: string, key = newOperationKey()): Promise<void> { await this.mutate(`/voice-samples/${encodeURIComponent(id)}/retire`, 'POST', {}, { 'Idempotency-Key': key }); }

  async getBoard(): Promise<BoardSnapshot> {
    return decodeBoardSnapshot(await this.request('/board'));
  }

  async setOpportunityTags(
    opportunityId: string,
    input: { expected_revision: number; tags: string[] },
    key = newOperationKey(),
  ): Promise<{ id: string; title: string; state: string; revision: number; tags: string[] }> {
    const value = object(await this.mutate(`/opportunities/${encodeURIComponent(opportunityId)}/tags`, 'PUT', input, { 'Idempotency-Key': key }));
    if (!value || !Number.isSafeInteger(value.revision) || !Array.isArray(value.tags)) {
      throw new Error('Invalid response: updated opportunity tags are malformed');
    }
    const tags = value.tags.flatMap((entry) => typeof entry === 'string' ? [entry] : []);
    return {
      id: requiredString(value.id, 'opportunity.id'),
      title: requiredString(value.title, 'opportunity.title'),
      state: requiredString(value.state, 'opportunity.state'),
      revision: Number(value.revision),
      tags,
    };
  }

  async getDailyQueue(filters: { q?: string; party_id?: string; tag?: string; status?: string; due_before?: string; missing_material?: boolean; list_id?: string } = {}): Promise<DailyQueueSnapshot> {
    const query = new URLSearchParams({ view: 'daily' });
    for (const [key, value] of Object.entries(filters)) if (typeof value === 'string' && value.length) query.set(key, value);
    if (filters.missing_material) query.set('missing_material', 'true');
    return decodeDailyQueue(await this.request(`/queue?${query.toString()}`));
  }

  async getInsights(): Promise<InsightsReport> {
    return decodeInsights(await this.request('/insights'));
  }

  async getMaintenanceSettings(): Promise<MaintenanceSettings> {
    const value = object(await this.request('/settings/maintenance'));
    if (!value) throw new Error('Invalid response: maintenance settings are malformed');
    return { backup_directory: requiredString(value.backup_directory, 'backup_directory'), interval_hours: Number(value.interval_hours), retention_count: Number(value.retention_count), revision: Number(value.revision), last_successful_at: typeof value.last_successful_at === 'string' ? value.last_successful_at : null };
  }

  async updateMaintenanceSettings(input: Pick<MaintenanceSettings, 'backup_directory' | 'interval_hours' | 'retention_count'> & { expected_revision: number }, key = newOperationKey()): Promise<MaintenanceSettings> {
    const value = object(await this.mutate('/settings/maintenance', 'PUT', input, { 'Idempotency-Key': key }));
    if (!value) throw new Error('Invalid response: maintenance settings are malformed');
    return { backup_directory: requiredString(value.backup_directory, 'backup_directory'), interval_hours: Number(value.interval_hours), retention_count: Number(value.retention_count), revision: Number(value.revision), last_successful_at: typeof value.last_successful_at === 'string' ? value.last_successful_at : null };
  }

  async listBackups(): Promise<{ items: BackupRecord[]; last_successful_at: string | null }> {
    const value = object(await this.request('/backups'));
    if (!value || !Array.isArray(value.items)) throw new Error('Invalid response: backups are malformed');
    return { items: value.items as BackupRecord[], last_successful_at: typeof value.last_successful_at === 'string' ? value.last_successful_at : null };
  }

  async createBackup(key = newOperationKey()): Promise<BackupRecord> {
    return await this.mutate('/backups', 'POST', {}, { 'Idempotency-Key': key }) as BackupRecord;
  }

  async createFullExport(key = newOperationKey()): Promise<Blob> {
    return this.requestBlob('/exports', { method: 'POST', headers: { 'Idempotency-Key': key } });
  }

  async previewRestore(backup_id: string): Promise<RestorePreview> {
    return await this.mutate('/restore/preview', 'POST', { backup_id }, {}) as RestorePreview;
  }

  async previewFullImport(archive: Blob): Promise<RestorePreview> {
    return await this.request('/imports/preview', { method: 'POST', body: archive, headers: { 'Content-Type': 'application/zip' } }) as RestorePreview;
  }

  async commitFullImport(preview: RestorePreview, key = newOperationKey()): Promise<Record<string, unknown>> {
    return object(await this.mutate('/imports/commit', 'POST', { preview_id: preview.id, preview_digest: preview.digest, target_must_be_empty: true }, { 'Idempotency-Key': key })) ?? {};
  }

  async commitRestore(preview: RestorePreview, acknowledge_erasure_resurrection: boolean, key = newOperationKey()): Promise<Record<string, unknown>> {
    return object(await this.mutate('/restore/commit', 'POST', { preview_id: preview.id, preview_digest: preview.digest, acknowledge_erasure_resurrection }, { 'Idempotency-Key': key })) ?? {};
  }

  async previewPartyErasure(id: string): Promise<ErasePreview> {
    return await this.mutate(`/parties/${encodeURIComponent(id)}/erase/preview`, 'POST', {}, {}) as ErasePreview;
  }

  async eraseParty(id: string, preview: ErasePreview, key = newOperationKey()): Promise<MaintenanceOperation> {
    return await this.mutate(`/parties/${encodeURIComponent(id)}/erase`, 'POST', { preview_id: preview.id, preview_digest: preview.digest }, { 'Idempotency-Key': key }) as MaintenanceOperation;
  }

  async getMaintenanceOperation(id: string): Promise<MaintenanceOperation> {
    return await this.request(`/maintenance/operations/${encodeURIComponent(id)}`) as MaintenanceOperation;
  }

  async runCoach(input: { period_start?: string; period_end?: string } = {}, key = newOperationKey()): Promise<{ job_id: string }> {
    const result = object(await this.mutate('/coach/run', 'POST', input, { 'Idempotency-Key': key }));
    if (!result) throw new Error('Invalid response: Coach run receipt is malformed');
    return { job_id: requiredString(result.job_id, 'coach.job_id') };
  }

  async getPackSettings(): Promise<PackSettings> {
    const value = object(await this.request('/settings'));
    if (!value || !Number.isSafeInteger(value.revision) || !Array.isArray(value.available_packs)) throw new Error('Invalid response: pack settings are malformed');
    const available_packs = value.available_packs.map((raw) => { const pack = object(raw); if (!pack) throw new Error('Invalid response: available pack is malformed'); return { id: requiredString(pack.id, 'pack.id'), name: requiredString(pack.name, 'pack.name'), version: requiredString(pack.version, 'pack.version'), document: requiredString(pack.document, 'pack.document') }; });
    return { revision: Number(value.revision), active_pack_id: requiredString(value.active_pack_id, 'settings.active_pack_id'), active_pack_version: requiredString(value.active_pack_version, 'settings.active_pack_version'), active_document: requiredString(value.active_document, 'settings.active_document'), available_packs };
  }

  async getAdminOverview(): Promise<AdminOverview> {
    return decodeAdminOverview(await this.request('/admin/overview'));
  }

  async createAdminBackup(key = newOperationKey()): Promise<AdminBackup> {
    return decodeAdminBackup(await this.mutate('/admin/backups', 'POST', {}, { 'Idempotency-Key': key }));
  }

  async createAdminPreview(input: { operation: AdminOperation; party_id?: string; backup_id?: string }, key = newOperationKey()): Promise<AdminPreview> {
    return decodeAdminPreview(await this.mutate('/admin/previews', 'POST', input, { 'Idempotency-Key': key }));
  }

  async commitAdminPreview(input: { operation: AdminOperation; preview_id: string; digest: string; expected_workspace_epoch: string; expected_event_cursor: string; confirmation: string }, key = newOperationKey()): Promise<{ job_id: string }> {
    await this.refreshSession();
    if (this.session?.epoch !== input.expected_workspace_epoch) throw new Error('The workspace epoch changed after preview. Review a fresh impact summary before continuing.');
    const { operation, ...command } = input;
    const result = object(await this.mutate(`/admin/previews/${encodeURIComponent(input.preview_id)}/commit`, 'POST', command, { 'Idempotency-Key': key }));
    if (!result) throw new Error('Invalid response: administrative commit receipt is malformed');
    if (operation === 'restore' || operation === 'erase_workspace') await this.refreshSession();
    return { job_id: requiredString(result.job_id, 'admin_job_id') };
  }

  async getAdminJob(id: string): Promise<AdminJob> {
    return decodeAdminJob(await this.request(`/admin/jobs/${encodeURIComponent(id)}`));
  }

  async validatePack(document: string): Promise<PackValidation> {
    const value = object(await this.mutate('/packs/validate', 'POST', { document }, { 'Idempotency-Key': newOperationKey() }));
    if (!value || typeof value.valid !== 'boolean' || !Array.isArray(value.errors)) throw new Error('Invalid response: pack validation result is malformed');
    return { valid: value.valid, pack_id: typeof value.pack_id === 'string' ? value.pack_id : null, version: typeof value.version === 'string' ? value.version : null, normalized_document: typeof value.normalized_document === 'string' ? value.normalized_document : null, errors: value.errors.map((raw) => { const item = object(raw); if (!item) throw new Error('Invalid response: pack validation error is malformed'); return { line: Number.isSafeInteger(item.line) ? Number(item.line) : null, field: typeof item.field === 'string' ? item.field : null, message: requiredString(item.message, 'pack_error.message') }; }) };
  }

  async activatePack(input: { pack_id: string; version: string; document: string; expected_settings_revision: number; stage_mapping: Record<string, string> }, key = newOperationKey()): Promise<PackSettings> {
    await this.mutate('/packs/activate', 'POST', input, { 'Idempotency-Key': key });
    return this.getPackSettings();
  }

  async decideCoachQuestion(id: string, key = newOperationKey()): Promise<void> {
    await this.mutate(`/questions/${encodeURIComponent(id)}/dismiss`, 'POST', {}, { 'Idempotency-Key': key });
  }

  async createBoardList(input: components['schemas']['CreateBoardList'], key = newOperationKey()): Promise<components['schemas']['CreatedBoardList']> {
    const result = object(await this.mutate('/board/lists', 'POST', input, { 'Idempotency-Key': key }));
    if (!result || !Number.isSafeInteger(result.board_revision)) throw new Error('Invalid response: created board list is malformed');
    return { list: decodeBoardList(result.list), board_revision: Number(result.board_revision) };
  }

  async reorderBoardLists(input: components['schemas']['ReorderBoardLists'], key = newOperationKey()): Promise<BoardList[]> {
    const result = object(await this.mutate('/board/lists/reorder', 'POST', input, { 'Idempotency-Key': key }));
    if (!result || !Array.isArray(result.lists)) throw new Error('Invalid response: reordered board lists are malformed');
    return result.lists.map(decodeBoardList);
  }

  async renameBoardList(id: string, input: components['schemas']['RenameBoardList'], key = newOperationKey()): Promise<BoardList> {
    return decodeWrappedBoardList(await this.mutate(`/board/lists/${encodeURIComponent(id)}`, 'PATCH', input, { 'Idempotency-Key': key })).list;
  }

  async archiveBoardList(id: string, input: components['schemas']['ArchiveBoardList'], key = newOperationKey()): Promise<components['schemas']['ArchivedBoardList']> {
    return decodeArchivedBoardList(await this.mutate(`/board/lists/${encodeURIComponent(id)}/archive`, 'POST', input, { 'Idempotency-Key': key }));
  }

  async moveOpportunityPlacement(id: string, input: MovePlacementInput, key = newOperationKey()): Promise<components['schemas']['OpportunityPlacement']> {
    return decodeMovedPlacement(await this.mutate(`/opportunities/${encodeURIComponent(id)}/placement`, 'POST', input, { 'Idempotency-Key': key }));
  }

  async getOpportunityNotes(id: string): Promise<OpportunityNotes> {
    return decodeOpportunityNotes(await this.request(`/opportunities/${encodeURIComponent(id)}/notes`));
  }

  async updateOpportunityNotes(id: string, markdown: string, expected_revision: number, key = newOperationKey()): Promise<OpportunityNotes> {
    return decodeOpportunityNotes(await this.mutate(`/opportunities/${encodeURIComponent(id)}/notes`, 'PATCH', { markdown, expected_revision }, { 'Idempotency-Key': key }));
  }

  async toggleOpportunityTask(id: string, input: components['schemas']['ToggleOpportunityTask'], key = newOperationKey()): Promise<OpportunityNotes> {
    return decodeOpportunityNotes(await this.mutate(`/opportunities/${encodeURIComponent(id)}/notes/tasks/toggle`, 'POST', input, { 'Idempotency-Key': key }));
  }

  async createAgentChat(input: { role: AgentChat['role']; scope_kind: AgentChat['scope_kind']; scope_id?: string; asset_kind?: AgentChat['asset_kind'] }, key = newOperationKey()): Promise<AgentChat> {
    return decodeAgentChat(await this.mutate('/agent/chats', 'POST', input, { 'Idempotency-Key': key }));
  }

  async listAgentChats(filter: { role?: AgentChat['role']; scope_kind?: AgentChat['scope_kind']; scope_id?: string } = {}): Promise<AgentChatSummary[]> {
    const query = new URLSearchParams(Object.entries(filter).filter((entry): entry is [string, string] => typeof entry[1] === 'string'));
    const value = await this.request(`/agent/chats${query.size ? `?${query.toString()}` : ''}`);
    if (!Array.isArray(value)) throw new Error('Invalid response: agent chat history is malformed');
    return value.map((raw) => { const item = object(raw); if (!item || !Number.isSafeInteger(item.revision)) throw new Error('Invalid response: agent chat summary is malformed'); return { id: requiredString(item.id, 'chat.id'), role: requiredString(item.role, 'chat.role') as AgentChat['role'], scope_kind: requiredString(item.scope_kind, 'chat.scope_kind') as AgentChat['scope_kind'], scope_id: typeof item.scope_id === 'string' ? item.scope_id : null, asset_kind: typeof item.asset_kind === 'string' ? item.asset_kind as AgentChat['asset_kind'] : null, revision: Number(item.revision), last_turn: typeof item.last_turn === 'string' ? item.last_turn : null, updated_at: requiredString(item.updated_at, 'chat.updated_at'), active_job_id: typeof item.active_job_id === 'string' ? item.active_job_id : null }; });
  }

  async getAgentChat(id: string): Promise<AgentChat> { return decodeAgentChat(await this.request(`/agent/chats/${encodeURIComponent(id)}`)); }

  async addAgentTurn(id: string, text: string, key = newOperationKey()): Promise<{ chat_id: string; turn_id: string; job_id: string; revision: number }> {
    const result = object(await this.mutate(`/agent/chats/${encodeURIComponent(id)}/turns`, 'POST', { text }, { 'Idempotency-Key': key }));
    if (!result || !Number.isSafeInteger(result.revision)) throw new Error('Invalid response: Interviewer turn result is malformed');
    return { chat_id: requiredString(result.chat_id, 'chat_id'), turn_id: requiredString(result.turn_id, 'turn_id'), job_id: requiredString(result.job_id, 'job_id'), revision: Number(result.revision) };
  }

  async setInterviewerQuestion(id: string, question_id: string, action: 'ask' | 'skip' | 'defer' | 'unknown', key = newOperationKey()): Promise<void> {
    await this.mutate(`/agent/chats/${encodeURIComponent(id)}/questions`, 'POST', { question_id, action }, { 'Idempotency-Key': key });
  }

  async confirmInterviewerCandidate(id: string, input: { title: string; fields: Record<string, unknown>; expected_chat_revision: number }, key = newOperationKey()): Promise<void> {
    await this.mutate(`/agent/candidates/${encodeURIComponent(id)}/confirm`, 'POST', input, { 'Idempotency-Key': key });
  }

  async dismissInterviewerCandidate(id: string, key = newOperationKey()): Promise<void> {
    await this.mutate(`/agent/candidates/${encodeURIComponent(id)}/dismiss`, 'POST', {}, { 'Idempotency-Key': key });
  }

  async getRoleJob(id: string): Promise<RoleJob> {
    const result = object(await this.request(`/jobs/${encodeURIComponent(id)}`));
    if (!result || typeof result.status !== 'string') throw new Error('Invalid response: role job is malformed');
    return { id: requiredString(result.id, 'job.id'), role: requiredString(result.role, 'job.role'), status: result.status, result: object(result.result), last_error: typeof result.last_error === 'string' ? result.last_error : null, scope_kind: typeof result.scope_kind === 'string' ? result.scope_kind : undefined, scope_id: typeof result.scope_id === 'string' ? result.scope_id : undefined };
  }

  async getBriefItems(scopeKind: 'party' | 'segment', scopeId: string): Promise<BriefItem[]> {
    const value = await this.request(`/briefs/${scopeKind}/${encodeURIComponent(scopeId)}/items`);
    if (!Array.isArray(value)) throw new Error('Invalid response: brief items must be a list');
    return value.map(decodeBriefItem);
  }

  async generateBrief(scopeKind: 'party' | 'segment', scopeId: string, key = newOperationKey()): Promise<{ job_id: string }> {
    const result = object(await this.mutate(`/briefs/${scopeKind}/${encodeURIComponent(scopeId)}/items`, 'POST', {}, { 'Idempotency-Key': key }));
    if (!result) throw new Error('Invalid response: brief job result is malformed');
    return { job_id: requiredString(result.job_id, 'job_id') };
  }

  async decideBriefItem(id: string, input: { decision: 'confirm' | 'correct' | 'dismiss'; corrected_text?: string; expected_revision: number }, key = newOperationKey()): Promise<void> {
    await this.mutate(`/brief-items/${encodeURIComponent(id)}/decision`, 'POST', input, { 'Idempotency-Key': key });
  }

  async listFluency(): Promise<VocabularyFluency[]> {
    const value = await this.request('/fluency');
    if (!Array.isArray(value)) throw new Error('Invalid response: vocabulary fluency list is malformed');
    return value.map((raw) => { const item = object(raw); if (!item || !['unknown', 'learning', 'fluent'].includes(String(item.status)) || !Number.isSafeInteger(item.revision)) throw new Error('Invalid response: vocabulary fluency item is malformed'); return { term: requiredString(item.term, 'fluency.term'), status: item.status as VocabularyFluency['status'], revision: Number(item.revision) }; });
  }

  async updateFluency(term: string, status: VocabularyFluency['status'], expected_revision: number, key = newOperationKey()): Promise<VocabularyFluency> {
    const value = object(await this.mutate(`/fluency/${encodeURIComponent(term)}`, 'PUT', { status, expected_revision }, { 'Idempotency-Key': key }));
    if (!value || !Number.isSafeInteger(value.revision)) throw new Error('Invalid response: vocabulary fluency update is malformed');
    return { term: requiredString(value.term, 'fluency.term'), status: requiredString(value.status, 'fluency.status') as VocabularyFluency['status'], revision: Number(value.revision) };
  }

  async listSegments(): Promise<Array<{ id: string; name: string; description: string | null; revision: number }>> {
    const value = await this.request('/segments');
    if (!Array.isArray(value)) throw new Error('Invalid response: segments must be a list');
    return value.map((item) => { const segment = object(item); if (!segment) throw new Error('Invalid response: segment is malformed'); return { id: requiredString(segment.id, 'segment.id'), name: requiredString(segment.name, 'segment.name'), description: typeof segment.description === 'string' ? segment.description : null, revision: Number(segment.revision) }; });
  }

  async createSegment(name: string, description?: string, key = newOperationKey()): Promise<{ id: string; name: string; description: string | null; revision: number }> {
    const value = object(await this.mutate('/segments', 'POST', { name, ...(description === undefined ? {} : { description }) }, { 'Idempotency-Key': key }));
    if (!value) throw new Error('Invalid response: created segment is malformed');
    return { id: requiredString(value.id, 'segment.id'), name: requiredString(value.name, 'segment.name'), description: typeof value.description === 'string' ? value.description : null, revision: Number(value.revision) };
  }

  async updateSegmentParty(segmentId: string, partyId: string, action: 'add' | 'remove', key = newOperationKey()): Promise<void> {
    await this.mutate(`/segments/${encodeURIComponent(segmentId)}/parties`, 'POST', { party_id: partyId, action }, { 'Idempotency-Key': key });
  }

  async getAgentPrompt(role: 'interviewer' | 'briefer'): Promise<{ role: string; version_id: string; version: number; content: string; sha256: string }> {
    const value = object(await this.request(`/agent/prompts/${role}`)); if (!value) throw new Error('Invalid response: prompt is malformed');
    return { role: requiredString(value.role, 'prompt.role'), version_id: requiredString(value.version_id, 'prompt.version_id'), version: Number(value.version), content: requiredString(value.content, 'prompt.content'), sha256: requiredString(value.sha256, 'prompt.sha256') };
  }

  async updateAgentPrompt(role: 'interviewer' | 'briefer', expected_version_id: string, content: string, key = newOperationKey()): Promise<void> {
    await this.mutate(`/agent/prompts/${role}`, 'PUT', { expected_version_id, content }, { 'Idempotency-Key': key });
  }

  async getQuestionBank(): Promise<{ version_id: string; content: Record<string, unknown> }> {
    const value = object(await this.request('/agent/question-bank')); if (!value || !object(value.content)) throw new Error('Invalid response: question bank is malformed');
    return { version_id: requiredString(value.version_id, 'question_bank.version_id'), content: value.content as Record<string, unknown> };
  }

  async updateQuestionBank(expected_version_id: string, content: Record<string, unknown>, key = newOperationKey()): Promise<void> {
    await this.mutate('/agent/question-bank', 'PUT', { expected_version_id, content }, { 'Idempotency-Key': key });
  }

  async releaseMaterial(id: string): Promise<ReleasedMaterial> {
    return decodeReleasedMaterial(await this.request(`/materials/${encodeURIComponent(id)}/release`, { method: 'POST', body: '{}' }));
  }

  async getMaterial(id: string): Promise<Material> {
    return decodeMaterial(await this.request(`/materials/${encodeURIComponent(id)}`));
  }

  async approveMaterial(id: string, override_reasons: Record<string, string>): Promise<Material> {
    const approval = object(await this.request(`/materials/${encodeURIComponent(id)}/approve`, {
      method: 'POST', body: JSON.stringify({ override_reasons }),
    }));
    if (!approval) throw new Error('Invalid response: approval receipt is malformed');
    const material = await this.getMaterial(id);
    return { ...material, gate: decodeGateReport(approval.gate) };
  }

  async evaluateMaterialGate(id: string, override_reasons: Record<string, string> = {}): Promise<GateReport> {
    return decodeGateReport(await this.request(`/materials/${encodeURIComponent(id)}/gate`, {
      method: 'POST', body: JSON.stringify({ override_reasons }),
    }));
  }

  async acknowledgeRelease(id: string, succeeded: boolean): Promise<void> {
    await this.request(`/releases/${encodeURIComponent(id)}/ack`, { method: 'POST', body: JSON.stringify({ succeeded }) });
  }

  async listThreads(filter: { opportunity_id?: string; recipient_id?: string } = {}): Promise<ContactThread[]> {
    const query = new URLSearchParams(Object.entries(filter).filter((entry): entry is [string, string] => typeof entry[1] === 'string'));
    const value = await this.request(`/threads${query.size ? `?${query.toString()}` : ''}`);
    if (!Array.isArray(value)) throw new Error('Invalid response: contact threads must be a list');
    return value.map((raw) => {
      const thread = object(raw);
      if (!thread || !['waiting', 'conversing', 'timed_out', 'closed_declined', 'resolved', 'dormant'].includes(String(thread.state))
        || !Number.isSafeInteger(thread.current_touch) || Number(thread.current_touch) < 0) throw new Error('Invalid response: contact thread is malformed');
      return {
        id: requiredString(thread.id, 'thread.id'), opportunity_id: requiredString(thread.opportunity_id, 'thread.opportunity_id'),
        recipient_id: requiredString(thread.recipient_id, 'thread.recipient_id'), state: thread.state as ContactThread['state'],
        current_touch: Number(thread.current_touch), expires_at: typeof thread.expires_at === 'string' ? thread.expires_at : null,
        next_action_at: typeof thread.next_action_at === 'string' ? thread.next_action_at : null,
        sent_at: typeof thread.sent_at === 'string' ? thread.sent_at : null, updated_at: requiredString(thread.updated_at, 'thread.updated_at'),
        sends: this.decodeSendHistory(thread.sends),
      };
    });
  }

  async sendOnThread(threadId: string, input: components['schemas']['SendInput'], key: string): Promise<SendRecord> {
    const value = object(await this.mutate(`/threads/${encodeURIComponent(threadId)}/send`, 'POST', input, { 'Idempotency-Key': key }));
    if (!value || !Number.isSafeInteger(value.touch_no) || !Number.isSafeInteger(value.primary_version_no) || typeof value.capacity_charged !== 'boolean' || !/^[a-f0-9]{64}$/.test(String(value.content_sha256))) throw new Error('Invalid response: sent touch receipt is malformed');
    return { id: requiredString(value.id, 'send.id'), thread_id: requiredString(value.thread_id, 'send.thread_id'), touch_no: Number(value.touch_no), primary_material_id: requiredString(value.primary_material_id, 'send.primary_material_id'), primary_version_no: Number(value.primary_version_no), recipient_id: requiredString(value.recipient_id, 'send.recipient_id'), content_sha256: requiredString(value.content_sha256, 'send.content_sha256'), sent_at: requiredString(value.sent_at, 'send.sent_at'), response_deadline: requiredString(value.response_deadline, 'send.response_deadline'), deadline_zone: requiredString(value.deadline_zone, 'send.deadline_zone'), next_action_at: typeof value.next_action_at === 'string' ? value.next_action_at : null, capacity_charged: value.capacity_charged };
  }

  async reopenThread(id: string): Promise<ContactThread> {
    const value = object(await this.mutate(`/threads/${encodeURIComponent(id)}/reopen`, 'POST', {}, { 'Idempotency-Key': newOperationKey() }));
    if (!value || !['waiting', 'conversing', 'timed_out', 'closed_declined', 'resolved', 'dormant'].includes(String(value.state))) throw new Error('Invalid response: reopened thread is malformed');
    return (await this.decodeSingleThread(value));
  }

  async recordOutcome(sendId: string, input: components['schemas']['OutcomeInput'], key = newOperationKey()): Promise<ContactOutcome> {
    const value = object(await this.mutate(`/sends/${encodeURIComponent(sendId)}/outcome`, 'POST', input, { 'Idempotency-Key': key }));
    if (!value) throw new Error('Invalid response: outcome receipt is malformed');
    const outcome = this.decodeOutcome(value);
    if (!outcome) throw new Error('Invalid response: outcome receipt is malformed');
    return outcome;
  }

  async clearPartyDnc(partyId: string, input: components['schemas']['DncClearanceInput'], key = newOperationKey()): Promise<PartyPolicyRecord> {
    const value = object(await this.mutate(`/entities/${encodeURIComponent(partyId)}/clear-dnc`, 'POST', input, { 'Idempotency-Key': key }));
    if (!value || typeof value.do_not_contact !== 'boolean') throw new Error('Invalid response: party policy receipt is malformed');
    return { party_id: requiredString(value.party_id, 'policy.party_id'), state: requiredString(value.state, 'policy.state') as PartyPolicyRecord['state'], do_not_contact: value.do_not_contact, highest_rung: Number.isSafeInteger(value.highest_rung) ? Number(value.highest_rung) : 0, touch_count: Number.isSafeInteger(value.touch_count) ? Number(value.touch_count) : 0, revision: Number.isSafeInteger(value.revision) ? Number(value.revision) : 1 };
  }

  async recordResolution(opportunityId: string, input: components['schemas']['ResolutionInput'], key = newOperationKey()): Promise<ContactResolution> {
    const value = object(await this.mutate(`/opportunities/${encodeURIComponent(opportunityId)}/resolution`, 'POST', input, { 'Idempotency-Key': key }));
    if (!value) throw new Error('Invalid response: resolution receipt is malformed');
    return { id: requiredString(value.id, 'resolution.id'), opportunity_id: requiredString(value.opportunity_id, 'resolution.opportunity_id'), thread_id: typeof value.thread_id === 'string' ? value.thread_id : null, result: requiredString(value.result, 'resolution.result'), they_benefited: requiredString(value.they_benefited, 'resolution.they_benefited'), i_benefited: requiredString(value.i_benefited, 'resolution.i_benefited'), noted_at: requiredString(value.noted_at, 'resolution.noted_at') };
  }

  async listCapacity(): Promise<CapacityRecord[]> {
    const value = await this.request('/capacity');
    if (!Array.isArray(value)) throw new Error('Invalid response: capacity ledger must be a list');
    return value.map((raw) => { const row = object(raw); if (!row || !['total', 'committed', 'remaining'].every((field) => typeof row[field] === 'number' && Number.isFinite(row[field]))) throw new Error('Invalid response: capacity row is malformed'); return { period: requiredString(row.period, 'capacity.period'), unit: requiredString(row.unit, 'capacity.unit'), total: Number(row.total), committed: Number(row.committed), remaining: Number(row.remaining), updated_at: requiredString(row.updated_at, 'capacity.updated_at') }; });
  }

  async updateCapacity(input: components['schemas']['CapacityInput'], key = newOperationKey()): Promise<CapacityRecord> {
    const value = object(await this.mutate('/capacity', 'POST', input, { 'Idempotency-Key': key }));
    if (!value || typeof value.remaining !== 'number') throw new Error('Invalid response: capacity update receipt is malformed');
    return { period: requiredString(value.period, 'capacity.period'), unit: requiredString(value.unit, 'capacity.unit'), total: Number(value.total), committed: Number(value.committed), remaining: Number(value.remaining), updated_at: requiredString(value.updated_at, 'capacity.updated_at') };
  }

  async stageUpload(file: Blob, filename?: string): Promise<components['schemas']['Upload']> {
    const headers = new Headers({ Accept: 'application/json' });
    headers.set('Content-Type', file.type || 'application/octet-stream');
    if (filename) headers.set('x-parley-filename', encodeURIComponent(filename));
    if (this.session) {
      headers.set('X-Parley-Token', this.session.mutation_token);
      headers.set('X-Parley-Epoch', this.session.epoch);
    }
    const response = await fetch('/api/v1/uploads', { method: 'POST', body: file, headers, credentials: 'same-origin', cache: 'no-store' });
    const payload: unknown = await response.json().catch(() => null);
    if (!response.ok) throw decodeApiError(payload, response.status);
    const upload = object(payload);
    if (!upload || typeof upload.id !== 'string') throw new Error('Invalid response: upload receipt is malformed');
    return upload as components['schemas']['Upload'];
  }

  async getResourceBoard(options: { limit?: number; cursor?: string; includeOffboard?: boolean } = {}): Promise<ResourceBoardSnapshot> {
    const query = new URLSearchParams();
    if (options.limit !== undefined) query.set('limit', String(options.limit));
    if (options.cursor) query.set('cursor', options.cursor);
    if (options.includeOffboard) query.set('include_offboard', 'true');
    return decodeResourceBoardSnapshot(await this.request(`/resource-board?${query.toString()}`));
  }

  async retireResourceCard(id: string, key = newOperationKey()): Promise<{ entity: { id: string; kind: string; readiness: string; revision: number }; affected_versions: Array<{ material_id: string; version_no: number }> }> {
    const result = object(await this.mutate(`/resource-cards/${encodeURIComponent(id)}/retire`, 'POST', {}, { 'Idempotency-Key': key }));
    if (!result || !object(result.entity) || !Array.isArray(result.affected_versions)) throw new Error('Invalid response: retirement receipt is malformed');
    const entity = result.entity as { id: string; kind: string; readiness: string; revision: number };
    return { entity, affected_versions: result.affected_versions.flatMap((entry) => { const item = object(entry); if (!item || !Number.isSafeInteger(item.version_no)) return []; return [{ material_id: requiredString(item.material_id, 'affected.material_id'), version_no: Number(item.version_no) }]; }) };
  }

  async getEntityCardDeliveries(id: string): Promise<DeliveryRow[]> {
    const envelope = object(await this.request(`/resource-cards/${encodeURIComponent(id)}/deliveries`));
    if (!envelope || !Array.isArray(envelope.data)) throw new Error('Invalid response: delivery list is malformed');
    return envelope.data.map(decodeDeliveryRow);
  }

  async getEntityCard(id: string): Promise<EntityCard> {
    const envelope = object(await this.request(`/resource-cards/${encodeURIComponent(id)}`));
    if (!envelope || !object(envelope.data)) throw new Error('Invalid response: card read is malformed');
    return decodeEntityCard(envelope.data);
  }

  async commitResourceCard(input: components['schemas']['ResourceCardCommitInput'], key: string): Promise<ResourceCardCommitResult> {
    return decodeResourceCardCommitResult(await this.mutate('/resource-card-commits', 'POST', input, { 'Idempotency-Key': key }));
  }

  async moveResourceCard(input: components['schemas']['MoveResourceCardInput'], key = newOperationKey()): Promise<MoveResourceCardResult> {
    return decodeMoveResourceCardResult(await this.mutate('/resource-board/moves', 'POST', input, { 'Idempotency-Key': key }));
  }

  async createEdge(input: components['schemas']['EdgeInput'], key = newOperationKey()): Promise<{ edge: EdgeView }> {
    const result = object(await this.mutate('/edges', 'POST', input, { 'Idempotency-Key': key }));
    if (!result || !object(result.edge)) throw new Error('Invalid response: created edge is malformed');
    return { edge: decodeEdgeView(result.edge) };
  }

  async updateEdge(id: string, input: components['schemas']['UpdateEdgeInput'], key = newOperationKey()): Promise<{ edge: EdgeView; author_kind: string }> {
    const result = object(await this.mutate(`/edges/${encodeURIComponent(id)}`, 'PATCH', input, { 'Idempotency-Key': key }));
    if (!result || !object(result.edge) || typeof result.author_kind !== 'string') throw new Error('Invalid response: updated edge is malformed');
    return { edge: decodeEdgeView(result.edge), author_kind: result.author_kind };
  }

  async deleteEdge(id: string, key = newOperationKey()): Promise<void> {
    await this.mutate(`/edges/${encodeURIComponent(id)}`, 'DELETE', {}, { 'Idempotency-Key': key });
  }

  async listAttachments(entityId: string): Promise<EntityAttachment[]> {
    const value = object(await this.request(`/entities/${encodeURIComponent(entityId)}/attachments`));
    if (!value || !Array.isArray(value.data)) throw new Error('Invalid response: attachment list is malformed');
    return value.data.map(decodeEntityAttachment);
  }

  async bindAttachment(entityId: string, uploadId: string, key = newOperationKey()): Promise<EntityAttachment> {
    const result = object(await this.mutate(`/entities/${encodeURIComponent(entityId)}/attachments`, 'POST', { upload_id: uploadId }, { 'Idempotency-Key': key }));
    if (!result || !object(result.attachment)) throw new Error('Invalid response: bound attachment is malformed');
    return decodeEntityAttachment(result.attachment);
  }

  async unbindAttachment(entityId: string, reference: string, key = newOperationKey()): Promise<void> {
    await this.mutate(`/entities/${encodeURIComponent(entityId)}/attachments/${encodeURIComponent(reference)}`, 'DELETE', {}, { 'Idempotency-Key': key });
  }

  async getAttachmentContent(entityId: string, reference: string): Promise<{ bytes: ArrayBuffer; filename: string; contentType: string; size: number }> {
    const headers = new Headers({ Accept: 'application/octet-stream' });
    if (this.session) {
      headers.set('X-Parley-Token', this.session.mutation_token);
      headers.set('X-Parley-Epoch', this.session.epoch);
    }
    const response = await fetch(`/api/v1/entities/${encodeURIComponent(entityId)}/attachments/${encodeURIComponent(reference)}/content`, { headers, credentials: 'same-origin', cache: 'no-store' });
    if (!response.ok) {
      let payload: unknown = null;
      try { payload = await response.json(); } catch { /* non-JSON error body */ }
      throw decodeApiError(payload, response.status);
    }
    const disposition = response.headers.get('content-disposition') ?? '';
    const filenameMatch = /filename="([^"]+)"/.exec(disposition);
    const bytes = await response.arrayBuffer();
    return { bytes, filename: filenameMatch?.[1] ?? 'attachment.bin', contentType: response.headers.get('content-type') ?? 'application/octet-stream', size: bytes.byteLength };
  }

  private decodeSingleThread(raw: Record<string, unknown>): ContactThread {
    if (!Number.isSafeInteger(raw.current_touch)) throw new Error('Invalid response: contact thread is malformed');
    return { id: requiredString(raw.id, 'thread.id'), opportunity_id: requiredString(raw.opportunity_id, 'thread.opportunity_id'), recipient_id: requiredString(raw.recipient_id, 'thread.recipient_id'), state: requiredString(raw.state, 'thread.state') as ContactThread['state'], current_touch: Number(raw.current_touch), expires_at: typeof raw.expires_at === 'string' ? raw.expires_at : null, next_action_at: typeof raw.next_action_at === 'string' ? raw.next_action_at : null, sent_at: typeof raw.sent_at === 'string' ? raw.sent_at : null, updated_at: requiredString(raw.updated_at, 'thread.updated_at'), sends: this.decodeSendHistory(raw.sends) };
  }

  private decodeSendHistory(raw: unknown): ThreadSend[] {
    if (raw === undefined || raw === null) return [];
    if (!Array.isArray(raw)) throw new Error('Invalid response: thread send history is malformed');
    return raw.map((entry) => {
      const send = object(entry);
      if (!send || !Number.isSafeInteger(send.touch_no) || !Number.isSafeInteger(send.primary_version_no) || typeof send.capacity_charged !== 'boolean' || !/^[a-f0-9]{64}$/.test(String(send.content_sha256))) throw new Error('Invalid response: thread send history is malformed');
      return {
        id: requiredString(send.id, 'send.id'), touch_no: Number(send.touch_no), primary_material_id: requiredString(send.primary_material_id, 'send.primary_material_id'),
        primary_version_no: Number(send.primary_version_no), recipient_id: requiredString(send.recipient_id, 'send.recipient_id'), content_sha256: requiredString(send.content_sha256, 'send.content_sha256'),
        sent_at: requiredString(send.sent_at, 'send.sent_at'), response_deadline: requiredString(send.response_deadline, 'send.response_deadline'),
        deadline_zone: requiredString(send.deadline_zone, 'send.deadline_zone'), next_action_at: typeof send.next_action_at === 'string' ? send.next_action_at : null,
        capacity_charged: send.capacity_charged, outcome: this.decodeOutcome(object(send.outcome)),
      };
    });
  }

  private decodeOutcome(raw: Record<string, unknown> | null): ContactOutcome | null {
    if (!raw) return null;
    if (!raw || !['id', 'send_id', 'class', 'recorded_at'].every((field) => typeof raw[field] === 'string')) throw new Error('Invalid response: outcome history is malformed');
    return { id: requiredString(raw.id, 'outcome.id'), send_id: requiredString(raw.send_id, 'outcome.send_id'), class: requiredString(raw.class, 'outcome.class'), rung_accepted: Number.isSafeInteger(raw.rung_accepted) ? Number(raw.rung_accepted) : null, received_at: typeof raw.received_at === 'string' ? raw.received_at : null, recorded_at: requiredString(raw.recorded_at, 'outcome.recorded_at'), note: typeof raw.note === 'string' ? raw.note : null };
  }

  private async mutate(path: string, method: string, body: unknown, headers: Record<string, string>): Promise<unknown> {
    return this.request(path, {
      method,
      body: JSON.stringify(body),
      headers,
    });
  }

  private async request(path: string, init: RequestInit = {}, authenticated = true): Promise<unknown> {
    const headers = new Headers(init.headers);
    headers.set('Accept', 'application/json');
    headers.set('Content-Type', 'application/json');
    if (authenticated && this.session) {
      headers.set('X-Parley-Token', this.session.mutation_token);
      headers.set('X-Parley-Epoch', this.session.epoch);
    }
    let response: Response;
    try {
      response = await fetch(`/api/v1${path}`, {
        ...init,
        headers,
        credentials: 'same-origin',
        cache: 'no-store',
      });
    } catch (error) {
      throw error instanceof Error ? error : new TypeError('Connection failed');
    }
    let payload: unknown;
    try {
      payload = response.status === 204 ? null : await response.json();
    } catch {
      throw new ApiError('The local service returned an unreadable response.', response.status, 'INVALID_RESPONSE');
    }
    if (!response.ok) throw decodeApiError(payload, response.status);
    return payload;
  }

  async downloadMaterialBytes(materialId: string, releaseId: string): Promise<{ bytes: ArrayBuffer; contentType: string; filename: string; size: number }> {
    const headers = new Headers({ Accept: 'application/octet-stream' });
    if (this.session) {
      headers.set('X-Parley-Token', this.session.mutation_token);
      headers.set('X-Parley-Epoch', this.session.epoch);
    }
    const response = await fetch(`/api/v1/materials/${encodeURIComponent(materialId)}/download?release_id=${encodeURIComponent(releaseId)}`, {
      headers,
      credentials: 'same-origin',
      cache: 'no-store',
    });
    if (!response.ok) {
      let payload: unknown = null;
      try { payload = await response.json(); } catch { /* non-JSON error body */ }
      throw decodeApiError(payload, response.status);
    }
    const disposition = response.headers.get('content-disposition') ?? '';
    const filenameMatch = /filename="([^"]+)"/.exec(disposition);
    const bytes = await response.arrayBuffer();
    return {
      bytes,
      contentType: response.headers.get('content-type') ?? 'application/octet-stream',
      filename: filenameMatch?.[1] ?? 'download.bin',
      size: bytes.byteLength,
    };
  }

  private async requestBlob(path: string, init: RequestInit = {}): Promise<Blob> {
    const headers = new Headers(init.headers);
    headers.set('Accept', 'application/zip');
    if (this.session) {
      headers.set('X-Parley-Token', this.session.mutation_token);
      headers.set('X-Parley-Epoch', this.session.epoch);
    }
    const response = await fetch(`/api/v1${path}`, { ...init, headers, credentials: 'same-origin', cache: 'no-store' });
    if (!response.ok) throw new Error(`Export failed (${response.status}).`);
    return response.blob();
  }
}

export const parley = new ParleyClient();
