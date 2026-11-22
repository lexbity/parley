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
});
