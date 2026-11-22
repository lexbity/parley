import type { components } from './generated';

export type Resource = components['schemas']['Resource'];
export type GraphEntity = components['schemas']['GraphEntity'];
export type Entity = components['schemas']['Entity'];
export type SavedSource = components['schemas']['SavedSource'];
export type EntityInput = components['schemas']['EntityInput'];
export type CaptureInput = components['schemas']['CaptureInput'];
export type DirectoryEntity = { id:string; kind:'person'|'org'|'project'; name:string; descriptor:string; aliases:string[]; tags:string[]; revision:number };
export type DirectoryPage = { items:DirectoryEntity[]; total:number; page:number; page_size:number; has_more:boolean };
export type GraphEdge = { id:string; from_id:string; to_id:string; verb:string; revision:number; claim_id:string; claim_revision:number; wording:string; approved_wording:string|null; tier:number; speaker_kind:string; speaker_entity_id:string|null; occurrence_id:string|null; source_start:number|null; source_end:number|null; normalization_version:number|null; source_title:string|null; source_url:string|null };
export type Neighborhood = { focus_id:string; nodes:DirectoryEntity[]; edges:GraphEdge[]; positions:Array<{entity_id:string;x:number;y:number;pinned:boolean;revision:number}>; omitted_nodes:number; omitted_edges:number };
export type SourceDetail = SavedSource & { text:string; title:string|null; url:string|null; captured_at:string; actor_kind:string; actor_entity_id:string|null; normalization_version:number };
export type Opportunity = { id:string; title:string; status:'open'|'resolved'|'timed_out'|'archived'; revision:number; participants:Array<{id:string;kind:'person'|'org';name:string}>; created_at:string; updated_at:string };
export type OpportunityInterest = { id:string; opportunity_id:string; side:'me'|'them'; statement:string; tier:3; status:'active'|'dismissed'; revision:number };
export type OpportunityExchange = { id:string; opportunity_id:string; version:number; they_get:string; i_get:string; interest_ids:string[]; fairness_attested:boolean; attested_at:string|null };
export type MaterialSentence = {text:string;claims:Array<{type:'self_fact'|'other_fact'|'offer'|'question'|'ask'|'generic';ref_kind?:string;ref_id?:string}>;rules:string[]};
export type MaterialVersion = {id:string;material_id:string;sequence:number;body:string;sentences:MaterialSentence[];slot_fills:Record<string,string>;protected_tokens:string[];digest:string;created_at:string};
export type Material = {id:string;opportunity_id:string;recipient_id:string;recipient_kind:'person'|'org';recipient_name:string;kind:'outreach'|'follow_up'|'handoff';title:string;revision:number;current_version:MaterialVersion|null;versions:MaterialVersion[];claim_references:Array<{kind:string;id:string;label:string;tier:number}>;approval:MaterialApprovalDTO|null;updated_at:string};
export type MaterialGateDTO = {material_id:string;version_id:string;recipient_id:string;evaluated_at:string;results:Array<{id:string;severity:'hard'|'soft';pass:boolean;detail:string}>;hard_failures:string[];soft_failures:string[];copy_enabled:boolean};
export type MaterialApprovalDTO = {id:string;material_id:string;version_id:string;recipient_id:string;digest:string;approved_at:string;gate_snapshot:MaterialGateDTO};
export type CopyGrantDTO = {grant_id:string;material_id:string;version_id:string;recipient_id:string;digest:string;body:string;expires_at:string};
export type CopyGrantReceiptDTO = {grant_id:string;material_id:string;version_id:string;recipient_id:string;digest:string;status:'issued'|'acknowledged'|'failed'|'expired';acknowledged_at:string|null};
export type CaptureReviewSourceDTO = {occurrence_id:string;content_id:string;variant_id:string;normalized_sha256:string;raw_sha256:string;text:string;title:string|null;url:string|null;captured_at:string;actor_kind:string;actor_entity_id:string|null;normalization_version:number};
export type CaptureProposalDTO = {id:string;action:'create'|'link'|'update';target_kind:string;target_id:string|null;target_label:string;candidate_targets:Array<{id:string;kind:'person'|'org'|'project'|'artifact'|'capture'|'resource';label:string}>;statement:string;quote:string;quote_start:number|null;quote_end:number|null;tier:1|2|3;ambiguity:string|null;unresolved:boolean;candidate:Record<string,unknown>|null;target_revision:number|null;review_state:'proposed'|'confirmed'|'corrected'|'dismissed'|'unresolved'|'asked'|'source_only'};
export type CaptureReviewDTO = {batch_id:string;job_id:string;status:'queued'|'running'|'ok'|'invalid'|'failed'|'cancelled';revision:number;injection_suspected:boolean;source:CaptureReviewSourceDTO;opportunity_ids:string[];proposals:CaptureProposalDTO[]};
export type CaptureReviewReceiptDTO = {batch_id:string;revision:number;status:string;committed:Array<{proposal_id:string;review_state:'confirmed'|'corrected'|'dismissed'|'unresolved'|'source_only'}>;source:CaptureReviewSourceDTO};
export type OpportunityBoardItem = {id:string;revision:number;list_id:string;position:number;title:string;status:string;participants:Array<{id:string;kind:'person'|'org';name:string}>;tags:string[];next_task:string|null;deadline:string|null;missing_material:boolean};
export type BoardListDTO = {id:string;name:string;position:number;revision:number;archived:boolean};
export type BoardSnapshotDTO = {items:OpportunityBoardItem[];lists:BoardListDTO[];board_revision:number};
export type OpportunityNotesDTO = {opportunity_id:string;markdown:string;revision:number};
type Session = components['schemas']['Session'];
type EventPage = components['schemas']['EventPage'];
type CreateResource = components['schemas']['CreateResource'];
type UpdateResource = components['schemas']['UpdateResource'];

export function newOperationKey(): string {
  const timestamp = Date.now().toString(16).padStart(12, '0').slice(-12);
  const bytes = globalThis.crypto.getRandomValues(new Uint8Array(10));
  const entropy = Array.from(bytes, (value) => value.toString(16).padStart(2, '0')).join('');
  const variant = ((Number.parseInt(entropy[3] ?? '0', 16) & 0x3) | 0x8).toString(16);
  return `${timestamp.slice(0, 8)}-${timestamp.slice(8)}-7${entropy.slice(0, 3)}-${variant}${entropy.slice(4, 7)}-${entropy.slice(7, 19)}`;
}

function object(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function requiredString(value: unknown, field: string): string {
  if (typeof value !== 'string') throw new Error(`Invalid response: ${field} must be text`);
  return value;
}

function decodeSession(value: unknown): Session {
  const session = object(value);
  if (!session || !Array.isArray(session.capabilities) || !session.capabilities.every((item) => typeof item === 'string')
    || !Number.isInteger(session.wire_major) || !Number.isInteger(session.wire_minor)) {
    throw new Error('Invalid response: workspace session is malformed');
  }
  return {
    workspace_id: requiredString(session.workspace_id, 'workspace_id'),
    epoch: requiredString(session.epoch, 'epoch'),
    wire_major: session.wire_major as number,
    wire_minor: session.wire_minor as number,
    capabilities: session.capabilities,
    mutation_token: requiredString(session.mutation_token, 'mutation_token'),
    cursor: requiredString(session.cursor, 'cursor'),
  };
}

function decodeResource(value: unknown): Resource {
  const resource = object(value);
  const body = resource ? object(resource.body) : null;
  if (!resource || !body
    || !['gift', 'proof', 'artifact'].includes(String(resource.kind))
    || !['draft', 'ready', 'retired'].includes(String(resource.readiness))
    || !Number.isSafeInteger(resource.revision) || Number(resource.revision) < 1) {
    throw new Error('Invalid response: resource is malformed');
  }
  return {
    id: requiredString(resource.id, 'resource.id'),
    kind: resource.kind as Resource['kind'],
    title: requiredString(resource.title, 'resource.title'),
    body,
    readiness: resource.readiness as Resource['readiness'],
    revision: Number(resource.revision),
    etag: requiredString(resource.etag, 'resource.etag'),
  };
}

function decodeGraphEntity(value: unknown): GraphEntity {
  const entity = object(value);
  if (!entity
    || !['person', 'org', 'project', 'resource'].includes(String(entity.kind))
    || !Number.isSafeInteger(entity.revision) || Number(entity.revision) < 1) {
    throw new Error('Invalid response: directory entity is malformed');
  }
  return {
    id: requiredString(entity.id, 'entity.id'),
    kind: entity.kind as GraphEntity['kind'],
    name: requiredString(entity.name, 'entity.name'),
    revision: Number(entity.revision),
  };
}

function decodeEnvelopeData(value: unknown, decode: (item: unknown) => Resource | GraphEntity | SavedSource): { data: Resource | GraphEntity | SavedSource } {
  const envelope = object(value);
  if (!envelope) throw new Error('Invalid response: entity envelope is malformed');
  return { data: decode(envelope.data) };
}

function decodeEnvelopeList(value: unknown, decode: (item: unknown) => Resource | GraphEntity | SavedSource): { data: Array<Resource | GraphEntity | SavedSource>; cursor: string } {
  const envelope = object(value);
  if (!envelope || !Array.isArray(envelope.data)) throw new Error('Invalid response: resource list is malformed');
  requiredString(envelope.cursor, 'cursor');
  return { data: envelope.data.map(decode), cursor: requiredString(envelope.cursor, 'cursor') };
}

function decodeSavedSource(value: unknown): SavedSource {
  const source = object(value);
  if (!source) throw new Error('Invalid response: saved source is malformed');
  return {
    content_id: requiredString(source.content_id, 'source.content_id'),
    variant_id: requiredString(source.variant_id, 'source.variant_id'),
    occurrence_id: requiredString(source.occurrence_id, 'source.occurrence_id'),
    normalized_sha256: requiredString(source.normalized_sha256, 'source.normalized_sha256'),
    raw_sha256: requiredString(source.raw_sha256, 'source.raw_sha256'),
  };
}

function decodeSavedSourceList(value: unknown): SavedSource[] {
  if (!Array.isArray(value)) throw new Error('Invalid response: source list is malformed');
  return value.map(decodeSavedSource);
}

function decodeEventPage(value: unknown): EventPage {
  const page = object(value);
  if (!page || !Array.isArray(page.items)) throw new Error('Invalid response: event page is malformed');
  const items = page.items.map((value) => {
    const event = object(value);
    const payload = event ? object(event.payload) : null;
    if (!event || !payload || !Number.isInteger(event.revision)) throw new Error('Invalid response: event is malformed');
    return {
      cursor: requiredString(event.cursor, 'event.cursor'),
      event_id: requiredString(event.event_id, 'event.event_id'),
      entity_type: requiredString(event.entity_type, 'event.entity_type'),
      entity_id: requiredString(event.entity_id, 'event.entity_id'),
      revision: Number(event.revision),
      event_type: requiredString(event.event_type, 'event.event_type'),
      payload,
    };
  });
  return {
    items,
    next_cursor: requiredString(page.next_cursor, 'next_cursor'),
    observed_cursor: requiredString(page.observed_cursor, 'observed_cursor'),
  };
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code: string,
    readonly details: Array<{ field?: string; message: string }> = [],
  ) {
    super(message);
  }
}

function decodeApiError(value: unknown, status: number): ApiError {
  const root = object(value);
  const error = root ? object(root.error) : null;
  const details = error && Array.isArray(error.details)
    ? error.details.flatMap((item) => {
      const detail = object(item);
      if (!detail || typeof detail.message !== 'string') return [];
      return [{ ...(typeof detail.field === 'string' ? { field: detail.field } : {}), message: detail.message }];
    })
    : [];
  return new ApiError(
    error && typeof error.message === 'string' ? error.message : `Request failed (${status})`,
    status,
    error && typeof error.code === 'string' ? error.code : 'INVALID_ERROR_RESPONSE',
    details,
  );
}

export class ParleyClient {
  private session: Session | null = null;
  private connection: Promise<void> | null = null;

  get workspaceEpoch(): string {
    if (!this.session) throw new Error('Workspace session is not connected.');
    return this.session.epoch;
  }

  get eventCursor(): string {
    if (!this.session) throw new Error('Workspace session is not connected.');
    return this.session.cursor;
  }

  /**
   * Redeem the bootstrap fragment once and load the workspace session. Every
   * caller awaits the same promise so the one-time secret is spent exactly
   * once and no request is sent before the session is known (RFC-0003 FQ1).
   */
  connect(): Promise<void> {
    if (!this.connection) {
      this.connection = this.establishSession().catch((error) => {
        this.connection = null;
        throw error;
      });
    }
    return this.connection;
  }

  private async establishSession(): Promise<void> {
    const fragment = new URLSearchParams(window.location.hash.slice(1));
    const bootstrap = fragment.get('bootstrap');
    if (bootstrap) {
      history.replaceState(null, '', `${location.pathname}${location.search}`);
      await this.request('/session/bootstrap', {
        method: 'POST',
        body: JSON.stringify({ secret: bootstrap }),
      }, false);
    }
    this.session = decodeSession(await this.request('/session'));
  }

  async listResources(): Promise<Resource[]> {
    const { data } = decodeEnvelopeList(await this.request('/entities?kind=resource'), decodeResource);
    return data as Resource[];
  }

  async listParties(): Promise<GraphEntity[]> {
    const { data } = decodeEnvelopeList(
      await this.request('/entities?kind=person&kind=org'),
      decodeGraphEntity,
    );
    return data as GraphEntity[];
  }

  async searchDirectory(filters: { q?:string; kind?:string; tag?:string } = {}, signal?:AbortSignal, page=1, pageSize=50): Promise<DirectoryPage> {
    const query = new URLSearchParams();
    if (filters.q) query.set('q', filters.q); if (filters.kind) query.set('kind', filters.kind); if (filters.tag) query.set('tag', filters.tag);query.set('page',String(page));query.set('page_size',String(pageSize));
    const value = object(await this.request(`/directory${query.size ? `?${query}` : ''}`,{signal}));
    if (!value || !Array.isArray(value.data)||!Number.isSafeInteger(value.total)||!Number.isSafeInteger(value.page)||!Number.isSafeInteger(value.page_size)||typeof value.has_more!=='boolean') throw new Error('Invalid response: directory is malformed');
    const items=value.data.map((item) => {
      const row=object(item); if (!row || !Array.isArray(row.aliases) || !Array.isArray(row.tags) || !Number.isSafeInteger(row.revision)) throw new Error('Invalid response: directory row is malformed');
      return { id:requiredString(row.id,'directory.id'), kind:requiredString(row.kind,'directory.kind') as DirectoryEntity['kind'], name:requiredString(row.name,'directory.name'), descriptor:requiredString(row.descriptor,'directory.descriptor'), aliases:row.aliases.map((v)=>requiredString(v,'alias')), tags:row.tags.map((v)=>requiredString(v,'tag')), revision:Number(row.revision) };
    });
    return {items,total:Number(value.total),page:Number(value.page),page_size:Number(value.page_size),has_more:value.has_more};
  }

  async updateDirectory(entity:DirectoryEntity,input:{name:string;descriptor:string;aliases:string[];tags:string},key:string):Promise<DirectoryEntity>{
    const response=object(await this.mutate(`/directory/${encodeURIComponent(entity.id)}`,'PATCH',{expected_revision:entity.revision,name:input.name,descriptor:input.descriptor,aliases:input.aliases,tags:input.tags.split(',').map(value=>value.trim()).filter(Boolean)},{'Idempotency-Key':key}));
    if(!response||!Number.isSafeInteger(response.revision)||!Array.isArray(response.aliases)||!Array.isArray(response.tags))throw new Error('Invalid response: directory update is malformed');
    return {id:requiredString(response.id,'directory.id'),kind:requiredString(response.kind,'directory.kind') as DirectoryEntity['kind'],name:requiredString(response.name,'directory.name'),descriptor:requiredString(response.descriptor,'directory.descriptor'),aliases:response.aliases.map(v=>requiredString(v,'alias')),tags:response.tags.map(v=>requiredString(v,'tag')),revision:Number(response.revision)};
  }

  async getNeighborhood(id:string, viewScope='party', viewId=id, signal?:AbortSignal): Promise<Neighborhood> {
    const query=new URLSearchParams({view_scope:viewScope,view_id:viewId}); const value=object(await this.request(`/graph/${encodeURIComponent(id)}?${query}`,{signal}));
    if (!value || !Array.isArray(value.nodes) || !Array.isArray(value.edges) || !Array.isArray(value.positions)) throw new Error('Invalid response: graph neighborhood is malformed');
    if(!Number.isSafeInteger(value.omitted_nodes)||!Number.isSafeInteger(value.omitted_edges))throw new Error('Invalid response: graph counts are malformed');
    return value as unknown as Neighborhood;
  }

  async getSource(occurrenceId:string): Promise<SourceDetail> {
    const value=object(await this.request(`/captures/${encodeURIComponent(occurrenceId)}`));
    if (!value || typeof value.text!=='string') throw new Error('Invalid response: source detail is malformed');
    return { ...decodeSavedSource(value), text:value.text, title:typeof value.title==='string'?value.title:null, url:typeof value.url==='string'?value.url:null, captured_at:requiredString(value.captured_at,'source.captured_at'), actor_kind:requiredString(value.actor_kind,'source.actor_kind'), actor_entity_id:typeof value.actor_entity_id==='string'?value.actor_entity_id:null, normalization_version:Number(value.normalization_version) };
  }

  async correctClaim(claimId:string, expectedRevision:number, wording:string): Promise<{claim_id:string;revision:number;approved_wording:string}> {
    const value=object(await this.mutate(`/claims/${encodeURIComponent(claimId)}/correction`,'POST',{expected_revision:expectedRevision,wording},{'Idempotency-Key':newOperationKey()}));
    if (!value || !Number.isSafeInteger(value.revision) || Number(value.revision) < 1) throw new Error('Invalid response: correction receipt is malformed');
    return {claim_id:requiredString(value.claim_id,'claim_id'),revision:Number(value.revision),approved_wording:requiredString(value.approved_wording,'approved_wording')};
  }

  async saveLayout(input:{view_scope:string;view_id:string;entity_id:string;expected_revision:number;x:number;y:number}):Promise<void> {
    await this.mutate('/layout','POST',input,{'Idempotency-Key':newOperationKey()});
  }

  async createRelated(input:{kind:string;name:string;related_entity_id:string;verb:string;occurrence_id:string;quote:string;view_scope:string;view_id:string;x:number;y:number}):Promise<GraphEntity> {
    const data=await this.request('/graph/related',{method:'POST',body:JSON.stringify(input),headers:{'Idempotency-Key':newOperationKey()}});
    return decodeGraphEntity(data);
  }

  async linkExisting(input:{from_id:string;to_id:string;verb:string;occurrence_id:string;quote:string}):Promise<string>{
    const value=object(await this.mutate('/graph/edges','POST',input,{'Idempotency-Key':newOperationKey()}));if(!value)return Promise.reject(new Error('Invalid response: edge receipt is malformed'));return requiredString(value.edge_id,'edge_id');
  }

  async getResource(id: string): Promise<Resource> {
    const { data } = decodeEnvelopeData(await this.request(`/entities/${encodeURIComponent(id)}`), decodeResource);
    return data as Resource;
  }

  async getEvents(after = '0'): Promise<EventPage> {
    return decodeEventPage(await this.request(`/events?after=${encodeURIComponent(after)}`));
  }

  async createResource(input: CreateResource, key: string): Promise<Resource> {
    const { data } = decodeEnvelopeData(
      await this.mutate('/entities', 'POST', input, { 'Idempotency-Key': key }),
      decodeResource,
    );
    return data as Resource;
  }

  async createEntity(input: EntityInput, key: string): Promise<GraphEntity> {
    const { data } = decodeEnvelopeData(
      await this.mutate('/entities', 'POST', input, { 'Idempotency-Key': key }),
      decodeGraphEntity,
    );
    return data as GraphEntity;
  }

  async updateResource(resource: Resource, input: UpdateResource, key: string): Promise<Resource> {
    const { data } = decodeEnvelopeData(
      await this.mutate(`/entities/${encodeURIComponent(resource.id)}`, 'PATCH', input, {
        'If-Match': resource.etag,
        'Idempotency-Key': key,
      }),
      decodeResource,
    );
    return data as Resource;
  }

  async saveSource(input: CaptureInput): Promise<SavedSource> {
    const response = await this.request('/captures', { method: 'POST', body: JSON.stringify(input) });
    return decodeSavedSource(response);
  }

  async listOpportunities(): Promise<Opportunity[]> {
    const value = object(await this.request('/opportunities'));
    if (!value || !Array.isArray(value.data)) throw new Error('Invalid response: opportunity list is malformed');
    return value.data.map((item) => {
      const row = object(item);
      if (!row || !Array.isArray(row.participants) || !Number.isSafeInteger(row.revision)) throw new Error('Invalid response: opportunity is malformed');
      return { id:requiredString(row.id,'opportunity.id'), title:requiredString(row.title,'opportunity.title'), status:requiredString(row.status,'opportunity.status') as Opportunity['status'], revision:Number(row.revision), participants:row.participants.map((participant) => { const p=object(participant); if(!p)throw new Error('Invalid response: opportunity participant is malformed'); return {id:requiredString(p.id,'participant.id'),kind:requiredString(p.kind,'participant.kind') as 'person'|'org',name:requiredString(p.name,'participant.name')}; }), created_at:requiredString(row.created_at,'opportunity.created_at'), updated_at:requiredString(row.updated_at,'opportunity.updated_at') };
    });
  }

  async createOpportunity(input:{title:string;participant_ids:string[]},key=newOperationKey()):Promise<Opportunity>{
    return this.decodeOpportunity(await this.mutate('/opportunities','POST',input,{'Idempotency-Key':key}));
  }

  async updateOpportunityParticipants(id:string,expectedRevision:number,participantIds:string[],key=newOperationKey()):Promise<Opportunity>{
    return this.decodeOpportunity(await this.mutate(`/opportunities/${encodeURIComponent(id)}/participants`,'PUT',{expected_revision:expectedRevision,participant_ids:participantIds},{'Idempotency-Key':key}));
  }

  async createInterest(id:string,input:{side:'me'|'them';statement:string},key=newOperationKey()):Promise<OpportunityInterest>{
    const value=object(await this.mutate(`/opportunities/${encodeURIComponent(id)}/interests`,'POST',input,{'Idempotency-Key':key}));
    if(!value||!Number.isSafeInteger(value.revision))throw new Error('Invalid response: interest is malformed');
    return {id:requiredString(value.id,'interest.id'),opportunity_id:requiredString(value.opportunity_id,'interest.opportunity_id'),side:requiredString(value.side,'interest.side') as 'me'|'them',statement:requiredString(value.statement,'interest.statement'),tier:3,status:requiredString(value.status,'interest.status') as 'active'|'dismissed',revision:Number(value.revision)};
  }

  async listOpportunityInterests(id:string,signal?:AbortSignal):Promise<OpportunityInterest[]>{
    const value=object(await this.request(`/opportunities/${encodeURIComponent(id)}/interests`,{signal}));if(!value||!Array.isArray(value.data))throw new Error('Invalid response: opportunity interests are malformed');return value.data.map((item)=>{const row=object(item);if(!row||!Number.isSafeInteger(row.revision))throw new Error('Invalid response: interest is malformed');return{id:requiredString(row.id,'interest.id'),opportunity_id:requiredString(row.opportunity_id,'interest.opportunity_id'),side:requiredString(row.side,'interest.side') as 'me'|'them',statement:requiredString(row.statement,'interest.statement'),tier:3,status:requiredString(row.status,'interest.status') as 'active'|'dismissed',revision:Number(row.revision)};});
  }

  async updateExchange(id:string,input:{expected_revision:number;they_get:string;i_get:string;interest_ids:string[];fairness_attested:boolean},key=newOperationKey()):Promise<OpportunityExchange>{
    const value=object(await this.mutate(`/opportunities/${encodeURIComponent(id)}/exchange`,'PUT',input,{'Idempotency-Key':key}));
    if(!value||!Number.isSafeInteger(value.version)||!Array.isArray(value.interest_ids))throw new Error('Invalid response: exchange is malformed');
    return {id:requiredString(value.id,'exchange.id'),opportunity_id:requiredString(value.opportunity_id,'exchange.opportunity_id'),version:Number(value.version),they_get:requiredString(value.they_get,'they_get'),i_get:requiredString(value.i_get,'i_get'),interest_ids:value.interest_ids.map(x=>requiredString(x,'interest_id')),fairness_attested:value.fairness_attested===true,attested_at:typeof value.attested_at==='string'?value.attested_at:null};
  }

  async getOpportunityExchange(id:string,signal?:AbortSignal):Promise<OpportunityExchange|null>{
    const value=await this.request(`/opportunities/${encodeURIComponent(id)}/exchange`,{signal});if(value===null)return null;const row=object(value);if(!row||!Number.isSafeInteger(row.version)||!Array.isArray(row.interest_ids))throw new Error('Invalid response: exchange is malformed');return{id:requiredString(row.id,'exchange.id'),opportunity_id:requiredString(row.opportunity_id,'exchange.opportunity_id'),version:Number(row.version),they_get:requiredString(row.they_get,'they_get'),i_get:requiredString(row.i_get,'i_get'),interest_ids:row.interest_ids.map((x)=>requiredString(x,'interest_id')),fairness_attested:row.fairness_attested===true,attested_at:typeof row.attested_at==='string'?row.attested_at:null};
  }

  async createMaterial(opportunityId:string,input:{recipient_id:string;kind:'outreach'|'follow_up'|'handoff';title:string},key=newOperationKey()):Promise<Material>{
    return this.decodeMaterial(await this.mutate(`/opportunities/${encodeURIComponent(opportunityId)}/materials`,'POST',input,{'Idempotency-Key':key}));
  }

  async listOpportunityMaterials(opportunityId:string,signal?:AbortSignal):Promise<Material[]>{const value=object(await this.request(`/opportunities/${encodeURIComponent(opportunityId)}/materials`,{signal}));if(!value||!Array.isArray(value.data))throw new Error('Invalid response: material list is malformed');return value.data.map((item)=>this.decodeMaterial(item));}

  async getBoard(signal?:AbortSignal):Promise<BoardSnapshotDTO>{return this.decodeBoard(await this.request('/board',{signal}));}
  async createBoardList(input:{name:string;expected_revision:number},key=newOperationKey()):Promise<BoardSnapshotDTO>{return this.decodeBoard(await this.mutate('/board/lists','POST',input,{'Idempotency-Key':key}));}
  async renameBoardList(id:string,input:{name:string;expected_revision:number},key=newOperationKey()):Promise<BoardSnapshotDTO>{return this.decodeBoard(await this.mutate(`/board/lists/${encodeURIComponent(id)}`,'PATCH',input,{'Idempotency-Key':key}));}
  async reorderBoardLists(input:{ordered_ids:string[];expected_revision:number},key=newOperationKey()):Promise<BoardSnapshotDTO>{return this.decodeBoard(await this.mutate('/board/lists/order','PUT',input,{'Idempotency-Key':key}));}
  async archiveBoardList(id:string,input:{destination_list_id:string;expected_revision:number},key=newOperationKey()):Promise<BoardSnapshotDTO>{return this.decodeBoard(await this.mutate(`/board/lists/${encodeURIComponent(id)}/archive`,'POST',input,{'Idempotency-Key':key}));}
  async moveBoardCard(id:string,input:{destination_list_id:string;anchor_opportunity_id:string|null;expected_board_revision:number;expected_source_revision:number;expected_destination_revision:number},key=newOperationKey()):Promise<BoardSnapshotDTO>{return this.decodeBoard(await this.mutate(`/board/cards/${encodeURIComponent(id)}/move`,'POST',input,{'Idempotency-Key':key}));}
  async reorderBoardCard(listId:string,input:{opportunity_id:string;anchor_opportunity_id:string|null;expected_revision:number},key=newOperationKey()):Promise<BoardSnapshotDTO>{return this.decodeBoard(await this.mutate(`/board/lists/${encodeURIComponent(listId)}/cards/reorder`,'POST',input,{'Idempotency-Key':key}));}
  async getOpportunityNotes(id:string,signal?:AbortSignal):Promise<OpportunityNotesDTO>{return this.decodeNotes(await this.request(`/opportunities/${encodeURIComponent(id)}/notes`,{signal}));}
  async saveOpportunityNotes(id:string,input:{expected_revision:number;markdown:string},key=newOperationKey()):Promise<OpportunityNotesDTO>{return this.decodeNotes(await this.mutate(`/opportunities/${encodeURIComponent(id)}/notes`,'PUT',input,{'Idempotency-Key':key}));}
  async toggleOpportunityTask(id:string,input:{expected_revision:number;start_byte:number;end_byte:number;line_hash:string;checked:boolean},key=newOperationKey()):Promise<OpportunityNotesDTO>{return this.decodeNotes(await this.mutate(`/opportunities/${encodeURIComponent(id)}/notes/tasks`,'PATCH',input,{'Idempotency-Key':key}));}

  async getMaterial(id:string,signal?:AbortSignal):Promise<Material>{return this.decodeMaterial(await this.request(`/materials/${encodeURIComponent(id)}`,{signal}));}

  async saveMaterialVersion(id:string,input:{expected_revision:number;body:string;sentences:MaterialSentence[];slot_fills:Record<string,string>;protected_tokens:string[]},key=newOperationKey()):Promise<Material>{
    return this.decodeMaterial(await this.mutate(`/materials/${encodeURIComponent(id)}/versions`,'PUT',input,{'Idempotency-Key':key}));
  }

  async getMaterialGate(id:string,versionId:string,signal?:AbortSignal):Promise<MaterialGateDTO>{
    const value=object(await this.request(`/materials/${encodeURIComponent(id)}/gate?version_id=${encodeURIComponent(versionId)}`,{signal}));
    if(!value||!Array.isArray(value.results)||!Array.isArray(value.hard_failures)||!Array.isArray(value.soft_failures))throw new Error('Invalid response: material gate is malformed');
    return value as unknown as MaterialGateDTO;
  }

  async approveMaterialVersion(id:string,input:{expected_revision:number;version_id:string;override_reasons:Record<string,string>},key=newOperationKey()):Promise<MaterialApprovalDTO>{
    const value=object(await this.mutate(`/materials/${encodeURIComponent(id)}/approval`,'POST',input,{'Idempotency-Key':key}));
    if(!value||!object(value.gate_snapshot))throw new Error('Invalid response: approval receipt is malformed');
    return value as unknown as MaterialApprovalDTO;
  }

  async issueCopyGrant(id:string,input:{version_id:string;recipient_id:string},key=newOperationKey()):Promise<CopyGrantDTO>{
    const value=object(await this.mutate(`/materials/${encodeURIComponent(id)}/copy-grant`,'POST',input,{'Idempotency-Key':key}));
    if(!value||typeof value.body!=='string')throw new Error('Invalid response: copy grant is malformed');
    return value as unknown as CopyGrantDTO;
  }

  async acknowledgeCopy(grantId:string,success:boolean,key:string):Promise<CopyGrantReceiptDTO>{
    const value=object(await this.mutate(`/copy-grants/${encodeURIComponent(grantId)}/acknowledgement`,'POST',{success},{'Idempotency-Key':key}));
    if(!value||typeof value.digest!=='string')throw new Error('Invalid response: copy acknowledgement is malformed');
    return value as unknown as CopyGrantReceiptDTO;
  }

  async getCopyGrantReceipt(grantId:string,signal?:AbortSignal):Promise<CopyGrantReceiptDTO>{
    const value=object(await this.request(`/copy-grants/${encodeURIComponent(grantId)}`,{signal}));
    if(!value||typeof value.digest!=='string')throw new Error('Invalid response: copy grant receipt is malformed');
    return value as unknown as CopyGrantReceiptDTO;
  }

  async stageCapture(input:{text:string;title:string|null;url:string|null;opportunity_ids:string[];actor_kind:'owner'|'party'|'unknown';actor_entity_id:string|null},key=newOperationKey()):Promise<CaptureReviewDTO>{
    const value=object(await this.mutate('/capture-stages','POST',input,{'Idempotency-Key':key}));
    if(!value||typeof value.batch_id!=='string')throw new Error('Invalid response: capture stage is malformed');
    return this.getCaptureReview(value.batch_id);
  }

  async getCaptureReview(batchId:string,signal?:AbortSignal):Promise<CaptureReviewDTO>{
    const value=object(await this.request(`/capture-stages/${encodeURIComponent(batchId)}`,{signal}));
    if(!value||!Array.isArray(value.proposals)||!object(value.source)||typeof value.injection_suspected!=='boolean')throw new Error('Invalid response: capture review is malformed');
    return value as unknown as CaptureReviewDTO;
  }

  async cancelCaptureJob(batchId:string,jobId:string,key=newOperationKey()):Promise<CaptureReviewDTO>{
    const value=object(await this.mutate(`/capture-stages/${encodeURIComponent(batchId)}/cancel`,'POST',{job_id:jobId},{'Idempotency-Key':key}));
    if(!value||!Array.isArray(value.proposals))throw new Error('Invalid response: capture cancellation is malformed');
    return value as unknown as CaptureReviewDTO;
  }

  async applyCaptureReview(batchId:string,expectedRevision:number,decisions:Array<{proposal_id:string;action:'confirm'|'correct'|'dismiss'|'unresolved';target_id:string|null;corrected_statement:string|null}>,key:string):Promise<CaptureReviewReceiptDTO>{
    const value=object(await this.mutate(`/capture-stages/${encodeURIComponent(batchId)}/decisions`,'POST',{expected_revision:expectedRevision,decisions},{'Idempotency-Key':key}));
    if(!value||!Array.isArray(value.committed)||!object(value.source))throw new Error('Invalid response: capture decision receipt is malformed');
    return value as unknown as CaptureReviewReceiptDTO;
  }

  async saveCaptureSourceOnly(batchId:string,expectedRevision:number,key:string):Promise<CaptureReviewReceiptDTO>{
    const value=object(await this.mutate(`/capture-stages/${encodeURIComponent(batchId)}/source-only`,'POST',{expected_revision:expectedRevision},{'Idempotency-Key':key}));
    if(!value||!Array.isArray(value.committed)||!object(value.source))throw new Error('Invalid response: source-only receipt is malformed');
    return value as unknown as CaptureReviewReceiptDTO;
  }

  private decodeOpportunity(value:unknown):Opportunity{
    const row=object(value);if(!row||!Array.isArray(row.participants)||!Number.isSafeInteger(row.revision))throw new Error('Invalid response: opportunity is malformed');
    return {id:requiredString(row.id,'opportunity.id'),title:requiredString(row.title,'opportunity.title'),status:requiredString(row.status,'opportunity.status') as Opportunity['status'],revision:Number(row.revision),participants:row.participants.map((participant)=>{const p=object(participant);if(!p)throw new Error('Invalid response: participant is malformed');return{id:requiredString(p.id,'participant.id'),kind:requiredString(p.kind,'participant.kind') as 'person'|'org',name:requiredString(p.name,'participant.name')};}),created_at:requiredString(row.created_at,'opportunity.created_at'),updated_at:requiredString(row.updated_at,'opportunity.updated_at')};
  }

  private decodeBoard(value:unknown):BoardSnapshotDTO {
    const row=object(value);
    if(!row||!Array.isArray(row.items)||!Array.isArray(row.lists)||!Number.isSafeInteger(row.board_revision))throw new Error('Invalid response: board snapshot is malformed');
    const lists=row.lists.map((value)=>{const list=object(value);if(!list||!Number.isSafeInteger(list.revision)||!Number.isSafeInteger(list.position)||typeof list.archived!=='boolean')throw new Error('Invalid response: board list is malformed');return{id:requiredString(list.id,'list.id'),name:requiredString(list.name,'list.name'),position:Number(list.position),revision:Number(list.revision),archived:list.archived};});
    const items=row.items.map((value)=>{const item=object(value);if(!item||!Array.isArray(item.participants)||!Array.isArray(item.tags)||!Number.isSafeInteger(item.revision)||!Number.isSafeInteger(item.position))throw new Error('Invalid response: board item is malformed');return{id:requiredString(item.id,'item.id'),revision:Number(item.revision),list_id:requiredString(item.list_id,'item.list_id'),position:Number(item.position),title:requiredString(item.title,'item.title'),status:requiredString(item.status,'item.status'),participants:item.participants.map((value)=>{const p=object(value);if(!p)throw new Error('Invalid response: board participant is malformed');return{id:requiredString(p.id,'participant.id'),kind:requiredString(p.kind,'participant.kind') as 'person'|'org',name:requiredString(p.name,'participant.name')};}),tags:item.tags.map((tag)=>requiredString(tag,'tag')),next_task:typeof item.next_task==='string'?item.next_task:null,deadline:typeof item.deadline==='string'?item.deadline:null,missing_material:item.missing_material===true};});
    return{items,lists,board_revision:Number(row.board_revision)};
  }

  private decodeNotes(value:unknown):OpportunityNotesDTO {const row=object(value);if(!row||!Number.isSafeInteger(row.revision))throw new Error('Invalid response: opportunity notes are malformed');return{opportunity_id:requiredString(row.opportunity_id,'notes.opportunity_id'),markdown:requiredString(row.markdown,'notes.markdown'),revision:Number(row.revision)};}

  private decodeMaterial(value:unknown):Material{
    const row=object(value);const version=row?.current_version===null?null:row?object(row.current_version):null;if(!row||(!version&&row.current_version!==null)||!Array.isArray(row.versions)||!Array.isArray(row.claim_references))throw new Error('Invalid response: material is malformed');
    const decodeVersion=(v:unknown):MaterialVersion=>{const item=object(v);if(!item||!Array.isArray(item.sentences)||!Array.isArray(item.protected_tokens)||!object(item.slot_fills))throw new Error('Invalid response: material version is malformed');return item as unknown as MaterialVersion;};
    return {...row,current_version:version?decodeVersion(version):null,versions:row.versions.map(decodeVersion)} as unknown as Material;
  }

  async listSources(): Promise<SavedSource[]> {
    return decodeSavedSourceList(await this.request('/captures'));
  }

  private async mutate(path: string, method: string, body: unknown, headers: Record<string, string>): Promise<unknown> {
    return this.request(path, {
      method,
      body: JSON.stringify(body),
      headers,
    });
  }

  private async request(path: string, init: RequestInit = {}, authenticated = true): Promise<unknown> {
    const requestEpoch=authenticated?this.session?.epoch:undefined;
    const headers = new Headers(init.headers);
    headers.set('Accept', 'application/json');
    headers.set('Content-Type', 'application/json');
    if (authenticated && this.session) {
      headers.set('X-Parley-Token', this.session.mutation_token);
      headers.set('X-Parley-Epoch', this.session.epoch);
    }
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
      throw new ApiError('The local service returned an unreadable response.', response.status, 'INVALID_RESPONSE');
    }
    const responseEpoch=response.headers.get('x-parley-epoch');
    if(responseEpoch&&requestEpoch&&responseEpoch!==requestEpoch)throw new ApiError('The workspace changed while this read was in progress.',409,'WORKSPACE_CHANGED');
    if (!response.ok) throw decodeApiError(payload, response.status);
    return payload;
  }
}

export const parley = new ParleyClient();
