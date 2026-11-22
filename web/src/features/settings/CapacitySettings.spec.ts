import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type CapacityRecord } from '../../api/client';
import CapacitySettings from './CapacitySettings.vue';

afterEach(() => vi.restoreAllMocks());

const ledger: CapacityRecord = { period: '2026-Q4', unit: 'project slots', total: 8, committed: 2, remaining: 6, updated_at: '2026-11-21T12:00:00Z' };

describe('CapacitySettings S07 ledger controls', () => {
  it('shows owner-reported remaining capacity and prevents resetting a committed period', async () => {
    vi.spyOn(parley, 'listCapacity').mockResolvedValue([ledger]);
    const update = vi.spyOn(parley, 'updateCapacity');
    const wrapper = mount(CapacitySettings);
    await flushPromises();
    expect(wrapper.text()).toContain('2 committed of 8 project slots · 6 remaining');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('could reset the committed ledger');
    expect(update).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('provisions a new ledger and reports the server-returned balance', async () => {
    vi.spyOn(parley, 'listCapacity').mockResolvedValue([]);
    vi.spyOn(parley, 'updateCapacity').mockResolvedValue({ ...ledger, period: '2027-Q1', total: 10, committed: 0, remaining: 10 });
    const wrapper = mount(CapacitySettings);
    await flushPromises();
    await wrapper.get('input').setValue('2027-Q1');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(parley.updateCapacity).toHaveBeenCalledWith({ period: '2027-Q1', unit: 'project slots', total: 1 });
    expect(wrapper.get('[role="status"]').text()).toContain('10 project slots remain in 2027-Q1');
    wrapper.unmount();
  });
});
