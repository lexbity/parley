import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type DailyQueueSnapshot } from '../../api/client';
import Daily from './Daily.vue';

afterEach(() => vi.restoreAllMocks());

const daily: DailyQueueSnapshot = {
  as_of: '2026-11-21T12:00:00Z', pack_id: 'job_search', pack_version: '1.0.0', daily_cap: 12, actionable_count: 2, deferred_count: 0,
  items: [
    { id: 'follow-1', opportunity_id: 'opp-1', title: 'Reply follow-up', type: 'followup_due', party_id: 'party-1', party_name: 'Dana Lee', due_at: '2026-11-22T12:00:00Z', reason: 'Response window remains open.', blocking_reason: null, remediation_target: { kind: 'opportunity', id: 'opp-1' } },
    { id: 'blocked-1', opportunity_id: 'opp-2', title: 'Missing material', type: 'blocked', party_id: null, party_name: null, due_at: null, reason: 'No approved output.', blocking_reason: 'Create material for the opportunity.', remediation_target: { kind: 'opportunity', id: 'opp-2' } },
  ],
};

describe('Daily S10 deterministic queue view', () => {
  it('renders server order and separate blockers without starting a Coach job', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'getDailyQueue').mockResolvedValue(daily);
    const runCoach = vi.spyOn(parley, 'runCoach');
    const wrapper = mount(Daily);
    await flushPromises();
    expect(wrapper.text()).toContain('12');
    expect(wrapper.get('ol').text()).toContain('Reply follow-up');
    expect(wrapper.get('ul').text()).toContain('Create material for the opportunity.');
    expect(parley.getDailyQueue).toHaveBeenCalledWith({});
    expect(runCoach).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('applies filters through the server request rather than sorting or ranking locally', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'getDailyQueue').mockResolvedValue({ ...daily, items: [] });
    const wrapper = mount(Daily);
    await flushPromises();
    await wrapper.get('input[aria-label="Find daily work"]').setValue('Dana');
    await wrapper.get('select[aria-label="Daily status filter"]').setValue('waiting');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(parley.getDailyQueue).toHaveBeenLastCalledWith({ q: 'Dana', status: 'waiting' });
    expect(wrapper.text()).toContain('No daily items match these filters.');
    wrapper.unmount();
  });
});
