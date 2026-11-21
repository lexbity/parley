export type ResourceDraft = { kind: 'gift' | 'proof' | 'artifact'; title: string; body: Record<string, unknown> };
export type Entity = { id: string; kind: string; name: string; revision: number };
export type Resource = { id: string; kind: string; title: string; readiness: string; revision: number };
export type SavedSource = { content_id: string; variant_id: string; occurrence_id: string; normalized_sha256: string; raw_sha256: string };

declare global { interface Window { __PARLEY_TOKEN__?: string } }

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const token = window.__PARLEY_TOKEN__;
  if (!token) throw new Error('The local workspace session is not available. Reopen Parley from its launcher.');
  const response = await fetch(path, {
    ...init,
    headers: { 'Content-Type': 'application/json', 'X-Parley-Token': token, ...(init.headers ?? {}) },
  });
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
