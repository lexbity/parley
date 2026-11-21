import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError, newOperationKey, ParleyClient, type Resource, type SavedSource } from './client';

const session = {
  workspace_id: 'workspace-1', epoch: 'epoch-1', wire_major: 1, wire_minor: 0,
  capabilities: ['resources.read', 'resources.write'], mutation_token: 'token-1', cursor: '0',
};
const resource: Resource = {
  id: 'resource 1', kind: 'gift', title: 'Offer', body: { description: 'A useful offer', delivery: 'on_request' },
  readiness: 'draft', revision: 1, etag: '"resource-1"',
};
const party = { id: 'party 1', kind: 'person' as const, name: 'Dana Lee', revision: 1 };
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
    const material = { id: 'mat-1', opportunity_id: 'op-1', recipient_id: 'party 1', recipient_name: 'Dana Lee', kind: 'outreach', version: 1, body: 'Hello Dana.', content_sha256: 'a'.repeat(64), status: 'approved', gate: null };
    const released = { release_id: 'release-1', material_id: 'mat-1', version: 1, recipient_id: 'party 1', body: 'Hello Dana.', content_sha256: 'a'.repeat(64), issued_at: '2026-11-21T12:00:00Z' };
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
});

describe('ParleyClient', () => {
  it('decodes and validates the S02 directory, graph, source, correction and layout operations', async () => {
    const directoryRow = {
      id: 'directory-1', kind: 'person', name: 'Dana Lee', descriptor: 'Northline',
      aliases: ['D. Lee'], tags: ['onboarding'], revision: 2,
    };
    const graph = { focus_id: directoryRow.id, nodes: [directoryRow], edges: [], positions: [], omitted_nodes: 0, omitted_edges: 0 };
    const detail = { ...source, text: 'A cited source.', title: null, url: null, captured_at: '2026-01-01T00:00:00Z', actor_kind: 'party', actor_entity_id: directoryRow.id, normalization_version: 1 };
    const client = new ParleyClient();
    const fetchMock = vi.spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(sessionResponse())
      .mockResolvedValueOnce(response({ data: [directoryRow], total: 1, page: 1, page_size: 25, has_more: false }))
      .mockResolvedValueOnce(response({ ...directoryRow, revision: 3 }))
      .mockResolvedValueOnce(response(graph))
      .mockResolvedValueOnce(response(detail))
      .mockResolvedValueOnce(response({ claim_id: 'claim-1', revision: 2, approved_wording: 'Owner wording.' }))
      .mockResolvedValueOnce(response({ entity_id: directoryRow.id, revision: 2, x: 25, y: 50 }))
      .mockResolvedValueOnce(response({ id: 'related-1', kind: 'person', name: 'Sam Lee', revision: 1 }, 201))
      .mockResolvedValueOnce(response({ edge_id: 'edge-1' }, 201));
    await client.connect();

    expect(await client.searchDirectory({ q: 'Dana', kind: 'person', tag: 'onboarding' }, undefined, 1, 25)).toEqual({
      items: [{ ...directoryRow, kind: 'person' }], total: 1, page: 1, page_size: 25, has_more: false,
    });
    expect(await client.updateDirectory({ ...directoryRow, kind: 'person' }, {
      name: 'Dana Lee', descriptor: 'Northline', aliases: ['D. Lee'], tags: 'onboarding, research',
    }, 'profile-key')).toMatchObject({ id: directoryRow.id, revision: 3 });
    expect(await client.getNeighborhood(directoryRow.id)).toEqual(graph);
    expect(await client.getSource(source.occurrence_id)).toEqual(detail);
    expect(await client.correctClaim('claim-1', 1, 'Owner wording.')).toMatchObject({ revision: 2, approved_wording: 'Owner wording.' });
    await client.saveLayout({ view_scope: 'party', view_id: directoryRow.id, entity_id: directoryRow.id, expected_revision: 1, x: 25, y: 50 });
    expect(await client.createRelated({ kind: 'person', name: 'Sam Lee', related_entity_id: directoryRow.id, verb: 'member_of', occurrence_id: source.occurrence_id, quote: 'A cited source.', view_scope: 'party', view_id: directoryRow.id, x: 1, y: 2 })).toMatchObject({ id: 'related-1' });
    expect(await client.linkExisting({ from_id: 'related-1', to_id: directoryRow.id, verb: 'member_of', occurrence_id: source.occurrence_id, quote: 'A cited source.' })).toBe('edge-1');
    expect(fetchMock.mock.calls[1]?.[0]).toContain('q=Dana&kind=person&tag=onboarding');
    expect(new Headers(fetchMock.mock.calls[2]?.[1]?.headers).get('Idempotency-Key')).toBe('profile-key');
  });

  it('rejects malformed S02 directory, graph, source and correction payloads', async () => {
    const malformedCases: Array<{ run: (client: ParleyClient) => Promise<unknown>; body: unknown; message: RegExp }> = [
      { run: (client) => client.searchDirectory(), body: { data: [], total: 0, page: 1, page_size: 25, has_more: 'no' }, message: /directory is malformed/ },
      { run: (client) => client.searchDirectory(), body: { data: [{ id: 'x' }], total: 1, page: 1, page_size: 25, has_more: false }, message: /directory row is malformed/ },
      { run: (client) => client.getNeighborhood('x'), body: { nodes: [], edges: [], positions: [], omitted_nodes: null, omitted_edges: 0 }, message: /graph counts are malformed/ },
      { run: (client) => client.getSource('x'), body: { ...source, text: 1 }, message: /source detail is malformed/ },
      { run: (client) => client.correctClaim('x', 1, 'copy'), body: { claim_id: 'x', revision: 0, approved_wording: 'copy' }, message: /correction receipt is malformed/ },
      { run: (client) => client.linkExisting({ from_id: 'a', to_id: 'b', verb: 'member_of', occurrence_id: 'o', quote: 'q' }), body: null, message: /edge receipt is malformed/ },
    ];
    for (const testCase of malformedCases) {
      const client = new ParleyClient();
      vi.spyOn(globalThis, 'fetch').mockResolvedValueOnce(sessionResponse()).mockResolvedValueOnce(response(testCase.body));
      await client.connect();
      await expect(testCase.run(client)).rejects.toThrow(testCase.message);
      vi.restoreAllMocks();
    }
  });

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
          revision: 1, event_type: 'resource.created', payload: { kind: 'gift' } }],
        next_cursor: '1', observed_cursor: '1',
      }));

    await client.connect();
    expect(await client.listResources()).toEqual([resource]);
    expect(await client.getResource(resource.id)).toEqual(resource);
    expect(await client.getEvents('7/8')).toMatchObject({ next_cursor: '1', items: [{ entity_id: resource.id }] });
    expect(fetchMock.mock.calls.slice(1).map(([url]) => url)).toEqual([
      '/api/v1/entities?kind=resource',
      '/api/v1/entities/resource%201',
      '/api/v1/events?after=7%2F8',
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
