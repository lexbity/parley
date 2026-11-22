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
  const sourceDetails = new Map<string, RecordData>();
  const graphEdges: Array<RecordData> = [];
  const positions = new Map<string, RecordData>();
  const opportunities = new Map<string, RecordData>();
  const boardLists = new Map<string, RecordData>([['list-inbox', { id:'list-inbox',name:'Opportunities',position:0,revision:1,archived:false }]]);
  const opportunityNotes = new Map<string, { markdown:string; revision:number }>();
  let boardRevision = 1;
  const opportunityInterests = new Map<string, RecordData[]>();
  const opportunityExchanges = new Map<string, RecordData>();
  const materials = new Map<string, RecordData>();
  const copyGrants = new Map<string, RecordData>();
  const captureStages = new Map<string, RecordData>();
  const receipts = new Map<string, { digest: string; result: RecordData }>();
  const events: EventRecord[] = [];
  let testClock = Date.UTC(2026, 0, 1);
  const now = () => new Date(testClock += 1000).toISOString();
  const tag = (resource: ResourceRecord) => `"p1.${epoch}.resource.${resource.id}.${resource.revision}"`;
  const pushEvent = (event: Omit<EventRecord, 'cursor'>) => {
    events.push({ ...event, cursor: String(events.length + 1) });
  };
  const rememberCommand = (request: IncomingMessage, digest: string, result: RecordData): { key:string; prior:RecordData|null } | null => {
    const key = request.headers['idempotency-key'];
    if (typeof key !== 'string' || !key) return null;
    const prior = receipts.get(key);
    if (prior) return prior.digest === digest ? { key, prior:prior.result } : { key, prior:{ __key_reused:true } };
    receipts.set(key,{digest,result});
    return {key,prior:null};
  };
  const gateFor = (material:RecordData):RecordData => {
    const exchange = opportunityExchanges.get(String(material.opportunity_id));
    const version=material.current_version&&typeof material.current_version==='object'?material.current_version as RecordData:null;
    const failures:RecordData[] = [];
    const hard = (id:string,pass:boolean,detail:string) => { if(!pass)failures.push({id,severity:'hard',pass:false,detail}); };
    const body = typeof version?.body==='string'?version.body:'';
    const sentences = Array.isArray(version?.sentences)?version.sentences as RecordData[]:[];
    const unsupported = sentences.some((sentence)=>Array.isArray(sentence.claims)&&(sentence.claims as RecordData[]).some((claim)=>['self_fact','other_fact','offer'].includes(String(claim.type))&&!claim.ref_id));
    hard('H1 EXCH_COMPLETE',Boolean(exchange&&String(exchange.they_get).trim()&&String(exchange.i_get).trim()&&Array.isArray(exchange.interest_ids)&&exchange.interest_ids.length>=2),'Link at least one interest from each side and state both benefits.');
    hard('H2 FAIR_ATTESTED',exchange?.fairness_attested===true,'Attest to the current exchange version.');
    hard('H3 GROUNDED',!unsupported,'Each factual sentence needs an eligible evidence reference.');
    const protectedTokens=Array.isArray(version?.protected_tokens)?version.protected_tokens as string[]:[];
    hard('H4 SLOTS_PRESERVED',protectedTokens.every((token)=>body.includes(token)),'Protected tokens must remain unchanged.');
    hard('H9 PARTY_OPEN',opportunities.get(String(material.opportunity_id))?.status==='open','The opportunity must remain open.');
    const soft:[string,boolean,string][]=[['S1 GIVE_FIRST',false,'Add or document a real gift.'],['S2 ANCHOR',false,'Add one grounded proof, fresh hook, or shared identity.'],['S3 STACK_LIMIT',true,'Message rule count is within the configured limit.'],['S4 FLUENCY',true,'No unclaimed glossary terms are used.'],['S5 TONE_LINT',true,'No deterministic tone errors.'],['S6 HOOK_FRESH',true,'No stale hook is used.'],['S7 CADENCE',true,'No cadence conflict.'],['S8 DAILY_CAP',true,'Daily suggestion cap is available.'],['S9 LENGTH',true,'Message is within the channel limit.'],['S10 BRIEF_REVIEWED',true,'No proposed landmine remains unreviewed.']];
    const results=[...failures,...soft.map(([id,pass,detail])=>({id,severity:'soft',pass,detail}))];
    const hardFailures=failures.map((item)=>item.id);
    const softFailures=soft.filter((item)=>!item[1]).map((item)=>item[0]);
    return {material_id:material.id,version_id:material.current_version&&((material.current_version as RecordData).id),recipient_id:material.recipient_id,evaluated_at:now(),results,hard_failures:hardFailures,soft_failures:softFailures,copy_enabled:hardFailures.length===0};
  };
  const boardSnapshot = ():RecordData => ({
    lists:[...boardLists.values()].sort((a,b)=>Number(a.position)-Number(b.position)||String(a.id).localeCompare(String(b.id))),
    items:[...opportunities.values()].map((opportunity)=>({
      id:opportunity.id,revision:opportunity.revision,list_id:opportunity.list_id??'list-inbox',position:Number(opportunity.position??0),title:opportunity.title,status:opportunity.status,
      participants:opportunity.participants,tags:opportunity.tags??[],next_task:opportunity.next_task??null,deadline:opportunity.deadline??null,missing_material:opportunity.missing_material===true,
    })).sort((a,b)=>Number(a.position)-Number(b.position)||String(a.id).localeCompare(String(b.id))),
    board_revision:boardRevision,
  });

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

    if (url.pathname === '/api/v1/board' && request.method === 'GET') return json(response,200,boardSnapshot());
    if (url.pathname === '/api/v1/board/lists' && request.method === 'POST') {
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}
      if(Number(input.expected_revision)!==boardRevision)return fail(response,412,'BOARD_STALE','Board order changed in another session.');
      const name=typeof input.name==='string'?input.name.trim():'';if(!name||name.length>120)return fail(response,422,'LIST_INVALID','Enter a list name of at most 120 characters.');
      const id=randomHex();const list={id,name,position:boardLists.size,revision:1,archived:false};const command=rememberCommand(request,createHash('sha256').update(JSON.stringify(input)).digest('hex'),boardSnapshot());if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,201,command.prior);boardLists.set(id,list);boardRevision++;return json(response,201,boardSnapshot());
    }
    const boardListMatch=url.pathname.match(/^\/api\/v1\/board\/lists\/([^/]+)(?:\/(archive|cards\/reorder))?$/);
    if(boardListMatch&&request.method==='PATCH'&&!boardListMatch[2]){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}const id=decodeURIComponent(boardListMatch[1]!);const list=boardLists.get(id);if(!list)return fail(response,404,'NOT_FOUND','Board list does not exist');if(Number(input.expected_revision)!==Number(list.revision))return fail(response,412,'LIST_STALE','Board list changed in another session.');const name=typeof input.name==='string'?input.name.trim():'';if(!name||name.length>120)return fail(response,422,'LIST_INVALID','Enter a list name of at most 120 characters.');const command=rememberCommand(request,createHash('sha256').update(JSON.stringify([id,input])).digest('hex'),boardSnapshot());if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,200,command.prior);boardLists.set(id,{...list,name,revision:Number(list.revision)+1});boardRevision++;return json(response,200,boardSnapshot());
    }
    if(url.pathname==='/api/v1/board/lists/order'&&request.method==='PUT'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}if(Number(input.expected_revision)!==boardRevision)return fail(response,412,'BOARD_STALE','Board changed in another session.');const ids=Array.isArray(input.ordered_ids)?input.ordered_ids.map(String):[];const active=[...boardLists.values()].filter(item=>item.archived!==true);if(ids.length!==active.length||new Set(ids).size!==active.length||ids.some(id=>!boardLists.has(id)||boardLists.get(id)!.archived===true))return fail(response,422,'LIST_ORDER_INVALID','Order must include each active list once.');const command=rememberCommand(request,createHash('sha256').update(JSON.stringify(input)).digest('hex'),boardSnapshot());if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,200,command.prior);ids.forEach((id,index)=>{const list=boardLists.get(id)!;boardLists.set(id,{...list,position:index,revision:Number(list.revision)+1});});boardRevision++;return json(response,200,boardSnapshot());
    }
    if(boardListMatch&&boardListMatch[2]==='archive'&&request.method==='POST'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}const id=decodeURIComponent(boardListMatch[1]!);const source=boardLists.get(id);const target=boardLists.get(String(input.destination_list_id));if(!source||!target||source.archived===true||target.archived===true||source.id===target.id)return fail(response,422,'ARCHIVE_DESTINATION_INVALID','Choose a different active destination list.');if(Number(input.expected_revision)!==Number(source.revision))return fail(response,412,'LIST_STALE','Board list changed in another session.');const command=rememberCommand(request,createHash('sha256').update(JSON.stringify([id,input])).digest('hex'),boardSnapshot());if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,200,command.prior);for(const [oppId,item] of opportunities){if(item.list_id===id)opportunities.set(oppId,{...item,list_id:target.id,position:Number(target.position)+1});}boardLists.set(id,{...source,archived:true,revision:Number(source.revision)+1});boardRevision++;return json(response,200,boardSnapshot());
    }
    if(boardListMatch&&boardListMatch[2]==='cards/reorder'&&request.method==='POST'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}const list=boardLists.get(decodeURIComponent(boardListMatch[1]!));const id=String(input.opportunity_id);const item=opportunities.get(id);if(!item||!list||item.list_id!==list.id)return fail(response,404,'NOT_FOUND','Opportunity is not in that list.');if(Number(input.expected_revision)!==Number(list.revision))return fail(response,412,'LIST_STALE','Board list changed in another session.');const ordered=[...opportunities.values()].filter(value=>value.list_id===list.id).sort((a,b)=>Number(a.position)-Number(b.position));const remaining=ordered.filter(value=>value.id!==id);const anchorId=input.anchor_opportunity_id as string|null;const anchor=anchorId===null?null:remaining.find(value=>value.id===anchorId);if(anchorId!==null&&!anchor)return fail(response,422,'ANCHOR_INVALID','Choose a current card as the anchor.');const at=anchor?remaining.indexOf(anchor)+1:0;remaining.splice(at,0,item);remaining.forEach((value,index)=>opportunities.set(String(value.id),{...value,position:index}));const command=rememberCommand(request,createHash('sha256').update(JSON.stringify([list.id,input])).digest('hex'),boardSnapshot());if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,200,command.prior);boardLists.set(String(list.id),{...list,revision:Number(list.revision)+1});boardRevision++;return json(response,200,boardSnapshot());
    }
    const boardCardMatch=url.pathname.match(/^\/api\/v1\/board\/cards\/([^/]+)\/move$/);
    if(boardCardMatch&&request.method==='POST'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}const id=decodeURIComponent(boardCardMatch[1]!);const item=opportunities.get(id);const source=item?boardLists.get(String(item.list_id)):null;const target=boardLists.get(String(input.destination_list_id));if(!item||!source||!target||target.archived===true)return fail(response,404,'NOT_FOUND','Opportunity or active destination list does not exist.');if(Number(input.expected_board_revision)!==boardRevision||Number(input.expected_source_revision)!==Number(source.revision)||Number(input.expected_destination_revision)!==Number(target.revision))return fail(response,412,'BOARD_STALE','Board or list order changed in another session.');const command=rememberCommand(request,createHash('sha256').update(JSON.stringify([id,input])).digest('hex'),boardSnapshot());if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,200,command.prior);const dest=[...opportunities.values()].filter(value=>value.list_id===target.id&&value.id!==id).sort((a,b)=>Number(a.position)-Number(b.position));const anchorId=input.anchor_opportunity_id as string|null;const anchor=anchorId===null?null:dest.find(value=>value.id===anchorId);if(anchorId!==null&&!anchor)return fail(response,422,'ANCHOR_INVALID','Choose a current destination card.');dest.splice(anchor?dest.indexOf(anchor)+1:0,0,{...item,list_id:target.id});dest.forEach((value,index)=>opportunities.set(String(value.id),{...value,list_id:target.id,position:index}));const remainder=[...opportunities.values()].filter(value=>value.list_id===source.id&&value.id!==id).sort((a,b)=>Number(a.position)-Number(b.position));remainder.forEach((value,index)=>opportunities.set(String(value.id),{...value,position:index}));boardLists.set(String(source.id),{...source,revision:Number(source.revision)+1});boardLists.set(String(target.id),{...target,revision:Number(target.revision)+1});boardRevision++;return json(response,200,boardSnapshot());
    }
    const notesMatch=url.pathname.match(/^\/api\/v1\/opportunities\/([^/]+)\/notes(?:\/tasks)?$/);
    if(notesMatch&&request.method==='GET'){const id=decodeURIComponent(notesMatch[1]!);if(!opportunities.has(id))return fail(response,404,'NOT_FOUND','Opportunity does not exist');const notes=opportunityNotes.get(id)??{markdown:'',revision:1};return json(response,200,{opportunity_id:id,...notes});}
    if(notesMatch&&(request.method==='PUT'||request.method==='PATCH')){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}const id=decodeURIComponent(notesMatch[1]!);if(!opportunities.has(id))return fail(response,404,'NOT_FOUND','Opportunity does not exist');const current=opportunityNotes.get(id)??{markdown:'',revision:1};if(Number(input.expected_revision)!==current.revision)return fail(response,412,'NOTES_STALE','Canonical notes changed in another session.');let markdown:string;if(request.method==='PUT'){if(typeof input.markdown!=='string'||Buffer.byteLength(input.markdown)>200000)return fail(response,422,'NOTES_INVALID','Notes must be UTF-8 Markdown up to 200,000 bytes.');markdown=input.markdown;}else{if(!Number.isSafeInteger(input.start_byte)||!Number.isSafeInteger(input.end_byte)||typeof input.line_hash!=='string'||typeof input.checked!=='boolean')return fail(response,422,'TASK_SPAN_INVALID','Task source span is malformed.');const bytes=Buffer.from(current.markdown,'utf8');const start=Number(input.start_byte),end=Number(input.end_byte);if(start<0||end<start||end>bytes.length)return fail(response,422,'TASK_SPAN_INVALID','Task source span is outside the notes.');const lineStart=bytes.lastIndexOf(10,Math.max(0,start-1))+1;let lineEnd=bytes.indexOf(10,end);if(lineEnd<0)lineEnd=bytes.length;const line=bytes.subarray(lineStart,lineEnd);if(createHash('sha256').update(line).digest('hex')!==input.line_hash||bytes.subarray(start,end).toString('utf8')!=='[ ]'&&bytes.subarray(start,end).toString('utf8')!=='[x]'&&bytes.subarray(start,end).toString('utf8')!=='[X]')return fail(response,412,'TASK_STALE','Task marker or source line changed.');markdown=Buffer.concat([bytes.subarray(0,start),Buffer.from(input.checked?'[x]':'[ ]'),bytes.subarray(end)]).toString('utf8');}
      const command=rememberCommand(request,createHash('sha256').update(JSON.stringify([id,input])).digest('hex'),{});if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior?.__key_reused)return fail(response,409,'KEY_REUSED','Idempotency key was used for a different command');if(command.prior){const saved=opportunityNotes.get(id)!;return json(response,200,{opportunity_id:id,...saved});}const saved={markdown,revision:current.revision+1};opportunityNotes.set(id,saved);return json(response,200,{opportunity_id:id,...saved});
    }

    if (url.pathname === '/api/v1/opportunities' && request.method === 'GET') {
      return json(response,200,{data:[...opportunities.values()],cursor:String(events.length)});
    }
    if (url.pathname === '/api/v1/opportunities' && request.method === 'POST') {
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}
      const title=typeof input.title==='string'?input.title.trim():'';const ids=Array.isArray(input.participant_ids)?input.participant_ids.map(String):[];
      if(!title||ids.some((id)=>!directory.has(id)))return fail(response,422,'OPPORTUNITY_INVALID','Title and existing participants are required.');
      const id=randomHex();const stamp=now();const result={id,title,status:'open',revision:1,participants:ids.map((partyId)=>{const row=directory.get(partyId)!;return{id:row.id,kind:row.kind,name:row.name};}),created_at:stamp,updated_at:stamp,list_id:'list-inbox',position:[...opportunities.values()].filter(item=>item.list_id==='list-inbox').length,tags:[],next_task:null,deadline:null,missing_material:false};
      const digest=createHash('sha256').update(JSON.stringify(input)).digest('hex');const command=rememberCommand(request,digest,result);if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,201,command.prior);
      opportunities.set(id,result);opportunityInterests.set(id,[]);opportunityNotes.set(id,{markdown:'',revision:1});boardRevision++;pushEvent({event_id:randomHex(),entity_type:'opportunity',entity_id:id,revision:1,event_type:'opportunity.created',payload:{}});return json(response,201,result);
    }
    const participantMatch=url.pathname.match(/^\/api\/v1\/opportunities\/([^/]+)\/participants$/);
    if(participantMatch&&request.method==='PUT'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}const id=decodeURIComponent(participantMatch[1]!);const current=opportunities.get(id);if(!current)return fail(response,404,'NOT_FOUND','Opportunity does not exist');if(Number(input.expected_revision)!==Number(current.revision))return fail(response,412,'STALE_REVISION','Opportunity participants changed.');const ids=Array.isArray(input.participant_ids)?input.participant_ids.map(String):[];if(ids.some((partyId)=>!directory.has(partyId)))return fail(response,422,'PARTICIPANT_UNKNOWN','Every participant must be a saved party.');const result={...current,participants:ids.map((partyId)=>{const row=directory.get(partyId)!;return{id:row.id,kind:row.kind,name:row.name};}),revision:Number(current.revision)+1,updated_at:now()};const command=rememberCommand(request,createHash('sha256').update(JSON.stringify([id,input])).digest('hex'),result);if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,200,command.prior);opportunities.set(id,result);return json(response,200,result);
    }
    const interestMatch=url.pathname.match(/^\/api\/v1\/opportunities\/([^/]+)\/interests$/);
    if(interestMatch&&request.method==='GET'){const rows=opportunityInterests.get(decodeURIComponent(interestMatch[1]!));return rows?json(response,200,{data:rows}):fail(response,404,'NOT_FOUND','Opportunity does not exist');}
    if(interestMatch&&request.method==='POST'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}const id=decodeURIComponent(interestMatch[1]!);if(!opportunities.has(id))return fail(response,404,'NOT_FOUND','Opportunity does not exist');const statement=typeof input.statement==='string'?input.statement.trim():'';if(!['me','them'].includes(String(input.side))||!statement)return fail(response,422,'INTEREST_INVALID','Choose a side and write an interest.');const result={id:randomHex(),opportunity_id:id,side:input.side,statement,tier:3,status:'active',revision:1};const command=rememberCommand(request,createHash('sha256').update(JSON.stringify([id,input])).digest('hex'),result);if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,201,command.prior);opportunityInterests.get(id)!.push(result);return json(response,201,result);
    }
    const exchangeMatch=url.pathname.match(/^\/api\/v1\/opportunities\/([^/]+)\/exchange$/);
    if(exchangeMatch&&request.method==='GET'){const id=decodeURIComponent(exchangeMatch[1]!);if(!opportunities.has(id))return fail(response,404,'NOT_FOUND','Opportunity does not exist');return json(response,200,opportunityExchanges.get(id)??null);}
    if(exchangeMatch&&request.method==='PUT'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}const id=decodeURIComponent(exchangeMatch[1]!);const current=opportunityExchanges.get(id);if(!opportunities.has(id))return fail(response,404,'NOT_FOUND','Opportunity does not exist');if(Number(input.expected_revision)!==(current?Number(current.version):0))return fail(response,412,'STALE_EXCHANGE','Exchange changed in another session.');const linked=Array.isArray(input.interest_ids)?input.interest_ids.map(String):[];const available=opportunityInterests.get(id)??[];const sides=new Set(linked.map((interestId)=>available.find((item)=>item.id===interestId)?.side).filter(Boolean));if(!String(input.they_get??'').trim()||!String(input.i_get??'').trim()||!sides.has('them')||!sides.has('me'))return fail(response,422,'EXCHANGE_INCOMPLETE','Link an active interest from each side and state both benefits.');const result={id:randomHex(),opportunity_id:id,version:(current?Number(current.version):0)+1,they_get:String(input.they_get),i_get:String(input.i_get),interest_ids:linked,fairness_attested:input.fairness_attested===true,attested_at:input.fairness_attested===true?now():null};const command=rememberCommand(request,createHash('sha256').update(JSON.stringify([id,input])).digest('hex'),result);if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,200,command.prior);opportunityExchanges.set(id,result);return json(response,200,result);
    }
    const opportunityMaterials=url.pathname.match(/^\/api\/v1\/opportunities\/([^/]+)\/materials$/);
    if(opportunityMaterials&&request.method==='GET'){const id=decodeURIComponent(opportunityMaterials[1]!);if(!opportunities.has(id))return fail(response,404,'NOT_FOUND','Opportunity does not exist');return json(response,200,{data:[...materials.values()].filter((item)=>item.opportunity_id===id)});}
    if(opportunityMaterials&&request.method==='POST'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}const opportunityId=decodeURIComponent(opportunityMaterials[1]!);const opportunity=opportunities.get(opportunityId);const recipient=opportunity?.participants instanceof Array?(opportunity.participants as RecordData[]).find((item)=>item.id===input.recipient_id):null;if(!opportunity||!recipient||!['outreach','follow_up','handoff'].includes(String(input.kind))||!String(input.title??'').trim())return fail(response,422,'MATERIAL_INVALID','Choose a participant and valid material details.');const id=randomHex();const result={id,opportunity_id:opportunityId,recipient_id:recipient.id,recipient_kind:recipient.kind,recipient_name:recipient.name,kind:input.kind,title:String(input.title).trim(),revision:1,current_version:null,versions:[],claim_references:[],approval:null,updated_at:now()};const command=rememberCommand(request,createHash('sha256').update(JSON.stringify([opportunityId,input])).digest('hex'),result);if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,201,command.prior);materials.set(id,result);return json(response,201,result);
    }
    const materialMatch=url.pathname.match(/^\/api\/v1\/materials\/([^/]+)(?:\/(versions|gate|approval|copy-grant))?$/);
    if(materialMatch){const id=decodeURIComponent(materialMatch[1]!);const operation=materialMatch[2];const material=materials.get(id);if(!material)return fail(response,404,'NOT_FOUND','Material does not exist');
      if(!operation&&request.method==='GET')return json(response,200,material);
      if(operation==='versions'&&request.method==='PUT'){
        let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}if(Number(input.expected_revision)!==Number(material.revision))return fail(response,412,'STALE_MATERIAL','Material changed in another session.');if(typeof input.body!=='string'||!Array.isArray(input.sentences)||!input.sentences.every((sentence)=>sentence&&typeof sentence.text==='string'&&Array.isArray(sentence.claims)&&Array.isArray(sentence.rules)))return fail(response,422,'MATERIAL_VERSION_INVALID','Body and sentence annotations are required.');const sequence=(Array.isArray(material.versions)?material.versions.length:0)+1;const version={id:randomHex(),material_id:id,sequence,body:input.body,sentences:input.sentences,slot_fills:input.slot_fills??{},protected_tokens:input.protected_tokens??[],digest:createHash('sha256').update(input.body).digest('hex'),created_at:now()};const result={...material,revision:Number(material.revision)+1,current_version:version,versions:[...(material.versions as RecordData[]),version],approval:null,updated_at:now()};const command=rememberCommand(request,createHash('sha256').update(JSON.stringify([id,input])).digest('hex'),result);if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,200,command.prior);materials.set(id,result);return json(response,200,result);
      }
      if(operation==='gate'&&request.method==='GET'){if(!material.current_version||String(material.current_version&& (material.current_version as RecordData).id)!==url.searchParams.get('version_id'))return fail(response,409,'VERSION_STALE','Requested material version is not current.');return json(response,200,gateFor(material));}
      if(operation==='approval'&&request.method==='POST'){
        let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}if(Number(input.expected_revision)!==Number(material.revision)||!material.current_version||(material.current_version as RecordData).id!==input.version_id)return fail(response,412,'VERSION_STALE','Material version changed before approval.');const gate=gateFor(material);if((gate.hard_failures as string[]).length)return fail(response,422,'GATE_HARD_FAILURE','Hard gate failures block approval.');const reasons=input.override_reasons&&typeof input.override_reasons==='object'?input.override_reasons as RecordData:{};for(const softId of gate.soft_failures as string[]){if(typeof reasons[softId]!=='string'||String(reasons[softId]).trim().length<10)return fail(response,422,'SOFT_OVERRIDE_REQUIRED',`A reason of at least ten characters is required for ${softId}.`);}const version=material.current_version as RecordData;const result={id:randomHex(),material_id:id,version_id:version.id,recipient_id:material.recipient_id,digest:version.digest,approved_at:now(),gate_snapshot:gate,override_reasons:reasons};const command=rememberCommand(request,createHash('sha256').update(JSON.stringify([id,input])).digest('hex'),result);if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,201,command.prior);const updated={...material,approval:result,updated_at:now()};materials.set(id,updated);return json(response,201,result);
      }
      if(operation==='copy-grant'&&request.method==='POST'){
        let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}const version=material.current_version as RecordData|null;const approval=material.approval as RecordData|null;if(!version||version.id!==input.version_id||material.recipient_id!==input.recipient_id||!approval||approval.version_id!==version.id||approval.recipient_id!==input.recipient_id)return fail(response,403,'RELEASE_NOT_APPROVED','Only the exact approved version and recipient may be released.');const gate=gateFor(material);if((gate.hard_failures as string[]).length)return fail(response,403,'GATE_CHANGED','Current prerequisites invalidate release.');const grantId=randomHex();const result={grant_id:grantId,material_id:id,version_id:version.id,recipient_id:material.recipient_id,digest:version.digest,body:version.body,expires_at:new Date(testClock+5*60_000).toISOString()};const command=rememberCommand(request,createHash('sha256').update(JSON.stringify([id,input])).digest('hex'),result);if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,201,command.prior);copyGrants.set(grantId,{...result,status:'issued',acknowledged_at:null});return json(response,201,result);
      }
    }
    const grantReceiptMatch=url.pathname.match(/^\/api\/v1\/copy-grants\/([^/]+)$/);
    if(grantReceiptMatch&&request.method==='GET'){const grant=copyGrants.get(decodeURIComponent(grantReceiptMatch[1]!));if(!grant)return fail(response,404,'NOT_FOUND','Copy grant does not exist');return json(response,200,{grant_id:grant.grant_id,material_id:grant.material_id,version_id:grant.version_id,recipient_id:grant.recipient_id,digest:grant.digest,status:grant.status,acknowledged_at:grant.acknowledged_at});}
    const grantAckMatch=url.pathname.match(/^\/api\/v1\/copy-grants\/([^/]+)\/acknowledgement$/);
    if(grantAckMatch&&request.method==='POST'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}const grantId=decodeURIComponent(grantAckMatch[1]!);const grant=copyGrants.get(grantId);if(!grant)return fail(response,404,'NOT_FOUND','Copy grant does not exist');const command=rememberCommand(request,createHash('sha256').update(JSON.stringify([grantId,input])).digest('hex'),{});if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior?.__key_reused)return fail(response,409,'KEY_REUSED','Idempotency key was used for a different command');if(command.prior){const known=copyGrants.get(grantId)!;return json(response,200,{grant_id:known.grant_id,material_id:known.material_id,version_id:known.version_id,recipient_id:known.recipient_id,digest:known.digest,status:known.status,acknowledged_at:known.acknowledged_at});}const result:RecordData={...grant,status:input.success===true?'acknowledged':'failed',acknowledged_at:now()};copyGrants.set(grantId,result);return json(response,200,{grant_id:result.grant_id,material_id:result.material_id,version_id:result.version_id,recipient_id:result.recipient_id,digest:result.digest,status:result.status,acknowledged_at:result.acknowledged_at});
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
      };
      captures.unshift(result);
      sourceDetails.set(result.occurrence_id,{...result,text:input.text,title:typeof input.title==='string'?input.title:null,url:typeof input.url==='string'?input.url:null,captured_at:now(),actor_kind:typeof input.actor_kind==='string'?input.actor_kind:'unknown',actor_entity_id:typeof input.actor_entity_id==='string'?input.actor_entity_id:null,normalization_version:1});
      return json(response, 201, result);
    }
    if(url.pathname==='/api/v1/capture-stages'&&request.method==='POST'){
      let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}
      if(typeof input.text!=='string'||!input.text.trim())return fail(response,422,'CAPTURE_EMPTY','Paste source text before staging.');
      const normalized=input.text.normalize('NFKC').replace(/\s+/g,' ').trim().toLocaleLowerCase();const normalizedHash=createHash('sha256').update(normalized).digest('hex');const group=sourceGroups.get(normalizedHash)??{id:randomHex(),variants:new Map<string,string>()};sourceGroups.set(normalizedHash,group);const rawHash=createHash('sha256').update(input.text).digest('hex');const variant=group.variants.get(rawHash)??randomHex();group.variants.set(rawHash,variant);const source={content_id:group.id,variant_id:variant,occurrence_id:randomHex(),normalized_sha256:normalizedHash,raw_sha256:rawHash,text:input.text,title:typeof input.title==='string'?input.title:null,url:typeof input.url==='string'?input.url:null,captured_at:now(),actor_kind:input.actor_kind??'unknown',actor_entity_id:input.actor_entity_id??null,normalization_version:1};sourceDetails.set(source.occurrence_id,source);captures.unshift({content_id:source.content_id,variant_id:variant,occurrence_id:source.occurrence_id,normalized_sha256:normalizedHash,raw_sha256:rawHash});const batchId=randomHex();const jobId=randomHex();const hiringQuote='The next engineer will work with Dana on onboarding.';const proposals:RecordData[]=[];const hiringIndex=input.text.indexOf(hiringQuote);if(hiringIndex>=0){proposals.push({id:randomHex(),action:'create',target_kind:'hiring_signal',target_id:null,target_label:'Hiring signal',candidate_targets:[],statement:'The source says the next engineer will work with Dana on onboarding.',quote:hiringQuote,quote_start:hiringIndex,quote_end:hiringIndex+hiringQuote.length,tier:1,ambiguity:null,unresolved:false,candidate:{summary:'Hiring signal'},target_revision:null,review_state:'proposed'});const danaIndex=input.text.indexOf('Dana',hiringIndex);if(danaIndex>=0)proposals.push({id:randomHex(),action:'link',target_kind:'person',target_id:null,target_label:'Dana (unresolved mention)',candidate_targets:[...directory.values()].filter((item)=>item.name.toLowerCase().includes('dana')).map((item)=>({id:item.id,kind:item.kind,label:item.name})),statement:'The source mentions Dana but does not identify which person.',quote:'Dana',quote_start:danaIndex,quote_end:danaIndex+4,tier:1,ambiguity:'The source does not disambiguate Dana by organization or role.',unresolved:true,candidate:null,target_revision:null,review_state:'proposed'});}
      const result={batch_id:batchId,job_id:jobId,status:/MOCK_RUNNING_CAPTURE/.test(input.text)?'running':'ok',revision:1,injection_suspected:/ignore\s+(?:all\s+)?previous|system\s+prompt|you\s+are\s+now/i.test(input.text),source,opportunity_ids:Array.isArray(input.opportunity_ids)?input.opportunity_ids.map(String):[],proposals};const command=rememberCommand(request,createHash('sha256').update(JSON.stringify(input)).digest('hex'),result);if(!command)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');if(command.prior)return command.prior.__key_reused?fail(response,409,'KEY_REUSED','Idempotency key was used for a different command'):json(response,202,command.prior);captureStages.set(batchId,result);pushEvent({event_id:randomHex(),entity_type:'capture_stage',entity_id:batchId,revision:1,event_type:'capture.staged',payload:{}});return json(response,202,result);
    }
    const captureStageMatch=url.pathname.match(/^\/api\/v1\/capture-stages\/([^/]+)(?:\/(decisions|source-only|cancel))?$/);
    if(captureStageMatch){const batchId=decodeURIComponent(captureStageMatch[1]!);const operation=captureStageMatch[2];const batch=captureStages.get(batchId);if(!batch)return fail(response,404,'NOT_FOUND','Capture stage does not exist');if(!operation&&request.method==='GET')return json(response,200,batch);
      if(operation==='cancel'&&request.method==='POST'){let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}if(input.job_id!==batch.job_id)return fail(response,409,'JOB_MISMATCH','Cancellation target does not match this stage.');if(batch.status==='queued'||batch.status==='running'){batch.status='cancelled';batch.revision=Number(batch.revision)+1;pushEvent({event_id:randomHex(),entity_type:'capture_stage',entity_id:batchId,revision:Number(batch.revision),event_type:'capture.cancelled',payload:{}});}return json(response,200,batch);}
      if((operation==='decisions'||operation==='source-only')&&request.method==='POST'){
        let input:RecordData;try{input=await readBody(request);}catch{return fail(response,400,'INVALID_JSON','Request body is invalid');}if(Number(input.expected_revision)!==Number(batch.revision))return fail(response,412,'STALE_CAPTURE_REVIEW','Capture review changed in another session.');const commandKey=request.headers['idempotency-key'];if(typeof commandKey!=='string'||!commandKey)return fail(response,400,'IDEMPOTENCY_KEY_REQUIRED','Idempotency-Key is required');const commandDigest=createHash('sha256').update(JSON.stringify([batchId,input])).digest('hex');const prior=receipts.get(commandKey);if(prior)return prior.digest===commandDigest?json(response,200,prior.result):fail(response,409,'KEY_REUSED','Idempotency key was used for a different command');const proposals=batch.proposals as RecordData[];const committed:Array<RecordData>=[];if(operation==='decisions'){if(!Array.isArray(input.decisions)||!input.decisions.length)return fail(response,422,'DECISIONS_REQUIRED','Choose at least one proposal decision.');for(const raw of input.decisions as RecordData[]){const proposal=proposals.find((item)=>item.id===raw.proposal_id);if(!proposal||proposal.review_state!=='proposed')return fail(response,412,'PROPOSAL_STALE','A selected proposal is no longer pending.');if(raw.action==='confirm'&&proposal.candidate_targets instanceof Array&&proposal.candidate_targets.length&&!raw.target_id)return fail(response,422,'TARGET_REQUIRED','Choose a target or keep the mention unresolved.');if(raw.action==='correct'&&typeof raw.corrected_statement!=='string')return fail(response,422,'CORRECTION_REQUIRED','Corrected wording is required.');const state=raw.action==='confirm'?'confirmed':raw.action==='correct'?'corrected':raw.action==='dismiss'?'dismissed':'unresolved';proposal.review_state=state;committed.push({proposal_id:proposal.id,review_state:state});}}
        else for(const proposal of proposals){if(proposal.review_state==='proposed'){proposal.review_state='source_only';committed.push({proposal_id:proposal.id,review_state:'source_only'});}}
        batch.revision=Number(batch.revision)+1;const decisionStatus=operation==='source-only'?'source_only':proposals.some((item)=>item.review_state==='proposed')?'partially_applied':'applied';const receipt={batch_id:batchId,revision:batch.revision,status:decisionStatus,committed,source:batch.source};receipts.set(commandKey,{digest:commandDigest,result:receipt});pushEvent({event_id:randomHex(),entity_type:'capture_stage',entity_id:batchId,revision:Number(batch.revision),event_type:'capture.reviewed',payload:{decision_count:committed.length}});return json(response,200,receipt);
      }
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
