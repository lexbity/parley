import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley } from '../../api/client';
import { SessionEventReader } from './events';

const page = (cursor: string, count = 1) => ({
  items: Array.from({ length: count }, (_, index) => ({
    cursor: `${cursor}-${index}`, event_id: `ev-${index}`, entity_type: 'opportunity', entity_id: 'o1',
    revision: index + 1, event_type: 'opportunity.phase_changed', occurred_at: '2026-11-23T00:00:00Z', payload: {},
  })),
  next_cursor: 'next', observed_cursor: cursor,
});

afterEach(() => { vi.restoreAllMocks(); });

describe('SessionEventReader', () => {
  it('polls once at a time from the session cursor and publishes new events', async () => {
    let resolve!: (value: ReturnType<typeof page>) => void;
    const getEvents = vi.spyOn(parley, 'getEvents').mockImplementation(() => new Promise((done) => { resolve = done; }));
    const onEvents = vi.fn();
    const reader = new SessionEventReader();
    reader.start({ epoch: () => 'epoch-1', onEvents });
    const first = reader.poll();
    const second = reader.poll();
    expect(getEvents).toHaveBeenCalledOnce();
    expect(reader.polling).toBe(true);
    resolve(page('4'));
    await Promise.all([first, second]);
    expect(onEvents).toHaveBeenCalledOnce();
    expect(reader.currentCursor).toBe('4');
    getEvents.mockResolvedValue(page('5'));
    await reader.poll();
    expect(getEvents).toHaveBeenCalledTimes(2);
    reader.stop();
  });

  it('suspends hidden polling and suspends callbacks after stop', async () => {
    const getEvents = vi.spyOn(parley, 'getEvents').mockResolvedValue(page('2'));
    const onEvents = vi.fn();
    const reader = new SessionEventReader();
    reader.start({ epoch: () => 'epoch-1', onEvents });
    vi.spyOn(globalThis.document, 'visibilityState', 'get').mockReturnValue('hidden');
    await reader.poll();
    expect(getEvents).not.toHaveBeenCalled();
    vi.spyOn(globalThis.document, 'visibilityState', 'get').mockReturnValue('visible');
    await reader.poll();
    expect(getEvents).toHaveBeenCalledOnce();
    reader.stop();
    await reader.poll();
    expect(getEvents).toHaveBeenCalledOnce();
  });

  it('rebases the cursor on epoch replacement and ignores stale responses', async () => {
    let resolve!: (value: ReturnType<typeof page>) => void;
    let epoch = 'epoch-1';
    vi.spyOn(parley, 'getEvents').mockImplementation(() => new Promise((done) => { resolve = done; }));
    vi.spyOn(parley, 'eventCursor', 'get').mockReturnValue('11');
    const onEvents = vi.fn();
    const reader = new SessionEventReader();
    reader.start({ epoch: () => epoch, onEvents });
    const pending = reader.poll();
    epoch = 'epoch-2';
    reader.rebase(epoch);
    expect(reader.currentCursor).toBe('11');
    resolve(page('99'));
    await pending;
    expect(onEvents).not.toHaveBeenCalled();
    reader.stop();
  });

  it('tolerates transport failures without publishing a partial view', async () => {
    vi.spyOn(parley, 'getEvents').mockRejectedValue(new Error('offline'));
    const getEvents = vi.mocked(parley.getEvents);
    const onEvents = vi.fn();
    const reader = new SessionEventReader();
    reader.start({ epoch: () => 'epoch-1', onEvents });
    await expect(reader.poll()).resolves.toBeUndefined();
    expect(onEvents).not.toHaveBeenCalled();
    reader.hint();
    await reader.poll();
    expect(getEvents).toHaveBeenCalled();
    expect(onEvents).not.toHaveBeenCalled();
    reader.stop();
  });
});
