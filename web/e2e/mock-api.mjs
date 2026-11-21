import { createHash, randomUUID } from 'node:crypto';
import { Buffer } from 'node:buffer';
import { createServer } from 'node:http';
import { URL } from 'node:url';

const resources = new Map();
const entities = new Map();
const groups = new Map();
const occurrences = [];
const receipts = new Map();
const json = (response, status, body) => {
  response.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' });
  response.end(JSON.stringify(body));
};
const server = createServer(async (request, response) => {
  if (request.headers.host !== '127.0.0.1:8934') return json(response, 400, { error: { code: 'invalid_host' } });
  if (request.url === '/api/v1/health') return json(response, 200, { status: 'ok' });
  if (request.headers['x-parley-token'] !== 'mock-token') return json(response, 401, { error: { code: 'unauthorized' } });
  const chunks = [];
  for await (const chunk of request) chunks.push(chunk);
  const body = Buffer.concat(chunks).toString();
  let input;
  try { input = body ? JSON.parse(body) : {}; } catch { return json(response, 400, { error: { code: 'invalid_json' } }); }
  const url = new URL(request.url, 'http://localhost');
  if (url.pathname === '/api/v1/resources' && request.method === 'GET') return json(response, 200, [...resources.values()]);
  if (url.pathname === '/api/v1/resources' && request.method === 'POST') {
    const key = request.headers['idempotency-key'];
    const canonical = JSON.stringify(input);
    if (receipts.has(key)) return receipts.get(key).canonical === canonical ? json(response, 201, receipts.get(key).result) : json(response, 409, { error: { code: 'idempotency_conflict' } });
    const result = { ...input, id: randomUUID(), readiness: 'draft', revision: 1 };
    resources.set(result.id, result); receipts.set(key, { canonical, result });
    return json(response, 201, result);
  }
  if (url.pathname === '/api/v1/entities' && request.method === 'GET') return json(response, 200, [...entities.values()]);
  if (url.pathname === '/api/v1/entities' && request.method === 'POST') {
    const result = { ...input, id: randomUUID(), revision: 1 };
    entities.set(result.id, result);
    return json(response, 201, result);
  }
  if (url.pathname === '/api/v1/captures' && request.method === 'GET') return json(response, 200, occurrences);
  if (url.pathname === '/api/v1/captures' && request.method === 'POST') {
    const normalized = input.text.normalize('NFKC').replace(/\s+/g, ' ').trim().toLocaleLowerCase();
    const hash = createHash('sha256').update(normalized).digest('hex');
    const content = groups.get(hash) ?? { id: randomUUID(), variants: new Map() };
    groups.set(hash, content);
    const rawHash = createHash('sha256').update(input.text).digest('hex');
    const variant = content.variants.get(rawHash) ?? randomUUID();
    content.variants.set(rawHash, variant);
    const result = { content_id: content.id, variant_id: variant, occurrence_id: randomUUID(), normalized_sha256: hash, raw_sha256: rawHash };
    occurrences.unshift(result);
    return json(response, 201, result);
  }
  return json(response, 404, { error: { code: 'not_found' } });
});
server.listen(8934, '127.0.0.1');
