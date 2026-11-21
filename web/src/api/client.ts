import type { components } from './generated';

export type Resource = components['schemas']['Resource'];
export type GraphEntity = components['schemas']['GraphEntity'];
export type Entity = components['schemas']['Entity'];
export type SavedSource = components['schemas']['SavedSource'];
export type EntityInput = components['schemas']['EntityInput'];
export type CaptureInput = components['schemas']['CaptureInput'];
export type Opportunity = components['schemas']['Opportunity'];
export type Material = components['schemas']['MaterialRecord'];
export type ReleasedMaterial = components['schemas']['ReleasedMaterial'];
export type DirectoryEntity = { id:string; kind:'person'|'org'|'project'; name:string; descriptor:string; aliases:string[]; tags:string[]; revision:number };
export type DirectoryPage = { items:DirectoryEntity[]; total:number; page:number; page_size:number; has_more:boolean };
export type GraphEdge = { id:string; from_id:string; to_id:string; verb:string; revision:number; claim_id:string; claim_revision:number; wording:string; approved_wording:string|null; tier:number; speaker_kind:string; speaker_entity_id:string|null; occurrence_id:string|null; source_start:number|null; source_end:number|null; normalization_version:number|null; source_title:string|null; source_url:string|null };
export type Neighborhood = { focus_id:string; nodes:DirectoryEntity[]; edges:GraphEdge[]; positions:Array<{entity_id:string;x:number;y:number;pinned:boolean;revision:number}>; omitted_nodes:number; omitted_edges:number };
export type SourceDetail = SavedSource & { text:string; title:string|null; url:string|null; captured_at:string; actor_kind:string; actor_entity_id:string|null; normalization_version:number };
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

function stringArray(value: unknown, field: string): string[] {
  if (!Array.isArray(value) || !value.every((item) => typeof item === 'string')) {
    throw new Error(`Invalid response: ${field} must be a text list`);
  }
  return value;
}

function decodeOpportunity(value: unknown): Opportunity {
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.revision) || Number(item.revision) < 1) {
    throw new Error('Invalid response: opportunity is malformed');
  }
  return {
    id: requiredString(item.id, 'opportunity.id'),
    title: requiredString(item.title, 'opportunity.title'),
    state: requiredString(item.state, 'opportunity.state'),
    revision: Number(item.revision),
    participant_ids: stringArray(item.participant_ids, 'opportunity.participant_ids'),
    source_variant_ids: stringArray(item.source_variant_ids, 'opportunity.source_variant_ids'),
    material_ids: stringArray(item.material_ids, 'opportunity.material_ids'),
  };
}

function decodeMaterial(value: unknown): Material {
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.version) || Number(item.version) < 1
    || !/^[a-f0-9]{64}$/.test(String(item.content_sha256))
    || !['draft', 'approved', 'released'].includes(String(item.status))
    || !(item.gate === null || object(item.gate))) {
    throw new Error('Invalid response: material is malformed');
  }
  return {
    id: requiredString(item.id, 'material.id'),
    opportunity_id: requiredString(item.opportunity_id, 'material.opportunity_id'),
    recipient_id: requiredString(item.recipient_id, 'material.recipient_id'),
    recipient_name: requiredString(item.recipient_name, 'material.recipient_name'),
    kind: requiredString(item.kind, 'material.kind'),
    version: Number(item.version),
    body: requiredString(item.body, 'material.body'),
    content_sha256: item.content_sha256 as string,
    status: item.status as Material['status'],
    gate: item.gate as Material['gate'],
  };
}

function decodeReleasedMaterial(value: unknown): ReleasedMaterial {
  const item = object(value);
  if (!item || !Number.isSafeInteger(item.version) || Number(item.version) < 1
    || !/^[a-f0-9]{64}$/.test(String(item.content_sha256))) {
    throw new Error('Invalid response: release is malformed');
  }
  return {
    release_id: requiredString(item.release_id, 'release.release_id'),
    material_id: requiredString(item.material_id, 'release.material_id'),
    version: Number(item.version),
    recipient_id: requiredString(item.recipient_id, 'release.recipient_id'),
    body: requiredString(item.body, 'release.body'),
    content_sha256: item.content_sha256 as string,
    issued_at: requiredString(item.issued_at, 'release.issued_at'),
  };
}

function decodeArray<T>(value: unknown, field: string, decode: (item: unknown) => T): T[] {
  if (!Array.isArray(value)) throw new Error(`Invalid response: ${field} must be a list`);
  return value.map(decode);
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

  async listSources(): Promise<SavedSource[]> {
    return decodeSavedSourceList(await this.request('/captures'));
  }

  async createOpportunity(title: string, primary_party_id: string): Promise<Opportunity> {
    return decodeOpportunity(await this.request('/opportunities', { method: 'POST', body: JSON.stringify({ title, primary_party_id }) }));
  }

  async listOpportunities(): Promise<Opportunity[]> {
    return decodeArray(await this.request('/opportunities'), 'opportunities', decodeOpportunity);
  }

  async listOutputs(): Promise<Material[]> {
    return decodeArray(await this.request('/outputs'), 'outputs', decodeMaterial);
  }

  async releaseMaterial(id: string): Promise<ReleasedMaterial> {
    return decodeReleasedMaterial(await this.request(`/materials/${encodeURIComponent(id)}/release`, { method: 'POST', body: '{}' }));
  }

  async acknowledgeRelease(id: string, succeeded: boolean): Promise<void> {
    await this.request(`/releases/${encodeURIComponent(id)}/ack`, { method: 'POST', body: JSON.stringify({ succeeded }) });
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
