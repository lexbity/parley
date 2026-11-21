import { createHash, randomUUID } from 'node:crypto';
import type { IncomingMessage, ServerResponse } from 'node:http';
import type { Plugin } from 'vite';

type RecordData = Record<string, unknown>;

function json(response: ServerResponse, status: number, body: unknown) {
  response.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' });
  response.end(JSON.stringify(body));
}

export function mockApi(): Plugin {
  const resources = new Map<string, RecordData>();
  const entities = new Map<string, RecordData>();
  const groups = new Map<string, { id: string; variants: Map<string, string> }>();
  const occurrences: RecordData[] = [];
  const opportunities = new Map<string, RecordData>();
  const outputs = new Map<string, RecordData>();
  const receipts = new Map<string, { canonical: string; result: RecordData }>();
  const token = 'mock-token';

  async function handle(request: IncomingMessage, response: ServerResponse) {
    if (request.headers.host !== `127.0.0.1:${request.socket.localPort}`) {
      return json(response, 400, { error: { code: 'invalid_host' } });
    }
    if (request.url === '/api/v1/health') {
      return json(response, 200, { status: 'ok' });
    }
    if (request.headers['x-parley-token'] !== token) {
      return json(response, 401, { error: { code: 'unauthorized' } });
    }
    const chunks: Buffer[] = [];
    for await (const chunk of request) {
      chunks.push(chunk as Buffer);
    }
    const body = Buffer.concat(chunks).toString();
    let input: RecordData;
    try {
      input = body ? JSON.parse(body) as RecordData : {};
      if (!input || typeof input !== 'object' || Array.isArray(input)) {
        return json(response, 400, { error: { code: 'invalid_json' } });
      }
    } catch {
      return json(response, 400, { error: { code: 'invalid_json' } });
    }

    const url = new URL(request.url ?? '/', 'http://127.0.0.1');
    if (url.pathname === '/api/v1/resources' && request.method === 'GET') {
      return json(response, 200, [...resources.values()]);
    }
    if (url.pathname === '/api/v1/resources' && request.method === 'POST') {
      const key = request.headers['idempotency-key'];
      if (typeof key !== 'string') {
        return json(response, 400, { error: { code: 'missing_idempotency_key' } });
      }
      const canonical = JSON.stringify(input);
      const receipt = receipts.get(key);
      if (receipt) {
        return receipt.canonical === canonical
          ? json(response, 201, receipt.result)
          : json(response, 409, { error: { code: 'idempotency_conflict' } });
      }
      const result = { ...input, id: randomUUID(), readiness: 'draft', revision: 1 };
      resources.set(result.id, result);
      receipts.set(key, { canonical, result });
      return json(response, 201, result);
    }
    if (url.pathname === '/api/v1/entities' && request.method === 'GET') {
      return json(response, 200, [...entities.values()]);
    }
    if (url.pathname === '/api/v1/entities' && request.method === 'POST') {
      const result = { ...input, id: randomUUID(), revision: 1 };
      entities.set(result.id, result);
      return json(response, 201, result);
    }
    if (url.pathname === '/api/v1/opportunities' && request.method === 'GET') {
      return json(response, 200, [...opportunities.values()]);
    }
    if (url.pathname === '/api/v1/opportunities' && request.method === 'POST') {
      const party = entities.get(String(input.primary_party_id));
      if (!party || !['person', 'org'].includes(String(party.kind)) || !String(input.title ?? '').trim()) {
        return json(response, 422, { error: { code: 'invalid_request', message: 'A title and existing participant are required.' } });
      }
      const result = { id: randomUUID(), title: String(input.title), state: 'open', revision: 1, participant_ids: [party.id], source_variant_ids: [], material_ids: [] };
      opportunities.set(result.id, result);
      return json(response, 201, result);
    }
    if (url.pathname === '/api/v1/outputs' && request.method === 'GET') {
      return json(response, 200, [...outputs.values()]);
    }
    if (url.pathname === '/api/v1/captures' && request.method === 'GET') {
      return json(response, 200, occurrences);
    }
    if (url.pathname === '/api/v1/captures' && request.method === 'POST') {
      const { text } = input;
      if (typeof text !== 'string') {
        return json(response, 400, { error: { code: 'invalid_capture' } });
      }
      const normalized = text.normalize('NFKC').replace(/\s+/g, ' ').trim().toLocaleLowerCase();
      const hash = createHash('sha256').update(normalized).digest('hex');
      const content = groups.get(hash) ?? { id: randomUUID(), variants: new Map<string, string>() };
      groups.set(hash, content);
      const rawHash = createHash('sha256').update(text).digest('hex');
      const variant = content.variants.get(rawHash) ?? randomUUID();
      content.variants.set(rawHash, variant);
      const result = {
        content_id: content.id,
        variant_id: variant,
        occurrence_id: randomUUID(),
        normalized_sha256: hash,
        raw_sha256: rawHash,
      };
      occurrences.unshift(result);
      return json(response, 201, result);
    }
    return json(response, 404, { error: { code: 'not_found' } });
  }

  return {
    name: 'parley-test-mock-api',
    configureServer(server) {
      server.middlewares.use((request, response, next) => {
        if (request.url?.startsWith('/api/')) {
          void handle(request, response).catch(next);
        } else {
          next();
        }
      });
    },
    transformIndexHtml() {
      return [{
        tag: 'script',
        children: `window.__PARLEY_TOKEN__ = ${JSON.stringify(token)};`,
        injectTo: 'head',
      }];
    },
  };
}
