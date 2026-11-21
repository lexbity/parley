import { createHash, randomBytes } from 'node:crypto';
import { Buffer } from 'node:buffer';
import { log } from 'node:console';
import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { extname, resolve, sep } from 'node:path';
import { fileURLToPath, URL } from 'node:url';
import process from 'node:process';

const root = resolve(fileURLToPath(new URL('../dist/', import.meta.url)));
const epoch = 'ffeeddccbbaa99887766554433221100';
const bootstraps = new Map();
const sessions = new Map();
const resources = new Map();
const receipts = new Map();
const events = [];
let testClock = Date.UTC(2026, 0, 1);
const now = () => new Date(testClock += 1000).toISOString();
const randomHex = (bytes = 16) => randomBytes(bytes).toString('hex');
const tag = (resource) => `"p1.${epoch}.resource.${resource.id}.${resource.revision}"`;
const json = (response, status, body, headers = {}) => {
  response.writeHead(status, { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store', ...headers });
  response.end(JSON.stringify(body));
};
const fail = (response, status, code, message) => json(response, status, { error: { code, message, details: [] } });

async function body(request) {
  const chunks = [];
  let length = 0;
  for await (const chunk of request) {
    length += chunk.length;
    if (length > 2 * 1024 * 1024) throw Object.assign(new Error('too large'), { status: 413 });
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks).toString('utf8'));
}

function authorize(request, response, needToken = true) {
  if (request.headers.host !== authority) {
    fail(response, 403, 'ORIGIN_DENIED', 'Request authority is not allowed');
    return null;
  }
  const origin = request.headers.origin;
  if (origin && origin !== `http://${authority}`) {
    fail(response, 403, 'ORIGIN_DENIED', 'Request origin is not allowed');
    return null;
  }
  const cookie = request.headers.cookie?.split(';').map((item) => item.trim()).find((item) => item.startsWith('parley_session='))?.slice(15);
  const session = cookie && sessions.get(cookie);
  if (!session) {
    fail(response, 401, 'SESSION_REQUIRED', 'Sign in to the local workspace');
    return null;
  }
  if (needToken && request.headers['x-parley-token'] !== session.token) {
    fail(response, 403, 'TOKEN_INVALID', 'Mutation token is invalid');
    return null;
  }
  if (needToken && request.headers['x-parley-epoch'] !== epoch) {
    fail(response, 409, 'WORKSPACE_REPLACED', 'Workspace changed; reconnect before continuing');
    return null;
  }
  return session;
}

const server = createServer(async (request, response) => {
  const url = new URL(request.url ?? '/', `http://${request.headers.host}`);
  if (url.pathname === '/health') return json(response, 200, { status: 'ready', model: 'not-probed' });
  if (url.pathname === '/api/v1/session/bootstrap' && request.method === 'POST') {
    if (request.headers.host !== authority || (request.headers.origin && request.headers.origin !== `http://${authority}`)) {
      return fail(response, 403, 'ORIGIN_DENIED', 'Request origin is not allowed');
    }
    let input;
    try { input = await body(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
    const bootstrapExpiry = bootstraps.get(input.secret);
    bootstraps.delete(input.secret);
    if (!bootstrapExpiry || bootstrapExpiry < Date.now()) return fail(response, 401, 'BOOTSTRAP_INVALID', 'Bootstrap secret is invalid or expired');
    const sessionId = randomHex(32);
    const token = randomHex(32);
    sessions.set(sessionId, { token });
    response.writeHead(200, {
      'content-type': 'application/json; charset=utf-8',
      'cache-control': 'no-store',
      'set-cookie': `parley_session=${sessionId}; HttpOnly; SameSite=Strict; Path=/api/v1; Max-Age=28800`,
    });
    return response.end(JSON.stringify({ ok: true }));
  }
  if (url.pathname.startsWith('/api/v1/')) {
    if (url.pathname === '/api/v1/session' && request.method === 'GET') {
      const session = authorize(request, response, false);
      if (!session) return;
      const sessionId = request.headers.cookie.split(';').map((item) => item.trim()).find((item) => item.startsWith('parley_session='))?.slice(15);
      return json(response, 200, {
        workspace_id: '00112233445566778899aabbccddeeff', epoch, wire_major: 1, wire_minor: 0,
        capabilities: ['resources.read', 'resources.write'], mutation_token: sessions.get(sessionId).token,
        cursor: String(events.length),
      }, { 'x-parley-epoch': epoch });
    }
    const session = authorize(request, response);
    if (!session) return;
    if (url.pathname === '/api/v1/entities' && request.method === 'GET') {
      const items = [...resources.values()].sort((a, b) => b.updated_at.localeCompare(a.updated_at) || a.id.localeCompare(b.id));
      return json(response, 200, { data: items, cursor: String(events.length) }, { 'x-parley-epoch': epoch });
    }
    if (url.pathname === '/api/v1/entities' && request.method === 'POST') {
      let input;
      try { input = await body(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const key = request.headers['idempotency-key'];
      if (!key) return fail(response, 400, 'IDEMPOTENCY_KEY_REQUIRED', 'Idempotency-Key is required');
      const digest = createHash('sha256').update(JSON.stringify(input)).digest('hex');
      const receipt = receipts.get(key);
      if (receipt) {
        if (receipt.digest !== digest) return fail(response, 409, 'KEY_REUSED', 'Idempotency key was used for a different command');
        return json(response, 201, { data: receipt.resource }, { etag: receipt.resource.etag, 'x-parley-epoch': epoch });
      }
      if (!['gift', 'proof', 'artifact'].includes(input.kind) || typeof input.title !== 'string' || input.title.length > 240 || !input.body || Array.isArray(input.body) || typeof input.body !== 'object') {
        return fail(response, 422, 'RESOURCE_INVALID', 'Resource type or name is invalid');
      }
      const resource = {
        id: randomHex(), kind: input.kind, title: input.title, body: input.body, readiness: 'draft', revision: 1,
      };
      resource.etag = tag(resource);
      resource.updated_at = now();
      resources.set(resource.id, resource);
      events.push({ cursor: String(events.length + 1), event_id: randomHex(), entity_type: 'resource', entity_id: resource.id, revision: 1, event_type: 'resource.created', payload: { kind: resource.kind } });
      receipts.set(key, { digest, resource });
      return json(response, 201, { data: resource }, { etag: resource.etag, 'x-parley-epoch': epoch });
    }
    const entityMatch = /^\/api\/v1\/entities\/([a-f0-9]+)$/.exec(url.pathname);
    if (entityMatch && request.method === 'GET') {
      const resource = resources.get(entityMatch[1]);
      if (!resource) return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
      return json(response, 200, { data: resource }, { etag: resource.etag, 'x-parley-epoch': epoch });
    }
    if (entityMatch && request.method === 'PATCH') {
      let input;
      try { input = await body(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      if (!request.headers['if-match']) return fail(response, 428, 'ETAG_REQUIRED', 'If-Match is required');
      const key = request.headers['idempotency-key'];
      if (!key) return fail(response, 400, 'IDEMPOTENCY_KEY_REQUIRED', 'Idempotency-Key is required');
      const digest = createHash('sha256').update(JSON.stringify([entityMatch[1], request.headers['if-match'], input])).digest('hex');
      const receipt = receipts.get(key);
      if (receipt) {
        if (receipt.digest !== digest) return fail(response, 409, 'KEY_REUSED', 'Idempotency key was used for a different command');
        return json(response, 200, { data: receipt.resource }, { etag: receipt.resource.etag, 'x-parley-epoch': epoch });
      }
      const resource = resources.get(entityMatch[1]);
      if (!resource) return fail(response, 404, 'NOT_FOUND', 'Resource does not exist');
      if (request.headers['if-match'] !== resource.etag) return fail(response, 412, 'STALE_REVISION', 'Resource changed in another session');
      const updated = { ...resource, title: input.title, body: input.body, revision: resource.revision + 1, updated_at: now() };
      updated.etag = tag(updated);
      resources.set(updated.id, updated);
      events.push({ cursor: String(events.length + 1), event_id: randomHex(), entity_type: 'resource', entity_id: updated.id, revision: updated.revision, event_type: 'resource.updated', payload: {} });
      receipts.set(key, { digest, resource: updated });
      return json(response, 200, { data: updated }, { etag: updated.etag, 'x-parley-epoch': epoch });
    }
    if (url.pathname === '/api/v1/events' && request.method === 'GET') {
      const after = Number(url.searchParams.get('after') ?? '0');
      if (!Number.isSafeInteger(after) || after < 0) return fail(response, 400, 'CURSOR_INVALID', 'Event cursor must be a non-negative decimal');
      return json(response, 200, { items: events.filter((item) => Number(item.cursor) > after), next_cursor: events.at(-1)?.cursor ?? String(after), observed_cursor: String(events.length) }, { 'x-parley-epoch': epoch });
    }
    return fail(response, 404, 'NOT_FOUND', 'Endpoint does not exist');
  }

  const relative = url.pathname === '/' ? 'index.html' : decodeURIComponent(url.pathname.slice(1));
  const file = resolve(root, relative);
  if (file !== root && !file.startsWith(root + sep)) return fail(response, 404, 'NOT_FOUND', 'Page does not exist');
  try {
    if (!(await stat(file)).isFile()) return fail(response, 404, 'NOT_FOUND', 'Page does not exist');
    const extension = extname(file);
    const types = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.svg': 'image/svg+xml', '.png': 'image/png', '.woff2': 'font/woff2' };
    response.writeHead(200, {
      'content-type': types[extension] ?? 'application/octet-stream',
      'cache-control': 'no-store',
      'x-content-type-options': 'nosniff',
      'content-security-policy': "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
    });
    response.end(await readFile(file));
  } catch {
    fail(response, 404, 'NOT_FOUND', 'Page does not exist');
  }
});

let authority = '';
server.listen(0, '127.0.0.1', () => {
  authority = `127.0.0.1:${server.address().port}`;
  bootstraps.set(randomHex(32), Date.now() + 60_000);
  bootstraps.set(randomHex(32), Date.now() + 60_000);
  log(`http://${authority}/#bootstrap=${[...bootstraps.keys()][0]}`);
  log(`http://${authority}/#bootstrap=${[...bootstraps.keys()][1]}`);
});

process.on('SIGTERM', () => server.close(() => process.exit(0)));
