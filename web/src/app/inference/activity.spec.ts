import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parley } from '../../api/client';
import { observeRoleJob } from './activity';

describe('inference activity role-job reconciler (AR04)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('observes active jobs and reports the terminal state exactly once', async () => {
    const job = (status: string) => ({
      id: 'job-1', role: 'interviewer', status, result: null, last_error: null,
      review_ref: null,
    });
    const getRoleJob = vi.spyOn(parley, 'getRoleJob')
      .mockResolvedValueOnce(job('running'))
      .mockResolvedValueOnce(job('succeeded'));
    const updates: string[] = [];
    const terminals: string[] = [];
    const failed: unknown[] = [];
    const stop = observeRoleJob(
      'job-1',
      (next) => updates.push(next.status),
      (next) => terminals.push(next.status),
      (reason) => failed.push(reason),
    );
    await vi.advanceTimersByTimeAsync(0);
    expect(updates).toEqual(['running']);
    await vi.advanceTimersByTimeAsync(2_000);
    expect(updates).toEqual(['running', 'succeeded']);
    expect(terminals).toEqual(['succeeded']);
    expect(failed).toEqual([]);
    // After terminal, no further polling occurs.
    await vi.advanceTimersByTimeAsync(20_000);
    expect(getRoleJob).toHaveBeenCalledTimes(2);
    stop();
  });

  it('aborts on epoch change and never calls back after stop', async () => {
    const getRoleJob = vi.spyOn(parley, 'getRoleJob')
      .mockResolvedValue({ id: 'job-1', role: 'interviewer', status: 'running', result: null, last_error: null, review_ref: null });
    const updates: string[] = [];
    const stop = observeRoleJob('job-1', (next) => updates.push(next.status), () => {}, () => {});
    await vi.advanceTimersByTimeAsync(0);
    expect(updates).toEqual(['running']);
    stop();
    await vi.advanceTimersByTimeAsync(20_000);
    expect(getRoleJob).toHaveBeenCalledTimes(1);
  });

  it('keeps polling after a transient read failure until terminal', async () => {
    const failure = new Error('network');
    const getRoleJob = vi.spyOn(parley, 'getRoleJob')
      .mockRejectedValueOnce(failure)
      .mockResolvedValueOnce({ id: 'job-1', role: 'interviewer', status: 'failed', result: null, last_error: 'fail', review_ref: null });
    const terminals: string[] = [];
    const failed: unknown[] = [];
    const stop = observeRoleJob('job-1', () => {}, (next) => terminals.push(next.status), (reason) => failed.push(reason));
    await vi.advanceTimersByTimeAsync(0);
    expect(failed).toEqual([failure]);
    await vi.advanceTimersByTimeAsync(8_000);
    expect(terminals).toEqual(['failed']);
    expect(getRoleJob).toHaveBeenCalledTimes(2);
    stop();
  });
});
