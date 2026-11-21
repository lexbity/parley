export type ResourceDraft = { kind: 'gift' | 'proof' | 'artifact'; title: string; body: Record<string, unknown> };
export type Entity = { id: string; kind: string; name: string; revision: number };
export type Resource = { id: string; kind: string; title: string; readiness: string; revision: number };
export type SavedSource = { content_id: string; variant_id: string; occurrence_id: string; normalized_sha256: string; raw_sha256: string };
export type Opportunity = { id: string; title: string; state: string; revision: number; participant_ids: string[]; source_variant_ids: string[]; material_ids: string[] };
export type Material = { id: string; opportunity_id: string; recipient_id: string; recipient_name: string; kind: string; version: number; body: string; content_sha256: string; status: string };
export type ReleasedMaterial = { release_id: string; material_id: string; version: number; recipient_id: string; body: string; content_sha256: string; issued_at: string };

declare global { interface Window { __PARLEY_TOKEN__?: string } }

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const token = window.__PARLEY_TOKEN__;
  if (!token) throw new Error('The local workspace session is not available. Reopen Parley from its launcher.');
  const response = await fetch(path, {
    ...init,
    headers: { 'Content-Type': 'application/json', 'X-Parley-Token': token, ...(init.headers ?? {}) },
  });
  if (response.status === 204) return undefined as T;
  const payload = await response.json() as T | { error?: { message?: string; code?: string } };
  if (!response.ok) {
    const error = payload as { error?: { message?: string; code?: string } };
    throw new Error(error.error?.message ?? error.error?.code ?? `Request failed (${response.status})`);
  }
  return payload as T;
}

export function createResource(input: ResourceDraft): Promise<Resource> {
  const key = crypto.randomUUID();
  return request('/api/v1/resources', { method: 'POST', headers: { 'Idempotency-Key': key }, body: JSON.stringify(input) });
}
export function listResources(): Promise<Resource[]> { return request('/api/v1/resources'); }
export function captureSource(text: string): Promise<SavedSource> {
  return request('/api/v1/captures', { method: 'POST', body: JSON.stringify({ text, title: null, url: null }) });
}
export function listSources(): Promise<SavedSource[]> { return request('/api/v1/captures'); }
export function listEntities(): Promise<Entity[]> { return request('/api/v1/entities'); }
export function createEntity(kind: string, name: string): Promise<Entity> {
  return request('/api/v1/entities', { method: 'POST', body: JSON.stringify({ kind, name }) });
}
export function createOpportunity(title: string, primary_party_id: string): Promise<Opportunity> {
  return request('/api/v1/opportunities', { method: 'POST', body: JSON.stringify({ title, primary_party_id }) });
}
export function listOpportunities(): Promise<Opportunity[]> { return request('/api/v1/opportunities'); }
export function listOutputs(): Promise<Material[]> { return request('/api/v1/outputs'); }
export async function releaseMaterial(id: string): Promise<ReleasedMaterial> {
  return request(`/api/v1/materials/${encodeURIComponent(id)}/release`, { method: 'POST', body: '{}' });
}
export async function acknowledgeRelease(id: string, succeeded: boolean): Promise<void> {
  await request(`/api/v1/releases/${encodeURIComponent(id)}/ack`, { method: 'POST', body: JSON.stringify({ succeeded }) });
}
