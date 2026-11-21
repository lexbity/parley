import { afterEach, describe, expect, it, vi } from 'vitest';
import { captureSource, createEntity, createOpportunity, createResource, listEntities, listOpportunities, listOutputs, listResources, listSources } from './client';

const response = (payload: unknown, ok = true, status = 200) => ({ ok, status, json: async () => payload }) as Response;

describe('local API client', () => {
  afterEach(() => { vi.unstubAllGlobals(); });

  it('requires the launcher session before making a request', async () => {
    vi.stubGlobal('fetch', vi.fn());
    await expect(listEntities()).rejects.toThrow('Reopen Parley from its launcher');
    expect(fetch).not.toHaveBeenCalled();
  });

  it('sends the workspace token and typed resource, capture and directory requests', async () => {
    vi.stubGlobal('__PARLEY_TOKEN__', 'owner-token');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(response([])));
    vi.stubGlobal('crypto', { randomUUID: () => 'request-key' });
    await createResource({ kind: 'gift', title: 'Offer', body: {} });
    await listResources();
    await captureSource('Captured text');
    await listSources();
    await listEntities();
    await createEntity('person', 'Dana');
    await createOpportunity('Onboarding', 'party-1');
    await listOpportunities(); await listOutputs();
    expect(fetch).toHaveBeenCalledTimes(9);
    const [url, init] = vi.mocked(fetch).mock.calls[0]!;
    expect(url).toBe('/api/v1/resources');
    expect(init?.headers).toMatchObject({ 'X-Parley-Token': 'owner-token', 'Idempotency-Key': 'request-key' });
  });

  it('binds an opportunity to an explicit participant ID', async () => {
    vi.stubGlobal('__PARLEY_TOKEN__', 'owner-token');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(response({ id: 'opp-1' })));
    await createOpportunity('Onboarding', 'party-7');
    expect(fetch).toHaveBeenCalledWith('/api/v1/opportunities', expect.objectContaining({
      method: 'POST',
      body: JSON.stringify({ title: 'Onboarding', primary_party_id: 'party-7' }),
    }));
  });

  it('uses the API error message, code, and status fallback in that order', async () => {
    vi.stubGlobal('__PARLEY_TOKEN__', 'owner-token');
    vi.stubGlobal('fetch', vi.fn()
      .mockResolvedValueOnce(response({ error: { message: 'stale revision' } }, false, 409))
      .mockResolvedValueOnce(response({ error: { code: 'blocked' } }, false, 422))
      .mockResolvedValueOnce(response({}, false, 503)));
    await expect(listEntities()).rejects.toThrow('stale revision');
    await expect(listEntities()).rejects.toThrow('blocked');
    await expect(listEntities()).rejects.toThrow('Request failed (503)');
  });
});
