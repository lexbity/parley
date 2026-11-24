import { parley } from './client';

/**
 * Owner command client (UCR-001 §7). Every request uses the singleton
 * session's exact authentication exchange (`X-Parley-Token`, `X-Parley-Epoch`,
 * session cookie with same-origin credentials). Feature components never call
 * fetch themselves; they go through this module.
 */

export type CommandSurface = 'palette' | 'agent';
export type CommandEffect = 'read' | 'write' | 'destructive';
export type CommandContextValue = string | boolean | number;
export type CommandContext = Record<string, CommandContextValue>;
export type JsonSchema = Record<string, unknown>;
export type CommandArgs = Record<string, unknown>;

export interface CommandPrompt {
  field: string;
  picker: string;
  title: string;
}

export interface CommandDescriptor {
  id: string;
  version: number;
  title: string;
  summary: string;
  description: string;
  keywords: string[];
  effect: CommandEffect;
  requires_permission: string;
  surfaces: CommandSurface[];
  agent_tier?: 1 | 2;
  args_schema: JsonSchema;
  result_schema: JsonSchema;
  supports_dry_run: boolean;
  when?: string;
  palette: { context_fields: string[]; prompts: CommandPrompt[] };
}

export interface CommandManifest {
  contract_major: number;
  manifest_version: string;
  truncated: boolean;
  commands: CommandDescriptor[];
}

export interface CommandSearchItem {
  id: string;
  version: number;
  title: string;
  summary: string;
  effect: CommandEffect;
}

export interface CommandSearchResult {
  items: CommandSearchItem[];
  total: number;
  next_cursor: string | null;
}

export interface CommandInvocation {
  version: number;
  args: CommandArgs;
  dry_run: boolean;
}

export interface CommandSuccess {
  ok: true;
  command_version: number;
  audit_id: string;
  original_audit_id?: string | null;
  replayed: boolean;
  dry_run: boolean;
  result: Record<string, unknown>;
}

export interface CommandErrorDetail {
  path?: string;
  problem?: string;
  [key: string]: unknown;
}

export interface CommandFailure {
  ok: false;
  command_version?: number;
  audit_id?: string;
  error: { code: string; message: string; details: CommandErrorDetail[] };
}

export type CommandResponse = CommandSuccess | CommandFailure;

export class CommandApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code: string,
    readonly details: CommandErrorDetail[] = [],
    readonly response: CommandFailure | null = null,
  ) {
    super(message);
  }

  get isConflict(): boolean {
    return this.status === 409 || this.status === 412
      || ['conflict', 'stale_revision', 'version_mismatch'].includes(this.code);
  }
}

function object(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function requiredString(value: unknown, field: string): string {
  if (typeof value !== 'string') throw new Error(`Invalid command response: ${field} must be text`);
  return value;
}

function stringList(value: unknown, field: string): string[] {
  if (!Array.isArray(value) || !value.every((item) => typeof item === 'string')) {
    throw new Error(`Invalid command response: ${field} must be a text list`);
  }
  return value as string[];
}

function decodePrompt(value: unknown): CommandPrompt {
  const prompt = object(value);
  if (!prompt) throw new Error('Invalid command response: palette prompt is malformed');
  return {
    field: requiredString(prompt.field, 'prompt.field'),
    picker: requiredString(prompt.picker, 'prompt.picker'),
    title: requiredString(prompt.title, 'prompt.title'),
  };
}

function decodeDescriptor(value: unknown): CommandDescriptor {
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.version) || typeof item.supports_dry_run !== 'boolean') {
    throw new Error('Invalid command response: descriptor is malformed');
  }
  const palette = object(item.palette);
  if (!palette) throw new Error('Invalid command response: descriptor palette is malformed');
  const surfaces = stringList(item.surfaces, 'descriptor.surfaces');
  const agentTier = item.agent_tier;
  if (agentTier !== undefined && ![1, 2].includes(Number(agentTier))) {
    throw new Error('Invalid command response: descriptor agent tier is malformed');
  }
  return {
    id: requiredString(item.id, 'descriptor.id'),
    version: Number(item.version),
    title: requiredString(item.title, 'descriptor.title'),
    summary: typeof item.summary === 'string' ? item.summary : '',
    description: typeof item.description === 'string' ? item.description : '',
    keywords: item.keywords === undefined ? [] : stringList(item.keywords, 'descriptor.keywords'),
    effect: requiredString(item.effect, 'descriptor.effect') as CommandEffect,
    requires_permission: typeof item.requires_permission === 'string' ? item.requires_permission : '',
    surfaces: surfaces as CommandSurface[],
    ...(agentTier === undefined ? {} : { agent_tier: Number(agentTier) as 1 | 2 }),
    args_schema: object(item.args_schema) ?? {},
    result_schema: object(item.result_schema) ?? {},
    supports_dry_run: item.supports_dry_run,
    ...(typeof item.when === 'string' ? { when: item.when } : {}),
    palette: {
      context_fields: stringList(palette.context_fields, 'descriptor.palette.context_fields'),
      prompts: Array.isArray(palette.prompts) ? palette.prompts.map(decodePrompt) : [],
    },
  };
}

function decodeManifest(value: unknown): CommandManifest {
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.contract_major) || !Array.isArray(item.commands)
    || typeof item.truncated !== 'boolean') {
    throw new Error('Invalid command response: manifest is malformed');
  }
  return {
    contract_major: Number(item.contract_major),
    manifest_version: requiredString(item.manifest_version, 'manifest_version'),
    truncated: item.truncated,
    commands: item.commands.map(decodeDescriptor),
  };
}

function decodeSearch(value: unknown): CommandSearchResult {
  const item = object(value);
  if (!item || !Array.isArray(item.items) || !Number.isSafeInteger(item.total)) {
    throw new Error('Invalid command response: search result is malformed');
  }
  return {
    items: item.items.map((raw) => {
      const hit = object(raw);
      if (!hit || !Number.isSafeInteger(hit.version)) throw new Error('Invalid command response: search item is malformed');
      return {
        id: requiredString(hit.id, 'search.id'),
        version: Number(hit.version),
        title: requiredString(hit.title, 'search.title'),
        summary: typeof hit.summary === 'string' ? hit.summary : '',
        effect: requiredString(hit.effect, 'search.effect') as CommandEffect,
      };
    }),
    total: Number(item.total),
    next_cursor: typeof item.next_cursor === 'string' ? item.next_cursor : null,
  };
}

function decodeFailure(value: unknown, status: number): CommandFailure {
  const root = object(value);
  const error = root ? object(root.error) : null;
  if (!error || typeof error.message !== 'string' || typeof error.code !== 'string') {
    return {
      ok: false,
      ...(root && Number.isSafeInteger(root.command_version) ? { command_version: Number(root.command_version) } : {}),
      ...(root && typeof root.audit_id === 'string' ? { audit_id: root.audit_id } : {}),
      error: { code: 'INVALID_RESPONSE', message: `Command failed (${status})`, details: [] },
    };
  }
  const details = Array.isArray(error.details)
    ? error.details.flatMap((raw) => {
      const detail = object(raw);
      return detail ? [detail as CommandErrorDetail] : [];
    })
    : [];
  return {
    ok: false,
    ...(root && Number.isSafeInteger(root.command_version) ? { command_version: Number(root.command_version) } : {}),
    ...(root && typeof root.audit_id === 'string' ? { audit_id: root.audit_id } : {}),
    error: { code: error.code, message: error.message, details },
  };
}

function decodeSuccess(value: unknown): CommandSuccess {
  const item = object(value);
  if (!item || item.ok !== true || !Number.isSafeInteger(item.command_version)
    || typeof item.audit_id !== 'string' || typeof item.replayed !== 'boolean'
    || typeof item.dry_run !== 'boolean' || !object(item.result)) {
    throw new Error('Invalid command response: invocation receipt is malformed');
  }
  return {
    ok: true,
    command_version: Number(item.command_version),
    audit_id: item.audit_id,
    ...(typeof item.original_audit_id === 'string' ? { original_audit_id: item.original_audit_id } : {}),
    replayed: item.replayed,
    dry_run: item.dry_run,
    result: item.result as Record<string, unknown>,
  };
}

export class CommandClient {
  async query(context: CommandContext = {}, surface: CommandSurface = 'palette'): Promise<CommandManifest> {
    return decodeManifest(await this.request('/query', {
      method: 'POST',
      body: JSON.stringify({ surface, context }),
    }));
  }

  async search(query: string, limit = 10, context: CommandContext = {}): Promise<CommandSearchResult> {
    return decodeSearch(await this.request('/search', {
      method: 'POST',
      body: JSON.stringify({ query, limit, context }),
    }));
  }

  async describe(id: string): Promise<CommandDescriptor> {
    return decodeDescriptor(await this.request(`/${encodeURIComponent(id)}`, { method: 'GET' }));
  }

  /**
   * Invoke a command with an immutable operation key. Every distinct intent
   * (selection, revision, args or command version) must get a fresh key.
   */
  async invoke(
    id: string,
    invocation: CommandInvocation,
    operationKey: string,
    signal?: AbortSignal,
  ): Promise<CommandSuccess> {
    const value = await this.request(`/${encodeURIComponent(id)}`, {
      method: 'POST',
      body: JSON.stringify(invocation),
      headers: { 'Idempotency-Key': operationKey },
      ...(signal ? { signal } : {}),
    });
    const root = object(value);
    if (root && root.ok === false) {
      const failure = decodeFailure(value, 200);
      throw new CommandApiError(
        failure.error.message,
        422,
        failure.error.code,
        failure.error.details,
        failure,
      );
    }
    return decodeSuccess(value);
  }

  /** Start one scoped run. Every distinct intent needs a fresh operation key. */
  async startRun(input: StartRunInput, operationKey: string, signal?: AbortSignal): Promise<StartedRun> {
    const bytes = new TextEncoder().encode(input.task).length;
    if (bytes < 1 || bytes > 8192) throw new Error('Task must be 1..8192 bytes');
    return decodeStartedRun(await this.requestApi('/agent/runs', {
      method: 'POST',
      body: JSON.stringify(input),
      headers: { 'Idempotency-Key': operationKey },
      ...(signal ? { signal } : {}),
    }));
  }

  async listRuns(options: { status?: RunState; limit?: number; cursor?: string } = {}): Promise<RunList> {
    return decodeRunList(await this.requestApi(`/agent/runs${runQuery(options)}`, { method: 'GET' }));
  }

  async getRun(runId: string): Promise<RunStatus> {
    return decodeRunStatus(await this.requestApi(`/agent/runs/${encodeURIComponent(runId)}`, { method: 'GET' }));
  }

  async getRunTranscript(runId: string, options: { limit?: number; cursor?: string } = {}): Promise<TranscriptPage> {
    return decodeTranscript(await this.requestApi(`/agent/runs/${encodeURIComponent(runId)}/transcript${runQuery(options)}`, { method: 'GET' }));
  }

  /** Cancellation takes its own operation key, never the start key. */
  async cancelRun(runId: string, operationKey: string): Promise<CancelRunResult> {
    return decodeCancel(await this.requestApi(`/agent/runs/${encodeURIComponent(runId)}/cancel`, {
      method: 'POST',
      headers: { 'Idempotency-Key': operationKey },
    }));
  }

  private async request(path: string, init: RequestInit = {}): Promise<unknown> {
    return this.requestApi(`/commands${path}`, init);
  }

  private async requestApi(path: string, init: RequestInit = {}): Promise<unknown> {
    await parley.connect();
    const headers = parley.authHeaders(
      init.headers instanceof Headers
        ? Object.fromEntries(init.headers.entries())
        : { ...((init.headers as Record<string, string> | undefined) ?? {}) },
    );
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
      throw new CommandApiError('The local service returned an unreadable response.', response.status, 'INVALID_RESPONSE');
    }
    if (!response.ok) {
      const failure = decodeFailure(payload, response.status);
      throw new CommandApiError(
        failure.error.message,
        response.status,
        failure.error.code,
        failure.error.details,
        failure,
      );
    }
    return payload;
  }
}

export const commands = new CommandClient();

// --- Delegated runs (UCR-001 U02). These DTOs are owner-visible projections;
// no tool, provider or worker credential is ever accepted by this client.
export const RUN_STATES = ['queued', 'running', 'waiting_owner', 'waiting_job', 'succeeded', 'failed', 'cancelled', 'expired'] as const;
export type RunState = typeof RUN_STATES[number];
export const RUN_TARGET_KINDS = ['party', 'opportunity', 'resource', 'material', 'segment'] as const;
export type RunTargetKind = typeof RUN_TARGET_KINDS[number];
export const RUN_PERMISSIONS = ['party.read', 'opportunity.read', 'opportunity.create', 'opportunity.phase.edit', 'party.edit', 'opportunity.task.edit', 'brief.start', 'job.read'] as const;
export type RunPermission = typeof RUN_PERMISSIONS[number];
export const RUN_TRANSCRIPT_SPEAKERS = ['owner', 'assistant', 'tool', 'system'] as const;

export interface RunTarget { kind: RunTargetKind; id: string }
export interface StartRunInput {
  task: string;
  targets: RunTarget[];
  permissions: RunPermission[];
  allow_created_opportunities: boolean;
}
export interface StartedRun {
  run_id: string;
  state: 'queued';
  revision: number;
  granted_actions: string[];
  targets: RunTarget[];
  deadline_at: string;
  phase: string;
}
export interface RunListItem { run_id: string; state: RunState; revision: number; deadline_at: string }
export interface RunList { items: RunListItem[]; next_cursor: string | null }
export interface RunReview {
  job_id: string;
  scope_kind: string;
  scope_id: string;
}

export interface RunStatus {
  run_id: string;
  state: RunState;
  revision: number;
  deadline_at: string;
  turn_count: number;
  transcript_bytes: number;
  wake_condition: string | null;
  review: RunReview | null;
}
export interface TranscriptTurn { turn_no: number; speaker: 'owner' | 'assistant' | 'tool' | 'system'; ordinal: number; text: string }
export interface TranscriptPage { items: TranscriptTurn[]; next_cursor: string | null }
export interface CancelRunResult { run_id: string; state: 'cancelled' | 'unchanged' }

function decodeRunState(value: unknown, field: string): RunState {
  if (!RUN_STATES.includes(value as RunState)) throw new Error(`Invalid run response: ${field} has an unknown state`);
  return value as RunState;
}

function decodeRunTarget(value: unknown): RunTarget {
  const target = object(value);
  if (!target || !RUN_TARGET_KINDS.includes(target.kind as RunTargetKind)) {
    throw new Error('Invalid run response: target is malformed');
  }
  return { kind: target.kind as RunTargetKind, id: requiredString(target.id, 'target.id') };
}

function decodeRunTargets(value: unknown): RunTarget[] {
  if (!Array.isArray(value)) throw new Error('Invalid run response: targets must be a list');
  return value.map(decodeRunTarget);
}

function decodeRunRevision(value: unknown): number {
  if (!Number.isSafeInteger(value) || Number(value) < 1) throw new Error('Invalid run response: revision must be a positive integer');
  return Number(value);
}

function decodeNullableText(value: unknown, field: string): string | null {
  if (value === null || value === undefined) return null;
  if (typeof value !== 'string') throw new Error(`Invalid run response: ${field} must be text or null`);
  return value;
}

function decodeRunCursor(value: unknown): string | null {
  const cursor = decodeNullableText(value, 'next_cursor');
  if (cursor !== null && !/^[0-9a-f]+$/.test(cursor)) throw new Error('Invalid run response: next_cursor is malformed');
  return cursor;
}

function decodeStartedRun(value: unknown): StartedRun {
  const run = object(value);
  if (!run) throw new Error('Invalid run response: start receipt is malformed');
  const state = decodeRunState(run.state, 'state');
  if (state !== 'queued') throw new Error('Invalid run response: a new run must be queued');
  if (!Number.isSafeInteger(run.revision)) throw new Error('Invalid run response: revision is malformed');
  return {
    run_id: requiredString(run.run_id, 'run_id'),
    state: 'queued',
    revision: decodeRunRevision(run.revision),
    granted_actions: stringList(run.granted_actions, 'granted_actions'),
    targets: decodeRunTargets(run.targets),
    deadline_at: requiredString(run.deadline_at, 'deadline_at'),
    phase: requiredString(run.phase, 'phase'),
  };
}

function decodeRunList(value: unknown): RunList {
  const list = object(value);
  if (!list || !Array.isArray(list.items)) throw new Error('Invalid run response: run list is malformed');
  return {
    items: list.items.map((raw) => {
      const item = object(raw);
      if (!item || !Number.isSafeInteger(item.revision)) throw new Error('Invalid run response: run row is malformed');
      return {
        run_id: requiredString(item.run_id, 'run_id'),
        state: decodeRunState(item.state, 'state'),
        revision: decodeRunRevision(item.revision),
        deadline_at: requiredString(item.deadline_at, 'deadline_at'),
      };
    }),
    next_cursor: decodeNullableText(list.next_cursor, 'next_cursor'),
  };
}

function decodeRunStatus(value: unknown): RunStatus {
  const status = object(value);
  if (!status || !Number.isSafeInteger(status.turn_count) || !Number.isSafeInteger(status.transcript_bytes)) {
    throw new Error('Invalid run response: status is malformed');
  }
  let review: RunReview | null = null;
  if (status.review !== null && status.review !== undefined) {
    const raw = object(status.review);
    if (!raw) throw new Error('Invalid run response: review is malformed');
    review = {
      job_id: requiredString(raw.job_id, 'review.job_id'),
      scope_kind: requiredString(raw.scope_kind, 'review.scope_kind'),
      scope_id: requiredString(raw.scope_id, 'review.scope_id'),
    };
  }
  return {
    run_id: requiredString(status.run_id, 'run_id'),
    state: decodeRunState(status.state, 'state'),
    revision: decodeRunRevision(status.revision),
    deadline_at: requiredString(status.deadline_at, 'deadline_at'),
    turn_count: Number(status.turn_count),
    transcript_bytes: Number(status.transcript_bytes),
    wake_condition: decodeNullableText(status.wake_condition, 'wake_condition'),
    review,
  };
}

function decodeTranscript(value: unknown): TranscriptPage {
  const page = object(value);
  if (!page || !Array.isArray(page.items)) throw new Error('Invalid run response: transcript is malformed');
  return {
    items: page.items.map((raw) => {
      const turn = object(raw);
      if (!turn || !Number.isSafeInteger(turn.turn_no) || !Number.isSafeInteger(turn.ordinal)
        || !RUN_TRANSCRIPT_SPEAKERS.includes(turn.speaker as TranscriptTurn['speaker'])) {
        throw new Error('Invalid run response: transcript turn is malformed');
      }
      return {
        turn_no: Number(turn.turn_no),
        speaker: turn.speaker as TranscriptTurn['speaker'],
        ordinal: Number(turn.ordinal),
        text: requiredString(turn.text, 'turn.text'),
      };
    }),
    next_cursor: decodeRunCursor(page.next_cursor),
  };
}

function decodeCancel(value: unknown): CancelRunResult {
  const result = object(value);
  if (!result || !['cancelled', 'unchanged'].includes(String(result.state))) {
    throw new Error('Invalid run response: cancellation receipt is malformed');
  }
  return { run_id: requiredString(result.run_id, 'run_id'), state: result.state as CancelRunResult['state'] };
}

function runQuery(params: { status?: string; limit?: number; cursor?: string }): string {
  const query = new URLSearchParams();
  if (params.status) {
    if (!RUN_STATES.includes(params.status as RunState)) throw new Error('Invalid run filter: unknown status');
    query.set('status', params.status);
  }
  if (params.limit !== undefined) {
    if (!Number.isSafeInteger(params.limit) || params.limit < 1 || params.limit > 20) throw new Error('Invalid run page: limit must be 1..20');
    query.set('limit', String(params.limit));
  }
  if (params.cursor) query.set('cursor', params.cursor);
  const text = query.toString();
  return text ? `?${text}` : '';
}
