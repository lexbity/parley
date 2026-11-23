import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type InsightsReport } from '../../api/client';
import Insights from './Insights.vue';

afterEach(() => vi.restoreAllMocks());

const report: InsightsReport = {
  as_of: '2026-11-21T12:00:00Z', period_start: '2026-11-16', period_end: '2026-11-22', pack_id: 'job_search', pack_version: '1.0.0', sample_size: 0,
  metrics: [{ id: 'reply', label: 'Reply rate', numerator: 0, denominator: 0, rate: null, lower: null, upper: null, state: 'insufficient_observations' }],
  comparisons: [], gaps: [], questions: [],
};

describe('Insights S10', () => {
  it('shows explicit insufficient-data state and does not run Coach on reads', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'getInsights').mockResolvedValue(report);
    const runCoach = vi.spyOn(parley, 'runCoach');
    const wrapper = mount(Insights);
    await flushPromises();
    expect(wrapper.text()).toContain('0/0');
    expect(wrapper.text()).toContain('Insufficient observations');
    expect(parley.getInsights).toHaveBeenCalledTimes(1);
    expect(runCoach).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('starts Coach only after the owner requests it and refreshes the report on success', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'getInsights').mockResolvedValue(report);
    vi.spyOn(parley, 'runCoach').mockResolvedValue({ job_id: 'coach-job' });
    vi.spyOn(parley, 'getRoleJob').mockResolvedValue({ id: 'coach-job', role: 'coach', status: 'succeeded', result: {}, last_error: null });
    const wrapper = mount(Insights);
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Run Coach for this week')!.trigger('click');
    await flushPromises();
    expect(parley.runCoach).toHaveBeenCalledTimes(1);
    expect(parley.getRoleJob).toHaveBeenCalledWith('coach-job');
    expect(parley.getInsights).toHaveBeenCalledTimes(2);
    expect(wrapper.text()).toContain('Coach report refreshed from the committed run.');
    wrapper.unmount();
  });

  it('renders comparisons, gaps and questions with measured intervals and dispatches owner actions', async () => {
    const detailed: InsightsReport = {
      ...report,
      sample_size: 8,
      metrics: [{ id: 'reply', label: 'Reply rate', numerator: 4, denominator: 8, rate: 0.5, lower: 0.2, upper: 0.8, state: 'measured' }],
      comparisons: [{ label: 'Gift vs proof', state: 'distinguishable', left: report.metrics[0]!, right: report.metrics[0]! }],
      gaps: [{ id: 'proof', label: 'Proof coverage', covered: 2, total: 6, blocked_count: 1, suggested_kind: 'proof' }],
      questions: [{ id: 'q1', kind: 'proof', text: 'What changed?', target_kind: 'opportunity', target_id: 'o1', impact: 2, state: 'open' }],
    };
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'getInsights').mockResolvedValue(detailed);
    vi.spyOn(parley, 'decideCoachQuestion').mockResolvedValue();
    const wrapper = mount(Insights);
    await flushPromises();
    expect(wrapper.text()).toContain('95% Wilson interval 20.0–80.0%');
    expect(wrapper.text()).toContain('Gift vs proof');
    expect(wrapper.text()).toContain('Proof coverage · 2/6 covered');
    await wrapper.findAll('button').find((button) => button.text() === 'Open related work')!.trigger('click');
    expect(wrapper.emitted('navigate')?.[0]).toEqual(['/queue/o1']);
    await wrapper.findAll('button').find((button) => button.text() === 'Dismiss question')!.trigger('click');
    await flushPromises();
    expect(parley.decideCoachQuestion).toHaveBeenCalledWith('q1');
    expect(wrapper.text()).toContain('Question dismissed');
    wrapper.unmount();
  });

  it('reports a failed Coach job and lets the owner stop waiting on a queued one', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'getInsights').mockResolvedValue(report);
    vi.spyOn(parley, 'runCoach').mockResolvedValue({ job_id: 'job-fail' });
    vi.spyOn(parley, 'getRoleJob').mockResolvedValueOnce({ id: 'job-fail', role: 'coach', status: 'failed', result: {}, last_error: 'provider unavailable' });
    const wrapper = mount(Insights);
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Run Coach for this week')!.trigger('click');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('provider unavailable');

    vi.spyOn(parley, 'runCoach').mockResolvedValue({ job_id: 'job-queued' });
    vi.spyOn(parley, 'getRoleJob').mockResolvedValue({ id: 'job-queued', role: 'coach', status: 'queued', result: {}, last_error: null });
    await wrapper.findAll('button').find((button) => button.text() === 'Run Coach for this week')!.trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('Coach is running as a durable job');
    await wrapper.findAll('button').find((button) => button.text() === 'Stop waiting')!.trigger('click');
    expect(wrapper.text()).toContain('Stopped waiting for the Coach result');
    wrapper.unmount();
  });

  it('surfaces an insight read failure without leaving a stale report', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'getInsights').mockRejectedValue(new Error('insights unavailable'));
    const wrapper = mount(Insights);
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('insights unavailable');
    expect(wrapper.text()).not.toContain('Week 2026-11-16');
    wrapper.unmount();
  });
});
