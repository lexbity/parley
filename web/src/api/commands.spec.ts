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
