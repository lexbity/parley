import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley } from './client';
import { CommandApiError, commands, type CommandDescriptor, type CommandManifest } from './commands';

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const descriptor: CommandDescriptor = {
  id: 'opportunity.set_phase',
  version: 1,
  title: 'Change opportunity phase',
  summary: 'Move an open opportunity to a planning phase',
  description: 'Moves an open opportunity to a planning stage.',
  keywords: ['queue', 'stage'],
  effect: 'write',
  requires_permission: 'opportunity:phase_edit',
  surfaces: ['palette', 'agent'],
  agent_tier: 2,
  args_schema: { type: 'object', additionalProperties: false },
  result_schema: { type: 'object' },
  supports_dry_run: false,
  when: 'route == "queue" && selection.opportunity',
  palette: { context_fields: ['route', 'selection.opportunity'], prompts: [{ field: 'stage_id', picker: 'planning_stage', title: 'Planning phase' }] },
};

const manifest: CommandManifest = {
  contract_major: 1,
  manifest_version: 'sha256:abc',
  truncated: false,
  commands: [descriptor],
};

function stubFetch(body: unknown, status = 200) {
  const fetchMock = vi.fn().mockResolvedValue({
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  });
  vi.stubGlobal('fetch', fetchMock);
  vi.spyOn(parley, 'connect').mockResolvedValue();
  vi.spyOn(parley, 'authHeaders').mockImplementation((extra = {}) => ({
    Accept: 'application/json', 'Content-Type': 'application/json',
    'X-Parley-Token': 'token-1', 'X-Parley-Epoch': 'epoch-1', ...extra,
  }));
  return fetchMock;
}

describe('CommandClient', () => {
  it('queries the palette manifest with owner credentials and a closed context', async () => {
    const fetchMock = stubFetch(manifest);
    const result = await commands.query({ route: 'queue', 'selection.opportunity': true });
    expect(result).toEqual(manifest);
    expect(fetchMock).toHaveBeenCalledOnce();
    const [url, init] = fetchMock.mock.calls[0]! as [string, RequestInit];
    expect(url).toBe('/api/v1/commands/query');
    expect(init.credentials).toBe('same-origin');
    expect(JSON.parse(String(init.body))).toEqual({ surface: 'palette', context: { route: 'queue', 'selection.opportunity': true } });
    const headers = init.headers as Record<string, string>;
    expect(headers['X-Parley-Token']).toBe('token-1');
    expect(headers['X-Parley-Epoch']).toBe('epoch-1');
  });

  it('searches with an explicit limit and decodes search hits', async () => {
    const fetchMock = stubFetch({ items: [{ id: descriptor.id, version: 1, title: descriptor.title, summary: descriptor.summary, effect: 'write' }], total: 1, next_cursor: null });
    const result = await commands.search('phase', 5);
    expect(result.items[0]?.id).toBe(descriptor.id);
    expect(JSON.parse(String((fetchMock.mock.calls[0]![1] as RequestInit).body))).toEqual({ query: 'phase', limit: 5, context: {} });
  });

  it('describes a single command by immutable id', async () => {
    const fetchMock = stubFetch(descriptor);
    expect((await commands.describe('opportunity.set_phase')).id).toBe(descriptor.id);
    expect(fetchMock.mock.calls[0]![0]).toBe('/api/v1/commands/opportunity.set_phase');
  });

  it('sends the operation key and invocation envelope, and decodes the receipt', async () => {
    const fetchMock = stubFetch({ ok: true, command_version: 1, audit_id: 'aud_1', replayed: false, dry_run: false, result: { stage_id: 'preparing' } });
    const receipt = await commands.invoke('opportunity.set_phase', {
      version: 1, args: { stage_id: 'preparing' }, dry_run: false,
    }, 'key-1');
    expect(receipt.audit_id).toBe('aud_1');
    const init = fetchMock.mock.calls[0]![1] as RequestInit;
    expect((init.headers as Record<string, string>)['Idempotency-Key']).toBe('key-1');
    expect(JSON.parse(String(init.body))).toEqual({ version: 1, args: { stage_id: 'preparing' }, dry_run: false });
  });

  it('passes an abort signal through to a stale invocation', async () => {
    const fetchMock = stubFetch({ ok: true, command_version: 1, audit_id: 'aud_1', replayed: false, dry_run: false, result: {} });
    const controller = new AbortController();
    await commands.invoke('opportunity.undo_phase_change', { version: 1, args: {}, dry_run: false }, 'key-2', controller.signal);
    expect((fetchMock.mock.calls[0]![1] as RequestInit).signal).toBe(controller.signal);
  });

  it('surfaces a command failure envelope with code, message and details', async () => {
    stubFetch({ ok: false, command_version: 1, error: { code: 'conflict', message: 'Phase revision changed', details: [{ path: '/expected_phase_revision', problem: 'stale' }] } }, 409);
    await expect(commands.invoke('opportunity.set_phase', { version: 1, args: {}, dry_run: false }, 'key-3'))
      .rejects.toMatchObject({ status: 409, code: 'conflict', isConflict: true });
  });

  it('rejects unreadable responses and transport errors', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, status: 200, json: async () => { throw new Error('bad json'); } }));
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'authHeaders').mockReturnValue({});
    await expect(commands.query()).rejects.toBeInstanceOf(CommandApiError);
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue('offline'));
    await expect(commands.query()).rejects.toBeInstanceOf(TypeError);
  });

  it('rejects malformed manifests, search rows and envelope fallbacks', async () => {
    stubFetch({ contract_major: 'one', manifest_version: 'v', truncated: false, commands: [] });
    await expect(commands.query()).rejects.toThrow(/manifest is malformed/);
    stubFetch({ contract_major: 1, manifest_version: 'v', truncated: false, commands: [{ ...descriptor, keywords: [7] }] });
    await expect(commands.query()).rejects.toThrow(/keywords must be a text list/);
    stubFetch({ contract_major: 1, manifest_version: 'v', truncated: false, commands: [{ ...descriptor, agent_tier: 9 }] });
    await expect(commands.query()).rejects.toThrow(/agent tier is malformed/);
    stubFetch({ items: {}, total: 0, next_cursor: null });
    await expect(commands.search('phase')).rejects.toThrow(/search result is malformed/);
    stubFetch({ error: 'not-an-object' }, 409);
    await expect(commands.describe('opportunity.set_phase')).rejects.toMatchObject({ code: 'INVALID_RESPONSE', status: 409 });
    stubFetch({ ok: false, error: { code: 'conflict', message: 'stale revision', details: [] } });
    await expect(commands.invoke('opportunity.set_phase', { version: 1, args: {}, dry_run: false }, 'key-bad'))
      .rejects.toMatchObject({ status: 422, code: 'conflict', message: 'stale revision' });
  });

  it('rejects malformed descriptors and receipts', async () => {
    stubFetch({ contract_major: 1, manifest_version: 'v', truncated: false, commands: [{ id: 'x' }] });
    await expect(commands.query()).rejects.toThrow(/descriptor is malformed/);
    stubFetch({ ok: true });
    await expect(commands.invoke('opportunity.set_phase', { version: 1, args: {}, dry_run: false }, 'key-4')).rejects.toThrow(/receipt is malformed/);
  });
});

const startInput = {
  task: 'Summarise the granted party',
  targets: [{ kind: 'party' as const, id: 'party-1' }],
  permissions: ['party.read' as const],
  allow_created_opportunities: false,
};
const startedReceipt = {
  run_id: 'run-1', state: 'queued', revision: 1, granted_actions: ['party.read'],
  targets: startInput.targets, deadline_at: '2026-11-23T10:00:00Z', phase: 'queued',
};

describe('Agent run client (U02)', () => {
  it('starts a scoped run with the shared session and its own operation key', async () => {
    const fetchMock = stubFetch(startedReceipt, 201);
    const result = await commands.startRun(startInput, 'start-key-1');
    expect(result).toEqual(startedReceipt);
    const [url, init] = fetchMock.mock.calls[0]! as [string, RequestInit];
    expect(url).toBe('/api/v1/agent/runs');
    expect(init.method).toBe('POST');
    expect((init.headers as Record<string, string>)['Idempotency-Key']).toBe('start-key-1');
    expect((init.headers as Record<string, string>)['X-Parley-Token']).toBe('token-1');
    expect(JSON.parse(String(init.body))).toEqual(startInput);
  });

  it('rejects an empty or oversized task before any request', async () => {
    const fetchMock = stubFetch(startedReceipt);
    await expect(commands.startRun({ ...startInput, task: '' }, 'k')).rejects.toThrow(/1\.\.8192 bytes/);
    await expect(commands.startRun({ ...startInput, task: 'ø'.repeat(4097) }, 'k')).rejects.toThrow(/1\.\.8192 bytes/);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('lists runs with a closed status filter, bounded limit and cursor', async () => {
    const fetchMock = stubFetch({ items: [{ run_id: 'run-1', state: 'succeeded', revision: 3, deadline_at: '2026-11-23T10:00:00Z' }], next_cursor: 'run-1' });
    const result = await commands.listRuns({ status: 'succeeded', limit: 2, cursor: 'run-0' });
    expect(result.items[0]?.state).toBe('succeeded');
    expect(fetchMock.mock.calls[0]![0]).toBe('/api/v1/agent/runs?status=succeeded&limit=2&cursor=run-0');
    await expect(commands.listRuns({ status: 'unknown' as never })).rejects.toThrow(/unknown status/);
    await expect(commands.listRuns({ limit: 21 })).rejects.toThrow(/1\.\.20/);
  });

  it('reads authoritative run status and plain-text transcript pages', async () => {
    stubFetch({ run_id: 'run-1', state: 'succeeded', revision: 4, deadline_at: '2026-11-23T10:00:00Z', turn_count: 2, transcript_bytes: 90, wake_condition: null });
    const status = await commands.getRun('run/1');
    expect(status).toMatchObject({ turn_count: 2, wake_condition: null });
    const fetchMock = stubFetch({ items: [{ turn_no: 2, speaker: 'tool', ordinal: 1, text: 'Scoped Party' }, { turn_no: 2, speaker: 'assistant', ordinal: 2, text: 'Done.' }], next_cursor: '323a31' });
    const page = await commands.getRunTranscript('run-1', { limit: 2, cursor: '323a30' });
    expect(page.items.map((item) => item.speaker)).toEqual(['tool', 'assistant']);
    expect(page.next_cursor).toBe('323a31');
    expect(fetchMock.mock.calls[0]![0]).toBe('/api/v1/agent/runs/run-1/transcript?limit=2&cursor=323a30');
  });

  it('cancels with a separate key and decodes unchanged replays', async () => {
    const fetchMock = stubFetch({ run_id: 'run-1', state: 'unchanged' });
    expect((await commands.cancelRun('run-1', 'cancel-key-1')).state).toBe('unchanged');
    const [url, init] = fetchMock.mock.calls[0]! as [string, RequestInit];
    expect(url).toBe('/api/v1/agent/runs/run-1/cancel');
    expect((init.headers as Record<string, string>)['Idempotency-Key']).toBe('cancel-key-1');
  });

  it('rejects malformed run receipts and surfaces the standard error envelope', async () => {
    stubFetch({ ...startedReceipt, state: 'running' });
    await expect(commands.startRun(startInput, 'k')).rejects.toThrow(/must be queued/);
    stubFetch({ ...startedReceipt, targets: [{ kind: 'root', id: 'x' }] });
    await expect(commands.startRun(startInput, 'k')).rejects.toThrow(/target is malformed/);
    stubFetch({ items: [{ run_id: 'r', state: 'pending', revision: 1, deadline_at: 'x' }], next_cursor: null });
    await expect(commands.listRuns()).rejects.toThrow(/unknown state/);
    stubFetch({ run_id: 'r', state: 'queued', revision: 0, deadline_at: 'x', turn_count: 0, transcript_bytes: 0, wake_condition: null });
    await expect(commands.getRun('r')).rejects.toThrow(/revision must be a positive integer/);
    stubFetch({ items: [{ turn_no: 1, speaker: 'root', ordinal: 1, text: 'x' }], next_cursor: null });
    await expect(commands.getRunTranscript('r')).rejects.toThrow(/turn is malformed/);
    stubFetch({ items: [], next_cursor: 'not-hex!' });
    await expect(commands.getRunTranscript('r')).rejects.toThrow(/next_cursor is malformed/);
    stubFetch({ run_id: 'r', state: 'done' });
    await expect(commands.cancelRun('r', 'k')).rejects.toThrow(/cancellation receipt is malformed/);
    stubFetch({ error: { code: 'target_unavailable', message: 'Run is not available', details: [] } }, 404);
    await expect(commands.getRun('missing')).rejects.toMatchObject({ status: 404, code: 'target_unavailable' });
  });
});

function stubFetchWithHeaders(body: unknown, status: number, location: string | null) {
  const fetchMock = vi.fn().mockResolvedValue({
    ok: status >= 200 && status < 300,
    status,
    headers: { get: (name: string) => (name.toLowerCase() === 'location' ? location : null) },
    json: async () => body,
  });
  vi.stubGlobal('fetch', fetchMock);
  vi.spyOn(parley, 'connect').mockResolvedValue();
  vi.spyOn(parley, 'authHeaders').mockImplementation((extra = {}) => ({
    Accept: 'application/json', 'Content-Type': 'application/json',
    'X-Parley-Token': 'token-1', 'X-Parley-Epoch': 'epoch-1', ...extra,
  }));
  return fetchMock;
}

describe('CommandClient approvals (U08)', () => {
  const approvalRow = {
    id: 'aprv-1', command_id: 'resource.retire', command_version: 1, revision: 1,
    status: 'pending', expires_at: '2026-11-23T00:10:00Z',
    preview_digest: 'a'.repeat(64), title: 'Gift A', impacted_versions_count: 2,
    args: { resource_id: 'res-1' }, preview: { title: 'Gift A' },
  };

  it('lists approvals and decodes active rows', async () => {
    stubFetch({ items: [approvalRow], next_cursor: null });
    const result = await commands.listApprovals({ status: 'pending' });
    expect(result.items[0]?.status).toBe('pending');
    expect(result.items[0]?.args).toEqual({ resource_id: 'res-1' });
  });

  it('reads one approval detail', async () => {
    stubFetch(approvalRow);
    const detail = await commands.getApproval('aprv-1');
    expect(detail.preview_digest).toHaveLength(64);
    expect(detail.preview?.title).toBe('Gift A');
  });

  it('grants and denies with a decision envelope and operation key', async () => {
    const grantFetch = stubFetch({ id: 'aprv-1', revision: 2, status: 'granted' });
    const granted = await commands.grantApproval('aprv-1', { expected_revision: 1, preview_digest: 'a'.repeat(64) }, 'decision-1');
    expect(granted.status).toBe('granted');
    const [, init] = grantFetch.mock.calls[0]! as [string, RequestInit];
    expect((init.headers as Record<string, string>)['Idempotency-Key']).toBe('decision-1');
    expect(JSON.parse(String(init.body))).toEqual({ expected_revision: 1, preview_digest: 'a'.repeat(64) });
    stubFetch({ id: 'aprv-1', revision: 2, status: 'denied' });
    expect((await commands.denyApproval('aprv-1', { expected_revision: 1, preview_digest: 'a'.repeat(64) }, 'decision-2')).status).toBe('denied');
  });

  it('resumes an approved invocation with the frozen key and approval id', async () => {
    stubFetch({ ok: true, command_version: 1, audit_id: 'aud_1', replayed: false, dry_run: false, result: { readiness: 'retired' } });
    const receipt = await commands.invoke('resource.retire', {
      version: 1,
      args: { resource_id: 'res-1', expected_card_revision: 1, expected_content_revision: 1 },
      dry_run: false,
      approval_id: 'aprv-1',
    }, 'frozen-key');
    expect(receipt.result.readiness).toBe('retired');
  });

  it('surfaces approval_required with the owner-only location', async () => {
    stubFetchWithHeaders(
      { ok: false, command_version: 1, audit_id: 'aud_1', error: { code: 'approval_required', message: 'Review the requested retirement in Approvals.', details: [] } },
      403,
      '/api/v1/approvals/aprv-1',
    );
    await expect(commands.invoke('resource.retire', { version: 1, args: { resource_id: 'res-1' }, dry_run: false }, 'k'))
      .rejects.toMatchObject({ status: 403, code: 'approval_required', approvalLocation: '/api/v1/approvals/aprv-1' });
  });
});

describe('CommandClient approval decoding (U08)', () => {
  it('rejects malformed approval lists and decisions', async () => {
    stubFetch({ items: 'none', next_cursor: null });
    await expect(commands.listApprovals()).rejects.toThrow(/approval list is malformed/);
    stubFetch({ id: 'aprv-1', revision: 1, status: 'pending' });
    await expect(commands.getApproval('aprv-1')).rejects.toThrow(/approval row is malformed/);
    stubFetch({ id: 'aprv-1', command_id: 'resource.retire', command_version: 1, revision: 1, status: 'maybe', expires_at: 'x', preview_digest: 'a'.repeat(64) });
    await expect(commands.getApproval('aprv-1')).rejects.toThrow(/unknown status/);
    stubFetch({ id: 'aprv-1', status: 'granted' });
    await expect(commands.grantApproval('aprv-1', { expected_revision: 1, preview_digest: 'a'.repeat(64) }, 'k'))
      .rejects.toThrow(/decision receipt is malformed/);
  });
});

describe('CommandClient notification stream (U08)', () => {
  it('parses SSE signals and closes on cleanup', async () => {
    let index = 0;
    const chunks = [
      'event: epoch_changed\ndata: {"epoch":"e1"}\n\n',
      ': heartbeat\n\n',
      'event: approvals_changed\ndata: {}\n\n',
      'event: run_changed\ndata: {}\n\n',
    ];
    const reader = {
      read: async () => {
        if (index < chunks.length) return { done: false, value: new TextEncoder().encode(chunks[index++]) };
        return { done: true, value: undefined };
      },
    };
    const abort = vi.fn();
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      body: { getReader: () => reader },
      headers: { get: () => null },
    }));
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'authHeaders').mockImplementation((extra = {}) => ({
      Accept: 'application/json', 'Content-Type': 'application/json',
      'X-Parley-Token': 'token-1', 'X-Parley-Epoch': 'epoch-1', ...extra,
    }));
    const signals: string[] = [];
    const stop = await commands.notify({ onSignal: (signal) => signals.push(signal) });
    await vi.waitFor(() => expect(signals).toContain('approvals_changed'));
    expect(signals).toEqual(['epoch_changed', 'approvals_changed', 'run_changed']);
    stop();
    expect(abort).not.toHaveBeenCalled(); // cleanup uses its own AbortController
  });
});
