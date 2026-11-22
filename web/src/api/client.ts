import type { components } from './generated';

export type Resource = components['schemas']['Resource'];
export type GraphEntity = components['schemas']['GraphEntity'];
export type Entity = components['schemas']['Entity'];
export type SavedSource = components['schemas']['SavedSource'];
export type EntityInput = components['schemas']['EntityInput'];
export type CaptureInput = components['schemas']['CaptureInput'];
export type Opportunity = components['schemas']['Opportunity'];
export type Material = components['schemas']['MaterialRecord'];
export type ReleasedMaterial = components['schemas']['ReleasedMaterial'];
export type BoardList = components['schemas']['BoardList'];
export type BoardPlacement = { opportunity: Opportunity; opportunity_id: string; list_id: string; position: number; placement_revision: number; next_task: string | null };
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
    revision: Number(entity.revision),
  };
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

function decodeBoardPlacement(value: unknown): BoardPlacement {
  const item = object(value);
  const placement = item ? object(item.placement) : null;
  if (!item || !placement || !Number.isSafeInteger(placement.position) || !Number.isSafeInteger(placement.revision)
    || Number(placement.revision) < 1) {
    throw new Error('Invalid response: board placement is malformed');
  }
  return {
    opportunity: decodeOpportunity(item.opportunity),
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
    gate: item.gate as Material['gate'],
    attachment: decodeAttachment(item.attachment),
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

  async acknowledgeRelease(id: string, succeeded: boolean): Promise<void> {
    await this.request(`/releases/${encodeURIComponent(id)}/ack`, { method: 'POST', body: JSON.stringify({ succeeded }) });
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
}

export const parley = new ParleyClient();
