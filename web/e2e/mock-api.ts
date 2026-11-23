import { allowedAssertion } from '../src/features/parties/relations.ts';
import { parsePartyCsv, previewCsv, importDigest, maxImportRows, maxImportBytes } from '../mock/party-import.ts';
import type { PartyImportMapping, PartyImportPreview } from '../src/api/client';
import { createHash, randomBytes } from 'node:crypto';
import type { IncomingMessage, ServerResponse } from 'node:http';
import type { Plugin } from 'vite';
import { buildDailyQueue, buildInsights, PACK_DOCUMENTS, validatePackDocument } from '../mock/coach.ts';
import { adminImpact, adminOverview, adminPreview as encodeAdminPreview, type AdminBackupRecord, type AdminOperation } from '../mock/admin.ts';

/**
 * The one fixture-mode implementation of the Parley local API (RFC-0003
 * FE-13.2). Session/bootstrap, unified entities, captures and events behave
 * like the Rust service with a deterministic clock. The same plugin backs the
 * Playwright mock project and `pnpm --dir web run dev:mock`.
 */

type RecordData = Record<string, unknown>;

type ResourceRecord = {
  id: string;
  kind: string;
  title: string;
  body: RecordData;
  readiness: string;
  revision: number;
  etag: string;
  updated_at: string;
};

type EntityRecord = { id: string; kind: string; name: string; focus: string; revision: number };

type SavedSource = {
  content_id: string;
  variant_id: string;
  occurrence_id: string;
  normalized_sha256: string;
  raw_sha256: string;
  job_id?: string | null;
  job_status?: string;
};

type UploadRecord = {
  id: string;
  sha256: string;
  bytes: Buffer;
  filename: string;
  mime: string;
  share_permission: boolean;
  reviewed: boolean;
  revision: number;
  created_at: string;
};

type ReleaseRecord = {
  release_id: string;
  material_id: string;
  version: number;
  recipient_id: string;
  body: string;
  content_sha256: string;
  issued_at: string;
  attachment: RecordData | null;
  consumed: boolean;
  acknowledged: boolean | null;
  bytes: Buffer;
};

type EventRecord = {
  cursor: string;
  event_id: string;
  entity_type: string;
  entity_id: string;
  revision: number;
  event_type: string;
  occurred_at: string;
  payload: RecordData;
};

type AgentRunTarget = { kind: string; id: string };
type AgentRunRecord = {
  run_id: string;
  epoch: string;
  state: string;
  revision: number;
  deadline_at: string;
  targets: AgentRunTarget[];
  permissions: string[];
  turn_count: number;
  transcript_bytes: number;
  wake_condition: string | null;
};
type AgentTurnRecord = { turn_no: number; speaker: string; ordinal: number; text: string };

const randomHex = (bytes = 16) => randomBytes(bytes).toString('hex');

export interface MockApi {
  plugin: Plugin;
  /** Mint a fresh one-time bootstrap secret for a session redemption. */
  issueBootstrap(): string;
}

export function createMockApi(): MockApi {
  let epoch = 'ffeeddccbbaa99887766554433221100';
  const bootstraps = new Map<string, number>();
  const sessions = new Map<string, string>();
  const resources = new Map<string, ResourceRecord>();
  const directory = new Map<string, EntityRecord>();
  const notesByEntity = new Map<string, { markdown: string; revision: number }>();
  const partyImportOrigins = new Map<string, { batch_id: string; source_record: number }>();
  const importStages = new Map<string, { csv: ReturnType<typeof parsePartyCsv>; epoch: string; expires: number; review?: { mapping: PartyImportMapping; result: PartyImportPreview } }>();
  const entityAttachments = new Map<string, RecordData[]>();
  interface BoardColumn { id: string; title: string; position: number; revision: number }
  interface ResourcePlacementView { column_id: string; position: number; revision: number }
  interface ResourceEntityRecord { id: string; kind: string; name: string; focus: string; revision: number; subtype: string; body: RecordData; readiness: string; notes: { markdown: string; revision: number }; etag: string }
  const resourceColumns: BoardColumn[] = [
    { id: 'inbox', title: 'Inbox', position: 1024, revision: 1 },
    { id: 'developing', title: 'Developing', position: 2048, revision: 1 },
    { id: 'available', title: 'Available', position: 3072, revision: 1 },
  ];
  const resourcePlacements = new Map<string, ResourcePlacementView>();
  const entityResources = new Map<string, ResourceEntityRecord>();
  const sourceTextByOccurrence = new Map<string, { text: string; title: string | null; url: string | null }>();
  const graphEdges: RecordData[] = [];
  const graphPositions = new Map<string, { x: number; y: number; revision: number }>();
  const sourceGroups = new Map<string, { id: string; variants: Map<string, string> }>();
  const captures: SavedSource[] = [];
  const extractorJobs = new Map<string, RecordData>();
  const roleJobs = new Map<string, RecordData>();
  const agentChats = new Map<string, RecordData>();
  const briefItems = new Map<string, RecordData[]>();
  const segments = new Map<string, RecordData>();
  const segmentMembers = new Map<string, Set<string>>();
  const fluency = new Map<string, { term: string; status: 'unknown' | 'learning' | 'fluent'; revision: number }>();
  const writingInterests = new Map<string, RecordData[]>();
  const writingExchanges = new Map<string, RecordData[]>();
  const templateFamilies: Array<{ id: string; name: string }> = [];
  const writingTemplates: RecordData[] = [];
  const voiceSamples: RecordData[] = [];
  let embeddingModel = '';
  const questionBank = { version_id: 'question-bank-v1', content: { version: 1, questions: { proof: [{ id: 'result', text: 'What changed, and how did you measure the result?' }], gift: [{ id: 'offer', text: 'What useful thing could you offer?' }], artifact: [{ id: 'artifact', text: 'What have you made?' }] } } };
  const prompts = new Map<string, RecordData>([['interviewer', { role: 'interviewer', version_id: 'interviewer-v1', version: 1, content: 'Bundled Interviewer policy', sha256: 'a'.repeat(64) }], ['briefer', { role: 'briefer', version_id: 'briefer-v1', version: 1, content: 'Bundled Briefer policy', sha256: 'b'.repeat(64) }]]);
  const jobsByOccurrence = new Map<string, string>();
  const opportunities = new Map<string, RecordData>();
  const outputs = new Map<string, RecordData>();
  const uploads = new Map<string, UploadRecord>();
  const materialRecords = new Map<string, RecordData>();
  const releaseRecords = new Map<string, ReleaseRecord>();
  const threads = new Map<string, RecordData>();
  const sends = new Map<string, RecordData>();
  const outcomes = new Map<string, RecordData>();
  const resolutions = new Map<string, RecordData>();
  const capacityLedger = new Map<string, RecordData>();
  const coachQuestions = new Map<string, RecordData>();
  const validatedPacks = new Map<string, RecordData>();
  let activePackId = PACK_DOCUMENTS[0]!.id;
  let activePackVersion = PACK_DOCUMENTS[0]!.version;
  let activePackDocument = PACK_DOCUMENTS[0]!.document;
  let packSettingsRevision = 1;
  const partySuppression = new Map<string, boolean>();
  const boardLists: Array<{ id: string; title: string; position: number; archived: boolean; revision: number }> = [
    { id: 'list-default', title: 'Inbox', position: 0, archived: false, revision: 1 },
  ];
  const placements = new Map<string, { list_id: string; position: number; placement_revision: number }>();
  const notesByOpportunity = new Map<string, { markdown: string; revision: number }>();
  let provider = { endpoint: 'http://127.0.0.1:11434', model: '', num_ctx: 8192, num_predict: 1024, locale: 'en' };
  const receipts = new Map<string, { digest: string; result: RecordData; status?: number }>();
  // U02 authoritative run and turn rows survive snapshots; worker timers,
  // replay keys and pending leases are transient and are scrubbed on restore.
  const agentRuns = new Map<string, AgentRunRecord>();
  const agentTurns = new Map<string, AgentTurnRecord[]>();
  const agentStartKeys = new Map<string, { digest: string; run_id: string; body: RecordData }>();
  const agentCancelKeys = new Map<string, { digest: string; body: RecordData }>();
  const agentTimers = new Map<string, ReturnType<typeof setTimeout>[]>();
  const events: EventRecord[] = [];
  const adminBackups: AdminBackupRecord[] = [];
  const adminPreviews = new Map<string, { operation: AdminOperation; digest: string; epoch: string; cursor: string; expires: number; backupId: string | null; partyId: string | null }>();
  const adminJobs = new Map<string, RecordData>();
  let testClock = Date.UTC(2026, 0, 1);
  const now = () => new Date(testClock += 1000).toISOString();
  capacityLedger.set('quarter:project slots', { period: 'quarter', unit: 'project slots', total: 15, committed: 0, updated_at: now() });
  const tag = (resource: ResourceRecord) => `"p1.${epoch}.resource.${resource.id}.${resource.revision}"`;
  const entityTag = (entity: { id: string; revision: number }) => `"p1.${epoch}.entity.${entity.id}.${entity.revision}"`;
  const notesTag = (entityId: string, revision: number) => `"p1.${epoch}.notes.${entityId}.${revision}"`;
  const pushEvent = (event: Omit<EventRecord, 'cursor' | 'occurred_at'>) => {
    events.push({ ...event, occurred_at: now(), cursor: String(events.length + 1) });
  };

  function deepClone<T>(value: T): T {
    if (Buffer.isBuffer(value)) return Buffer.from(value) as T;
    if (value instanceof Map) return new Map([...value.entries()].map(([key, item]) => [deepClone(key), deepClone(item)])) as T;
    if (value instanceof Set) return new Set([...value].map(deepClone)) as T;
    if (Array.isArray(value)) return value.map(deepClone) as T;
    if (typeof value === 'object' && value !== null) return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, deepClone(item)])) as T;
    return value;
  }

  function workspaceCollections(): Record<string, unknown> {
    return { resources, directory, notesByEntity, partyImportOrigins, entityAttachments, sourceTextByOccurrence, graphEdges, graphPositions, sourceGroups, captures, extractorJobs, roleJobs, agentChats, briefItems, segments, segmentMembers, fluency, writingInterests, writingExchanges, templateFamilies, writingTemplates, voiceSamples, opportunities, outputs, uploads, materialRecords, releaseRecords, threads, sends, outcomes, resolutions, capacityLedger, coachQuestions, validatedPacks, boardLists, placements, notesByOpportunity, opportunityPhases, agentRuns, agentTurns, events };
  }

  function clearAgentTimers(runId?: string) {
    if (runId) {
      for (const timer of agentTimers.get(runId) ?? []) clearTimeout(timer);
      agentTimers.delete(runId);
      return;
    }
    for (const timers of agentTimers.values()) for (const timer of timers) clearTimeout(timer);
    agentTimers.clear();
  }

  function scheduleAgent(runId: string, step: () => void, delay: number) {
    const timer = setTimeout(() => {
      const remaining = (agentTimers.get(runId) ?? []).filter((item) => item !== timer);
      if (remaining.length) agentTimers.set(runId, remaining);
      else agentTimers.delete(runId);
      step();
    }, delay);
    timer.unref?.();
    agentTimers.set(runId, [...(agentTimers.get(runId) ?? []), timer]);
  }

  function takeWorkspaceSnapshot(): Record<string, unknown> {
    return { collections: deepClone(workspaceCollections()), owner: { provider: deepClone(provider), questionBank: deepClone(questionBank), prompts: deepClone(prompts), embeddingModel, activePackId, activePackVersion, activePackDocument, packSettingsRevision } };
  }

  function restoreWorkspaceSnapshot(snapshot: Record<string, unknown>) {
    const collections = snapshot.collections as Record<string, unknown>;
    for (const [name, current] of Object.entries(workspaceCollections())) {
      const saved = collections[name];
      if (current instanceof Map && saved instanceof Map) { current.clear(); for (const [key, value] of saved) current.set(deepClone(key), deepClone(value)); }
      else if (Array.isArray(current) && Array.isArray(saved)) current.splice(0, current.length, ...deepClone(saved));
    }
    clearAgentTimers();
    agentStartKeys.clear();
    agentCancelKeys.clear();
    const owner = snapshot.owner as RecordData;
    provider = deepClone(owner.provider as typeof provider);
    Object.assign(questionBank, deepClone(owner.questionBank as typeof questionBank));
    prompts.clear(); for (const [key, value] of owner.prompts as Map<string, RecordData>) prompts.set(key, deepClone(value));
    embeddingModel = String(owner.embeddingModel);
    activePackId = String(owner.activePackId); activePackVersion = String(owner.activePackVersion); activePackDocument = String(owner.activePackDocument); packSettingsRevision = Number(owner.packSettingsRevision);
  }

  function scrubPartyFromBackup(value: unknown, partyId: string, partyName: string): unknown {
    if (typeof value === 'string') return partyName ? value.split(partyName).join('[erased party]') : value;
    if (value instanceof Map) {
      const next = new Map<unknown, unknown>();
      for (const [key, item] of value) if (key !== partyId) next.set(key, scrubPartyFromBackup(item, partyId, partyName));
      return next;
    }
    if (value instanceof Set) return new Set([...value].filter((item) => item !== partyId).map((item) => scrubPartyFromBackup(item, partyId, partyName)));
    if (Array.isArray(value)) return value.filter((item) => {
      if (typeof item !== 'object' || item === null) return item !== partyId;
      const row = item as RecordData;
      return row.id !== partyId && row.party_id !== partyId && row.recipient_id !== partyId && row.scope_id !== partyId;
    }).map((item) => scrubPartyFromBackup(item, partyId, partyName));
    if (typeof value === 'object' && value !== null) {
      const row = value as RecordData;
      const next: RecordData = {};
      for (const [key, item] of Object.entries(row)) {
        if (key === 'participant_ids' && Array.isArray(item)) next[key] = item.filter((id) => id !== partyId);
        else if (['entity_id', 'from_entity_id', 'to_entity_id'].includes(key) && item === partyId) next[key] = '[erased]';
        else if (key === 'payload' && typeof item === 'object' && item !== null && (item as RecordData).party_id === partyId) next[key] = { erased: true };
        else next[key] = scrubPartyFromBackup(item, partyId, partyName);
      }
      return next;
    }
    return value;
  }

  function tasks(markdown: string) {
    const bytes = new TextEncoder().encode(markdown);
    const decoder = new TextDecoder();
    const text = decoder.decode(bytes);
    const lines = text.split('\n');
    const found: Array<{ byte_start: number; byte_end: number; line_sha256: string; checked: boolean; text: string }> = [];
    let byteOffset = 0;
    let fence: string | null = null;
    for (const line of lines) {
      const fenceMatch = /^ {0,3}(`{3,}|~{3,})/.exec(line);
      if (fenceMatch) {
        const marker = fenceMatch[1]![0]!;
        fence = fence === null ? marker : fence === marker ? null : fence;
      } else if (fence === null && !/^\s*>/.test(line) && !/^(?: {4}|\t)/.test(line)) {
        const match = /^(\s*(?:[-*+]|\d+[.)])\s+)\[([ xX])\]\s+(.*)$/.exec(line);
        if (match) {
          found.push({
            byte_start: byteOffset,
            byte_end: byteOffset + Buffer.byteLength(line, 'utf8'),
            line_sha256: createHash('sha256').update(line).digest('hex'),
            checked: match[2]!.toLowerCase() === 'x',
            text: match[3]!,
          });
        }
      }
      byteOffset += Buffer.byteLength(line, 'utf8') + 1;
    }
    return found;
  }

  function boardSnapshot() {
    return {
      lists: boardLists.slice().sort((a, b) => a.position - b.position || a.id.localeCompare(b.id)),
      cards: [...opportunities.values()].map((opportunity) => {
        const placement = placements.get(String(opportunity.id))!;
        const note = notesByOpportunity.get(String(opportunity.id)) ?? { markdown: '', revision: 1 };
        const nextTask = tasks(note.markdown).find((task) => !task.checked)?.text ?? null;
        return { opportunity, placement: { opportunity_id: opportunity.id, list_id: placement.list_id, position: placement.position, revision: placement.placement_revision, next_task: nextTask } };
      }).sort((a, b) => a.placement.list_id.localeCompare(b.placement.list_id) || a.placement.position - b.placement.position || String(a.opportunity.id).localeCompare(String(b.opportunity.id))),
    };
  }

  function notesRecord(id: string) {
    const notes = notesByOpportunity.get(id) ?? { markdown: '', revision: 1 };
    return { opportunity_id: id, markdown: notes.markdown, revision: notes.revision, next_task: tasks(notes.markdown).find((task) => !task.checked)?.text ?? null, tasks: tasks(notes.markdown) };
  }

  // Parley-Queue sheet: phases are canonical pack stages; next actions are
  // derived server-side, never guessed from a label.
  const PACK_STAGES: Array<{ stage_id: string; label: string; class: string; kind: string; ordinal: number }> = [
    { stage_id: 'exploring', label: 'Exploring', class: 'identified', kind: 'planning', ordinal: 10 },
    { stage_id: 'preparing', label: 'Preparing', class: 'prepared', kind: 'planning', ordinal: 20 },
    { stage_id: 'in_conversation', label: 'In conversation', class: 'responded', kind: 'planning', ordinal: 30 },
    { stage_id: 'closed', label: 'Closed', class: 'closed', kind: 'terminal', ordinal: 40 },
  ];
  const opportunityPhases = new Map<string, { stage_id: string | null; pack_id: string; pack_version: string; revision: number; event_id: string }>();

  function sheetRow(opportunity: RecordData) {
    const id = String(opportunity.id);
    const phaseState = opportunityPhases.get(id);
    const stage = phaseState?.stage_id ? PACK_STAGES.find((item) => item.stage_id === phaseState.stage_id) : null;
    const participants = (opportunity.participant_ids as string[]).map((partyId: string, index: number) => {
      const entity = directory.get(partyId);
      return { id: partyId, name: entity?.name ?? partyId, kind: entity?.kind ?? 'person', role: index === 0 ? 'primary' : 'participant' };
    });
    const lifecycle = String(opportunity.state);
    const waiting = [...threads.values()].filter((thread) => thread.opportunity_id === id && thread.state === 'waiting').sort((a, b) => String(a.expires_at ?? '').localeCompare(String(b.expires_at ?? '')) || String(a.id).localeCompare(String(b.id)));
    let nextAction;
    if (lifecycle !== 'open') {
      nextAction = { kind: 'terminal', label: 'No open action', reason: `Opportunity is ${lifecycle}.`, date: null, thread_id: null, recipient_id: null, recipient_name: null };
    } else if (waiting.length) {
      const first = waiting[0]!;
      const entity = directory.get(String(first.recipient_id));
      nextAction = {
        kind: 'wait', label: first.expires_at ? `Waiting until ${String(first.expires_at).slice(0, 10)}` : `Waiting on ${entity?.name ?? 'contact'}`,
        reason: `${entity?.name ?? 'Contact'}'s reply window is still open.`,
        date: first.expires_at ?? null, thread_id: first.id, recipient_id: first.recipient_id, recipient_name: entity?.name ?? null,
      };
    } else {
      nextAction = { kind: 'choose', label: 'Choose a next action', reason: 'No work or wait is known for this opportunity.', date: null, thread_id: null, recipient_id: null, recipient_name: null };
    }
    return {
      opportunity_id: id, title: opportunity.title, lifecycle, close_reason: opportunity.close_reason ?? null,
      phase: stage && phaseState ? { stage_id: stage.stage_id, label: stage.label, class: stage.class, kind: stage.kind, pack_id: phaseState.pack_id, pack_version: phaseState.pack_version, revision: phaseState.revision } : null,
      pack_id: activePackId, pack_version: activePackVersion,
      participants, next_action: nextAction, other_action_count: 0,
    };
  }

  function sheetResponse(search: string, phase: string | null, sort: string, cursor: number, limit: number) {
    const needle = search.trim().toLocaleLowerCase();
    const rows = [...opportunities.values()].filter((opportunity) => {
      if (phase === '__unmapped__') return !opportunityPhases.get(String(opportunity.id))?.stage_id;
      if (phase) return opportunityPhases.get(String(opportunity.id))?.stage_id === phase;
      return true;
    }).filter((opportunity) => {
      if (!needle) return true;
      const partyNames = (opportunity.participant_ids as string[]).map((partyId: string) => directory.get(partyId)?.name ?? partyId).join(' ').toLocaleLowerCase();
      return String(opportunity.title).toLocaleLowerCase().includes(needle) || partyNames.includes(needle);
    }).map(sheetRow);
    rows.sort((a, b) => sort === 'name' ? String(a.title).localeCompare(String(b.title)) || String(a.opportunity_id).localeCompare(String(b.opportunity_id)) : (a.next_action.date === null ? 1 : (b.next_action.date === null ? -1 : String(a.next_action.date).localeCompare(String(b.next_action.date)))) || String(a.title).localeCompare(String(b.title)));
    const total = rows.length;
    const page = rows.slice(cursor, cursor + limit);
    const phases = PACK_STAGES.map((stage) => ({ stage_id: stage.stage_id, label: stage.label, class: stage.class, kind: stage.kind, pack_id: activePackId, pack_version: activePackVersion, count: [...opportunities.values()].filter((opportunity) => opportunityPhases.get(String(opportunity.id))?.stage_id === stage.stage_id).length }));
    phases.push({ stage_id: '__unmapped__', label: 'Unmapped', class: 'unmapped', kind: 'planning', pack_id: activePackId, pack_version: activePackVersion, count: [...opportunities.values()].filter((opportunity) => !opportunityPhases.get(String(opportunity.id))?.stage_id).length });
    return { generation: String(events.length), total, rows: page, phases, pack: { id: activePackId, version: activePackVersion }, cursor: cursor ? String(cursor) : null, next_cursor: cursor + page.length < total ? String(cursor + page.length) : null };
  }

  // --- UCR-001 command registry fixture. Independent state transitions:
  // the command path shares this fixture's phase state with the REST adapter,
  // but never proxies to the Rust service or to the REST wire DTO.
  const COMMAND_SET = 'opportunity.set_phase';
  const COMMAND_UNDO = 'opportunity.undo_phase_change';
  const COMMAND_CREATE = 'opportunity.create';
  const COMMAND_ARG_FIELDS: Record<string, string[]> = {
    [COMMAND_CREATE]: ['title', 'primary_party_id'],
    [COMMAND_SET]: ['opportunity_id', 'expected_phase_revision', 'expected_pack_id', 'expected_pack_version', 'stage_id'],
    [COMMAND_UNDO]: ['opportunity_id', 'expected_phase_revision', 'expected_pack_id', 'expected_pack_version', 'original_event_id'],
  };
  const commandDescriptors: RecordData[] = [
    {
      id: COMMAND_CREATE, version: 1, title: 'Create opportunity',
      summary: 'Create an opportunity for a selected party',
      description: 'Creates one open opportunity with the selected active party as its primary participant and places it in the first board list.',
      keywords: ['queue', 'new', 'create', 'lead'], effect: 'write', requires_permission: 'opportunity.create',
      surfaces: ['palette', 'agent'], agent_tier: 2, supports_dry_run: false,
      when: 'route == "queue"',
      args_schema: { type: 'object', additionalProperties: false, required: COMMAND_ARG_FIELDS[COMMAND_CREATE], properties: {} },
      result_schema: { type: 'object', additionalProperties: false },
      palette: { context_fields: ['route'], prompts: [{ field: 'title', picker: 'text', title: 'Opportunity title' }, { field: 'primary_party_id', picker: 'party', title: 'Primary party' }] },
    },
    {
      id: COMMAND_SET, version: 1, title: 'Change opportunity phase',
      summary: 'Move an open opportunity to a planning phase',
      description: 'Moves an open opportunity to a planning stage. Closing, sending and resolution have separate owner workflows.',
      keywords: ['queue', 'stage'], effect: 'write', requires_permission: 'opportunity.phase_edit',
      surfaces: ['palette', 'agent'], agent_tier: 2, supports_dry_run: false,
      when: 'route == "queue" && selection.opportunity',
      args_schema: { type: 'object', additionalProperties: false, required: COMMAND_ARG_FIELDS[COMMAND_SET], properties: {} },
      result_schema: { type: 'object', additionalProperties: false },
      palette: { context_fields: ['route', 'selection.opportunity'], prompts: [{ field: 'stage_id', picker: 'planning_stage', title: 'Planning phase' }] },
    },
    {
      id: COMMAND_UNDO, version: 1, title: 'Undo phase change',
      summary: 'Revert the latest manual phase change',
      description: 'Returns the opportunity to the stage recorded before its latest manual phase transition, including Unmapped.',
      keywords: ['queue', 'stage', 'undo'], effect: 'write', requires_permission: 'opportunity.phase_edit',
      surfaces: ['palette', 'agent'], agent_tier: 2, supports_dry_run: false,
      when: 'route == "queue" && selection.opportunity',
      args_schema: { type: 'object', additionalProperties: false, required: COMMAND_ARG_FIELDS[COMMAND_UNDO], properties: {} },
      result_schema: { type: 'object', additionalProperties: false },
      palette: { context_fields: ['route', 'selection.opportunity'], prompts: [] },
    },
  ];
  const commandManifestVersion = `sha256:${createHash('sha256').update(JSON.stringify(commandDescriptors)).digest('hex')}`;
  const commandReceipts = new Map<string, { digest: string; status: number; body: RecordData; audit_id: string }>();
  const commandContextAllowed = (key: string) => key === 'route'
    || (/^selection\.[a-z0-9_]+$/.test(key) && !key.includes('__'));

  function canonicalArgs(value: unknown): string {
    if (Array.isArray(value)) return `[${value.map(canonicalArgs).join(',')}]`;
    if (value && typeof value === 'object') {
      const source = value as RecordData;
      return `{${Object.keys(source).sort().map((key) => `${JSON.stringify(key)}:${canonicalArgs(source[key])}`).join(',')}}`;
    }
    return JSON.stringify(value ?? null);
  }

  function commandRank(descriptor: RecordData, query: string): [number, number] {
    const title = String(descriptor.title).toLocaleLowerCase();
    if (!query) return [5, 0];
    if (title === query) return [0, 0];
    if (title.startsWith(query)) return [1, 0];
    if (title.split(/\s+/).some((token) => token.startsWith(query))) return [2, 0];
    const source = [...title];
    const target = [...query];
    let index = 0;
    let skipped = 0;
    let started = false;
    for (const char of target) {
      while (index < source.length && source[index] !== char) { if (started) skipped += 1; index += 1; }
      if (index >= source.length) return [4, 0];
      started = true;
      index += 1;
    }
    return [3, skipped];
  }

  function validateCommandArgs(id: string, args: RecordData): RecordData | null {
    const fields = COMMAND_ARG_FIELDS[id];
    if (!fields) return null;
    for (const field of fields) {
      const value = args[field];
      if (field === 'expected_phase_revision') {
        if (!Number.isSafeInteger(value) || Number(value) < 1) {
          return { path: `/${field}`, problem: 'expected a positive integer' };
        }
      } else if (typeof value !== 'string' || !value.trim()) {
        return { path: `/${field}`, problem: 'expected a non-empty string' };
      }
    }
    for (const key of Object.keys(args)) {
      if (!fields.includes(key)) return { path: `/${key}`, problem: 'additional property is not allowed' };
    }
    return null;
  }

  function commandResult(id: string, args: RecordData) {
    if (id === COMMAND_CREATE) {
      const title = String(args.title ?? '').trim();
      const party = directory.get(String(args.primary_party_id));
      if (!title || !party || !['person', 'org'].includes(party.kind)) {
        return { status: 404, body: { code: 'target_unavailable', message: 'Primary party is not available', details: [] } };
      }
      const record = { id: randomHex(), title, state: 'open', revision: 1, participant_ids: [party.id], source_variant_ids: [], material_ids: [], tags: [] };
      opportunities.set(record.id, record);
      const homeList = boardLists.find((list) => !list.archived)!;
      const position = [...placements.values()].filter((placement) => placement.list_id === homeList.id).length;
      placements.set(record.id, { list_id: homeList.id, position, placement_revision: 1 });
      notesByOpportunity.set(record.id, { markdown: '', revision: 1 });
      pushEvent({ event_id: randomHex(), entity_type: 'opportunity', entity_id: record.id, revision: 1, event_type: 'opportunity.created', payload: { primary_party_id: party.id, origin: 'command.registry' } });
      return {
        status: 200,
        body: {
          opportunity_id: record.id, revision: 1, primary_party_id: party.id,
          list_id: homeList.id, position, event_cursor: String(events.length),
        },
      };
    }
    const opportunity = opportunities.get(String(args.opportunity_id));
    if (!opportunity) return { status: 404, body: { code: 'unknown_command', message: 'Opportunity does not exist', details: [] } };
    const current = opportunityPhases.get(String(opportunity.id)) ?? { stage_id: null, pack_id: activePackId, pack_version: activePackVersion, revision: 1, event_id: '' };
    const reverted = id === COMMAND_UNDO;
    if (args.expected_pack_id !== current.pack_id || args.expected_pack_version !== current.pack_version) {
      return { status: 409, body: { code: 'conflict', message: 'Active pack identity changed; reload the Queue sheet', details: [] } };
    }
    if (Number(args.expected_phase_revision) !== current.revision) {
      return { status: 409, body: { code: 'conflict', message: `Expected phase revision ${String(args.expected_phase_revision)} but the opportunity is at ${current.revision}`, details: [{ path: '/expected_phase_revision', problem: 'stale revision' }] } };
    }
    if (reverted && current.event_id !== String(args.original_event_id)) {
      return { status: 409, body: { code: 'conflict', message: 'Undo cannot apply: a newer phase change replaced the referenced transition', details: [{ path: '/original_event_id', problem: 'stale event' }] } };
    }
    let stageId: string | null;
    if (reverted) {
      const event = events.find((item) => item.event_id === String(args.original_event_id));
      const payload = event?.payload ?? {};
      const previous = payload.from_stage_id;
      if (previous !== null && (typeof previous !== 'string' || !PACK_STAGES.some((stage) => stage.stage_id === previous))) {
        return { status: 409, body: { code: 'conflict', message: 'The original phase event has no restorable predecessor', details: [] } };
      }
      // A null predecessor is a recorded Unmapped state and is restorable.
      stageId = previous === null ? null : String(previous);
    } else {
      stageId = String(args.stage_id);
    }
    const stage = stageId ? PACK_STAGES.find((item) => item.stage_id === stageId) : null;
    if (stageId && (!stage || stage.kind === 'terminal')) {
      return { status: 422, body: { code: 'invalid_args', message: `Stage '${stageId}' is not an active planning stage`, details: [{ path: '/stage_id', problem: 'unknown or terminal stage' }] } };
    }
    if (String(opportunity.state) !== 'open') {
      return { status: 409, body: { code: 'conflict', message: `This opportunity is ${String(opportunity.state)}`, details: [] } };
    }
    const noOp = !reverted && current.stage_id === stageId;
    const eventId = noOp ? null : randomHex();
    const revision = noOp ? current.revision : current.revision + 1;
    const next = { stage_id: stageId, pack_id: activePackId, pack_version: activePackVersion, revision, event_id: eventId ?? current.event_id };
    if (!noOp) {
      opportunityPhases.set(String(opportunity.id), next);
      opportunity.revision = Number(opportunity.revision) + 1;
      pushEvent({
        event_id: eventId!, entity_type: 'opportunity', entity_id: String(opportunity.id), revision: Number(opportunity.revision),
        event_type: 'opportunity.phase_changed',
        payload: { from_stage_id: current.stage_id, to_stage_id: stageId, pack_id: activePackId, pack_version: activePackVersion, operation: reverted ? 'undo' : 'manual', origin: 'command.registry', reversal_of: reverted ? String(args.original_event_id) : null },
      });
    }
    return {
      status: 200,
      body: {
        opportunity_id: String(opportunity.id), phase_revision: revision, previous_stage_id: current.stage_id,
        stage_id: stageId, event_id: eventId, event_cursor: String(events.length),
      },
    };
  }

  // --- U02 delegated runs. One fixture state machine produces the same
  // queued → running → succeeded facts as the Rust runtime without sharing
  // its worker, lease or provider state.
  const AGENT_STATES = ['queued', 'running', 'waiting_owner', 'waiting_job', 'succeeded', 'failed', 'cancelled', 'expired'];
  const AGENT_TARGET_KINDS = ['party', 'opportunity', 'resource', 'material', 'segment'];
  const AGENT_PERMISSIONS = ['party.read', 'opportunity.read', 'opportunity.create', 'opportunity.phase.edit'];

  function agentTargetLabel(target: AgentRunTarget): string {
    if (target.kind === 'party') return directory.get(target.id)?.name ?? target.id;
    if (target.kind === 'opportunity') return String(opportunities.get(target.id)?.title ?? target.id);
    return target.id;
  }

  function addAgentTurn(run: AgentRunRecord, turn: AgentTurnRecord) {
    const turns = agentTurns.get(run.run_id) ?? [];
    turns.push(turn);
    agentTurns.set(run.run_id, turns);
    run.turn_count = turns.length;
    run.transcript_bytes += Buffer.byteLength(turn.text, 'utf8');
  }

  function setAgentState(run: AgentRunRecord, state: string, wake: string | null = null) {
    run.state = state;
    run.revision += 1;
    run.wake_condition = wake;
  }

  function advanceAgentRun(runId: string) {
    const run = agentRuns.get(runId);
    if (!run || run.state !== 'queued') return;
    scheduleAgent(runId, () => {
      if (run.state !== 'queued' || run.epoch !== epoch) return;
      setAgentState(run, 'running', 'tool_dispatch');
      scheduleAgent(runId, () => {
        if (run.state !== 'running') return;
        const names = run.targets.map(agentTargetLabel).join(', ') || 'no targets';
        addAgentTurn(run, { turn_no: 2, speaker: 'tool', ordinal: 1, text: `Scoped read of ${names} via party.get_summary / opportunity.get_summary` });
        scheduleAgent(runId, () => {
          if (run.state !== 'running') return;
          addAgentTurn(run, { turn_no: 2, speaker: 'assistant', ordinal: 0, text: `Completed scoped read of ${names} using only the granted read permissions.` });
          setAgentState(run, 'succeeded', 'completed');
        }, 600);
      }, 600);
    }, 600);
  }

  function encodeAgentCursor(turnNo: number, ordinal: number): string {
    return Buffer.from(`${turnNo}:${ordinal}`, 'utf8').toString('hex');
  }

  function decodeAgentCursor(cursor: string): [number, number] {
    const [turn, ordinal] = Buffer.from(cursor, 'hex').toString('utf8').split(':');
    const turnNo = Number(turn);
    const position = Number(ordinal);
    return Number.isSafeInteger(turnNo) && Number.isSafeInteger(position) ? [turnNo, position] : [0, 0];
  }

  function agentRunDto(run: AgentRunRecord) {
    return {
      run_id: run.run_id, state: run.state, revision: run.revision,
      granted_actions: run.permissions, targets: run.targets,
      deadline_at: run.deadline_at, phase: 'queued',
    };
  }

  function agentStatusDto(run: AgentRunRecord) {
    return {
      run_id: run.run_id, state: run.state, revision: run.revision, deadline_at: run.deadline_at,
      turn_count: run.turn_count, transcript_bytes: run.transcript_bytes, wake_condition: run.wake_condition,
    };
  }

  function json(response: ServerResponse, status: number, body: unknown) {
    response.writeHead(status, {
      'content-type': 'application/json; charset=utf-8',
      'cache-control': 'no-store',
      'x-parley-epoch': epoch,
    });
    response.end(JSON.stringify(body));
  }

  function fail(response: ServerResponse, status: number, code: string, message: string) {
    return json(response, status, { error: { code, message, details: [] } });
  }

  async function readBody(request: IncomingMessage): Promise<RecordData> {
    const chunks: Buffer[] = [];
    let length = 0;
    for await (const chunk of request) {
      length += (chunk as Buffer).length;
      if (length > 2 * 1024 * 1024) throw new Error('too large');
      chunks.push(chunk as Buffer);
    }
    return JSON.parse(Buffer.concat(chunks).toString('utf8')) as RecordData;
  }

  function syncResourceCard(resource: ResourceRecord) {
    const existing = entityResources.get(resource.id);
    const card: ResourceEntityRecord = {
      id: resource.id, kind: resource.kind === 'artifact' ? 'artifact' : 'resource', focus: '',
      name: resource.title, subtype: resource.kind, body: resource.body, readiness: resource.readiness,
      revision: resource.revision, notes: existing?.notes ?? { markdown: '', revision: 1 }, etag: '',
    };
    card.etag = entityTag(card);
    entityResources.set(card.id, card);
  }

  function findEntity(id: string): RecordData | undefined {
    const entity = directory.get(id);
    if (entity) return entity;
    const resource = entityResources.get(id);
    return resource && resource.readiness !== 'retired' ? { id: resource.id, kind: resource.kind, name: resource.name, focus: resource.focus, revision: resource.revision } : undefined;
  }

  function resourceCountsFor(resourceId: string): RecordData {
    const files = (entityAttachments.get(resourceId) ?? []).length;
    const connections = graphEdges.filter(
      (edge) => edge.state !== 'retracted' && (String(edge.from_id) === resourceId || String(edge.to_id) === resourceId),
    ).length;
    return { files, connections, proposed: 0, deliveries: 0 };
  }

  function resourceGapsFor(resourceId: string): string[] {
    const res = entityResources.get(resourceId);
    if (!res) return [];
    const body = res.body as RecordData;
    const text = (key: string) => typeof body[key] === 'string' ? String(body[key]).trim() : '';
    const gaps: string[] = [];
    if (res.subtype === 'proof') {
      if (!text('result')) gaps.push('Result');
      if (!text('context')) gaps.push('Context');
      if (!text('time')) gaps.push('Time');
      if (!text('owner_role')) gaps.push('Owner role');
    } else if (res.subtype === 'gift') {
      if (!text('effort')) gaps.push('Effort estimate');
    } else if (res.subtype === 'artifact') {
      if (!text('reference')) gaps.push('Available reference');
      if (body.share_permission !== true) gaps.push('Permission to share');
    }
    return gaps;
  }

  function applyPlacement(resourceId: string, input: RecordData): boolean {
    const columnId = input.column_id as string | null;
    if (columnId === null || columnId === undefined) {
      const prior = resourcePlacements.get(resourceId);
      resourcePlacements.delete(resourceId);
      if (prior) {
        const column = resourceColumns.find((c) => c.id === prior.column_id);
        if (column) column.revision = Number(column.revision) + 1;
        pushEvent({ event_id: randomHex(), entity_type: 'resource', entity_id: resourceId, revision: prior.revision + 1, event_type: 'resource.removed_from_board', payload: { column_id: prior.column_id } });
      }
      return true;
    }
    const column = resourceColumns.find((c) => c.id === columnId);
    if (!column) return false;
    const before = typeof input.before_resource_id === 'string' ? input.before_resource_id : null;
    if (before === resourceId) return false;
    const rows = [...resourcePlacements.entries()]
      .filter(([id, p]) => id !== resourceId && p.column_id === columnId)
      .sort((a, b) => a[1].position - b[1].position || a[0].localeCompare(b[0]));
    const list = rows.map(([id]) => id);
    let index = list.length;
    if (before !== null) {
      const at = list.indexOf(before);
      if (at < 0) return false;
      index = at;
    }
    list.splice(index, 0, resourceId);
    const prior = resourcePlacements.get(resourceId);
    list.forEach((id, i) => {
      const existing = resourcePlacements.get(id);
      resourcePlacements.set(id, { column_id: columnId, position: (i + 1) * 1024, revision: (existing?.revision ?? 0) + 1 });
    });
    column.revision = Number(column.revision) + 1;
    const source = resourceColumns.find(c => c.id === prior?.column_id);
    if (source && source !== column) source.revision = Number(source.revision) + 1;
    pushEvent({
      event_id: randomHex(), entity_type: 'resource', entity_id: resourceId,
      revision: resourcePlacements.get(resourceId)!.revision,
      event_type: prior ? 'resource.card_moved' : 'resource.placed',
      payload: { column_id: columnId },
    });
    return true;
  }

  function bindUploadTo(entityId: string, uploadId: string): boolean {
    const upload = uploads.get(uploadId);
    if (!upload) return false;
    const bound = [...entityAttachments.values()].flat().some((a) => a.upload_id === uploadId);
    if (bound) return false;
    const row = {
      id: randomHex(), entity_id: entityId, upload_id: uploadId,
      filename: upload.filename, content_type: upload.mime, size: upload.bytes.length,
      sha256: upload.sha256, share_permission: upload.share_permission,
      reviewed_by_owner: upload.reviewed, created_at: now(),
    };
    const list = entityAttachments.get(entityId) ?? [];
    list.push(row);
    entityAttachments.set(entityId, list);
    return true;
  }

  function unbindAttachmentFrom(entityId: string, ref: string): void {
    const list = entityAttachments.get(entityId) ?? [];
    entityAttachments.set(entityId, list.filter((a) => a.id !== ref));
  }

  const RESERVED_DELIVERY_VERBS = ['shared with', 'delivered to', 'sent to'];
  function reservedDeliveryVerb(verb: unknown): string | null {
    const normalized = String(verb ?? '').trim().toLowerCase().replace(/_/g, ' ').replace(/\s+/g, ' ');
    return RESERVED_DELIVERY_VERBS.includes(normalized) ? String(verb ?? '').trim() : null;
  }

  function relateFrom(fromId: string, rel: RecordData, info: { edge_id: string | null; to_id: string | null }): string | null {
    const verb = String(rel.verb ?? '').trim();
    const basis = String(rel.basis ?? '').trim();
    if (!verb || !basis) return 'invalid';
    let toId: string;
    if (typeof rel.target_id === 'string') {
      toId = rel.target_id;
      const target = findEntity(toId);
      if (!target) return 'missing';
      if (rel.expected_target_revision != null && rel.expected_target_revision !== target.revision) return 'stale';
    } else if (rel.create_party && typeof rel.create_party === 'object') {
      const party = rel.create_party as RecordData;
      const name = String(party.name ?? '').trim();
      if (!name || !['person', 'org'].includes(String(party.kind))) return 'invalid';
      toId = randomHex();
      directory.set(toId, { id: toId, name, kind: String(party.kind), focus: String(party.focus ?? ''), revision: 1 });
    } else if (rel.create && typeof rel.create === 'object') {
      const create = rel.create as RecordData;
      const title = String(create.title ?? '').trim();
      const subtype = String(create.subtype ?? '');
      if (!title || !['gift', 'proof', 'artifact', 'skill', 'interest', 'note'].includes(subtype)) return 'invalid';
      const id = randomHex();
      const child: ResourceEntityRecord = {
        id, kind: 'resource', name: title, focus: '', revision: 1, subtype,
        body: (create.body ?? {}) as RecordData, readiness: 'draft',
        notes: { markdown: String(create.notes_md ?? ''), revision: 1 }, etag: '',
      };
      child.etag = entityTag(child);
      entityResources.set(id, child);
      toId = id;
      if (create.placement && typeof create.placement === 'object' && !applyPlacement(id, create.placement as RecordData)) return 'invalid placement';
      for (const upload of (create.staged_uploads ?? []) as string[]) if (!bindUploadTo(id, upload)) return 'invalid upload';
    } else {
      return 'invalid';
    }
    if (toId === fromId) return 'self';
    const start = rel.incoming === true ? toId : fromId;
    const end = rel.incoming === true ? fromId : toId;
    const duplicate = graphEdges.some(
      (edge) => edge.state !== 'retracted' && String(edge.from_id) === start && edge.verb === verb && String(edge.to_id) === end,
    );
    if (duplicate) return 'duplicate';
    const edge: RecordData = {
      id: randomHex(), from_id: start, verb, to_id: end, revision: 1, basis,
      author_kind: 'owner', state: 'active', owner_authored: true, occurrence_id: null, quote: basis,
      tier: 1, speaker_kind: 'owner', captured_at: now(), source_title: null, source_url: null,
    };
    graphEdges.push(edge);
    info.edge_id = String(edge.id);
    info.to_id = toId;
    pushEvent({ event_id: randomHex(), entity_type: 'edge', entity_id: edge.id as string, revision: 1, event_type: 'edge.created', payload: {} });
    return null;
  }

  function authorize(request: IncomingMessage, needToken = true): string | null {
    const cookie = request.headers.cookie
      ?.split(';')
      .map((item) => item.trim())
      .find((item) => item.startsWith('parley_session='))
      ?.slice(15);
    const session = cookie && sessions.get(cookie);
    if (!session) return null;
    if (needToken && request.headers['x-parley-token'] !== session) return null;
    if (needToken && request.headers['x-parley-epoch'] !== epoch) return null;
    return cookie ?? null;
  }

  async function handle(request: IncomingMessage, response: ServerResponse): Promise<void> {
    const url = new URL(request.url ?? '/', `http://${request.headers.host ?? '127.0.0.1'}`);
    if (url.pathname === '/api/v1/health') {
      return json(response, 200, { status: 'ready', model: 'not-probed' });
    }
    if (url.pathname === '/api/v1/session/bootstrap' && request.method === 'POST') {
      let input: RecordData;
      try {
        input = await readBody(request);
      } catch {
        return fail(response, 400, 'INVALID_JSON', 'Request body is invalid');
      }
      const secret = typeof input.secret === 'string' ? input.secret : '';
      const expiry = bootstraps.get(secret);
      bootstraps.delete(secret);
      if (!expiry || expiry < Date.now()) {
        return fail(response, 401, 'BOOTSTRAP_INVALID', 'Bootstrap secret is invalid or expired');
      }
      const sessionId = randomHex(32);
      const token = randomHex(32);
      sessions.set(sessionId, token);
      response.writeHead(200, {
        'content-type': 'application/json; charset=utf-8',
        'cache-control': 'no-store',
        'set-cookie': `parley_session=${sessionId}; HttpOnly; SameSite=Strict; Path=/api/v1; Max-Age=28800`,
      });
      response.end(JSON.stringify({ ok: true }));
    }
    const cookie = authorize(request, false);
    if (url.pathname === '/api/v1/session' && request.method === 'GET') {
      if (!cookie) return fail(response, 401, 'SESSION_REQUIRED', 'Sign in to the local workspace');
      return json(response, 200, {
        workspace_id: '00112233445566778899aabbccddeeff',
        epoch,
        wire_major: 1,
        wire_minor: 0,
        capabilities: ['resources.read', 'resources.write', 'party.sheet', 'party.notes', 'party.dossier', 'party.imports', 'party.attachments', 'party.relations'],
        mutation_token: sessions.get(cookie),
        cursor: String(events.length),
      });
    }
    if (!authorize(request)) {
      return fail(response, cookie ? 403 : 401, cookie ? 'TOKEN_INVALID' : 'SESSION_REQUIRED', 'Sign in to the local workspace');
    }

    if (url.pathname === '/api/v1/commands/query' && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      if (input.surface !== 'palette') return fail(response, 403, 'forbidden', 'Owner requests must use the palette surface');
      const context = (input.context ?? {}) as RecordData;
      if (Object.keys(context).length > 16 || Object.keys(context).some((key) => !commandContextAllowed(key))) {
        return fail(response, 400, 'malformed_request', 'Context key is not allowlisted');
      }
      const commandsForContext = commandDescriptors.filter((descriptor) => {
        const when = String(descriptor.when ?? '');
        if (!when.includes('route == "queue"')) return false;
        if (when.includes('selection.opportunity')) return context['selection.opportunity'] === true;
        return true;
      });
      return json(response, 200, {
        contract_major: 1, manifest_version: commandManifestVersion, truncated: false,
        commands: commandsForContext,
      });
    }

    if (url.pathname === '/api/v1/commands/search' && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      if (typeof input.query !== 'string' || input.query.length > 256) return fail(response, 400, 'malformed_request', 'Query is invalid');
      const limit = input.limit === undefined ? 10 : Number(input.limit);
      if (!Number.isSafeInteger(limit) || limit < 1 || limit > 25) return fail(response, 400, 'malformed_request', 'Limit must be between 1 and 25');
      const context = (input.context ?? {}) as RecordData;
      if (Object.keys(context).some((key) => !commandContextAllowed(key))) return fail(response, 400, 'malformed_request', 'Context key is not allowlisted');
      const needle = input.query.trim().toLocaleLowerCase();
      const items = commandDescriptors
        .map((descriptor) => ({ descriptor, score: commandRank(descriptor, needle) }))
        .filter((entry) => entry.score[0] < 4)
        .sort((left, right) => left.score[0] - right.score[0] || left.score[1] - right.score[1] || String(left.descriptor.id).localeCompare(String(right.descriptor.id)))
        .slice(0, limit)
        .map(({ descriptor }) => ({
          id: descriptor.id, version: descriptor.version, title: descriptor.title,
          summary: descriptor.summary, effect: descriptor.effect,
        }));
      return json(response, 200, { items, total: items.length, next_cursor: null });
    }

    const commandDescribe = /^\/api\/v1\/commands\/([^/]+)$/.exec(url.pathname);
    if (commandDescribe && request.method === 'GET') {
      const descriptor = commandDescriptors.find((item) => item.id === decodeURIComponent(commandDescribe[1]!));
      if (!descriptor) return fail(response, 404, 'unknown_command', 'Command is not available');
      return json(response, 200, descriptor);
    }
    if (commandDescribe && request.method === 'POST') {
      const id = decodeURIComponent(commandDescribe[1]!);
      const descriptor = commandDescriptors.find((item) => item.id === id);
      if (!descriptor) return fail(response, 404, 'unknown_command', 'Command is not available');
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || !key.trim()) return fail(response, 400, 'INVALID_KEY', 'An Idempotency-Key is required');
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      if (input.version !== 1) return fail(response, 409, 'version_mismatch', 'Command version is stale; re-describe and collect fresh consent');
      if (!input.args || typeof input.args !== 'object' || Array.isArray(input.args)) return fail(response, 422, 'invalid_args', 'Args must be an object');
      const args = input.args as RecordData;
      const invalid = validateCommandArgs(id, args);
      if (invalid) return json(response, 422, { ok: false, error: { code: 'invalid_args', message: 'Command arguments are invalid', details: [invalid] } });
      const digest = createHash('sha256').update(canonicalArgs(args)).digest('hex');
      const receiptKey = `${epoch}|${id}|${key}`;
      const existing = commandReceipts.get(receiptKey);
      if (existing) {
        if (existing.digest !== digest) {
          return json(response, 422, { ok: false, error: { code: 'idempotency_mismatch', message: 'Idempotency key was used for a different command', details: [] } });
        }
        return json(response, existing.status, { ...existing.body, replayed: true, original_audit_id: existing.audit_id });
      }
      const outcome = commandResult(id, args);
      const auditId = randomHex(12);
      if (outcome.status === 200) {
        const body = {
          ok: true, command_version: 1, audit_id: auditId, replayed: false,
          dry_run: input.dry_run === true, result: outcome.body,
        };
        commandReceipts.set(receiptKey, { digest, status: 200, body, audit_id: auditId });
        return json(response, 200, body);
      }
      const failure = {
        ok: false, command_version: 1, audit_id: auditId,
        error: outcome.body as { code: string; message: string; details: RecordData[] },
      };
      commandReceipts.set(receiptKey, { digest, status: outcome.status, body: failure, audit_id: auditId });
      return json(response, outcome.status, failure);
    }

    if (url.pathname === '/api/v1/agent/runs' && request.method === 'POST') {
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || !key.trim()) return fail(response, 400, 'INVALID_KEY', 'An Idempotency-Key is required');
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      if (typeof input.task !== 'string' || !input.task.trim() || Buffer.byteLength(input.task, 'utf8') > 8192) {
        return fail(response, 422, 'invalid_args', 'Task must be 1..=8192 bytes');
      }
      const targets = input.targets as unknown;
      if (!Array.isArray(targets) || targets.some((raw) => {
        const target = raw as RecordData | null;
        return !target || typeof target !== 'object' || Array.isArray(target)
          || !AGENT_TARGET_KINDS.includes(String(target.kind)) || typeof target.id !== 'string' || !target.id;
      })) return fail(response, 422, 'invalid_args', 'Targets are invalid');
      const permissions = input.permissions as unknown;
      if (!Array.isArray(permissions) || permissions.some((permission) => typeof permission !== 'string' || !AGENT_PERMISSIONS.includes(permission))) {
        return fail(response, 422, 'invalid_args', 'Permissions are invalid');
      }
      if (typeof input.allow_created_opportunities !== 'boolean') {
        return fail(response, 422, 'invalid_args', 'allow_created_opportunities must be a boolean');
      }
      const digest = createHash('sha256').update(canonicalArgs({
        epoch, task: input.task, targets, permissions, allow_created_opportunities: input.allow_created_opportunities,
      })).digest('hex');
      const receiptKey = `${epoch}|${key}`;
      const previous = agentStartKeys.get(receiptKey);
      if (previous) {
        return previous.digest === digest
          ? json(response, 201, previous.body)
          : fail(response, 409, 'KEY_REUSED', 'Operation key was used for a different run');
      }
      const runId = `agent-run-${randomHex(8)}`;
      const run: AgentRunRecord = {
        run_id: runId, epoch, state: 'queued', revision: 1,
        deadline_at: new Date(testClock + 15 * 60_000).toISOString(),
        targets: (targets as AgentRunTarget[]).map((target) => ({ kind: target.kind, id: target.id })),
        permissions: permissions as string[], turn_count: 1,
        transcript_bytes: Buffer.byteLength(input.task, 'utf8'), wake_condition: 'queued',
      };
      agentRuns.set(runId, run);
      agentTurns.set(runId, [{ turn_no: 1, speaker: 'owner', ordinal: 1, text: input.task }]);
      const body = agentRunDto(run);
      agentStartKeys.set(receiptKey, { digest, run_id: runId, body });
      advanceAgentRun(runId);
      return json(response, 201, body);
    }

    if (url.pathname === '/api/v1/agent/runs' && request.method === 'GET') {
      const state = url.searchParams.get('status') ?? '';
      if (state && !AGENT_STATES.includes(state)) return fail(response, 400, 'invalid_args', 'Unknown status filter');
      const limitRaw = url.searchParams.get('limit');
      const limit = limitRaw === null ? 10 : Number(limitRaw);
      if (!Number.isSafeInteger(limit) || limit < 1 || limit > 20) return fail(response, 400, 'invalid_args', 'Limit must be between 1 and 20');
      const cursor = url.searchParams.get('cursor') ?? '';
      const rows = [...agentRuns.values()]
        .filter((run) => run.epoch === epoch && (!state || run.state === state) && run.run_id > cursor)
        .sort((left, right) => left.run_id.localeCompare(right.run_id));
      const page = rows.slice(0, limit);
      return json(response, 200, {
        items: page.map((run) => ({ run_id: run.run_id, state: run.state, revision: run.revision, deadline_at: run.deadline_at })),
        next_cursor: rows.length > page.length && page.length ? page[page.length - 1]!.run_id : null,
      });
    }

    const agentCancelRoute = /^\/api\/v1\/agent\/runs\/([^/]+)\/cancel$/.exec(url.pathname);
    if (agentCancelRoute && request.method === 'POST') {
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || !key.trim()) return fail(response, 400, 'INVALID_KEY', 'An Idempotency-Key is required');
      const id = decodeURIComponent(agentCancelRoute[1]!);
      const run = agentRuns.get(id);
      if (!run || run.epoch !== epoch) return fail(response, 404, 'target_unavailable', 'Run is not available');
      const digest = createHash('sha256').update(canonicalArgs({ epoch, path: url.pathname })).digest('hex');
      const receiptKey = `${epoch}|${id}|${key}`;
      const previous = agentCancelKeys.get(receiptKey);
      if (previous) {
        return previous.digest === digest ? json(response, 200, previous.body) : fail(response, 409, 'KEY_REUSED', 'Operation key was used for a different cancellation');
      }
      clearAgentTimers(id);
      let state = 'unchanged';
      if (!['succeeded', 'failed', 'cancelled', 'expired'].includes(run.state)) {
        setAgentState(run, 'cancelled', 'cancelled');
        state = 'cancelled';
      }
      const body = { run_id: id, state };
      agentCancelKeys.set(receiptKey, { digest, body });
      return json(response, 200, body);
    }

    const agentTranscriptRoute = /^\/api\/v1\/agent\/runs\/([^/]+)\/transcript$/.exec(url.pathname);
    if (agentTranscriptRoute && request.method === 'GET') {
      const id = decodeURIComponent(agentTranscriptRoute[1]!);
      const run = agentRuns.get(id);
      if (!run || run.epoch !== epoch) return fail(response, 404, 'target_unavailable', 'Run is not available');
      const limitRaw = url.searchParams.get('limit');
      const limit = limitRaw === null ? 20 : Number(limitRaw);
      if (!Number.isSafeInteger(limit) || limit < 1 || limit > 20) return fail(response, 400, 'invalid_args', 'Limit must be between 1 and 20');
      const [turnNo, ordinal] = decodeAgentCursor(url.searchParams.get('cursor') ?? '');
      const turns = (agentTurns.get(id) ?? [])
        .filter((turn) => turn.turn_no > turnNo || (turn.turn_no === turnNo && turn.ordinal > ordinal))
        .sort((left, right) => left.turn_no - right.turn_no || left.ordinal - right.ordinal || left.speaker.localeCompare(right.speaker));
      const page = turns.slice(0, limit);
      const last = page[page.length - 1];
      return json(response, 200, {
        items: page.map((turn) => ({ turn_no: turn.turn_no, speaker: turn.speaker, ordinal: turn.ordinal, text: turn.text })),
        next_cursor: turns.length > page.length && last ? encodeAgentCursor(last.turn_no, last.ordinal) : null,
      });
    }

    const agentStatusRoute = /^\/api\/v1\/agent\/runs\/([^/]+)$/.exec(url.pathname);
    if (agentStatusRoute && request.method === 'GET') {
      const id = decodeURIComponent(agentStatusRoute[1]!);
      const run = agentRuns.get(id);
      if (!run || run.epoch !== epoch) return fail(response, 404, 'target_unavailable', 'Run is not available');
      return json(response, 200, agentStatusDto(run));
    }

    const importRoute = /^\/api\/v1\/party-imports(?:\/([^/]+)\/(preview|commit))?$/.exec(url.pathname);
    if (importRoute && request.method === 'POST') {
      const stageId = importRoute[1];
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || !key) return fail(response, 400, 'IDEMPOTENCY_KEY_REQUIRED', 'Operation key required.');
      try {
        if (!stageId) {
          const chunks: Buffer[] = [];
          let size = 0;
          for await (const chunk of request) {
            size += (chunk as Buffer).length;
            if (size > maxImportBytes) return fail(response, 413, 'CSV_TOO_LARGE', 'Split this CSV into files of at most ' + maxImportBytes + ' bytes.');
            chunks.push(chunk as Buffer);
          }
          const bytes = Buffer.concat(chunks);
          const csv = parsePartyCsv(bytes);
          const id = randomHex();
          importStages.set(id, { csv, epoch, expires: Date.now() + 30 * 60_000 });
          return json(response, 201, { data: { stage_id: id, headers: csv.headers, row_count: csv.records.length, max_rows: maxImportRows, max_bytes: maxImportBytes } });
        }
        const input = await readBody(request);
        const digest = importDigest([epoch,url.pathname,input]);
        const oldReceipt = receipts.get(key);
        if (oldReceipt) return oldReceipt.digest === digest ? json(response, 200, { data: oldReceipt.result }) : fail(response, 409, 'KEY_REUSED', 'Operation key refers to a different request.');
        const staged = importStages.get(stageId);
        if (!staged || staged.epoch !== epoch || Date.now() > staged.expires) return fail(response, 410, 'IMPORT_EXPIRED', 'Stage the CSV again.');
        if (importRoute[2] === 'preview') {
          const mapping = input as PartyImportMapping;
          const preview = previewCsv(staged.csv, mapping, directory.values());
          const result = { ...preview, preview_id: randomHex(), digest: importDigest([stageId,epoch,mapping,preview]) };
          staged.review = { mapping, result };
          return json(response, 200, { data: result });
        }
        const saved = staged.review;
        if (!saved || input.preview_id !== saved.result.preview_id || input.digest !== saved.result.digest) return fail(response, 409, 'STALE_PREVIEW', 'Review the CSV again.');
        const current = previewCsv(staged.csv, saved.mapping, directory.values());
        if (importDigest([stageId,epoch,saved.mapping,current]) !== saved.result.digest) return fail(response, 409, 'STALE_PREVIEW', 'Party matches changed. Review the CSV again.');
        if (!current.create_count || current.error_count) return fail(response, 422, 'IMPORT_INVALID', 'Review or skip all flagged rows before importing.');
        const batch_id = randomHex();
        const created = current.rows.filter(r => r.decision === 'create').map(row => {
          const party = { id: randomHex(), name: row.name, focus: row.focus, kind: saved.mapping.party_type, revision: 1 };
          directory.set(party.id, party); notesByEntity.set(party.id, { markdown: row.notes, revision: 1 });
          partyImportOrigins.set(party.id, { batch_id, source_record: row.source_record });
          pushEvent({ event_id: randomHex(), entity_type: 'entity', entity_id: party.id, revision: 1, event_type: 'entity.created', payload: { batch_id, source_record: row.source_record } });
          return { source_record: row.source_record, party_id: party.id };
        });
        const result = { batch_id, created, skipped_count: current.skip_count };
        receipts.set(key, { digest, result }); importStages.delete(stageId);
        return json(response, 201, { data: result });
      } catch (reason) { return fail(response, 422, 'IMPORT_INVALID', reason instanceof Error ? reason.message : 'Invalid CSV.'); }
    }

    const fileRoute = /^\/api\/v1\/entities\/([^/]+)\/attachments(?:\/([^/]+)(\/content)?)?$/.exec(url.pathname);
    if (fileRoute) {
      const [,ownerId,ref,content] = fileRoute;
      if (!directory.has(ownerId!) && !resources.has(ownerId!)) return fail(response, 404, 'NOT_FOUND', 'Card unavailable.');
      const files = entityAttachments.get(ownerId!) ?? [];
      const file = files.find(f => f.id === ref);
      if (content && request.method === 'GET') {
        const upload = file && uploads.get(String(file.upload_id));
        if (!upload) return fail(response, 404, 'NOT_FOUND', 'File unavailable.');
        response.writeHead(200, { 'content-type': 'application/octet-stream', 'cache-control': 'no-store', 'x-content-type-options': 'nosniff' });
        response.end(upload.bytes); return;
      }
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || !key) return fail(response, 400, 'IDEMPOTENCY_KEY_REQUIRED', 'Operation key required.');
      const body = request.method === 'POST' ? await readBody(request) : null;
      const digest = importDigest([epoch,url.pathname,request.method,body]);
      const previous = receipts.get(key);
      if (previous) {
        if (digest !== previous.digest) return fail(response, 409, 'KEY_REUSED', 'Operation key belongs to another request.');
        return json(response, body ? 200 : 204, body ? { data: previous.result } : null);
      }
      if (body && !ref) {
        const upload = uploads.get(String(body.upload_id));
        if (!upload) return fail(response, 404, 'UPLOAD_UNAVAILABLE', 'Reupload the file.');
        const existing = files.find(f => f.upload_id === upload.id);
        const result = existing ?? { id: randomHex(), upload_id: upload.id, filename: upload.filename, size: upload.bytes.length, sha256: upload.sha256, content_type: upload.mime, reviewed_by_owner: false, share_permission: false, created_at: now() };
        if (!existing) entityAttachments.set(ownerId!, [...files,result]);
        receipts.set(key, { digest, result });
        return json(response, 201, { data: result });
      }
      if (request.method === 'DELETE' && file && !content) {
        entityAttachments.set(ownerId!, files.filter(f => f.id !== ref));
        receipts.set(key, { digest, result: {} });
        return json(response, 204, null);
      }
      return fail(response, 404, 'NOT_FOUND', 'Attachment unavailable.');
    }
    if (url.pathname === '/api/v1/admin/overview' && request.method === 'GET') {
      return json(response, 200, adminOverview({ asOf: now(), epoch, cursor: String(events.length), backups: adminBackups, parties: [...directory.values()] }));
    }
    if (url.pathname === '/api/v1/admin/backups' && request.method === 'POST') {
      try { await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || !key) return fail(response, 400, 'IDEMPOTENCY_REQUIRED', 'Backup creation requires an idempotency key');
      const receiptKey = `admin.backup:${key}`;
      const digest = createHash('sha256').update('{}').digest('hex');
      const previous = receipts.get(receiptKey);
      if (previous) return previous.digest === digest ? json(response, 201, previous.result) : fail(response, 409, 'KEY_REUSED', 'Idempotency key was used for a different command');
      const snapshot = takeWorkspaceSnapshot();
      const createdAt = now();
      const backup: AdminBackupRecord = { id: randomHex(), created_at: createdAt, schema_version: 1, workspace_epoch: epoch, size_bytes: Buffer.byteLength(JSON.stringify(snapshot)), integrity: 'verified', snapshot };
      adminBackups.unshift(backup);
      pushEvent({ event_id: randomHex(), entity_type: 'workspace', entity_id: '1', revision: events.length + 1, event_type: 'backup.verified', payload: { backup_id: backup.id, schema_version: backup.schema_version } });
      const result = { id: backup.id, created_at: backup.created_at, schema_version: backup.schema_version, workspace_epoch: backup.workspace_epoch, size_bytes: backup.size_bytes, integrity: backup.integrity };
      receipts.set(receiptKey, { digest, result });
      return json(response, 201, result);
    }
    if (url.pathname === '/api/v1/admin/previews' && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const operation = input.operation;
      if (!['restore', 'erase_party', 'erase_workspace'].includes(String(operation))) return fail(response, 422, 'ADMIN_OPERATION_INVALID', 'Administrative operation is unsupported');
      const targetOperation = operation as AdminOperation;
      const backup = targetOperation === 'restore' ? adminBackups.find((item) => item.id === input.backup_id && item.integrity === 'verified') ?? null : null;
      const party = targetOperation === 'erase_party' ? directory.get(String(input.party_id)) ?? null : null;
      if (targetOperation === 'restore' && !backup) return fail(response, 422, 'BACKUP_UNVERIFIED', 'Select a verified backup');
      if (targetOperation === 'erase_party' && !party) return fail(response, 404, 'PARTY_NOT_FOUND', 'Select an existing party');
      const id = randomHex();
      const impactState = targetOperation === 'restore' ? { ...workspaceCollections(), restore_backup: backup } : workspaceCollections();
      const { impact, preserved_shared: preservedShared } = adminImpact(targetOperation, impactState, party?.id);
      const cursor = String(events.length);
      const digestPayload = { operation: targetOperation, epoch, cursor, backup_id: backup?.id ?? null, party_id: party?.id ?? null, impact, preservedShared };
      const digest = createHash('sha256').update(JSON.stringify(digestPayload)).digest('hex');
      const expires = testClock + 5 * 60_000;
      adminPreviews.set(id, { operation: targetOperation, digest, epoch, cursor, expires, backupId: backup?.id ?? null, partyId: party?.id ?? null });
      return json(response, 201, encodeAdminPreview({ id, operation: targetOperation, digest, epoch, cursor, expiresAt: new Date(expires).toISOString(), impact, preservedShared, backup, party }));
    }
    const adminCommit = /^\/api\/v1\/admin\/previews\/([^/]+)\/commit$/.exec(url.pathname);
    if (adminCommit && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || !key) return fail(response, 400, 'IDEMPOTENCY_REQUIRED', 'Administrative commit requires an idempotency key');
      const receiptKey = `admin.commit:${key}`;
      const commandDigest = createHash('sha256').update(JSON.stringify(input)).digest('hex');
      const priorReceipt = receipts.get(receiptKey);
      if (priorReceipt) return priorReceipt.digest === commandDigest ? json(response, 202, priorReceipt.result) : fail(response, 409, 'KEY_REUSED', 'Idempotency key was used for a different command');
      const preview = adminPreviews.get(adminCommit[1]!);
      if (!preview || input.digest !== preview.digest || input.preview_id !== adminCommit[1]) return fail(response, 412, 'PREVIEW_STALE', 'Review a fresh impact preview');
      if (testClock >= preview.expires) return fail(response, 410, 'PREVIEW_EXPIRED', 'This impact preview expired');
      if (preview.epoch !== epoch || preview.cursor !== String(events.length) || input.expected_workspace_epoch !== epoch || input.expected_event_cursor !== String(events.length)) return fail(response, 412, 'PREVIEW_STALE', 'Workspace changed after preview; review the new impact');
      if (input.confirmation !== preview.digest.slice(-12)) return fail(response, 403, 'CONFIRMATION_REQUIRED', 'Type the exact preview confirmation token');
      adminPreviews.delete(adminCommit[1]!);
      let summary: string;
      if (preview.operation === 'restore') {
        const backup = adminBackups.find((item) => item.id === preview.backupId && item.integrity === 'verified');
        if (!backup) return fail(response, 412, 'BACKUP_UNAVAILABLE', 'The reviewed backup is no longer available');
        restoreWorkspaceSnapshot(deepClone(backup.snapshot));
        receipts.clear(); adminPreviews.clear(); adminJobs.clear();
        epoch = randomHex();
        pushEvent({ event_id: randomHex(), entity_type: 'workspace', entity_id: '1', revision: events.length + 1, event_type: 'workspace.restored', payload: { backup_id: backup.id, schema_version: backup.schema_version } });
        summary = `Workspace restored from the verified backup dated ${backup.created_at}.`;
      } else if (preview.operation === 'erase_workspace') {
        for (const value of Object.values(workspaceCollections())) {
          if (value instanceof Map) value.clear(); else if (Array.isArray(value)) value.splice(0, value.length);
        }
        resources.clear(); directory.clear(); opportunities.clear(); sends.clear(); threads.clear(); outcomes.clear(); resolutions.clear(); adminBackups.splice(0, adminBackups.length);
        partySuppression.clear(); receipts.clear(); adminPreviews.clear(); adminJobs.clear();
        provider = { endpoint: 'http://127.0.0.1:11434', model: '', num_ctx: 8192, num_predict: 1024, locale: 'en' };
        Object.assign(questionBank, { version_id: 'question-bank-v1', content: { version: 1, questions: { proof: [{ id: 'result', text: 'What changed, and how did you measure the result?' }], gift: [{ id: 'offer', text: 'What useful thing could you offer?' }], artifact: [{ id: 'artifact', text: 'What have you made?' }] } } });
        prompts.clear();
        prompts.set('interviewer', { role: 'interviewer', version_id: 'interviewer-v1', version: 1, content: 'Bundled Interviewer policy', sha256: 'a'.repeat(64) });
        prompts.set('briefer', { role: 'briefer', version_id: 'briefer-v1', version: 1, content: 'Bundled Briefer policy', sha256: 'b'.repeat(64) });
        embeddingModel = ''; activePackId = PACK_DOCUMENTS[0]!.id; activePackVersion = PACK_DOCUMENTS[0]!.version; activePackDocument = PACK_DOCUMENTS[0]!.document; packSettingsRevision = 1;
        epoch = randomHex();
        pushEvent({ event_id: randomHex(), entity_type: 'workspace', entity_id: '1', revision: 1, event_type: 'workspace.erased', payload: { erased: true } });
        summary = 'Workspace records and derived content were erased.';
      } else {
        const partyId = preview.partyId!;
        const partyName = directory.get(partyId)?.name ?? '';
        directory.delete(partyId);
        for (let index = graphEdges.length - 1; index >= 0; index -= 1) if (graphEdges[index]?.from_entity_id === partyId || graphEdges[index]?.to_entity_id === partyId) graphEdges.splice(index, 1);
        graphPositions.delete(partyId);
        for (const opportunity of opportunities.values()) {
          if (Array.isArray(opportunity.participant_ids) && opportunity.participant_ids.includes(partyId)) opportunity.participant_ids = opportunity.participant_ids.filter((id) => id !== partyId);
        }
        const removedThreads = new Set([...threads.values()].filter((thread) => thread.recipient_id === partyId).map((thread) => String(thread.id)));
        for (const id of threads.keys()) if (removedThreads.has(id)) threads.delete(id);
        const removedSends = new Set([...sends.values()].filter((send) => send.recipient_id === partyId || removedThreads.has(String(send.thread_id))).map((send) => String(send.id)));
        for (const id of sends.keys()) if (removedSends.has(id)) sends.delete(id);
        for (const [id, item] of outcomes) if (removedSends.has(String(item.send_id))) outcomes.delete(id);
        for (const [id, item] of materialRecords) if (item.recipient_id === partyId) materialRecords.delete(id);
        for (const [id, item] of outputs) if (item.recipient_id === partyId) outputs.delete(id);
        for (const [id, item] of resolutions) if (item.recipient_id === partyId) resolutions.delete(id);
        briefItems.delete(`party:${partyId}`); writingInterests.delete(partyId); writingExchanges.delete(partyId);
        for (const [id, chat] of agentChats) if (chat.scope_kind === 'party' && chat.scope_id === partyId) agentChats.delete(id);
        for (const event of events) if (event.entity_id === partyId || event.payload.party_id === partyId || event.payload.recipient_id === partyId) event.payload = { erased: true };
        for (const backup of adminBackups) {
          backup.snapshot = scrubPartyFromBackup(backup.snapshot, partyId, partyName) as Record<string, unknown>;
          backup.size_bytes = Buffer.byteLength(JSON.stringify(backup.snapshot));
          backup.integrity = 'verified';
        }
        pushEvent({ event_id: randomHex(), entity_type: 'party', entity_id: partyId, revision: 1, event_type: 'party.erased', payload: { erased: true } });
        summary = 'Party-scoped records were erased. Independent source records remain available where they are shared.';
      }
      const jobId = randomHex();
      adminJobs.set(jobId, { id: jobId, operation: preview.operation, status: 'succeeded', summary, new_epoch: epoch, last_error: null });
      const result = { job_id: jobId, status: 'queued' };
      receipts.set(receiptKey, { digest: commandDigest, result });
      return json(response, 202, result);
    }
    const adminJobMatch = /^\/api\/v1\/admin\/jobs\/([^/]+)$/.exec(url.pathname);
    if (adminJobMatch && request.method === 'GET') {
      const job = adminJobs.get(adminJobMatch[1]!);
      return job ? json(response, 200, job) : fail(response, 404, 'ADMIN_JOB_NOT_FOUND', 'Administrative job does not exist in this session');
    }

    if (url.pathname === '/api/v1/template-families' && request.method === 'GET') return json(response, 200, { items: templateFamilies });
    if (url.pathname === '/api/v1/template-families' && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const family = { id: randomHex(), name: String(input.name) }; templateFamilies.push(family); return json(response, 201, family);
    }
    if (url.pathname === '/api/v1/templates' && request.method === 'GET') return json(response, 200, writingTemplates);
    if (url.pathname === '/api/v1/templates' && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const family = { id: randomHex(), name: String(input.name ?? 'Template') };
      templateFamilies.push(family);
      const template = { id: randomHex(), family_id: family.id, version: 1, status: 'draft', name: String(input.name), body: String(input.body), sentence_rules: [], stage_class: 'identified', channel: 'email', parent_version_id: null };
      writingTemplates.unshift(template);
      return json(response, 201, template);
    }
    const templateVersionMatch = /^\/api\/v1\/templates\/([^/]+)\/versions$/.exec(url.pathname);
    if (templateVersionMatch && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const template = { id: randomHex(), family_id: templateVersionMatch[1], version: 1, status: 'draft', ...input, parent_version_id: input.parent_version_id ?? null }; writingTemplates.unshift(template); return json(response, 201, template);
    }
    const templateTransition = /^\/api\/v1\/templates\/([^/]+)\/(approve|activate)$/.exec(url.pathname);
    if (templateTransition && request.method === 'POST') { const template = writingTemplates.find((item) => item.id === templateTransition[1]); if (!template) return fail(response, 404, 'TEMPLATE_NOT_FOUND', 'Template does not exist'); template.status = templateTransition[2] === 'approve' ? 'approved' : 'active'; response.writeHead(204); response.end(); return; }
    if (url.pathname === '/api/v1/embedding-model' && request.method === 'GET') return json(response, 200, { model: embeddingModel, model_digest: null, calibrated_kinds: [] });
    if (url.pathname === '/api/v1/embedding-model' && request.method === 'PUT') { let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); } embeddingModel = String(input.model); return json(response, 200, { model: embeddingModel, model_digest: null, calibration_required: true }); }
    if (url.pathname === '/api/v1/voice-samples' && request.method === 'GET') return json(response, 200, { items: voiceSamples });
    if (url.pathname === '/api/v1/voice-samples' && request.method === 'POST') { let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); } const sample = { id: randomHex(), ...input, revision: 1, status: 'active' }; voiceSamples.push(sample); return json(response, 201, sample); }
    const voiceRetire = /^\/api\/v1\/voice-samples\/([^/]+)\/retire$/.exec(url.pathname);
    if (voiceRetire && request.method === 'POST') { const sample = voiceSamples.find((item) => item.id === voiceRetire[1]); if (!sample) return fail(response, 404, 'VOICE_SAMPLE_NOT_FOUND', 'Voice sample does not exist'); sample.status = 'retired'; return json(response, 200, { id: sample.id, status: 'retired', revision: 2 }); }
    if (url.pathname === '/api/v1/matching/calibrations' && request.method === 'GET') return json(response, 200, { model: embeddingModel, model_digest: null, scoring_config_digest: null, calibrations: [] });
    const writingInterestsMatch = /^\/api\/v1\/opportunities\/([^/]+)\/interests$/.exec(url.pathname);
    if (writingInterestsMatch && request.method === 'GET') return json(response, 200, { opportunity_id: writingInterestsMatch[1], recipient_id: url.searchParams.get('recipient_id'), items: (writingInterests.get(`${writingInterestsMatch[1]}:${url.searchParams.get('recipient_id')}`) ?? []).map((item) => ({ ...item, claim: null })) });
    if (writingInterestsMatch && request.method === 'POST') { let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); } const key = `${writingInterestsMatch[1]}:${String(input.recipient_id)}`; const item = { id: randomHex(), revision: 1, status: 'active', ...input, check_suggestion: input.check_suggestion ?? null, claim_id: null, claim_revision: null }; writingInterests.set(key, [...(writingInterests.get(key) ?? []), item]); return json(response, 201, item); }
    const writingExchangeMatch = /^\/api\/v1\/opportunities\/([^/]+)\/exchange(?:\/([^/]+)\/attest)?$/.exec(url.pathname);
    if (writingExchangeMatch && request.method === 'GET') { const versions = writingExchanges.get(`${writingExchangeMatch[1]}:${url.searchParams.get('recipient_id')}`) ?? []; return json(response, 200, { opportunity_id: writingExchangeMatch[1], recipient_id: url.searchParams.get('recipient_id'), current: versions.at(-1) ?? null, versions }); }
    if (writingExchangeMatch && url.pathname.endsWith('/attest') && request.method === 'POST') { const recipient = writingExchangeMatch[2]!; const versions = writingExchanges.get(`${writingExchangeMatch[1]}:${recipient}`) ?? []; const current = versions.at(-1); if (!current) return fail(response, 404, 'EXCHANGE_MISSING', 'Exchange does not exist'); current.fairness_attested = true; return json(response, 200, { version: current.version, fairness_attested: true }); }
    if (writingExchangeMatch && request.method === 'POST') { let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); } const key = `${writingExchangeMatch[1]}:${String(input.recipient_id)}`; const versions = writingExchanges.get(key) ?? []; const version = { version: versions.length + 1, they_get: input.they_get, i_get: input.i_get, fairness_attested: false, interest_ids: input.interest_ids, interests: [], attestation_digest: null, attested_at: null }; versions.push(version); writingExchanges.set(key, versions); return json(response, 201, version); }
    const writingMatch = /^\/api\/v1\/opportunities\/([^/]+)\/matches$/.exec(url.pathname);
    if (writingMatch && request.method === 'POST') return fail(response, 503, 'EMBED_MODEL_UNAVAILABLE', 'No local embedding provider is available. Manual resource selection remains available.');
    if (writingMatch && request.method === 'GET') return fail(response, 404, 'MATCH_NOT_FOUND', 'No completed match exists for this opportunity and recipient.');
    if (/^\/api\/v1\/templates\/build$/.test(url.pathname) && request.method === 'POST') return fail(response, 503, 'MODEL_UNAVAILABLE', 'Template Builder is unavailable; create a manual template version.');
    if (/^\/api\/v1\/opportunities\/[^/]+\/materials\/compose$/.test(url.pathname) && request.method === 'POST') return fail(response, 503, 'MODEL_UNAVAILABLE', 'Composer is unavailable; save an owner-authored draft.');

    if (url.pathname === '/api/v1/agent/question-bank' && request.method === 'GET') return json(response, 200, questionBank);
    if (url.pathname === '/api/v1/agent/question-bank' && request.method === 'PUT') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      if (input.expected_version_id !== questionBank.version_id || !input.content || typeof input.content !== 'object') return fail(response, 409, 'QUESTION_BANK_STALE', 'Reload the active bank before editing');
      questionBank.version_id = randomHex(); questionBank.content = input.content as typeof questionBank.content; return json(response, 200, { version_id: questionBank.version_id, sha256: 'c'.repeat(64) });
    }
    const promptMatch = /^\/api\/v1\/agent\/prompts\/(interviewer|briefer)$/.exec(url.pathname);
    if (promptMatch && request.method === 'GET') return json(response, 200, prompts.get(promptMatch[1]!)!);
    if (promptMatch && request.method === 'PUT') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const current = prompts.get(promptMatch[1]!)!; if (input.expected_version_id !== current.version_id) return fail(response, 409, 'PROMPT_STALE', 'Reload the active prompt before editing');
      const next = { ...current, version: Number(current.version) + 1, version_id: randomHex(), content: String(input.content), sha256: 'd'.repeat(64) }; prompts.set(promptMatch[1]!, next); return json(response, 200, next);
    }
    if (url.pathname === '/api/v1/agent/chats' && request.method === 'GET') {
      const role = url.searchParams.get('role'); const scopeKind = url.searchParams.get('scope_kind'); const scopeId = url.searchParams.get('scope_id');
      return json(response, 200, [...agentChats.values()].filter((chat) => (!role || chat.role === role) && (!scopeKind || chat.scope_kind === scopeKind) && (!scopeId || chat.scope_id === scopeId)).map((chat) => ({ id: chat.id, role: chat.role, scope_kind: chat.scope_kind, scope_id: chat.scope_id, asset_kind: chat.asset_kind, revision: chat.revision, last_turn: (chat.turns as RecordData[]).at(-1)?.text ?? null, updated_at: now(), active_job_id: chat.active_job_id })));
    }
    if (url.pathname === '/api/v1/agent/chats' && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const id = randomHex(); const chat = { id, role: input.role, scope_kind: input.scope_kind, scope_id: input.scope_id ?? null, asset_kind: input.asset_kind ?? null, revision: 1, active_job_id: null, turns: [], questions: [], candidates: [] };
      agentChats.set(id, chat); return json(response, 201, chat);
    }
    if (url.pathname === '/api/v1/segments' && request.method === 'GET') return json(response, 200, [...segments.values()].sort((a, b) => String(a.name).localeCompare(String(b.name))));
    if (url.pathname === '/api/v1/segments' && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      if (typeof input.name !== 'string' || !input.name.trim()) return fail(response, 422, 'SEGMENT_INVALID', 'Segment needs a name');
      const segment = { id: randomHex(), name: input.name.trim(), description: typeof input.description === 'string' ? input.description : null, revision: 1 }; segments.set(segment.id, segment); segmentMembers.set(segment.id, new Set()); return json(response, 201, segment);
    }
    const segmentMember = /^\/api\/v1\/segments\/([^/]+)\/parties$/.exec(url.pathname);
    if (segmentMember && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const members = segmentMembers.get(segmentMember[1]!); if (!members || !directory.has(String(input.party_id))) return fail(response, 404, 'SEGMENT_OR_PARTY_NOT_FOUND', 'Segment or party does not exist');
      if (input.action === 'add') members.add(String(input.party_id)); else if (input.action === 'remove') members.delete(String(input.party_id)); else return fail(response, 422, 'SEGMENT_ACTION_INVALID', 'Action must be add or remove');
      return json(response, 200, { segment_id: segmentMember[1], party_id: input.party_id, action: input.action });
    }
    if (url.pathname === '/api/v1/fluency' && request.method === 'GET') return json(response, 200, [...fluency.values()].sort((a, b) => a.term.localeCompare(b.term)));
    const fluencyMatch = /^\/api\/v1\/fluency\/([^/]+)$/.exec(url.pathname);
    if (fluencyMatch && request.method === 'PUT') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      if (!['unknown', 'learning', 'fluent'].includes(String(input.status))) return fail(response, 422, 'FLUENCY_INVALID', 'Unknown fluency state');
      const term = decodeURIComponent(fluencyMatch[1]!); const old = fluency.get(term);
      if (old && input.expected_revision !== old.revision) return fail(response, 409, 'FLUENCY_STALE', 'Vocabulary fluency changed');
      const updated = { term, status: input.status as 'unknown' | 'learning' | 'fluent', revision: (old?.revision ?? 0) + 1 }; fluency.set(term, updated); return json(response, 200, updated);
    }
    const chatPath = /^\/api\/v1\/agent\/chats\/([^/]+)(?:\/(turns|questions))?$/.exec(url.pathname);
    if (chatPath) {
      const chat = agentChats.get(chatPath[1]!); if (!chat) return fail(response, 404, 'CHAT_NOT_FOUND', 'Chat does not exist');
      if (!chatPath[2] && request.method === 'GET') return json(response, 200, chat);
      if (chatPath[2] === 'questions' && request.method === 'POST') {
        let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
        const state = { question_id: String(input.question_id), state: input.action === 'ask' ? 'asked' : input.action === 'skip' ? 'skipped' : input.action === 'defer' ? 'deferred' : 'unknown' };
        const questions = chat.questions as RecordData[]; const index = questions.findIndex((item) => item.question_id === state.question_id); if (index < 0) questions.push(state); else questions[index] = state;
        if (input.action === 'ask') {
          const assetKind = chat.asset_kind as 'proof' | 'gift' | 'artifact';
          (chat.turns as RecordData[]).push({ id: randomHex(), turn_no: (chat.turns as RecordData[]).length + 1, speaker: 'agent', text: questionBank.content.questions[assetKind]?.find((item) => item.id === input.question_id)?.text ?? 'What would you like to share?', job_id: null, created_at: now() });
        }
        chat.revision = Number(chat.revision) + 1; return json(response, 200, { chat_id: chat.id, ...state, revision: chat.revision });
      }
      if (chatPath[2] === 'turns' && request.method === 'POST') {
        let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
        const turns = chat.turns as RecordData[]; turns.push({ id: randomHex(), turn_no: turns.length + 1, speaker: 'owner', text: String(input.text), job_id: null, created_at: now() });
        const jobId = randomHex(); const interviewer = chat.role === 'interviewer'; const candidateId = randomHex();
        turns.push({ id: randomHex(), turn_no: turns.length + 1, speaker: 'agent', text: interviewer ? 'What context and time period should I record?' : 'I will review only this scoped material. What should we clarify?', job_id: jobId, created_at: now() });
        if (interviewer) (chat.candidates as RecordData[]).push({ id: candidateId, asset_kind: chat.asset_kind, fields: {}, owner_quotes: {}, missing_fields: ['what'], state: 'proposed', resource_id: null });
        chat.revision = Number(chat.revision) + 1; chat.active_job_id = jobId; roleJobs.set(jobId, { id: jobId, role: interviewer ? 'interviewer' : 'briefer', status: 'succeeded', result: interviewer ? { candidate_id: candidateId } : { response: 'I will review only this scoped material.' }, last_error: null });
        return json(response, 202, { chat_id: chat.id, turn_id: turns.at(-2)!.id, job_id: jobId, revision: chat.revision });
      }
    }
    const candidateMatch = /^\/api\/v1\/agent\/candidates\/([^/]+)\/(confirm|dismiss)$/.exec(url.pathname);
    if (candidateMatch && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      for (const chat of agentChats.values()) {
        const candidate = (chat.candidates as RecordData[]).find((item) => item.id === candidateMatch[1]);
        if (candidate) {
          if (candidateMatch[2] === 'dismiss') {
            candidate.state = 'dismissed';
            return json(response, 200, { candidate_id: candidate.id, state: candidate.state });
          }
          const id = randomHex();
          const title = String(input.title ?? '').trim();
          if (!title || !input.fields || typeof input.fields !== 'object') return fail(response, 422, 'CANDIDATE_INVALID', 'Enter a title and typed fields');
          if (input.column_id && !applyPlacement(id, { column_id: input.column_id })) return fail(response, 422, 'INVALID_REQUEST', 'Choose a column on the resource board');
          const card: ResourceEntityRecord = { id, kind: 'resource', name: title, subtype: String(candidate.asset_kind), focus: '',
            body: input.fields as RecordData, revision: 1, readiness: 'draft', notes: { markdown: '', revision: 1 }, etag: '' };
          card.etag = entityTag(card);
          entityResources.set(id, card);
          candidate.state = 'saved';
          candidate.resource_id = id;
          candidate.fields = input.fields;
          return json(response, 201, { id, kind: card.subtype, title, body: card.body, readiness: card.readiness });
        }
      }
      return fail(response, 404, 'CANDIDATE_NOT_FOUND', 'Candidate does not exist');
    }
    if (url.pathname === '/api/v1/jobs' && request.method === 'GET') return json(response, 200, []);
    const roleJobMatch = /^\/api\/v1\/jobs\/([^/]+)$/.exec(url.pathname);
    if (roleJobMatch && request.method === 'GET' && roleJobs.has(roleJobMatch[1]!)) return json(response, 200, roleJobs.get(roleJobMatch[1]!)!);
    const briefMatch = /^\/api\/v1\/briefs\/(party|segment)\/([^/]+)\/items$/.exec(url.pathname);
    if (briefMatch && request.method === 'GET') return json(response, 200, briefItems.get(`${briefMatch[1]}:${briefMatch[2]}`) ?? []);
    if (briefMatch && request.method === 'POST') {
      const key = `${briefMatch[1]}:${briefMatch[2]}`; const itemId = randomHex(); const item = { id: itemId, scope_kind: briefMatch[1], scope_id: briefMatch[2], kind: 'jargon', text: 'Example term', detail: { plain_meaning: 'Example', use_or_avoid: 'owner_decides', applies_to: 'org' }, tier: 2, quote: null, check_suggestion: 'Check recent professional posts', speaker_kind: 'model_prior', status: 'proposed', revision: 1, supersedes_id: null };
      briefItems.set(key, [item]); fluency.set(item.text, { term: item.text, status: 'unknown', revision: 1 }); const jobId = randomHex(); roleJobs.set(jobId, { id: jobId, role: 'briefer', status: 'succeeded', result: { item_count: 1 }, last_error: null }); return json(response, 202, { job_id: jobId, scope_kind: briefMatch[1], scope_id: briefMatch[2], state: 'queued' });
    }
    const briefDecision = /^\/api\/v1\/brief-items\/([^/]+)\/decision$/.exec(url.pathname);
    if (briefDecision && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      for (const items of briefItems.values()) { const item = items.find((candidate) => candidate.id === briefDecision[1]); if (item) { item.status = String(input.decision === 'correct' ? 'corrected' : input.decision === 'confirm' ? 'confirmed' : 'dismissed'); item.revision = Number(item.revision) + 1; return json(response, 200, { id: item.id, decision: input.decision, revision: item.revision }); } }
      return fail(response, 404, 'BRIEF_ITEM_NOT_FOUND', 'Brief item does not exist');
    }

    if (url.pathname === '/api/v1/graph' && request.method === 'GET') {
      const centerId = url.searchParams.get('center_id');
      const viewScope = url.searchParams.get('view_scope') ?? 'leads';
      const viewId = url.searchParams.get('view_id') ?? 'all';
      const allEntities = new Map<string, RecordData>(directory);
      for (const [id, res] of entityResources) {
        if (res.readiness === 'retired') continue;
        allEntities.set(id, { id, kind: 'resource', name: String(res.name), focus: '', revision: Number(res.revision) });
      }
      const relatedIds = new Set<string>(centerId ? [centerId] : allEntities.keys());
      for (const edge of graphEdges) {
        if (edge.state === 'retracted') continue;
        if (edge.from_id === centerId) relatedIds.add(String(edge.to_id));
        if (edge.to_id === centerId) relatedIds.add(String(edge.from_id));
      }
      const selected = [...allEntities.values()].filter(entity => relatedIds.has(String(entity.id)))
        .sort((a, b) => Number(b.id === centerId) - Number(a.id === centerId) || String(a.name).localeCompare(String(b.name))).slice(0, 200);
      const nodes: RecordData[] = selected.map((entity, index) => {
        const id = String(entity.id);
        const key = `${viewScope}:${viewId}:${id}`;
        const position = graphPositions.get(key) ?? { x: (index % 8) * 240, y: Math.floor(index / 8) * 160, revision: 0 };
        return { ...entity, id, x: position.x, y: position.y, position_revision: position.revision };
      });
      const visible = new Set(nodes.map((node) => String(node.id)));
      const scopedEdges = graphEdges.filter(edge => edge.state !== 'retracted' && visible.has(String(edge.from_id)) && visible.has(String(edge.to_id)));
      const edges = scopedEdges.slice(0, 400);
      return json(response, 200, { nodes, edges, omitted_nodes: Math.max(0, relatedIds.size - nodes.length), omitted_edges: Math.max(0, scopedEdges.length - edges.length) });
    }
    if (url.pathname === '/api/v1/graph/layout' && request.method === 'PUT') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      if (!Array.isArray(input.positions) || !input.positions.length || input.positions.length > 200) return fail(response, 422, 'GRAPH_LAYOUT_INVALID', 'Position batch is outside its bounds');
      const saved: RecordData[] = [];
      for (const value of input.positions as RecordData[]) {
        const entityId = String(value.entity_id); const scope = String(input.view_scope); const viewId = String(input.view_id);
        const key = `${scope}:${viewId}:${entityId}`; const current = graphPositions.get(key) ?? { x: 0, y: 0, revision: 0 };
        if (!directory.has(entityId) || Number(value.expected_revision) !== current.revision || !Number.isFinite(value.x) || !Number.isFinite(value.y)) return fail(response, 412, 'GRAPH_LAYOUT_STALE', 'Reload the graph before saving positions');
        const next = { x: Number(value.x), y: Number(value.y), revision: current.revision + 1 }; graphPositions.set(key, next);
        saved.push({ entity_id: entityId, ...next });
      }
      return json(response, 200, { positions: saved });
    }
    if (url.pathname === '/api/v1/graph/related' && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const commandKey = request.headers['idempotency-key'];
      if (typeof commandKey !== 'string' || !commandKey || commandKey.length > 100) return fail(response, 400, 'IDEMPOTENCY_KEY_REQUIRED', 'Idempotency-Key is required');
      const commandDigest = createHash('sha256').update(JSON.stringify(input)).digest('hex');
      const prior = receipts.get(commandKey);
      if (prior) return prior.digest === commandDigest ? json(response, 201, prior.result) : fail(response, 409, 'KEY_REUSED', 'Idempotency key was used for a different command');
      const parent = directory.get(String(input.related_entity_id));
      const source = sourceTextByOccurrence.get(String(input.occurrence_id));
      if (parent && input.expected_parent_revision !== undefined && Number(input.expected_parent_revision) !== parent.revision) {
        return fail(response, 412, 'STALE_REVISION', 'The parent changed.');
      }
      const targetIsParent = input.direction === 'incoming' || (input.direction === undefined && input.kind === 'person');
      const childKind = String(input.kind);
      const parentKind = parent?.kind ?? '';
      const allowed = allowedAssertion(targetIsParent ? childKind : parentKind, String(input.verb), targetIsParent ? parentKind : childKind);
      const staged = Array.isArray(input.staged_upload_ids) ? input.staged_upload_ids.map(id => uploads.get(String(id))) : [];
      if (!parent || !source || !allowed || typeof input.name !== 'string' || !input.name.trim() || typeof input.quote !== 'string' || !input.quote.trim() || !source.text.includes(input.quote) || staged.some(upload => !upload)) {
        return fail(response, 422, 'RELATIONSHIP_REJECTED', 'Entity, direction, source, quote and files must be valid');
      }
      const entity: EntityRecord = { id: randomHex(), kind: childKind, name: input.name.trim(), focus: '', revision: 1 };
      directory.set(entity.id, entity);
      if (typeof input.notes === 'string') notesByEntity.set(entity.id, { markdown: input.notes.replace(/\r\n?/g, '\n'), revision: 1 });
      entityAttachments.set(entity.id, staged.flatMap(upload => upload ? [{ id: randomHex(), upload_id: upload.id, filename: upload.filename, size: upload.bytes.length, sha256: upload.sha256, content_type: upload.mime, reviewed_by_owner: false, share_permission: false, created_at: now() }] : []));
      graphEdges.push({ id: randomHex(), from_id: targetIsParent ? entity.id : parent.id, to_id: targetIsParent ? parent.id : entity.id, verb: String(input.verb), revision: 1, occurrence_id: String(input.occurrence_id), quote: input.quote, tier: 1, speaker_kind: 'unknown', captured_at: now(), source_title: source.title, source_url: source.url });
      const key = `${String(input.view_scope)}:${String(input.view_id)}:${entity.id}`;
      graphPositions.set(key, { x: Number(input.x), y: Number(input.y), revision: 1 });
      pushEvent({ event_id: randomHex(), entity_type: 'edge', entity_id: entity.id, revision: 1, event_type: 'edge.created', payload: {} });
      receipts.set(commandKey, { digest: commandDigest, result: entity });
      return json(response, 201, entity);
    }

    const edgeRoute = /^\/api\/v1\/edges(?:\/([^/]+))?$/.exec(url.pathname);
    if (edgeRoute && ['POST', 'PATCH', 'DELETE'].includes(request.method ?? '')) {
      const input = await readBody(request);
      const id = edgeRoute[1];
      const key = request.headers['idempotency-key'];
      const digest = importDigest([epoch, request.method, url.pathname, input]);
      if (typeof key !== 'string' || !key) return fail(response, 400, 'IDEMPOTENCY_KEY_REQUIRED', 'Operation key required.');
      const code = request.method === 'POST' ? 201 : request.method === 'PATCH' ? 200 : 204;
      const prior = receipts.get(key);
      if (prior) return prior.digest === digest ? json(response, code, prior.result) : fail(response, 409, 'KEY_REUSED', 'Operation key already used.');
      const target = graphEdges.find(edge => edge.id === id);
      if (id && (!target || target.revision !== input.expected_revision)) return fail(response, 412, 'STALE_REVISION', 'The relationship changed.');
      if (target && code === 204) {
        graphEdges.splice(graphEdges.indexOf(target), 1);
        receipts.set(key, { digest, result: {} });
        return json(response, 204, null);
      }
      const from = directory.get(String(input.from_id));
      const to = directory.get(String(input.to_id));
      const source = sourceTextByOccurrence.get(String(input.occurrence_id));
      if (!from || !to || from.revision !== input.expected_from_revision || to.revision !== input.expected_to_revision) return fail(response, 412, 'STALE_REVISION', 'An endpoint changed.');
      if (from.id === to.id || !allowedAssertion(from.kind, String(input.verb), to.kind) || !source || typeof input.quote !== 'string' || !input.quote.trim() || !source.text.includes(input.quote)) {
        return fail(response, 422, 'RELATIONSHIP_REJECTED', 'Choose a valid relationship and exact source quote.');
      }
      const duplicate = graphEdges.find(edge => edge.from_id === from.id && edge.to_id === to.id && edge.verb === input.verb);
      if (target && duplicate && target !== duplicate) return fail(response, 422, 'RELATIONSHIP_REJECTED', 'Relationship already exists.');
      const current = target ?? duplicate;
      const changed = !!target || !current || current.quote !== input.quote || current.occurrence_id !== input.occurrence_id;
      const edge = { id: current?.id ?? randomHex(), from_id: from.id, to_id: to.id, verb: input.verb,
        revision: current ? Number(current.revision) + Number(changed) : 1,
        occurrence_id: input.occurrence_id, quote: input.quote, tier: 1, speaker_kind: 'unknown', captured_at: now(), source_title: source.title, source_url: source.url };
      if (current && changed) Object.assign(current, edge);
      if (!current) graphEdges.push(edge);
      const result = { data: { id: edge.id, from_id: edge.from_id, to_id: edge.to_id, verb: edge.verb, revision: edge.revision } };
      receipts.set(key, { digest, result });
      return json(response, code, result);
    }

    if (url.pathname === '/api/v1/directory' && request.method === 'GET') {
      const params = url.searchParams;
      const kind = params.get('kind');
      const query = params.get('q')?.toLowerCase() ?? '';
      const sort = params.get('sort') ?? 'name';
      const limit = Number(params.get('limit') ?? 50);
      const offset = Number(params.get('cursor') ?? 0);
      const batchId = params.get('batch_id');
      if (!Number.isInteger(limit) || limit < 1 || limit > 200 || !Number.isInteger(offset) || offset < 0) return fail(response, 422, 'DIRECTORY_INVALID', 'Invalid directory page.');
      const value = (row: EntityRecord) => sort === 'type' ? row.kind : sort === 'focus' ? row.focus : row.name;
      const entries = [...directory.values()].filter(row => (kind ? row.kind === kind : ['org', 'person'].includes(row.kind))
        && (row.name.toLowerCase().includes(query) || row.focus.toLowerCase().includes(query))
        && (!batchId || partyImportOrigins.get(row.id)?.batch_id === batchId));
      entries.sort((left, right) => value(left).toLowerCase().localeCompare(value(right).toLowerCase()) || left.id.localeCompare(right.id));
      const next = offset + limit;
      return json(response, 200, { data: entries.slice(offset, next).map(row => ({ ...row, etag: entityTag(row) })), next_cursor: next < entries.length ? String(next) : null, cursor: String(events.length) });
    }
    if (url.pathname === '/api/v1/entities' && request.method === 'GET') {
      const kinds = url.searchParams.getAll('kind');
      const resourceKinds = kinds.filter((value) => ['gift', 'proof', 'artifact', 'resource'].includes(value));
      const graphKinds = kinds.filter((value) => ['person', 'org', 'project'].includes(value));
      if (kinds.some((value) => !resourceKinds.includes(value) && !graphKinds.includes(value))) {
        return fail(response, 422, 'INVALID_KIND', 'Only workspace entity kinds can be listed in this slice');
      }
      const items: RecordData[] = [];
      if (resourceKinds.length || kinds.length === 0) {
        items.push(...[...resources.values()].sort((a, b) => b.updated_at.localeCompare(a.updated_at) || a.id.localeCompare(b.id)));
      }
      if (graphKinds.length || kinds.length === 0) {
        items.push(...[...directory.values()].sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id)).map((entity) => ({ ...entity, etag: entityTag(entity) })));
      }
      return json(response, 200, { data: items, cursor: String(events.length) });
    }
    if (url.pathname === '/api/v1/entities' && request.method === 'POST') {
      let input: RecordData;
      try {
        input = await readBody(request);
      } catch {
        return fail(response, 400, 'INVALID_JSON', 'Request body is invalid');
      }
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || key.length === 0 || key.length > 100) {
        return fail(response, 400, 'IDEMPOTENCY_KEY_REQUIRED', 'Idempotency-Key is required');
      }
      const digest = createHash('sha256').update(JSON.stringify(input)).digest('hex');
      const receipt = receipts.get(key);
      if (receipt) {
        return receipt.digest === digest
          ? json(response, 201, { data: receipt.result })
          : fail(response, 409, 'KEY_REUSED', 'Idempotency key was used for a different command');
      }
      if (['person', 'org', 'project', 'resource'].includes(String(input.kind))) {
        const name = typeof input.name === 'string' ? input.name.trim() : '';
        if (!name || name.length > 500) {
          return fail(response, 422, 'ENTITY_INVALID', 'Entity type or name is invalid');
        }
        const focus = typeof input.focus === 'string' ? input.focus : '';
        if (focus.length > 250) {
          return fail(response, 422, 'ENTITY_INVALID', 'Entity focus is invalid');
        }
        const staged = Array.isArray(input.staged_upload_ids) ? input.staged_upload_ids.map(id => uploads.get(String(id))) : [];
        if (staged.some(upload => !upload)) return fail(response, 422, 'UPLOAD_UNAVAILABLE', 'Reupload the file.');
        const entity: EntityRecord = { id: randomHex(), kind: String(input.kind), name, focus, revision: 1 };
        directory.set(entity.id, entity);
        if (typeof input.notes === 'string') notesByEntity.set(entity.id, { markdown: input.notes.replace(/\r\n?/g, '\n'), revision: 1 });
        entityAttachments.set(entity.id, staged.flatMap(upload => upload ? [{ id: randomHex(), upload_id: upload.id, filename: upload.filename, size: upload.bytes.length, sha256: upload.sha256, content_type: upload.mime, reviewed_by_owner: false, share_permission: false, created_at: now() }] : []));
        pushEvent({ event_id: randomHex(), entity_type: 'entity', entity_id: entity.id, revision: 1, event_type: 'entity.created', payload: {} });
        receipts.set(key, { digest, result: { ...entity, etag: entityTag(entity) } });
        return json(response, 201, { data: { ...entity, etag: entityTag(entity) } });
      }
      const validBody = Boolean(input.body) && !Array.isArray(input.body) && typeof input.body === 'object';
      if (!['gift', 'proof', 'artifact'].includes(String(input.kind))
        || typeof input.title !== 'string'
        || input.title.length > 240
        || !validBody) {
        return fail(response, 422, 'RESOURCE_INVALID', 'Resource type or name is invalid');
      }
      const resource: ResourceRecord = {
        id: randomHex(),
        kind: String(input.kind),
        title: input.title,
        body: input.body as RecordData,
        readiness: 'draft',
        revision: 1,
        etag: '',
        updated_at: now(),
      };
      resource.etag = tag(resource);
      resources.set(resource.id, resource);
      syncResourceCard(resource);
      pushEvent({ event_id: randomHex(), entity_type: 'resource', entity_id: resource.id, revision: 1, event_type: 'resource.created', payload: { kind: resource.kind } });
      receipts.set(key, { digest, result: resource });
      return json(response, 201, { data: resource });
    }
    const entityMatch = /^\/api\/v1\/entities\/([a-f0-9]+)$/.exec(url.pathname);
    if (entityMatch && request.method === 'GET') {
      const resource = resources.get(entityMatch[1]!);
      if (resource) return json(response, 200, { data: resource });
      const entity = directory.get(entityMatch[1]!);
      if (entity) return json(response, 200, { data: { ...entity, etag: entityTag(entity) } });
      const boardEntity = entityResources.get(entityMatch[1]!);
      if (boardEntity) return json(response, 200, { data: boardEntity });
      return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
    }
    if (entityMatch && request.method === 'PATCH') {
      let input: RecordData;
      try {
        input = await readBody(request);
      } catch {
        return fail(response, 400, 'INVALID_JSON', 'Request body is invalid');
      }
      const ifMatch = request.headers['if-match'];
      if (typeof ifMatch !== 'string') return fail(response, 428, 'ETAG_REQUIRED', 'If-Match is required');
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || key.length === 0 || key.length > 100) {
        return fail(response, 400, 'IDEMPOTENCY_KEY_REQUIRED', 'Idempotency-Key is required');
      }
      const digest = createHash('sha256').update(JSON.stringify([entityMatch[1], ifMatch, input])).digest('hex');
      const receipt = receipts.get(key);
      if (receipt) {
        return receipt.digest === digest
          ? json(response, 200, { data: receipt.result })
          : fail(response, 409, 'KEY_REUSED', 'Idempotency key was used for a different command');
      }
      const resource = resources.get(entityMatch[1]!);
      if (resource) {
        if (ifMatch !== resource.etag) return fail(response, 412, 'STALE_REVISION', 'Resource changed in another session');
        const updated: ResourceRecord = {
          ...resource,
          title: String(input.title),
          body: input.body as RecordData,
          revision: resource.revision + 1,
          updated_at: now(),
        };
        updated.etag = tag(updated);
        resources.set(updated.id, updated);
        syncResourceCard(updated);
        pushEvent({ event_id: randomHex(), entity_type: 'resource', entity_id: updated.id, revision: updated.revision, event_type: 'resource.updated', payload: {} });
        receipts.set(key, { digest, result: updated });
        return json(response, 200, { data: updated });
      }
      const entity = directory.get(entityMatch[1]!);
      if (!entity) return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
      if (ifMatch !== entityTag(entity)) return fail(response, 412, 'STALE_REVISION', 'Resource changed in another session');
      const name = typeof input.name === 'string' ? input.name.trim() : entity.name;
      const focus = typeof input.focus === 'string' ? input.focus : entity.focus;
      if (input.name !== undefined && (!name || name.length > 500)) return fail(response, 422, 'ENTITY_INVALID', 'Entity name is invalid');
      if (input.focus !== undefined && focus.length > 250) return fail(response, 422, 'ENTITY_INVALID', 'Entity focus is invalid');
      const updated: EntityRecord = { ...entity, name, focus, revision: entity.revision + 1 };
      directory.set(updated.id, updated);
      pushEvent({ event_id: randomHex(), entity_type: 'entity', entity_id: updated.id, revision: updated.revision, event_type: 'entity.updated', payload: {} });
      receipts.set(key, { digest, result: { ...updated, etag: entityTag(updated) } });
      return json(response, 200, { data: { ...updated, etag: entityTag(updated) } });
    }
    const entityNotesMatch = /^\/api\/v1\/entities\/([a-f0-9]+)\/notes$/.exec(url.pathname);
    if (entityNotesMatch && request.method === 'GET') {
      const entity = directory.get(entityNotesMatch[1]!);
      if (!entity) return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
      const notes = notesByEntity.get(entity.id) ?? { markdown: '', revision: 1 };
      return json(response, 200, { data: { entity_id: entity.id, markdown: notes.markdown, revision: notes.revision, etag: notesTag(entity.id, notes.revision) } });
    }
    if (entityNotesMatch && request.method === 'PUT') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const ifMatch = request.headers['if-match'];
      if (typeof ifMatch !== 'string') return fail(response, 428, 'ETAG_REQUIRED', 'If-Match is required');
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || key.length === 0 || key.length > 100) return fail(response, 400, 'IDEMPOTENCY_KEY_REQUIRED', 'Idempotency-Key is required');
      const entity = directory.get(entityNotesMatch[1]!);
      if (!entity) return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
      const digest = createHash('sha256').update(JSON.stringify([entity.id, ifMatch, input])).digest('hex');
      const previous = receipts.get(key);
      if (previous) return previous.digest === digest ? json(response, 200, { data: previous.result }) : fail(response, 409, 'KEY_REUSED', 'Operation key belongs to another command.');
      const current = notesByEntity.get(entity.id) ?? { markdown: '', revision: 1 };
      if (ifMatch !== notesTag(entity.id, current.revision) || Number(input.expected_notes_rev) !== current.revision) return fail(response, 412, 'STALE_REVISION', 'Notes changed in another session');
      const markdown = typeof input.markdown === 'string' ? input.markdown.replace(/\r\n/g, '\n').replace(/\r/g, '\n') : '';
      const next = { markdown, revision: current.revision + 1 };
      notesByEntity.set(entity.id, next);
      pushEvent({ event_id: randomHex(), entity_type: 'entity', entity_id: entity.id, revision: next.revision, event_type: 'entity.notes_updated', payload: {} });
      const result = { entity_id: entity.id, markdown, revision: next.revision, etag: notesTag(entity.id, next.revision) };
      receipts.set(key, { digest, result });
      return json(response, 200, { data: result });
    }
    const dossierMatch = /^\/api\/v1\/entities\/([a-f0-9]+)\/dossier$/.exec(url.pathname);
    if (dossierMatch && request.method === 'GET') {
      const entity = directory.get(dossierMatch[1]!);
      if (!entity) return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
      const visible = new Set<string>([entity.id]);
      for (const edge of graphEdges) {
        if (edge.from_id === entity.id) visible.add(String(edge.to_id));
        if (edge.to_id === entity.id) visible.add(String(edge.from_id));
      }
      const direct_links = graphEdges
        .filter((edge) => edge.from_id === entity.id || edge.to_id === entity.id)
        .slice(0, 60)
        .map((edge) => ({ id: String(edge.id), from_id: String(edge.from_id), to_id: String(edge.to_id), verb: String(edge.verb), revision: Number(edge.revision), tier: Number(edge.tier), wording: String(edge.quote) }));
      const files = (entityAttachments.get(entity.id) ?? []).slice(0, 60);
      const openOpportunities = [...opportunities.values()]
        .filter((item) => item.state === 'open' && Array.isArray(item.participant_ids) && item.participant_ids.includes(entity.id))
        .slice(0, 60)
        .map((item) => ({ id: String(item.id), title: String(item.title), role: 'participant' }));
      const associated_resources: RecordData[] = [];
      for (const opportunity of openOpportunities) {
        const source = Array.from(outputs.values()).find((material) => material.opportunity_id === opportunity.id) ?? null;
        if (source && typeof source === 'object' && !associated_resources.some((item) => item.id === source.id)) {
          associated_resources.push({ id: String(source.id), title: String(source.title ?? source.kind ?? source.id), kind: String(source.kind ?? 'material'), opportunity_id: opportunity.id, opportunity_title: opportunity.title });
        }
      }
      return json(response, 200, { data: { entity: { ...entity, etag: entityTag(entity) }, direct_links, files, import_origin: partyImportOrigins.get(entity.id) ?? null, opportunities: openOpportunities, associated_resources } });
    }
    if (url.pathname === '/api/v1/events' && request.method === 'GET') {
      const after = Number(url.searchParams.get('after') ?? '0');
      if (!Number.isSafeInteger(after) || after < 0) {
        return fail(response, 400, 'CURSOR_INVALID', 'Event cursor must be a non-negative decimal');
      }
      const afterCursor = url.searchParams.get('after') ?? '0';
      const entityType = url.searchParams.get('entity_type');
      const entityId = url.searchParams.get('entity_id');
      const items = events.filter((event) => Number(event.cursor) > after
        && (!entityType || event.entity_type === entityType)
        && (!entityId || event.entity_id === entityId));
      return json(response, 200, {
        items,
        next_cursor: events.at(-1)?.cursor ?? afterCursor,
        observed_cursor: String(events.length),
      });
    }
    if (url.pathname === '/api/v1/queue' && request.method === 'GET') {
      const view = url.searchParams.get('view') ?? 'sheet';
      if (view !== 'daily') {
        const cursor = Number(url.searchParams.get('cursor') ?? '0');
        const limit = Math.max(1, Math.min(200, Number(url.searchParams.get('limit') ?? '50')));
        return json(response, 200, sheetResponse(
          url.searchParams.get('search') ?? '',
          url.searchParams.get('phase'),
          url.searchParams.get('sort') ?? 'due',
          Number.isSafeInteger(cursor) && cursor >= 0 ? cursor : 0,
          limit,
        ));
      }
      const cap = activePackId === 'lead_gen' ? 15 : 12;
      const result = buildDailyQueue([...opportunities.values()], [...threads.values()], [...materialRecords.values()], cap, now(), activePackId, activePackVersion);
      const query = (url.searchParams.get('q') ?? '').trim().toLocaleLowerCase();
      const status = url.searchParams.get('status');
      const items = result.items.filter((item) => (!query || `${item.title} ${item.party_name ?? ''} ${item.reason}`.toLocaleLowerCase().includes(query))
        && (!status || [...opportunities.values()].find((opportunity) => opportunity.id === item.opportunity_id)?.state === status));
      return json(response, 200, { ...result, items });
    }
    if (url.pathname === '/api/v1/insights' && request.method === 'GET') {
      const end = new Date(testClock);
      const start = new Date(end);
      start.setUTCDate(start.getUTCDate() - ((start.getUTCDay() + 6) % 7));
      const periodStart = start.toISOString().slice(0, 10);
      const periodEndDate = new Date(start); periodEndDate.setUTCDate(periodEndDate.getUTCDate() + 6);
      return json(response, 200, buildInsights([...sends.values()], outcomes, resolutions, periodStart, periodEndDate.toISOString().slice(0, 10), now(), activePackId, activePackVersion, [...coachQuestions.values()]));
    }
    if (url.pathname === '/api/v1/coach/run' && request.method === 'POST') {
      const open = [...coachQuestions.values()].filter((question) => question.state === 'open');
      if (!open.length && opportunities.size && [...materialRecords.values()].filter((material) => material.status === 'approved').length === 0) {
        const id = randomHex();
        coachQuestions.set(id, { id, kind: 'missing_material', text: 'Which useful material could be prepared for an active opportunity?', target_kind: 'opportunity', target_id: String(opportunities.keys().next().value), impact: opportunities.size, state: 'open' });
      }
      const jobId = randomHex();
      roleJobs.set(jobId, { id: jobId, role: 'coach', status: 'succeeded', result: { snapshot: now() }, last_error: null });
      return json(response, 202, { job_id: jobId, status: 'queued' });
    }
    if (url.pathname === '/api/v1/settings' && request.method === 'GET') {
      const available = PACK_DOCUMENTS.map((pack) => ({ ...pack }));
      if (!available.some((pack) => pack.id === activePackId && pack.version === activePackVersion)) available.push({ id: activePackId, name: activePackId, version: activePackVersion, document: activePackDocument });
      return json(response, 200, { revision: packSettingsRevision, active_pack_id: activePackId, active_pack_version: activePackVersion, active_document: activePackDocument, available_packs: available });
    }
    if (url.pathname === '/api/v1/packs/validate' && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const result = validatePackDocument(String(input.document ?? ''));
      if (result.valid && result.normalized_document) validatedPacks.set(createHash('sha256').update(result.normalized_document).digest('hex'), result);
      return json(response, 200, result);
    }
    if (url.pathname === '/api/v1/packs/activate' && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      if (input.expected_settings_revision !== packSettingsRevision) return fail(response, 412, 'SETTINGS_STALE', 'Pack settings changed; reload and review the current selection');
      const document = typeof input.document === 'string' ? input.document : '';
      const digest = createHash('sha256').update(document.replace(/\r\n/g, '\n').trimEnd() + '\n').digest('hex');
      const validated = validatedPacks.get(digest);
      if (!validated || !validated.valid || validated.pack_id !== input.pack_id || validated.version !== input.version) return fail(response, 422, 'PACK_NOT_VALIDATED', 'Activate only the exact validated pack version');
      const changed = input.pack_id !== activePackId || input.version !== activePackVersion;
      const mapping = input.stage_mapping;
      if (changed && (!mapping || typeof mapping !== 'object' || Array.isArray(mapping) || Object.keys(mapping).length === 0)) return fail(response, 422, 'STAGE_MAPPING_REQUIRED', 'Map existing stage names before activating this pack version');
      activePackId = String(input.pack_id); activePackVersion = String(input.version); activePackDocument = validated.normalized_document as string; packSettingsRevision += 1;
      return json(response, 200, { revision: packSettingsRevision, active_pack_id: activePackId, active_pack_version: activePackVersion, active_document: activePackDocument, available_packs: [...PACK_DOCUMENTS.map((pack) => ({ ...pack })), ...(PACK_DOCUMENTS.some((pack) => pack.id === activePackId && pack.version === activePackVersion) ? [] : [{ id: activePackId, name: activePackId, version: activePackVersion, document: activePackDocument }])] });
    }
    const questionDismissMatch = /^\/api\/v1\/questions\/([^/]+)\/dismiss$/.exec(url.pathname);
    if (questionDismissMatch && request.method === 'POST') {
      const question = coachQuestions.get(questionDismissMatch[1]!);
      if (!question) return fail(response, 404, 'QUESTION_NOT_FOUND', 'Coach question does not exist');
      question.state = 'dismissed';
      return json(response, 200, { id: question.id, state: question.state });
    }
    // --- Resource board (R01) ---
    if (url.pathname === '/api/v1/resource-board' && request.method === 'GET') {
      if (!authorize(request, false)) return fail(response, 401, 'UNAUTHENTICATED', 'Sign in first');
      const includeOff = url.searchParams.get('include_offboard') === 'true';
      const limit = Math.min(Number(url.searchParams.get('limit') ?? 200) || 200, 500);
      const cursor = Number(url.searchParams.get('cursor') ?? 0) || 0;
      const placed: RecordData[] = [];
      for (const [resourceId, placement] of resourcePlacements) {
        const res = entityResources.get(resourceId);
        if (!res || res.readiness === 'retired') continue;
        placed.push({
          resource_id: resourceId, subtype: String(res.subtype), title: String(res.name),
          readiness: String(res.readiness), placement, counts: resourceCountsFor(resourceId),
          gaps: resourceGapsFor(resourceId), etag: entityTag(res),
        });
      }
      const columnOrder = (card: RecordData) => resourceColumns.findIndex(column => column.id === (card.placement as { column_id: string }).column_id);
      placed.sort((a, b) => columnOrder(a) - columnOrder(b) || Number((a.placement as { position: number }).position) - Number((b.placement as { position: number }).position) || String(a.resource_id).localeCompare(String(b.resource_id)));
      const page = placed.slice(cursor, cursor + limit);
      const next_cursor = cursor + limit < placed.length ? String(cursor + limit) : null;
      const off_board: RecordData[] = [];
      if (includeOff) {
        for (const [resourceId, res] of entityResources) {
          if (resourcePlacements.has(resourceId) || res.readiness === 'retired') continue;
          off_board.push({ resource_id: resourceId, subtype: String(res.subtype), title: String(res.name), readiness: String(res.readiness), legacy: resources.has(resourceId), counts: resourceCountsFor(resourceId), gaps: resourceGapsFor(resourceId), etag: entityTag(res) });
        }
          }
      return json(response, 200, { columns: resourceColumns, cards: page, off_board, next_cursor, event_cursor: events.length });
    }
    const resourceCardRead = /^\/api\/v1\/resource-cards\/([^/]+)$/.exec(url.pathname);
    if (resourceCardRead && request.method === 'GET') {
      const entity = entityResources.get(resourceCardRead[1]!);
      if (!entity) return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
      if (entity.readiness === 'retired') return fail(response, 404, 'RESOURCE_RETIRED', 'This resource was retired');
      return json(response, 200, { data: { ...entity, title: entity.name } });
    }
    const resourceDeliveries = /^\/api\/v1\/resource-cards\/([^/]+)\/deliveries$/.exec(url.pathname);
    if (resourceDeliveries && request.method === 'GET') {
      if (!entityResources.has(resourceDeliveries[1]!)) return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
      return json(response, 200, { data: [] });
    }
    const resourceRetire = /^\/api\/v1\/resource-cards\/([^/]+)\/retire$/.exec(url.pathname);
    if (resourceRetire && request.method === 'POST') {
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || !key) return fail(response, 400, 'IDEMPOTENCY_KEY_REQUIRED', 'Idempotency-Key is required');
      const entity = entityResources.get(resourceRetire[1]!);
      if (!entity || entity.readiness === 'retired') return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
      entity.readiness = 'retired';
      entity.revision = Number(entity.revision) + 1;
      entity.etag = entityTag(entity);
      const existing = resources.get(String(entity.id));
      if (existing) { existing.readiness = 'retired'; existing.revision += 1; existing.etag = tag(existing); }
      resourcePlacements.delete(entity.id as string);
      pushEvent({ event_id: randomHex(), entity_type: 'resource', entity_id: entity.id as string, revision: Number(entity.revision), event_type: 'resource.retired', payload: {} });
      const result = { entity: { id: entity.id, kind: 'resource', readiness: 'retired', revision: Number(entity.revision) }, affected_versions: [] };
      receipts.set(key, { digest: createHash('sha256').update(JSON.stringify({})).digest('hex'), result, status: 200 });
      return json(response, 200, result);
    }
    if (url.pathname === '/api/v1/resource-card-commits' && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || !key || key.length > 100) return fail(response, 400, 'IDEMPOTENCY_KEY_REQUIRED', 'Idempotency-Key is required');
      const digest = createHash('sha256').update(JSON.stringify(input)).digest('hex');
      const prior = receipts.get(key);
      if (prior) return prior.digest === digest ? json(response, prior.status ?? 201, prior.result) : fail(response, 409, 'KEY_REUSED', 'Idempotency key was used for a different command');
      const originalResources = deepClone(entityResources);
      const originalPlacements = deepClone(resourcePlacements);
      const originalFiles = deepClone(entityAttachments);
      const originalDirectory = deepClone(directory);
      const originalColumns = deepClone(resourceColumns);
      const originalEdges = graphEdges.length;
      const originalEvents = events.length;
      let committed = false;
      try {
        const relatedInfo = { edge_id: null as string | null, to_id: null as string | null };
        let entity: ResourceEntityRecord;
        let status = 200;
        if (input.create && typeof input.create === 'object') {
          const create = input.create as RecordData;
          const title = String(create.title ?? '').trim();
          const subtype = String(create.subtype ?? '');
          if (!title || title.length > 240 || !['gift', 'proof', 'artifact', 'skill', 'interest', 'note'].includes(subtype)) return fail(response, 422, 'INVALID_REQUEST', 'Card title or subtype is invalid');
          const id = randomHex();
          entity = { id, kind: 'resource', name: title, focus: '', revision: 1, subtype, body: (create.body ?? {}) as RecordData, readiness: 'draft', notes: { markdown: String(create.notes_md ?? ''), revision: 1 }, etag: '' };
          entity.etag = entityTag(entity);
          entityResources.set(id, entity);
          pushEvent({ event_id: randomHex(), entity_type: 'resource', entity_id: id, revision: 1, event_type: 'resource.created', payload: { subtype } });
          status = 201;
          for (const upload of (create.staged_uploads ?? []) as string[]) {
            if (!bindUploadTo(id, upload)) return fail(response, 422, 'INVALID_REQUEST', 'Staged upload does not exist or is already bound');
          }
          if (create.placement && typeof create.placement === 'object' && !applyPlacement(id, create.placement as RecordData)) return fail(response, 422, 'INVALID_REQUEST', 'Placement column or anchor is invalid');
          if (input.relation_add && typeof input.relation_add === 'object') {
            const reserved = reservedDeliveryVerb((input.relation_add as RecordData).verb);
            if (reserved) return fail(response, 422, 'INVALID_REQUEST', `A relationship cannot assert a delivery ('${reserved}'). Record the delivery through the opportunity's Mark sent workflow; delivery provenance is shown from recorded sends.`);
            const reason = relateFrom(id, input.relation_add as RecordData, relatedInfo);
            if (reason === 'stale') return fail(response, 412, 'STALE_REVISION', 'The related card changed in another session');
            if (reason) return fail(response, 422, 'INVALID_REQUEST', `Relationship rejected: ${reason}`);
          }
        } else if (input.edit && typeof input.edit === 'object') {
          const edit = input.edit as RecordData;
          const current = entityResources.get(String(edit.resource_id));
          if (!current) return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
          const changesContent = typeof edit.title === 'string' || typeof edit.subtype === 'string' || Boolean(edit.body) || Boolean((edit.staged_uploads as string[] | undefined)?.length || (edit.unbind_ids as string[] | undefined)?.length);
          if ((changesContent || typeof edit.notes_md !== 'string') && Number(current.revision) !== Number(edit.expected_revision)) return fail(response, 412, 'STALE_REVISION', 'The resource changed in another session');
          if (typeof edit.notes_md === 'string' && Number(current.notes?.revision) !== Number(edit.expected_notes_rev)) return fail(response, 412, 'STALE_REVISION', 'Notes changed in another session');
          if (typeof edit.title === 'string') {
            const title = edit.title.trim();
            if (!title || title.length > 240) return fail(response, 422, 'INVALID_REQUEST', 'Card title is invalid');
            current.name = title;
          }
          if (typeof edit.subtype === 'string') current.subtype = edit.subtype;
          if (edit.body && typeof edit.body === 'object') current.body = edit.body as RecordData;
          if (typeof edit.notes_md === 'string') current.notes = { markdown: edit.notes_md, revision: Number(current.notes?.revision ?? 1) + 1 };
          if (changesContent) current.revision = Number(current.revision) + 1;
          current.etag = entityTag(current);
          for (const uploadId of (edit.staged_uploads ?? []) as string[]) {
            if (!bindUploadTo(String(current.id), uploadId)) return fail(response, 422, 'INVALID_REQUEST', 'Staged upload does not exist or is already bound');
          }
          for (const ref of (edit.unbind_ids ?? []) as string[]) unbindAttachmentFrom(String(current.id), ref);
          pushEvent({ event_id: randomHex(), entity_type: 'resource', entity_id: current.id as string, revision: Number(current.revision), event_type: 'resource.updated', payload: {} });
          entity = current;
          if (input.placement && typeof input.placement === 'object' && !applyPlacement(String(current.id), input.placement as RecordData)) return fail(response, 422, 'INVALID_REQUEST', 'Placement column or anchor is invalid');
          if (input.relation_add && typeof input.relation_add === 'object') {
            const reserved = reservedDeliveryVerb((input.relation_add as RecordData).verb);
            if (reserved) return fail(response, 422, 'INVALID_REQUEST', `A relationship cannot assert a delivery ('${reserved}'). Record the delivery through the opportunity's Mark sent workflow; delivery provenance is shown from recorded sends.`);
            const reason = relateFrom(String(current.id), input.relation_add as RecordData, relatedInfo);
            if (reason === 'stale') return fail(response, 412, 'STALE_REVISION', 'The related card changed in another session');
            if (reason) return fail(response, 422, 'INVALID_REQUEST', `Relationship rejected: ${reason}`);
          }
        } else {
          return fail(response, 422, 'INVALID_REQUEST', 'A commit must create or edit a card');
        }
        const legacy = resources.get(entity.id);
        if (legacy && (legacy.title !== entity.name || JSON.stringify(legacy.body) !== JSON.stringify(entity.body))) {
          legacy.title = entity.name;
          legacy.body = entity.body;
          legacy.revision += 1;
          legacy.etag = tag(legacy);
        }
        const result = { entity: { ...entity, title: entity.name }, placement: resourcePlacements.get(String(entity.id)) ?? null, created_related: relatedInfo.edge_id ? { edge_id: relatedInfo.edge_id, to_id: relatedInfo.to_id } : null, affected_versions: [], event_cursor: events.length };
        receipts.set(key, { digest, result, status });
        committed = true;
        return json(response, status, result);
      } finally {
        if (!committed) {
          entityResources.clear(); for (const [key, value] of originalResources) entityResources.set(key, value);
          resourcePlacements.clear(); for (const [key, value] of originalPlacements) resourcePlacements.set(key, value);
          entityAttachments.clear(); for (const [key, value] of originalFiles) entityAttachments.set(key, value);
          directory.clear(); for (const [key, value] of originalDirectory) directory.set(key, value);
          resourceColumns.splice(0, resourceColumns.length, ...originalColumns);
          graphEdges.length = originalEdges;
          events.length = originalEvents;
        }
      }
    }
    if (url.pathname === '/api/v1/resource-board/moves' && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || !key) return fail(response, 400, 'IDEMPOTENCY_KEY_REQUIRED', 'Idempotency-Key is required');
      const digest = createHash('sha256').update(JSON.stringify(input)).digest('hex');
      const prior = receipts.get(key);
      if (prior) return prior.digest === digest ? json(response, 200, prior.result) : fail(response, 409, 'KEY_REUSED', 'Idempotency key was used for a different command');
      const resourceId = String(input.resource_id);
      if (!entityResources.has(resourceId)) return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
      const current = resourcePlacements.get(resourceId);
      if (input.expected_placement_revision != null && Number(input.expected_placement_revision) !== (current?.revision ?? 0)) return fail(response, 412, 'STALE_REVISION', 'Placement changed in another session');
      if (input.expected_source_column_revision != null && current) {
        const column = resourceColumns.find((c) => c.id === current.column_id);
        if (column && Number(column.revision) !== Number(input.expected_source_column_revision)) return fail(response, 412, 'STALE_REVISION', 'Source column changed in another session');
      }
      const destination = input.destination_column_id;
      if (typeof destination === 'string') {
        const column = resourceColumns.find((c) => c.id === destination);
        if (!column) return fail(response, 422, 'INVALID_REQUEST', 'Destination column does not exist');
        if (input.expected_destination_column_revision != null && Number(column.revision) !== Number(input.expected_destination_column_revision)) return fail(response, 412, 'STALE_REVISION', 'Destination column changed in another session');
        if (!applyPlacement(resourceId, { column_id: destination, before_resource_id: input.before_resource_id ?? null })) return fail(response, 422, 'INVALID_REQUEST', 'The anchor must be in the destination column');
      } else if (destination === null) {
        applyPlacement(resourceId, { column_id: null });
      } else {
        return fail(response, 422, 'INVALID_REQUEST', 'A destination column is required');
      }
      const placement = resourcePlacements.get(resourceId) ?? null;
      const result = { placement, source_column_revision: current ? resourceColumns.find((c) => c.id === current.column_id)?.revision ?? null : null, destination_column_revision: typeof destination === 'string' ? resourceColumns.find((c) => c.id === destination)?.revision ?? null : null };
      receipts.set(key, { digest, result, status: 200 });
      return json(response, 200, result);
    }
    if (url.pathname === '/api/v1/edges' && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || !key) return fail(response, 400, 'IDEMPOTENCY_KEY_REQUIRED', 'Idempotency-Key is required');
      const fromId = String(input.from_id); const toId = String(input.to_id); const verb = String(input.verb).trim(); const basis = String(input.basis).trim();
      if (!findEntity(fromId) || !findEntity(toId) || fromId === toId || !verb || !basis) return fail(response, 422, 'INVALID_REQUEST', 'Endpoints, verb and basis are required');
      if (graphEdges.some((edge) => edge.state !== 'retracted' && String(edge.from_id) === fromId && edge.verb === verb && String(edge.to_id) === toId)) return fail(response, 422, 'INVALID_REQUEST', 'This relationship already exists');
      const digest = createHash('sha256').update(JSON.stringify(input)).digest('hex');
      const prior = receipts.get(key);
      if (prior) return prior.digest === digest ? json(response, prior.status ?? 201, prior.result) : fail(response, 409, 'KEY_REUSED', 'Idempotency key was used for a different command');
      const edge: RecordData = { id: randomHex(), from_id: fromId, verb, to_id: toId, revision: 1, basis, author_kind: 'owner', state: 'active', occurrence_id: '', quote: '', tier: 1, speaker_kind: 'owner', captured_at: now(), source_title: null, source_url: null };
      graphEdges.push(edge);
      pushEvent({ event_id: randomHex(), entity_type: 'edge', entity_id: edge.id as string, revision: 1, event_type: 'edge.created', payload: {} });
      const result = { edge }; receipts.set(key, { digest, result, status: 201 });
      return json(response, 201, result);
    }
    const edgeMatch = /^\/api\/v1\/edges\/([^/]+)$/.exec(url.pathname);
    if (edgeMatch && request.method === 'PATCH') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const edge = graphEdges.find((candidate) => candidate.id === edgeMatch[1]);
      if (!edge || edge.state === 'retracted') return fail(response, 404, 'NOT_FOUND', 'Edge does not exist');
      if (Number(edge.revision) !== Number(input.expected_revision)) return fail(response, 412, 'STALE_REVISION', 'Edge changed in another session');
      if ('basis' in input && !edge.owner_authored) return fail(response, 422, 'INVALID_REQUEST', 'Only owner notes can be corrected here');
      if (reservedDeliveryVerb(input.verb)) return fail(response, 422, 'INVALID_REQUEST', 'Use Mark sent to record deliveries');
      if (input.reverse === true) [edge.from_id, edge.to_id] = [edge.to_id, edge.from_id];
      if (typeof input.verb === 'string' && input.verb.trim()) edge.verb = input.verb.trim();
      if (typeof input.basis === 'string' && input.basis.trim()) { edge.basis = input.basis.trim(); edge.quote = edge.basis; }
      edge.revision = Number(edge.revision) + 1;
      return json(response, 200, { edge, author_kind: edge.author_kind });
    }
    if (edgeMatch && request.method === 'DELETE') {
      const input = await readBody(request);
      const edge = graphEdges.find((candidate) => candidate.id === edgeMatch[1]);
      if (!edge || edge.state === 'retracted') return fail(response, 404, 'NOT_FOUND', 'Edge does not exist');
      if (edge.revision !== input.expected_revision) return fail(response, 412, 'STALE_REVISION', 'Edge changed in another session');
      edge.state = 'retracted';
      edge.revision = Number(edge.revision) + 1;
      pushEvent({ event_id: randomHex(), entity_type: 'edge', entity_id: edge.id as string, revision: Number(edge.revision), event_type: 'edge.detached', payload: {} });
      return json(response, 204, {});
    }
    const attachmentListMatch = /^\/api\/v1\/entities\/([^/]+)\/attachments$/.exec(url.pathname);
    if (attachmentListMatch && request.method === 'GET') {
      if (!findEntity(attachmentListMatch[1]!)) return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
      return json(response, 200, { data: entityAttachments.get(attachmentListMatch[1]!) ?? [] });
    }
    if (attachmentListMatch && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string' || !key) return fail(response, 400, 'IDEMPOTENCY_KEY_REQUIRED', 'Idempotency-Key is required');
      const entityId = attachmentListMatch[1]!;
      if (!findEntity(entityId)) return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
      const uploadId = String(input.upload_id);
      if (!bindUploadTo(entityId, uploadId)) return fail(response, 422, 'INVALID_REQUEST', 'Staged upload does not exist or is already bound');
      const attachment = (entityAttachments.get(entityId) ?? []).find((a) => a.upload_id === uploadId)!;
      const result = { attachment }; receipts.set(key, { digest: createHash('sha256').update(JSON.stringify(input)).digest('hex'), result, status: 201 });
      return json(response, 201, result);
    }
    const attachmentRefMatch = /^\/api\/v1\/entities\/([^/]+)\/attachments\/([^/]+)$/.exec(url.pathname);
    if (attachmentRefMatch && request.method === 'DELETE') {
      const [, entityId, ref] = attachmentRefMatch;
      const list = entityAttachments.get(entityId!) ?? [];
      const before = list.length;
      const next = list.filter((a) => a.id !== ref);
      if (next.length === before) return fail(response, 404, 'NOT_FOUND', 'Attachment binding does not exist');
      entityAttachments.set(entityId!, next);
      return json(response, 204, {});
    }
    const attachmentContentMatch = /^\/api\/v1\/entities\/([^/]+)\/attachments\/([^/]+)\/content$/.exec(url.pathname);
    if (attachmentContentMatch && request.method === 'GET') {
      const [, entityId, ref] = attachmentContentMatch;
      const attachment = (entityAttachments.get(entityId!) ?? []).find((a) => a.id === ref);
      if (!attachment) return fail(response, 404, 'NOT_FOUND', 'Attachment binding does not exist');
      const upload = uploads.get(String(attachment.upload_id));
      if (!upload) return fail(response, 404, 'NOT_FOUND', 'Attachment bytes are unavailable');
      response.writeHead(200, {
        'content-type': upload.mime || 'application/octet-stream',
        'content-disposition': `attachment; filename="${String(attachment.filename)}"`,
        'cache-control': 'no-store',
        'x-parley-epoch': epoch,
      });
      response.end(upload.bytes);
      return;
    }

    if (url.pathname === '/api/v1/board' && request.method === 'GET') return json(response, 200, boardSnapshot());
    if (url.pathname === '/api/v1/board/lists' && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const title = typeof input.title === 'string' ? input.title.trim() : '';
      if (!title || title.length > 120 || boardLists.some((list) => list.title.toLowerCase() === title.toLowerCase())) return fail(response, 422, 'BOARD_LIST_INVALID', 'List title must be unique and non-empty');
      const active = boardLists.filter((list) => !list.archived).sort((a, b) => a.position - b.position);
      const beforeIndex = typeof input.before_list_id === 'string' ? active.findIndex((list) => list.id === input.before_list_id) : -1;
      if (typeof input.before_list_id === 'string' && beforeIndex < 0) return fail(response, 422, 'BOARD_ANCHOR_INVALID', 'List anchor is invalid');
      const position = beforeIndex < 0 ? active.length : beforeIndex;
      const list = { id: randomHex(), title, position, archived: false, revision: 1 };
      boardLists.push(list);
      boardLists.filter((item) => !item.archived && item.id !== list.id).forEach((item) => { if (item.position >= list.position) { item.position += 1; item.revision += 1; } });
      return json(response, 201, { list, board_revision: events.length + 1 });
    }
    if (url.pathname === '/api/v1/board/lists/reorder' && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const active = boardLists.filter((list) => !list.archived);
      const ids = input.ordered_ids;
      const revisions = input.expected_revisions;
      if (!Array.isArray(ids) || !revisions || typeof revisions !== 'object' || Array.isArray(revisions)
        || ids.length !== active.length || new Set(ids).size !== active.length
        || active.some((list) => !ids.includes(list.id) || (revisions as RecordData)[list.id] !== list.revision)) {
        return fail(response, 409, 'BOARD_LIST_STALE', 'List order or revision is stale');
      }
      ids.forEach((id, position) => {
        const list = active.find((item) => item.id === id)!;
        list.position = position;
        list.revision += 1;
      });
      return json(response, 200, { lists: active.slice().sort((a, b) => a.position - b.position) });
    }
    const boardListMatch = /^\/api\/v1\/board\/lists\/([^/]+)$/.exec(url.pathname);
    if (boardListMatch && request.method === 'PATCH') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const list = boardLists.find((item) => item.id === boardListMatch[1]);
      if (!list) return fail(response, 404, 'NOT_FOUND', 'Board list does not exist');
      if (input.expected_revision !== list.revision) return fail(response, 409, 'BOARD_LIST_STALE', 'Board list changed');
      const title = typeof input.title === 'string' ? input.title.trim() : '';
      if (!title || boardLists.some((item) => item.id !== list.id && item.title.toLowerCase() === title.toLowerCase())) return fail(response, 422, 'BOARD_LIST_INVALID', 'List title must be unique and non-empty');
      list.title = title; list.revision += 1;
      return json(response, 200, { list });
    }
    const archiveMatch = /^\/api\/v1\/board\/lists\/([^/]+)\/archive$/.exec(url.pathname);
    if (archiveMatch && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const source = boardLists.find((item) => item.id === archiveMatch[1]);
      const destination = boardLists.find((item) => item.id === String(input.destination_list_id) && !item.archived);
      const active = boardLists.filter((item) => !item.archived);
      if (!source || source.archived || !destination || source.id === destination.id || active.length < 2) return fail(response, 422, 'BOARD_ARCHIVE_INVALID', 'Choose a different active destination and keep one active list');
      if (input.expected_list_revision !== source.revision || input.expected_destination_revision !== destination.revision) return fail(response, 409, 'BOARD_LIST_STALE', 'Board list changed');
      const moved = boardSnapshot().cards.filter((card) => card.placement.list_id === source.id).sort((a, b) => a.placement.position - b.placement.position);
      const destinationCards = boardSnapshot().cards.filter((card) => card.placement.list_id === destination.id).sort((a, b) => a.placement.position - b.placement.position);
      const nextPosition = destinationCards.length;
      moved.forEach((card, index) => {
        const placement = placements.get(String(card.opportunity.id))!;
        placement.list_id = destination.id; placement.position = nextPosition + index; placement.placement_revision += 1;
      });
      destination.revision += 1; source.archived = true; source.revision += 1;
      return json(response, 200, { archived_list: source, destination_list: destination, moved: moved.map((card, index) => ({ opportunity_id: card.opportunity.id, position: nextPosition + index, placement_revision: placements.get(String(card.opportunity.id))!.placement_revision })) });
    }
    if (url.pathname === '/api/v1/opportunities' && request.method === 'GET') {
      return json(response, 200, [...opportunities.values()]);
    }
    const opportunityById = /^\/api\/v1\/opportunities\/([^/]+)$/.exec(url.pathname);
    if (opportunityById && request.method === 'GET') {
      const opportunity = opportunities.get(opportunityById[1]!);
      if (!opportunity) return fail(response, 404, 'NOT_FOUND', 'Opportunity does not exist');
      return json(response, 200, opportunity);
    }
    const opportunityTags = /^\/api\/v1\/opportunities\/([^/]+)\/tags$/.exec(url.pathname);
    if (opportunityTags && request.method === 'PUT') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const opportunity = opportunities.get(opportunityTags[1]!);
      if (!opportunity) return fail(response, 404, 'NOT_FOUND', 'Opportunity does not exist');
      if (input.expected_revision !== opportunity.revision) return fail(response, 409, 'REVISION_CONFLICT', 'opportunity revision changed');
      const tags = Array.isArray(input.tags)
        ? [...new Set(input.tags.flatMap((entry) => typeof entry === 'string' && entry.trim() ? [entry.trim().toLowerCase()] : []))].sort()
        : [];
      opportunity.revision = Number(opportunity.revision) + 1;
      opportunity.tags = tags;
      const participants = (opportunity.participant_ids as string[]).map((id: string) => {
        const entity = directory.get(id);
        return { id, name: entity?.name ?? id, kind: entity?.kind ?? 'person' };
      });
      return json(response, 200, {
        id: opportunity.id, title: opportunity.title, state: opportunity.state, revision: opportunity.revision,
        participants, tags, next_task: opportunity.next_task ?? null, next_deadline: null, missing_material: false,
      });
    }
    const opportunityPhase = /^\/api\/v1\/opportunities\/([^/]+)\/phase$/.exec(url.pathname);
    if (opportunityPhase && request.method === 'PATCH') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const opportunity = opportunities.get(opportunityPhase[1]!);
      if (!opportunity) return fail(response, 404, 'NOT_FOUND', 'Opportunity does not exist');
      const stage = PACK_STAGES.find((item) => item.stage_id === String(input.target_stage_id));
      if (!stage || String(input.pack_version) !== activePackVersion) {
        return fail(response, 422, 'INVALID_STAGE', `Stage '${String(input.target_stage_id)}' is not valid for pack version ${activePackVersion}`);
      }
      const current = opportunityPhases.get(String(opportunity.id)) ?? { stage_id: null, pack_id: activePackId, pack_version: activePackVersion, revision: 1, event_id: '' };
      const expected = Number(input.expected_phase_revision);
      const reverted = typeof input.original_event_id === 'string' && input.original_event_id.length > 0;
      if (reverted && (current.event_id !== input.original_event_id || !current.event_id)) {
        return fail(response, 412, 'PHASE_CHANGED_ELSEWHERE', 'Undo cannot apply: a newer phase change replaced the referenced transition.');
      }
      if (expected !== current.revision) {
        return fail(response, 412, 'STALE_PHASE_REVISION', `Expected phase revision ${expected} but the opportunity is at revision ${current.revision}.`);
      }
      if (stage.kind === 'terminal') {
        return fail(response, 422, 'REQUIRES_LIFECYCLE_CONFIRM', `Stage '${stage.label}' is terminal; closing requires the explicit lifecycle flow.`);
      }
      if (String(opportunity.state) !== 'open') {
        return fail(response, 409, 'LIFECYCLE_BLOCKS_PHASE', `This opportunity is ${String(opportunity.state)}; reopen it before changing its phase.`);
      }
      if (!reverted && current.stage_id === stage.stage_id) {
        return json(response, 200, { row: sheetRow(opportunity), event_id: current.event_id, phase_revision: current.revision, reverted: false });
      }
      const eventId = randomHex();
      const next = { stage_id: stage.stage_id, pack_id: activePackId, pack_version: activePackVersion, revision: current.revision + 1, event_id: eventId };
      opportunityPhases.set(String(opportunity.id), next);
      opportunity.revision = Number(opportunity.revision) + 1;
      pushEvent({
        event_id: eventId, entity_type: 'opportunity', entity_id: String(opportunity.id), revision: Number(opportunity.revision),
        event_type: 'opportunity.phase_changed',
        payload: { from_stage_id: current.stage_id, to_stage_id: stage.stage_id, from_class: current.stage_id ? PACK_STAGES.find((item) => item.stage_id === current.stage_id)?.class ?? null : null, to_class: stage.class, pack_id: activePackId, pack_version: activePackVersion, operation: reverted ? 'undo' : 'manual', origin: 'queue.sheet', reversal_of: reverted ? input.original_event_id : null },
      });
      return json(response, 200, { row: sheetRow(opportunity), event_id: eventId, phase_revision: next.revision, reverted });
    }
    if (url.pathname === '/api/v1/opportunities' && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const participant = directory.get(String(input.primary_party_id));
      if (!participant || !['person', 'org'].includes(participant.kind) || typeof input.title !== 'string' || !input.title.trim()) {
        return fail(response, 422, 'OPPORTUNITY_INVALID', 'A title and selected participant are required');
      }
      const record = { id: randomHex(), title: input.title.trim(), state: 'open', revision: 1, participant_ids: [participant.id], source_variant_ids: [], material_ids: [], tags: [] };
      opportunities.set(record.id, record);
      const homeList = boardLists.find((list) => !list.archived)!;
      const position = [...placements.values()].filter((placement) => placement.list_id === homeList.id).length;
      placements.set(record.id, { list_id: homeList.id, position, placement_revision: 1 });
      notesByOpportunity.set(record.id, { markdown: '', revision: 1 });
      return json(response, 201, record);
    }
    const placementMatch = /^\/api\/v1\/opportunities\/([^/]+)\/placement$/.exec(url.pathname);
    if (placementMatch && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const opportunity = opportunities.get(placementMatch[1]!);
      const placement = placements.get(placementMatch[1]!);
      const source = boardLists.find((list) => list.id === placement?.list_id);
      const destination = boardLists.find((list) => list.id === String(input.list_id) && !list.archived);
      if (!opportunity || !placement || !source || !destination) return fail(response, 404, 'NOT_FOUND', 'Opportunity or active board list does not exist');
      if (input.expected_placement_revision !== placement.placement_revision || input.expected_source_list_revision !== source.revision || input.expected_list_revision !== destination.revision) return fail(response, 409, 'BOARD_PLACEMENT_STALE', 'Board placement changed');
      const sourceCards = boardSnapshot().cards.filter((card) => card.placement.list_id === source.id && card.opportunity.id !== opportunity.id).sort((a, b) => a.placement.position - b.placement.position);
      const destinationCards = source.id === destination.id ? sourceCards : boardSnapshot().cards.filter((card) => card.placement.list_id === destination.id).sort((a, b) => a.placement.position - b.placement.position);
      const beforeId = typeof input.before_opportunity_id === 'string' ? input.before_opportunity_id : null;
      const targetIndex = beforeId === null ? destinationCards.length : destinationCards.findIndex((card) => card.opportunity.id === beforeId);
      if (targetIndex < 0) return fail(response, 422, 'BOARD_ANCHOR_INVALID', 'Destination card anchor is invalid');
      destinationCards.splice(targetIndex, 0, { opportunity, placement: { opportunity_id: opportunity.id, list_id: destination.id, position: targetIndex, revision: placement.placement_revision, next_task: null } });
      destinationCards.forEach((card, position) => { const current = placements.get(String(card.opportunity.id))!; current.list_id = destination.id; current.position = position; if (card.opportunity.id === opportunity.id) current.placement_revision += 1; });
      if (source.id !== destination.id) sourceCards.forEach((card, position) => { placements.get(String(card.opportunity.id))!.position = position; });
      source.revision += 1;
      if (source.id !== destination.id) destination.revision += 1;
      return json(response, 200, { opportunity_id: opportunity.id, list_id: destination.id, position: targetIndex, placement_revision: placement.placement_revision, source_list_revision: source.revision, destination_list_revision: destination.revision });
    }
    const notesMatch = /^\/api\/v1\/opportunities\/([^/]+)\/notes(?:\/tasks\/toggle)?$/.exec(url.pathname);
    if (notesMatch && request.method === 'GET') {
      if (!opportunities.has(notesMatch[1]!)) return fail(response, 404, 'NOT_FOUND', 'Opportunity does not exist');
      return json(response, 200, notesRecord(notesMatch[1]!));
    }
    if (notesMatch && url.pathname.endsWith('/notes') && request.method === 'PATCH') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const current = notesByOpportunity.get(notesMatch[1]!);
      if (!current) return fail(response, 404, 'NOT_FOUND', 'Opportunity does not exist');
      if (input.expected_revision !== current.revision) return fail(response, 409, 'NOTES_STALE', 'Markdown notes changed');
      if (typeof input.markdown !== 'string' || Buffer.byteLength(input.markdown, 'utf8') > 1_000_000) return fail(response, 422, 'NOTES_INVALID', 'Markdown is invalid or too large');
      current.markdown = input.markdown.replace(/\r\n?/g, '\n'); current.revision += 1;
      return json(response, 200, notesRecord(notesMatch[1]!));
    }
    if (notesMatch && url.pathname.endsWith('/notes/tasks/toggle') && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const current = notesByOpportunity.get(notesMatch[1]!);
      if (!current) return fail(response, 404, 'NOT_FOUND', 'Opportunity does not exist');
      if (input.notes_revision !== current.revision) return fail(response, 409, 'NOTES_STALE', 'Markdown notes changed');
      const task = tasks(current.markdown).find((item) => item.byte_start === input.byte_start && item.byte_end === input.byte_end && item.line_sha256 === input.line_sha256);
      if (!task || typeof input.checked !== 'boolean') return fail(response, 409, 'TASK_STALE', 'The addressed Markdown task changed');
      const line = Buffer.from(current.markdown, 'utf8');
      const lineText = line.subarray(task.byte_start, task.byte_end).toString('utf8');
      const marker = /\[([ xX])\]/.exec(lineText);
      if (!marker || marker.index === undefined) return fail(response, 409, 'TASK_STALE', 'The addressed Markdown task changed');
      const replacement = input.checked ? 'x' : ' ';
      current.markdown = Buffer.concat([line.subarray(0, task.byte_start + marker.index + 1), Buffer.from(replacement), line.subarray(task.byte_start + marker.index + 2)]).toString('utf8');
      current.revision += 1;
      return json(response, 200, notesRecord(notesMatch[1]!));
    }
    if (url.pathname === '/api/v1/outputs' && request.method === 'GET') {
      return json(response, 200, [...outputs.values()]);
    }
    if (url.pathname === '/api/v1/uploads' && request.method === 'POST') {
      const chunks: Buffer[] = [];
      let length = 0;
      for await (const chunk of request) {
        length += (chunk as Buffer).length;
        if (length > 64 * 1024 * 1024) return fail(response, 413, 'UPLOAD_TOO_LARGE', 'Attachment exceeds the byte limit');
        chunks.push(chunk as Buffer);
      }
      const bytes = Buffer.concat(chunks);
      if (bytes.length === 0) return fail(response, 422, 'EMPTY_UPLOAD', 'Attachment is empty');
      const sha256 = createHash('sha256').update(bytes).digest('hex');
      const header = request.headers['x-parley-filename'];
      const filename = header ? new URLSearchParams(String(header)).get('name') ?? decodeURIComponent(String(header)) : 'attachment.bin';
      const mime = String(request.headers['content-type'] ?? 'application/octet-stream');
      const id = randomHex();
      const created_at = now();
      uploads.set(id, { id, sha256, bytes, filename, mime, share_permission: false, reviewed: false, revision: 1, created_at });
      return json(response, 201, { id, sha256, size: bytes.length, filename, mime, share_permission: false, reviewed: false, revision: 1, created_at });    }
    if (url.pathname === '/api/v1/uploads' && request.method === 'GET') {
      return json(response, 200, [...uploads.values()].map((upload) => ({ id: upload.id, sha256: upload.sha256, size: upload.bytes.length, filename: upload.filename, mime: upload.mime, share_permission: upload.share_permission, reviewed: upload.reviewed, revision: upload.revision, created_at: upload.created_at })));
    }
    const uploadReviewMatch = /^\/api\/v1\/uploads\/([^/]+)\/review$/.exec(url.pathname);
    if (uploadReviewMatch && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const upload = uploads.get(uploadReviewMatch[1]!);
      if (!upload) return fail(response, 404, 'UPLOAD_NOT_FOUND', 'Upload does not exist');
      if (upload.reviewed) {
        if (Boolean(input.share_permission) !== Boolean(upload.share_permission)) return fail(response, 422, 'REVIEW_IMMUTABLE', 'An upload review cannot be changed after it is recorded');
        return json(response, 200, { id: upload.id, sha256: upload.sha256, size: upload.bytes.length, filename: upload.filename, mime: upload.mime, share_permission: upload.share_permission, reviewed: true, revision: upload.revision, created_at: upload.created_at });
      }
      upload.reviewed = true;
      upload.share_permission = Boolean(input.share_permission);
      upload.revision += 1;
      return json(response, 200, { id: upload.id, sha256: upload.sha256, size: upload.bytes.length, filename: upload.filename, mime: upload.mime, share_permission: upload.share_permission, reviewed: true, revision: upload.revision, created_at: upload.created_at });
    }
    const s06MaterialMatch = /^\/api\/v1\/opportunities\/([^/]+)\/materials$/.exec(url.pathname);
    if (s06MaterialMatch && request.method === 'GET') {
      const items = [...materialRecords.values()].filter((record) => String(record.opportunity_id) === s06MaterialMatch[1]);
      return json(response, 200, items);
    }
    if (s06MaterialMatch && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const opportunity = opportunities.get(s06MaterialMatch[1]!);
      if (!opportunity) return fail(response, 404, 'OPPORTUNITY_NOT_FOUND', 'Opportunity does not exist');
      const recipientId = String(input.recipient_id);
      const party = [...directory.values()].find((entity) => entity.id === recipientId);
      if (!party) return fail(response, 422, 'RECIPIENT_NOT_FOUND', 'Recipient is not a known party');
      let attachment: RecordData | null = null;
      const attachInput = input.attachment as RecordData | null | undefined;
      if (attachInput && typeof attachInput.upload_id === 'string' && attachInput.upload_id) {
        const upload = uploads.get(attachInput.upload_id);
        if (!upload || !upload.reviewed || !upload.share_permission) return fail(response, 422, 'ATTACHMENT_NOT_REVIEWED', 'Attachment requires a reviewed, shareable upload');
        attachment = { upload_id: upload.id, sha256: upload.sha256, filename: upload.filename, content_type: upload.mime, size: upload.bytes.length };
      }
      const body = String(input.body);
      const content_sha256 = attachment ? String(attachment.sha256) : createHash('sha256').update(body).digest('hex');
      const id = randomHex();
      const record = { id, opportunity_id: s06MaterialMatch[1], recipient_id: recipientId, recipient_name: party.name, kind: String(input.kind), version: 1, body, content_sha256, status: 'draft', gate: null, attachment };
      materialRecords.set(id, record);
      outputs.set(id, record);
      return json(response, 201, record);
    }
    const s06ApproveRelease = /^\/api\/v1\/materials\/([^/]+)\/(approve|release)$/.exec(url.pathname);
    if (s06ApproveRelease && request.method === 'POST') {
      const record = materialRecords.get(s06ApproveRelease[1]!);
      if (!record) return fail(response, 404, 'MATERIAL_NOT_FOUND', 'Material does not exist');
      if (partySuppression.get(String(record.recipient_id))) {
        return fail(response, 422, 'PARTY_SUPPRESSED', 'The recipient is do-not-contact suppressed; approval and release require an explicit owner clearance reason');
      }
      if (s06ApproveRelease[2] === 'approve') {
        record.status = 'approved';
        return json(response, 200, { id: randomHex(), material_id: record.id, version: record.version, content_sha256: record.content_sha256, gate: { checks: [], copy_enabled: true }, approved_at: now() });
      }
      record.status = 'released';
      const attachment = record.attachment as RecordData | null;
      const bytes = attachment ? uploads.get(String(attachment.upload_id))?.bytes ?? Buffer.alloc(0) : Buffer.from(record.body as string, 'utf8');
      const release: ReleaseRecord = {
        release_id: randomHex(),
        material_id: String(record.id),
        version: Number(record.version),
        recipient_id: String(record.recipient_id),
        body: String(record.body),
        content_sha256: String(record.content_sha256),
        issued_at: now(),
        attachment,
        consumed: false,
        acknowledged: null,
        bytes,
      };
      releaseRecords.set(release.release_id, release);
      return json(response, 200, { release_id: release.release_id, material_id: release.material_id, version: release.version, recipient_id: release.recipient_id, body: release.body, content_sha256: release.content_sha256, issued_at: release.issued_at, attachment: release.attachment });
    }
    const materialVersionsMatch = /^\/api\/v1\/materials\/([^/]+)\/versions$/.exec(url.pathname);
    if (materialVersionsMatch && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const record = materialRecords.get(materialVersionsMatch[1]!);
      if (!record) return fail(response, 404, 'MATERIAL_NOT_FOUND', 'Material does not exist');
      if (String(input.recipient_id) !== String(record.recipient_id)
        || String(input.opportunity_id ?? record.opportunity_id) !== String(record.opportunity_id)) {
        return fail(response, 422, 'MATERIAL_RECIPIENT_MISMATCH', 'Revised material must keep its opportunity and recipient');
      }
      record.version = Number(record.version) + 1;
      record.body = String(input.body ?? record.body);
      record.updated_at = now();
      record.status = 'draft';
      return json(response, 201, { ...record });
    }
    const releaseAckMatch = /^\/api\/v1\/releases\/([^/]+)\/ack$/.exec(url.pathname);
    if (releaseAckMatch && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const release = releaseRecords.get(releaseAckMatch[1]!);
      if (!release) return fail(response, 404, 'RELEASE_NOT_FOUND', 'Release receipt does not exist');
      if (typeof input.succeeded !== 'boolean') return fail(response, 422, 'INVALID_RELEASE_ACK', 'Release acknowledgement needs an explicit success result');
      if (release.acknowledged !== null && release.acknowledged !== input.succeeded) return fail(response, 409, 'RELEASE_ACK_CONFLICT', 'Release receipt already has a different acknowledgement');
      release.acknowledged = input.succeeded;
      response.writeHead(204, { 'x-parley-epoch': epoch, 'cache-control': 'no-store' });
      response.end();
      return;
    }
    if (url.pathname === '/api/v1/threads' && request.method === 'GET') {
      const opportunityId = url.searchParams.get('opportunity_id');
      const recipientId = url.searchParams.get('recipient_id');
      const items = [...threads.values()].filter((thread) =>
        (!opportunityId || String(thread.opportunity_id) === opportunityId)
        && (!recipientId || String(thread.recipient_id) === recipientId));
      return json(response, 200, items);
    }
    const threadSendMatch = /^\/api\/v1\/threads\/([^/]+)\/send$/.exec(url.pathname);
    if (threadSendMatch && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const threadId = threadSendMatch[1]!;
      const opportunityId = String(input.opportunity_id);
      const recipientId = String(input.recipient_id);
      const primary = input.primary as RecordData;
      const material = materialRecords.get(String(primary.material_id));
      const release = releaseRecords.get(String(primary.release_id));
      if (!material || !release) return fail(response, 422, 'MATERIAL_NOT_FOUND', 'Primary material or release grant does not exist');
      if (String(material.opportunity_id) !== opportunityId || String(material.recipient_id) !== recipientId) return fail(response, 422, 'MATERIAL_RECIPIENT_MISMATCH', 'Primary material must target this thread\'s opportunity and recipient');
      if (String(release.material_id) !== String(primary.material_id) || Number(release.version) !== Number(primary.version)) return fail(response, 422, 'RELEASE_MISMATCH', 'Release grant does not match the material version');
      const companions = Array.isArray(input.companions) ? input.companions as RecordData[] : [];
      for (const companion of companions) {
        const companionMaterial = materialRecords.get(String(companion.material_id));
        const companionRelease = releaseRecords.get(String(companion.release_id));
        if (!companionMaterial || !companionRelease) return fail(response, 422, 'COMPANION_NOT_FOUND', 'Companion material or release grant does not exist');
        if (String(companionMaterial.opportunity_id) !== opportunityId || String(companionMaterial.recipient_id) !== recipientId) return fail(response, 422, 'COMPANION_RECIPIENT_MISMATCH', 'Companion release must target the same opportunity and recipient');
      }
      let thread = threads.get(threadId);
      if (!thread) {
        thread = { id: threadId, opportunity_id: opportunityId, recipient_id: recipientId, state: 'waiting', current_touch: 0, expires_at: null, next_action_at: null, sent_at: null, updated_at: now(), sends: [] };
        threads.set(threadId, thread);
      }
      const touchNo = Number(thread.current_touch) + 1;
      const deadlineInput = input.deadline as RecordData | null | undefined;
      const base = testClock;
      const deadlineMs = (() => {
        if (deadlineInput && typeof deadlineInput.at === 'string') return Date.parse(String(deadlineInput.at));
        if (deadlineInput && typeof deadlineInput.date === 'string') return Date.parse(`${String(deadlineInput.date)}T00:00:00Z`) + 86400000;
        const days = (deadlineInput && typeof deadlineInput.days === 'number') ? Number(deadlineInput.days) : 7;
        return base + days * 86400000;
      })();
      const sentAt = new Date(base).toISOString();
      const responseDeadline = new Date(deadlineMs).toISOString();
      const slotFills = Array.isArray(material.slot_fills) ? material.slot_fills as RecordData[] : [];
      const capacityFill = slotFills.find((fill) => String(fill.slot).startsWith('capacity_line:'));
      let capacityCharged = false;
      if (capacityFill) {
        const ledger = capacityLedger.get('quarter:project slots');
        if (ledger) {
          const remaining = Number(ledger.total) - Number(ledger.committed);
          const numbers = String(capacityFill.value).match(/\d+/g);
          if (numbers && numbers.some((n) => Number(n) === remaining)) {
            ledger.committed = Number(ledger.committed) + 1;
            ledger.updated_at = sentAt;
            capacityCharged = true;
          }
        }
      }
      const sendId = randomHex();
      const nextActionAt = new Date(base + 4 * 86400000).toISOString();
      const sendRecord = { id: sendId, thread_id: threadId, touch_no: touchNo, primary_material_id: String(primary.material_id), primary_version_no: Number(primary.version), recipient_id: recipientId, content_sha256: String(release.content_sha256), sent_at: sentAt, response_deadline: responseDeadline, deadline_zone: String((deadlineInput && deadlineInput.zone) || 'UTC'), next_action_at: nextActionAt, capacity_charged: capacityCharged };
      sends.set(sendId, sendRecord);
      thread.state = 'waiting'; thread.current_touch = touchNo; thread.expires_at = responseDeadline; thread.next_action_at = nextActionAt; thread.sent_at = sentAt; thread.updated_at = sentAt;
      thread.sends = [...(Array.isArray(thread.sends) ? thread.sends : []), { ...sendRecord, outcome: null }];
      return json(response, 201, sendRecord);
    }
    const threadReopenMatch = /^\/api\/v1\/threads\/([^/]+)\/reopen$/.exec(url.pathname);
    if (threadReopenMatch && request.method === 'POST') {
      const thread = threads.get(threadReopenMatch[1]!);
      if (!thread) return fail(response, 404, 'THREAD_NOT_FOUND', 'Thread does not exist');
      if (!['timed_out', 'closed_declined', 'resolved'].includes(String(thread.state))) return fail(response, 422, 'THREAD_REOPEN_INVALID', 'Thread cannot be reopened from its current state');
      thread.state = 'waiting'; thread.expires_at = null; thread.next_action_at = null; thread.updated_at = now();
      const opportunity = opportunities.get(String(thread.opportunity_id));
      if (opportunity && String(opportunity.state) === 'closed') { opportunity.state = 'open'; opportunity.close_reason = null; opportunity.revision = Number(opportunity.revision) + 1; opportunity.updated_at = now(); }
      return json(response, 200, thread);
    }
    const outcomeMatch = /^\/api\/v1\/sends\/([^/]+)\/outcome$/.exec(url.pathname);
    if (outcomeMatch && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const send = sends.get(outcomeMatch[1]!);
      if (!send) return fail(response, 404, 'SEND_NOT_FOUND', 'Send does not exist');
      const existing = outcomes.get(String(send.id));
      const outcome = { id: existing?.id ?? randomHex(), send_id: send.id, class: String(input.class), rung_accepted: input.rung_accepted ?? null, received_at: input.received_at ?? null, recorded_at: now(), note: input.note ?? null };
      if (existing && String(existing.class) !== String(input.class)) return fail(response, 422, 'OUTCOME_CONFLICT', 'An outcome is already recorded for this send');
      outcomes.set(String(send.id), outcome);
      const thread = threads.get(String(send.thread_id));
      if (thread && Array.isArray(thread.sends)) {
        thread.sends = thread.sends.map((entry: RecordData) => String(entry.id) === String(send.id) ? { ...entry, outcome } : entry);
      }
      if (String(input.class) === 'declined' && thread) {
        thread.state = 'closed_declined'; thread.expires_at = null; thread.updated_at = now();
        partySuppression.set(String(thread.recipient_id), true);
        for (const other of threads.values()) {
          if (String(other.recipient_id) === String(thread.recipient_id) && ['waiting', 'conversing'].includes(String(other.state))) {
            other.state = 'closed_declined'; other.expires_at = null;
          }
        }
        for (const opp of opportunities.values()) {
          const pairThreads = [...threads.values()].filter((t) => String(t.opportunity_id) === String(opp.id));
          const live = pairThreads.some((t) => ['waiting', 'conversing'].includes(String(t.state)));
          if (!live && String(opp.state) === 'open') { opp.state = 'closed'; opp.close_reason = 'declined'; }
        }
      } else if (thread && Number(send.touch_no) === Number(thread.current_touch) && String(thread.state) === 'waiting') {
        thread.state = 'conversing'; thread.expires_at = null; thread.updated_at = now();
      }
      return json(response, 200, outcome);
    }
    const clearDncMatch = /^\/api\/v1\/entities\/([a-f0-9]+)\/clear-dnc$/.exec(url.pathname);
    if (clearDncMatch && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const partyId = clearDncMatch[1]!;
      const reason = typeof input.reason === 'string' ? input.reason.trim() : '';
      if (reason.length < 10) return fail(response, 422, 'DNC_CLEARANCE_REASON', 'Do-not-contact clearance requires an explicit reason of at least ten characters');
      if (!directory.has(partyId)) return fail(response, 404, 'NOT_FOUND', 'Party does not exist');
      if (!partySuppression.get(partyId)) return fail(response, 422, 'NO_DNC_SUPPRESSION', 'Party has no do-not-contact suppression to clear');
      partySuppression.delete(partyId);
      pushEvent({ event_id: randomHex(), entity_type: 'party', entity_id: partyId, revision: 1, event_type: 'party.dnc_cleared', payload: { reason } });
      return json(response, 200, { party_id: partyId, state: 'open', do_not_contact: false, highest_rung: 0, touch_count: 0, revision: 1 });
    }
    const resolutionMatch = /^\/api\/v1\/opportunities\/([^/]+)\/resolution$/.exec(url.pathname);
    if (resolutionMatch && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const opportunity = opportunities.get(resolutionMatch[1]!);
      if (!opportunity) return fail(response, 404, 'OPPORTUNITY_NOT_FOUND', 'Opportunity does not exist');
      const id = randomHex();
      resolutions.set(id, { id, opportunity_id: resolutionMatch[1], thread_id: null, result: String(input.result), they_benefited: String(input.they_benefited), i_benefited: String(input.i_benefited), noted_at: now() });
      opportunity.state = 'resolved'; opportunity.close_reason = 'resolved'; opportunity.revision = Number(opportunity.revision) + 1; opportunity.updated_at = now();
      for (const thread of threads.values()) {
        if (String(thread.opportunity_id) === String(opportunity.id) && ['waiting', 'conversing', 'timed_out'].includes(String(thread.state))) {
          thread.state = 'resolved'; thread.expires_at = null;
        }
      }
      return json(response, 201, resolutions.get(id));
    }
    if (url.pathname === '/api/v1/capacity' && request.method === 'GET') {
      const items = [...capacityLedger.values()].map((record) => ({ ...record, remaining: Number(record.total) - Number(record.committed) }));
      return json(response, 200, items);
    }
    if (url.pathname === '/api/v1/capacity' && request.method === 'POST') {
      let input: RecordData; try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const key = `${String(input.period)}:${String(input.unit)}`;
      capacityLedger.set(key, { period: String(input.period), unit: String(input.unit), total: Number(input.total), committed: 0, updated_at: now() });
      const record = capacityLedger.get(key)!;
      return json(response, 200, { ...record, remaining: Number(record.total) - Number(record.committed) });
    }
    const s06DownloadMatch = /^\/api\/v1\/materials\/([^/]+)\/download$/.exec(url.pathname);
    if (s06DownloadMatch && request.method === 'GET') {
      const release = releaseRecords.get(url.searchParams.get('release_id') ?? '');
      if (!release) return fail(response, 404, 'RELEASE_NOT_FOUND', 'Release grant does not exist');
      if (release.consumed) return fail(response, 409, 'GRANT_CONSUMED', 'Release grant was already consumed by a download');
      release.consumed = true;
      const attachment = release.attachment as RecordData | null;
      response.writeHead(200, {
        'content-type': attachment ? String(attachment.content_type) : 'text/plain; charset=utf-8',
        'content-length': release.bytes.length,
        'content-disposition': attachment ? `attachment; filename="${String(attachment.filename)}"` : 'inline',
        'cache-control': 'no-store',
        'x-parley-epoch': epoch,
      });
      response.end(release.bytes);
      return;
    }
    if (url.pathname === '/api/v1/captures' && request.method === 'GET') {
      return json(response, 200, captures);
    }
    if (url.pathname === '/api/v1/captures' && request.method === 'POST') {
      let input: RecordData;
      try {
        input = await readBody(request);
      } catch {
        return fail(response, 400, 'INVALID_JSON', 'Request body is invalid');
      }
      if (typeof input.text !== 'string' || input.text.trim().length === 0) {
        return fail(response, 400, 'INVALID_CAPTURE', 'Captured source text is required');
      }
      const normalized = input.text.normalize('NFKC').replace(/\s+/g, ' ').trim().toLocaleLowerCase();
      const normalizedHash = createHash('sha256').update(normalized).digest('hex');
      const content = sourceGroups.get(normalizedHash) ?? { id: randomHex(), variants: new Map<string, string>() };
      sourceGroups.set(normalizedHash, content);
      const rawHash = createHash('sha256').update(input.text).digest('hex');
      const variantId = content.variants.get(rawHash) ?? randomHex();
      content.variants.set(rawHash, variantId);
      const result: SavedSource = {
        content_id: content.id,
        variant_id: variantId,
        occurrence_id: randomHex(),
        normalized_sha256: normalizedHash,
        raw_sha256: rawHash,
        job_status: 'queued',
      };
      sourceTextByOccurrence.set(result.occurrence_id, { text: input.text, title: typeof input.title === 'string' ? input.title : null, url: typeof input.url === 'string' ? input.url : null });
      // The mock uses a fixed deterministic proposal so browser tests exercise
      // review semantics without invoking a model. The Rust project remains
      // the authority for unavailable-provider persistence and real jobs.
      const jobId = randomHex();
      const batchId = randomHex();
      const proposalId = randomHex();
      result.job_id = jobId;
      const proposal = {
        id: proposalId,
        kind: 'signal',
        status: 'pending',
        candidate: {
          statement: input.text,
          quote: input.text,
          mention_name: input.text.includes('Dana Lee') ? 'Dana Lee' : null,
          mention_kind: input.text.includes('Dana Lee') ? 'person' : null,
        },
      };
      const job = {
        id: jobId,
        role: 'extractor',
        status: 'succeeded',
        current_attempt: 1,
        max_attempts: 3,
        progress_current: 1,
        progress_total: 1,
        injection_suspected: false,
        last_error_code: null,
        last_error: null,
        source_occurrence_id: result.occurrence_id,
        proposal_batch_id: batchId,
        proposals: [proposal],
      };
      extractorJobs.set(jobId, job);
      jobsByOccurrence.set(result.occurrence_id, jobId);
      captures.unshift(result);
      return json(response, 201, result);
    }

    const captureProposalMatch = /^\/api\/v1\/captures\/([^/]+)\/proposals$/.exec(url.pathname);
    if (captureProposalMatch && request.method === 'GET') {
      const jobId = jobsByOccurrence.get(captureProposalMatch[1]!);
      const job = jobId && extractorJobs.get(jobId);
      return job ? json(response, 200, job) : fail(response, 404, 'NOT_FOUND', 'No Extractor review exists for this source');
    }

    const jobMatch = /^\/api\/v1\/jobs\/([^/]+)$/.exec(url.pathname);
    if (jobMatch && request.method === 'GET') {
      const job = extractorJobs.get(jobMatch[1]!);
      return job ? json(response, 200, job) : fail(response, 404, 'NOT_FOUND', 'Extractor job does not exist');
    }
    if (jobMatch && request.method === 'POST') {
      const job = extractorJobs.get(jobMatch[1]!);
      if (!job) return fail(response, 404, 'NOT_FOUND', 'Extractor job does not exist');
      job.status = 'cancelled';
      job.last_error = 'Cancelled by owner';
      return json(response, 204, null);
    }
    const resolveMatch = /^\/api\/v1\/proposals\/([^/]+)\/resolve$/.exec(url.pathname);
    if (resolveMatch && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const job = [...extractorJobs.values()].find((candidate) => candidate.proposal_batch_id === resolveMatch[1]);
      if (!job || !Array.isArray(input.decisions)) return fail(response, 404, 'NOT_FOUND', 'Proposal batch does not exist');
      const proposals = job.proposals as Array<RecordData>;
      for (const decision of input.decisions as Array<RecordData>) {
        const proposal = proposals.find((candidate) => candidate.id === decision.item_id);
        if (!proposal) return fail(response, 422, 'PROPOSAL_INVALID', 'A selected proposal does not exist');
        proposal.status = decision.action === 'confirm' ? 'accepted' : String(decision.action);
      }
      return json(response, 200, { status: 'partially_applied' });
    }

    if (url.pathname === '/api/v1/agent/provider' && request.method === 'GET') return json(response, 200, provider);
    if (url.pathname === '/api/v1/agent/provider' && request.method === 'PUT') {
      provider = await readBody(request) as typeof provider;
      return json(response, 204, null);
    }
    if (url.pathname === '/api/v1/agent/provider/health' && request.method === 'POST') {
      return json(response, 200, { available: false, model: provider.model, models: [], message: 'Local provider is unavailable in the mock workspace.' });
    }
    return fail(response, 404, 'NOT_FOUND', 'Endpoint does not exist');
  }

  const plugin: Plugin = {
    name: 'parley-mock-api',
    configureServer(server) {
      server.middlewares.use((request, response, next) => {
        const url = request.url ?? '/';
        if (!url.startsWith('/api/') && url !== '/api') return next();
        void handle(request, response).catch((error) => {
          if (response.headersSent) return;
          fail(response, 500, 'MOCK_HANDLER_FAILED', error instanceof Error ? error.message : 'The fixture API handler failed');
        });
      });
    },
  };

  return {
    plugin,
    issueBootstrap() {
      const secret = randomHex(32);
      bootstraps.set(secret, Date.now() + 60_000);
      return secret;
    },
  };
}
