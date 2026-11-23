import { mount, flushPromises } from '@vue/test-utils';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parley, type BackupRecord, type ErasePreview, type GraphEntity, type RestorePreview } from '../../api/client';
import MaintenanceSettings from './MaintenanceSettings.vue';

const parties: GraphEntity[] = [{ id: 'p1', kind: 'person', name: 'Dana Lee', focus: '', revision: 1 }];
const backup: BackupRecord = { id: 'b1', created_at: '2026-11-23T00:00:00Z', status: 'verified', manifest_sha256: 'a'.repeat(64), workspace_id: 'w1', format_version: 1 };
const snapshot: RestorePreview = {
  id: 'pv1', digest: 'b'.repeat(64), backup, current_cursor: '7', erasure_resurrections: 1,
  required_bytes: 1024,
};
const erasure: ErasePreview = {
  id: 'ep1', digest: 'c'.repeat(64), domain_cursor: '7', affected_counts: { party: 1 },
  shared_sources: 0, owner_managed_backups: 1,
};

beforeEach(() => {
  vi.spyOn(parley, 'getMaintenanceSettings').mockResolvedValue({ backup_directory: '/backups', interval_hours: 24, retention_count: 14, revision: 1, last_successful_at: null });
  vi.spyOn(parley, 'listBackups').mockResolvedValue({ items: [backup], last_successful_at: null });
  vi.spyOn(parley, 'updateMaintenanceSettings').mockResolvedValue({ backup_directory: '/backups', interval_hours: 48, retention_count: 14, revision: 2, last_successful_at: null });
  vi.spyOn(parley, 'createBackup').mockResolvedValue(backup);
  vi.spyOn(parley, 'previewRestore').mockResolvedValue(snapshot);
  vi.spyOn(parley, 'commitRestore').mockResolvedValue({ state_sha256: 'd'.repeat(64) } as never);
  vi.spyOn(parley, 'previewFullImport').mockResolvedValue(snapshot);
  vi.spyOn(parley, 'commitFullImport').mockResolvedValue({ state_sha256: 'e'.repeat(64) });
  vi.spyOn(parley, 'previewPartyErasure').mockResolvedValue(erasure);
  vi.spyOn(parley, 'refreshSession').mockResolvedValue();
  vi.spyOn(parley, 'getMaintenanceOperation').mockResolvedValue({ id: 'op1', state: 'complete', external_copies_requiring_disposal: 1 } as never);
  vi.spyOn(parley, 'eraseParty').mockResolvedValue({ id: 'op1', operation: 'erase_party', status: 'succeeded', summary: null, new_epoch: 'epoch-2', last_error: null } as never);
  Object.defineProperty(globalThis.URL, 'createObjectURL', { configurable: true, value: vi.fn().mockReturnValue('blob:test') });
  Object.defineProperty(globalThis.URL, 'revokeObjectURL', { configurable: true, value: vi.fn() });
});

afterEach(() => vi.restoreAllMocks());

function button(wrapper: ReturnType<typeof mount>, label: string) {
  const found = wrapper.findAll('button').find((item) => item.text() === label);
  if (!found) throw new Error(`Missing button ${label}`);
  return found;
}

describe('MaintenanceSettings', () => {
  it('saves the schedule, creates a backup and exports private data', async () => {
    const wrapper = mount(MaintenanceSettings, { props: { parties } });
    await flushPromises();
    expect(wrapper.text()).toContain('Verified backup');
    await wrapper.get('input[maxlength="4096"]').setValue('/new/backups');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(parley.updateMaintenanceSettings).toHaveBeenCalledWith(expect.objectContaining({ backup_directory: '/new/backups', expected_revision: 1 }));
    expect(wrapper.text()).toContain('Backup destination and schedule saved.');
    await button(wrapper, 'Create verified backup now').trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('Verified backup b1 created.');
    vi.spyOn(parley, 'createFullExport').mockResolvedValue(new Blob(['zip']));
    await button(wrapper, 'Download personal data export').trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('Personal export downloaded');
    wrapper.unmount();
  });

  it('previews and commits a verified restore only after acknowledgement', async () => {
    const wrapper = mount(MaintenanceSettings, { props: { parties } });
    await flushPromises();
    await button(wrapper, 'Verify restore preview').trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('1 erased records may return');
    await wrapper.get('article input[type="checkbox"]').setValue(true);
    await button(wrapper, 'Commit verified restore').trigger('click');
    await flushPromises();
    expect(parley.commitRestore).toHaveBeenCalledWith(snapshot, true);
    expect(wrapper.text()).toContain('Restore published');
    wrapper.unmount();
  });

  it('validates an import archive before committing to an empty workspace', async () => {
    const wrapper = mount(MaintenanceSettings, { props: { parties } });
    await flushPromises();
    const file = new File(['zip'], 'export.zip', { type: 'application/zip' });
    const input = wrapper.get('input[accept=".zip,application/zip"]');
    Object.defineProperty(input.element, 'files', { configurable: true, value: [file] });
    await input.trigger('change');
    await button(wrapper, 'Validate import archive').trigger('click');
    await flushPromises();
    expect(parley.previewFullImport).toHaveBeenCalledWith(file);
    expect(wrapper.text()).toContain('Preview pv1');
    await wrapper.get('article input[type="checkbox"]').setValue(true);
    wrapper.unmount();
  });

  it('previews an erasure and completes it after a fresh session and cleanup', async () => {
    const wrapper = mount(MaintenanceSettings, { props: { parties } });
    await flushPromises();
    await wrapper.get('section[aria-label="Erase a party"] select').setValue('p1');
    await button(wrapper, 'Preview erasure').trigger('click');
    await flushPromises();
    expect(parley.previewPartyErasure).toHaveBeenCalledWith('p1');
    await wrapper.get('section[aria-label="Erase a party"] input').setValue('Dana Lee');
    await button(wrapper, 'Erase Dana Lee').trigger('click');
    await flushPromises();
    expect(parley.eraseParty).toHaveBeenCalledWith('p1', erasure);
    expect(wrapper.text()).toContain('Erasure completed');
    expect(wrapper.emitted('erased')).toBeDefined();
    wrapper.unmount();
  });
});
