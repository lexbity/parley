import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError, parley, type CoachJob, type CoachSnapshot, type InsightsReport } from '../../api/client';
import Insights from './Insights.vue';

const report: InsightsReport = {
  as_of: '2026-11-21T12:00:00Z', event_cursor: '0', effective_outcomes: [], strata: [], comparisons: [],
  rates: [{ metric: 'reply_rate', k: 0, n: 0, interval_95: null }],
};
const snapshot: CoachSnapshot = {
  period: '2026-W47', as_of: report.as_of, coverage: { current: 1, missing: 2, stale: 3, no_match: 1 },
  report: null, input_digest: null, active_job_id: null, latest_job_id: null,
  observations: [{ reason: 'missing_identity', target: { scope: 'owner' }, impact: 1 }],
};
const job: CoachJob = {
  id: 'coach-job', role: 'coach', status: 'running', state_revision: 1, attempt_count: 1,
  scope: { kind: 'weekly', period: '2026-W47' }, progress: null, error_code: null, review_ref: null,
};
const start = 'Run Coach for this week';

beforeEach(() => {
  vi.spyOn(parley, 'connect').mockResolvedValue();
  vi.spyOn(parley, 'getInsights').mockResolvedValue(report);
  vi.spyOn(parley, 'getWeeklyCoach').mockResolvedValue(snapshot);
  vi.spyOn(parley, 'listCoachQuestions').mockResolvedValue([]);
});
afterEach(() => vi.restoreAllMocks());

describe('Insights S10 and AR01', () => {
  it('shows explicit insufficient-data and assessment states without starting Coach', async () => {
    const startWeeklyCoach = vi.spyOn(parley, 'startWeeklyCoach');
    const wrapper = mount(Insights);
    await flushPromises();
    expect(wrapper.text()).toContain('0/0');
    expect(wrapper.text()).toContain('Insufficient observations');
    expect(wrapper.text()).toContain('Missing assessments');
    expect(wrapper.text()).toContain('Calibrated no-match');
    expect(wrapper.text()).toContain('Add a professional identity tag');
    expect(startWeeklyCoach).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('starts Coach only after the owner requests it and refreshes the report on success', async () => {
    vi.spyOn(parley, 'startWeeklyCoach').mockResolvedValue({ kind: 'queued', jobId: job.id });
    const getJob = vi.spyOn(parley, 'getCoachJob').mockResolvedValue({
      ...job, status: 'succeeded', review_ref: { kind: 'coach_report', id: 'report' },
    });
    const wrapper = mount(Insights);
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === start)!.trigger('click');
    await flushPromises();
    expect(parley.startWeeklyCoach).toHaveBeenCalledTimes(1);
    expect(getJob).toHaveBeenCalledWith(job.id, expect.any(AbortSignal));
    expect(parley.getInsights).toHaveBeenCalledTimes(2);
    expect(wrapper.text()).toContain('Coach questions for 2026-W47 are ready for review.');
    wrapper.unmount();
  });

  it('renders measured intervals, reauthorizes navigation, and permits dismissal', async () => {
    vi.spyOn(parley, 'getInsights').mockResolvedValue({
      ...report, rates: [{ metric: 'reply_rate', k: 4, n: 8, interval_95: { lower_95: 0.2, upper_95: 0.8 } }],
    });
    vi.spyOn(parley, 'listCoachQuestions').mockResolvedValue([
      { id: 'q1', question_key: 'a'.repeat(64), revision: 1, kind: 'missing_proof', text: 'What changed?', target: { opportunity_id: 'o1' }, selector: { kind: 'weekly', period: '2026-W47' }, impact: 2, priority: 0, status: 'open', action: 'interviewer', deferred_until: null, dependency_digest: null, suggested_resource_ref: null },
    ]);
    const canOpen = vi.spyOn(parley, 'getOpportunity').mockResolvedValue({} as Awaited<ReturnType<typeof parley.getOpportunity>>);
    const dismiss = vi.spyOn(parley, 'dismissCoachQuestion').mockResolvedValue();
    const wrapper = mount(Insights);
    await flushPromises();
    expect(wrapper.text()).toContain('95% Wilson interval 20.0%–80.0%');
    await wrapper.findAll('button').find((button) => button.text() === 'Open related work')!.trigger('click');
    await flushPromises();
    expect(canOpen).toHaveBeenCalledWith('o1');
    expect(wrapper.emitted('navigate')?.[0]).toEqual(['/queue/o1']);
    await wrapper.findAll('button').find((button) => button.text() === 'Dismiss question')!.trigger('click');
    await flushPromises();
    expect(dismiss).toHaveBeenCalledWith(expect.objectContaining({ id: 'q1', revision: 1 }));
    expect(wrapper.text()).toContain('Question dismissed');
    wrapper.unmount();
  });

  it('recovers a current job and cancels through the authenticated API', async () => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'visible' });
    vi.spyOn(parley, 'getWeeklyCoach').mockResolvedValue({ ...snapshot, active_job_id: job.id, latest_job_id: job.id });
    const getJob = vi.spyOn(parley, 'getCoachJob').mockResolvedValue(job);
    const cancel = vi.spyOn(parley, 'cancelJob').mockImplementation(async () => {
      getJob.mockResolvedValue({ ...job, status: 'cancelled', state_revision: 2, error_code: 'CANCELLED' });
    });
    const wrapper = mount(Insights);
    await flushPromises();
    expect(wrapper.text()).toContain('Coach is wording questions');
    await wrapper.findAll('button').find((button) => button.text() === 'Cancel Coach job')!.trigger('click');
    await flushPromises();
    expect(cancel).toHaveBeenCalledWith(job.id, expect.any(String));
    expect(wrapper.text()).toContain('Coach job for 2026-W47 cancelled.');
    wrapper.unmount();
  });

  it('keeps the deterministic report visible when the pairing is unavailable', async () => {
    vi.spyOn(parley, 'startWeeklyCoach').mockRejectedValue(new ApiError('unavailable', 503, 'CAPABILITY_UNQUALIFIED'));
    const wrapper = mount(Insights);
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === start)!.trigger('click');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('qualified local model');
    expect(wrapper.text()).toContain('data as of');
    wrapper.unmount();
  });

  it('surfaces an insight read failure without fabricating a report', async () => {
    vi.spyOn(parley, 'getInsights').mockRejectedValue(new Error('insights unavailable'));
    const wrapper = mount(Insights);
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('Could not load server-computed insights');
    expect(wrapper.text()).not.toContain('data as of');
    wrapper.unmount();
  });
});
