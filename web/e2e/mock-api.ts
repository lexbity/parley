import { createHash, randomBytes } from 'node:crypto';
import type { IncomingMessage, ServerResponse } from 'node:http';
import type { Plugin } from 'vite';

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

type EntityRecord = { id: string; kind: string; name: string; revision: number };

type SavedSource = {
  content_id: string;
  variant_id: string;
  occurrence_id: string;
  normalized_sha256: string;
  raw_sha256: string;
};

type EventRecord = {
  cursor: string;
  event_id: string;
  entity_type: string;
  entity_id: string;
  revision: number;
  event_type: string;
  payload: RecordData;
};

const epoch = 'ffeeddccbbaa99887766554433221100';
const randomHex = (bytes = 16) => randomBytes(bytes).toString('hex');

export interface MockApi {
  plugin: Plugin;
  /** Mint a fresh one-time bootstrap secret for a session redemption. */
  issueBootstrap(): string;
}

export function createMockApi(): MockApi {
  const bootstraps = new Map<string, number>();
  const sessions = new Map<string, string>();
  const resources = new Map<string, ResourceRecord>();
  const directory = new Map<string, EntityRecord>();
  const sourceGroups = new Map<string, { id: string; variants: Map<string, string> }>();
  const captures: SavedSource[] = [];
  const receipts = new Map<string, { digest: string; result: RecordData }>();
  const events: EventRecord[] = [];
  let testClock = Date.UTC(2026, 0, 1);
  const now = () => new Date(testClock += 1000).toISOString();
  const tag = (resource: ResourceRecord) => `"p1.${epoch}.resource.${resource.id}.${resource.revision}"`;
  const pushEvent = (event: Omit<EventRecord, 'cursor'>) => {
    events.push({ ...event, cursor: String(events.length + 1) });
  };

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
        capabilities: ['resources.read', 'resources.write'],
        mutation_token: sessions.get(cookie),
        cursor: String(events.length),
      });
    }
    if (!authorize(request)) {
      return fail(response, cookie ? 403 : 401, cookie ? 'TOKEN_INVALID' : 'SESSION_REQUIRED', 'Sign in to the local workspace');
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
        items.push(...[...directory.values()].sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id)));
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
        const entity: EntityRecord = { id: randomHex(), kind: String(input.kind), name, revision: 1 };
        directory.set(entity.id, entity);
        pushEvent({ event_id: randomHex(), entity_type: 'entity', entity_id: entity.id, revision: 1, event_type: 'entity.created', payload: {} });
        receipts.set(key, { digest, result: entity });
        return json(response, 201, { data: entity });
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
      pushEvent({ event_id: randomHex(), entity_type: 'resource', entity_id: resource.id, revision: 1, event_type: 'resource.created', payload: { kind: resource.kind } });
      receipts.set(key, { digest, result: resource });
      return json(response, 201, { data: resource });
    }
    const entityMatch = /^\/api\/v1\/entities\/([a-f0-9]+)$/.exec(url.pathname);
    if (entityMatch && request.method === 'GET') {
      const resource = resources.get(entityMatch[1]!);
      if (!resource) return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
      return json(response, 200, { data: resource });
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
      if (!resource) return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
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
      pushEvent({ event_id: randomHex(), entity_type: 'resource', entity_id: updated.id, revision: updated.revision, event_type: 'resource.updated', payload: {} });
      receipts.set(key, { digest, result: updated });
      return json(response, 200, { data: updated });
    }
    if (url.pathname === '/api/v1/events' && request.method === 'GET') {
      const after = Number(url.searchParams.get('after') ?? '0');
      if (!Number.isSafeInteger(after) || after < 0) {
        return fail(response, 400, 'CURSOR_INVALID', 'Event cursor must be a non-negative decimal');
      }
      const afterCursor = url.searchParams.get('after') ?? '0';
      const items = events.filter((event) => Number(event.cursor) > after);
      return json(response, 200, {
        items,
        next_cursor: events.at(-1)?.cursor ?? afterCursor,
        observed_cursor: String(events.length),
      });
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
      };
      captures.unshift(result);
      return json(response, 201, result);
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
