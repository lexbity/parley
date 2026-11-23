import { afterEach, describe, expect, it, vi } from 'vitest';
import type { CommandClient, CommandContext, CommandManifest } from '../../api/commands';
import { CommandManifestSource } from './manifest';

const manifest: CommandManifest = {
  contract_major: 1, manifest_version: 'v1', truncated: false,
  commands: [{ id: 'opportunity.set_phase', version: 1, title: 'Change opportunity phase', summary: '', description: '', keywords: [], effect: 'write', requires_permission: '', surfaces: ['palette'], args_schema: {}, result_schema: {}, supports_dry_run: false, palette: { context_fields: [], prompts: [] } }],
};

afterEach(() => { vi.useRealTimers(); });

function source(context: CommandContext, epoch = 'epoch-1') {
  let current = { context, epoch };
  const client = { query: vi.fn().mockResolvedValue(manifest) } as unknown as CommandClient;
  const instance = new CommandManifestSource({
    client,
    context: () => current.context,
    epoch: () => current.epoch,
    debounceMs: 120,
  });
  return {
    instance, client,
    set(nextContext: CommandContext, nextEpoch = current.epoch) { current = { context: nextContext, epoch: nextEpoch }; },
  };
}

describe('CommandManifestSource', () => {
  it('caches a manifest and reports freshness', async () => {
    const { instance } = source({ route: 'queue' });
    expect(instance.cached()).toBeNull();
    expect(await instance.refresh()).toBe(manifest);
    expect(instance.cached()).toBe(manifest);
    expect(instance.isFresh()).toBe(true);
  });

  it('debounces invalidations by 120 ms and coalesces bursts', async () => {
    vi.useFakeTimers();
    const { instance, client } = source({ route: 'resources' });
    instance.invalidate();
    instance.invalidate();
    instance.invalidate();
    await vi.advanceTimersByTimeAsync(119);
    expect(client.query).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(client.query).toHaveBeenCalledOnce();
    instance.stop();
  });

  it('discards an in-flight response after a route or epoch change', async () => {
    let resolve!: (value: CommandManifest) => void;
    const client = { query: vi.fn().mockImplementation(() => new Promise<CommandManifest>((done) => { resolve = done; })) } as unknown as CommandClient;
    let context: CommandContext = { route: 'resources' };
    let epoch = 'epoch-1';
    const instance = new CommandManifestSource({ client, context: () => context, epoch: () => epoch });
    const pending = instance.refresh();
    expect(instance.loading).toBe(true);
    context = { route: 'queue' };
    epoch = 'epoch-2';
    resolve(manifest);
    const result = await pending;
    expect(result).toBe(manifest);
    expect(instance.cached()).toBeNull();
    expect(instance.isFresh()).toBe(false);
    vi.mocked(client.query).mockResolvedValue(manifest);
    await instance.refresh();
    expect(instance.cached()).toBe(manifest);
    expect(instance.isFresh()).toBe(true);
  });

  it('revalidates before dispatch and falls back to cached display on failure', async () => {
    const { instance, client, set } = source({ route: 'queue' });
    await instance.refresh();
    set({ route: 'queue' }, 'epoch-2');
    client.query = vi.fn().mockRejectedValue(new Error('offline')) as unknown as CommandClient['query'];
    expect(await instance.revalidate()).toBe(manifest);
    const fresh = source({ route: 'queue' });
    fresh.client.query = vi.fn().mockRejectedValue(new Error('offline')) as unknown as CommandClient['query'];
    await expect(fresh.instance.revalidate()).rejects.toThrow(/unavailable/);
  });
});
