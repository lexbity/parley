import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type AdminBackup, type AdminOverview, type AdminPreview } from '../../api/client';
import DataControls from './DataControls.vue';

afterEach(() => vi.restoreAllMocks());

const backup: AdminBackup = { id: 'backup-1', created_at: '2026-11-21T12:00:00Z', schema_version: 2, workspace_epoch: 'epoch-1', size_bytes: 4096, integrity: 'verified' };
const overview: AdminOverview = { as_of: '2026-11-21T12:00:00Z', workspace_epoch: 'epoch-1', event_cursor: '14', last_verified_backup: backup, backups: [backup], parties: [{ id: 'party-1', kind: 'person', name: 'Dana Lee', descriptor: 'Northline' }] };
const preview: AdminPreview = { id: 'preview-1', operation: 'erase_party', digest: 'a'.repeat(64), workspace_epoch: 'epoch-1', event_cursor: '14', expires_at: '2026-11-21T12:05:00Z', backup: null, party: { id: 'party-1', name: 'Dana Lee' }, impact: { parties: 1, sent_touches: 2, materials: 1 }, preserved_shared: { shared_sources: 1 } };

describe('Workspace data controls S11', () => {
  it('shows exact impact and does not commit until the preview digest is confirmed', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'getAdminOverview').mockResolvedValue(overview);
    vi.spyOn(parley, 'createAdminPreview').mockResolvedValue(preview);
    const commit = vi.spyOn(parley, 'commitAdminPreview').mockRejectedValue(new Error('PREVIEW_STALE'));
    const wrapper = mount(DataControls);
    await flushPromises();
    await wrapper.get('select').setValue('erase_party');
    await wrapper.findAll('button').find((button) => button.text() === 'Preview exact impact')!.trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('sent touches: 2');
    expect(wrapper.text()).toContain('shared sources: 1 retained');
    const confirm = wrapper.findAll('button').find((button) => button.text() === 'Confirm erase this party')!;
    expect((confirm.element as HTMLButtonElement).disabled).toBe(true);
    await wrapper.get('input[aria-label="Confirm digest aaaaaaaaaaaa"]').setValue('aaaaaaaaaaaa');
    expect((confirm.element as HTMLButtonElement).disabled).toBe(false);
    await confirm.trigger('click');
    await flushPromises();
    expect(commit).toHaveBeenCalledWith({ operation: 'erase_party', preview_id: 'preview-1', digest: 'a'.repeat(64), expected_workspace_epoch: 'epoch-1', expected_event_cursor: '14', confirmation: 'aaaaaaaaaaaa' });
    expect(wrapper.get('[role="alert"]').text()).toContain('PREVIEW_STALE');
    wrapper.unmount();
  });

  it('backs up through a separate verified operation and refreshes the backup inventory', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'getAdminOverview').mockResolvedValue(overview);
    const create = vi.spyOn(parley, 'createAdminBackup').mockResolvedValue({ ...backup, id: 'backup-2' });
    const wrapper = mount(DataControls);
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Create and verify backup')!.trigger('click');
    await flushPromises();
    expect(create).toHaveBeenCalledOnce();
    expect(wrapper.findAll('[role="status"]').some((node) => node.text().includes('Verified backup created'))).toBe(true);
    expect(parley.getAdminOverview).toHaveBeenCalledTimes(2);
    wrapper.unmount();
  });
});
