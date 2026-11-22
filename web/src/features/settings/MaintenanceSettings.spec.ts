import { mount, flushPromises } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type GraphEntity } from '../../api/client';
import MaintenanceSettings from './MaintenanceSettings.vue';

afterEach(() => vi.restoreAllMocks());

describe('MaintenanceSettings S11 owner data operations', () => {
  it('keeps personal export distinct and waits for fresh-epoch erasure cleanup', async () => {
    vi.spyOn(parley, 'getMaintenanceSettings').mockResolvedValue({ backup_directory: '/private/backups', interval_hours: 24, retention_count: 14, revision: 1, last_successful_at: null });
    vi.spyOn(parley, 'listBackups').mockResolvedValue({ items: [], last_successful_at: null });
    vi.spyOn(parley, 'createFullExport').mockResolvedValue(new Blob(['archive']));
    vi.spyOn(parley, 'previewPartyErasure').mockResolvedValue({ id: 'preview-1', digest: 'd'.repeat(64), domain_cursor: '9', affected_counts: { events: 5 }, shared_sources: 1, owner_managed_backups: 2 });
    vi.spyOn(parley, 'eraseParty').mockResolvedValue({ id: 'op-1', kind: 'erase', state: 'pending_restart', epoch: 'epoch-new', external_copies_requiring_disposal: 2 });
    vi.spyOn(parley, 'refreshSession').mockResolvedValue();
    vi.spyOn(parley, 'getMaintenanceOperation').mockResolvedValue({ id: 'op-1', kind: 'erase', state: 'complete', epoch: 'epoch-new', external_copies_requiring_disposal: 2 });
    const party: GraphEntity = { id: 'p1', kind: 'person', name: 'Dana Lee', revision: 1 };
    const wrapper = mount(MaintenanceSettings, { props: { parties: [party] } }); await flushPromises();
    expect(wrapper.text()).toContain('does not approve or release recipient-facing content');
    await wrapper.findAll('select').at(-1)!.setValue('p1');
    await wrapper.findAll('button').find((button) => button.text() === 'Preview erasure')!.trigger('click'); await flushPromises();
    expect(wrapper.text()).toContain('Records affected: Dana Lee');
    await wrapper.findAll('input[autocomplete="off"]').at(-1)!.setValue('Dana Lee');
    expect(wrapper.text()).toContain('Erase Dana Lee');
    await wrapper.findAll('button').find((button) => button.text() === 'Erase Dana Lee')!.trigger('click'); await flushPromises();
    expect(parley.refreshSession).toHaveBeenCalledWith('epoch-new');
    expect(parley.getMaintenanceOperation).toHaveBeenCalledWith('op-1');
    expect(wrapper.text()).toContain('Erasure completed');
  });
});
