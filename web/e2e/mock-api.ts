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

type EntityRecord = { id: string; kind: string; name: string; revision: number; descriptor?:string; aliases?:string[]; tags?:string[] };

type SavedSource = {
  content_id: string;
  variant_id: string;
  occurrence_id: string;
  normalized_sha256: string;
  raw_sha256: string;
  job_id?: string | null;
  job_status?: string;
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
  const extractorJobs = new Map<string, RecordData>();
  const jobsByOccurrence = new Map<string, string>();
  const opportunities = new Map<string, RecordData>();
  const outputs = new Map<string, RecordData>();
  const sourceDetails = new Map<string, RecordData>();
  const graphEdges: Array<RecordData> = [];
  const positions = new Map<string, RecordData>();
  let provider = { endpoint: 'http://127.0.0.1:11434', model: '', num_ctx: 8192, num_predict: 1024, locale: 'en' };
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
    if (url.pathname === '/api/v1/opportunities' && request.method === 'GET') {
      return json(response, 200, [...opportunities.values()]);
    }
    if (url.pathname === '/api/v1/opportunities' && request.method === 'POST') {
      let input: RecordData;
      try { input = await readBody(request); } catch { return fail(response, 400, 'INVALID_JSON', 'Request body is invalid'); }
      const participant = directory.get(String(input.primary_party_id));
      if (!participant || !['person', 'org'].includes(participant.kind) || typeof input.title !== 'string' || !input.title.trim()) {
        return fail(response, 422, 'OPPORTUNITY_INVALID', 'A title and selected participant are required');
      }
      const record = { id: randomHex(), title: input.title.trim(), state: 'open', revision: 1, participant_ids: [participant.id], source_variant_ids: [], material_ids: [] };
      opportunities.set(record.id, record);
      return json(response, 201, record);
    }
    if (url.pathname === '/api/v1/outputs' && request.method === 'GET') {
      return json(response, 200, [...outputs.values()]);
    }
    if (url.pathname === '/api/v1/captures' && request.method === 'GET') {
      return json(response, 200, captures);
    }
    const sourceMatch = url.pathname.match(/^\/api\/v1\/captures\/([^/]+)$/);
    if (sourceMatch && request.method === 'GET') return sourceDetails.has(sourceMatch[1]!) ? json(response,200,sourceDetails.get(sourceMatch[1]!)!) : fail(response,404,'NOT_FOUND','Source occurrence does not exist');
    if (url.pathname === '/api/v1/directory' && request.method === 'GET') {
      const q=(url.searchParams.get('q')??'').toLocaleLowerCase(); const kind=url.searchParams.get('kind')??'';
      const tag=(url.searchParams.get('tag')??'').toLocaleLowerCase();const matched=[...directory.values()].filter(row=>(!kind||row.kind===kind)&&(`${row.name} ${row.descriptor??''} ${(row.aliases??[]).join(' ')}`).toLocaleLowerCase().includes(q)&&(!tag||(row.tags??[]).some(value=>value.toLocaleLowerCase()===tag))).sort((a,b)=>a.name.localeCompare(b.name)||a.id.localeCompare(b.id));const page=Number(url.searchParams.get('page')??1);const pageSize=Number(url.searchParams.get('page_size')??50);const data=matched.slice((page-1)*pageSize,page*pageSize).map(row=>({ ...row,descriptor:row.descriptor??'',aliases:row.aliases??[],tags:row.tags??[] }));
      return json(response,200,{data,total:matched.length,page,page_size:pageSize,has_more:page*pageSize<matched.length,cursor:String(events.length)});
    }
    const directoryUpdate=url.pathname.match(/^\/api\/v1\/directory\/([^/]+)$/);
    if(directoryUpdate&&request.method==='PATCH'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}
      const commandKey=request.headers['idempotency-key'];if(typeof commandKey!=='string'||!commandKey)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');const commandDigest=createHash('sha256').update(JSON.stringify([directoryUpdate[1],input])).digest('hex');const prior=receipts.get(commandKey);if(prior)return prior.digest===commandDigest?json(response,200,prior.result):fail(response,409,'KEY_REUSED','Idempotency key was used for a different command');
      const entity=directory.get(decodeURIComponent(directoryUpdate[1]!));if(!entity)return fail(response,404,'NOT_FOUND','Directory entry does not exist');if(Number(input.expected_revision)!==entity.revision)return fail(response,412,'STALE_REVISION','Directory entry changed in another session');
      const updated:EntityRecord={...entity,name:String(input.name),descriptor:String(input.descriptor),aliases:Array.isArray(input.aliases)?input.aliases.map(String):[],tags:Array.isArray(input.tags)?input.tags.map(String):[],revision:entity.revision+1};directory.set(entity.id,updated);pushEvent({event_id:randomHex(),entity_type:'entity',entity_id:entity.id,revision:updated.revision,event_type:'entity.updated',payload:{}});const result={...updated,descriptor:updated.descriptor,aliases:updated.aliases,tags:updated.tags};receipts.set(commandKey,{digest:commandDigest,result});return json(response,200,result);
    }
    const graphMatch=url.pathname.match(/^\/api\/v1\/graph\/([^/]+)$/);
    if(graphMatch&&request.method==='GET'){
      const focus=decodeURIComponent(graphMatch[1]!);if(!directory.has(focus))return fail(response,404,'NOT_FOUND','Directory entity does not exist');
      const edges=graphEdges.filter(edge=>edge.from_id===focus||edge.to_id===focus);const ids=new Set([focus,...edges.flatMap(edge=>[String(edge.from_id),String(edge.to_id)])]);
      const nodes=[...ids].map(id=>directory.get(id)).filter((value):value is EntityRecord=>Boolean(value)).map(row=>({...row,descriptor:'',aliases:[],tags:[]}));
      const viewId=url.searchParams.get('view_id')??focus;
      return json(response,200,{focus_id:focus,nodes,edges,positions:[...positions.values()].filter(position=>position.view_id===viewId),omitted_nodes:0,omitted_edges:0});
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
      sourceDetails.set(result.occurrence_id,{...result,text:input.text,title:typeof input.title==='string'?input.title:null,url:typeof input.url==='string'?input.url:null,captured_at:now(),actor_kind:typeof input.actor_kind==='string'?input.actor_kind:'unknown',actor_entity_id:typeof input.actor_entity_id==='string'?input.actor_entity_id:null,normalization_version:1});
      return json(response, 201, result);
    }
    if(url.pathname==='/api/v1/graph/related'&&request.method==='POST'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}
      const commandKey=request.headers['idempotency-key'];if(typeof commandKey!=='string'||!commandKey)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');
      const commandDigest=createHash('sha256').update(JSON.stringify(input)).digest('hex');const prior=receipts.get(commandKey);if(prior)return prior.digest===commandDigest?json(response,201,prior.result):fail(response,409,'KEY_REUSED','Idempotency key was used for a different command');
      const parent=directory.get(String(input.related_entity_id));const source=sourceDetails.get(String(input.occurrence_id));
      const quote=String(input.quote??'');if(!parent||!source||typeof source.text!=='string'||!quote||!source.text.includes(quote))return fail(response,422,'RELATIONSHIP_REJECTED','The source and exact quote must exist.');
      const personId=randomHex();const person:EntityRecord={id:personId,kind:String(input.kind),name:String(input.name),revision:1};directory.set(personId,person);
      const edge={id:randomHex(),from_id:personId,to_id:parent.id,verb:String(input.verb),revision:1,claim_id:randomHex(),claim_revision:1,wording:quote,approved_wording:null,tier:1,speaker_kind:'unknown',speaker_entity_id:null,occurrence_id:String(input.occurrence_id),source_start:source.text.indexOf(quote),source_end:source.text.indexOf(quote)+quote.length,normalization_version:1,source_title:source.title,source_url:source.url};graphEdges.push(edge);
      positions.set(`${input.view_scope}:${input.view_id}:${personId}`,{view_id:input.view_id,entity_id:personId,x:input.x,y:input.y,pinned:false,revision:1});
      pushEvent({event_id:randomHex(),entity_type:'edge',entity_id:String(edge.id),revision:1,event_type:'edge.created',payload:{}});receipts.set(commandKey,{digest:commandDigest,result:person});return json(response,201,person);
    }
    if(url.pathname==='/api/v1/graph/edges'&&request.method==='POST'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}
      const commandKey=request.headers['idempotency-key'];if(typeof commandKey!=='string'||!commandKey)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');const commandDigest=createHash('sha256').update(JSON.stringify(input)).digest('hex');const prior=receipts.get(commandKey);if(prior)return prior.digest===commandDigest?json(response,201,prior.result):fail(response,409,'KEY_REUSED','Idempotency key was used for a different command');
      const from=directory.get(String(input.from_id));const to=directory.get(String(input.to_id));const source=sourceDetails.get(String(input.occurrence_id));const quote=String(input.quote??'');if(!from||!to||from.kind!=='person'||to.kind!=='org'||input.verb!=='member_of'||!source||typeof source.text!=='string'||!quote||!source.text.includes(quote))return fail(response,422,'RELATIONSHIP_REJECTED','The selected endpoints or exact source quote are invalid.');
      if(graphEdges.some(edge=>edge.from_id===from.id&&edge.to_id===to.id&&edge.verb==='member_of'))return fail(response,409,'EDGE_EXISTS','This directed relationship already exists.');
      const edgeId=randomHex();const edge={id:edgeId,from_id:from.id,to_id:to.id,verb:'member_of',revision:1,claim_id:randomHex(),claim_revision:1,wording:quote,approved_wording:null,tier:1,speaker_kind:source.actor_kind,speaker_entity_id:source.actor_entity_id,occurrence_id:String(input.occurrence_id),source_start:source.text.indexOf(quote),source_end:source.text.indexOf(quote)+quote.length,normalization_version:1,source_title:source.title,source_url:source.url};graphEdges.push(edge);pushEvent({event_id:randomHex(),entity_type:'edge',entity_id:edgeId,revision:1,event_type:'edge.created',payload:{from_id:from.id,to_id:to.id}});const result={edge_id:edgeId};receipts.set(commandKey,{digest:commandDigest,result});return json(response,201,result);
    }
    const correctionMatch=url.pathname.match(/^\/api\/v1\/claims\/([^/]+)\/correction$/);
    if(correctionMatch&&request.method==='POST'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}
      const commandKey=request.headers['idempotency-key'];if(typeof commandKey!=='string'||!commandKey)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');const commandDigest=createHash('sha256').update(JSON.stringify([correctionMatch[1],input])).digest('hex');const prior=receipts.get(commandKey);if(prior)return prior.digest===commandDigest?json(response,200,prior.result):fail(response,409,'KEY_REUSED','Idempotency key was used for a different command');
      const edge=graphEdges.find(item=>item.claim_id===correctionMatch[1]);if(!edge)return fail(response,404,'NOT_FOUND','Claim does not exist');if(Number(input.expected_revision)!==Number(edge.claim_revision))return fail(response,412,'STALE_REVISION','Claim changed in another session');
      edge.claim_revision=Number(edge.claim_revision)+1;edge.approved_wording=String(input.wording);edge.tier=3;edge.speaker_kind='owner';pushEvent({event_id:randomHex(),entity_type:'claim',entity_id:String(edge.claim_id),revision:Number(edge.claim_revision),event_type:'claim.corrected',payload:{}});const result={claim_id:edge.claim_id,revision:edge.claim_revision,approved_wording:input.wording};receipts.set(commandKey,{digest:commandDigest,result});return json(response,200,result);
    }
    if(url.pathname==='/api/v1/layout'&&request.method==='POST'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}
      const commandKey=request.headers['idempotency-key'];if(typeof commandKey!=='string'||!commandKey)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');const commandDigest=createHash('sha256').update(JSON.stringify(input)).digest('hex');const prior=receipts.get(commandKey);if(prior)return prior.digest===commandDigest?json(response,200,prior.result):fail(response,409,'KEY_REUSED','Idempotency key was used for a different command');
      const key=`${input.view_scope}:${input.view_id}:${input.entity_id}`;const old=positions.get(key);if(Number(input.expected_revision)!==(old?Number(old.revision):0))return fail(response,412,'STALE_LAYOUT','View layout changed in another session');const position={entity_id:input.entity_id,x:input.x,y:input.y,pinned:true,revision:(old?Number(old.revision):0)+1,view_id:input.view_id};positions.set(key,position);const result={entity_id:input.entity_id,revision:position.revision,x:input.x,y:input.y};receipts.set(commandKey,{digest:commandDigest,result});return json(response,200,result);
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
