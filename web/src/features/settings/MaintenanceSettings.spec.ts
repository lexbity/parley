import { mount, flushPromises } from '@vue/test-utils';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError, parley, type BackupRecord, type ErasePreview, type GraphEntity, type RestorePreview  } from '../../api/client';
import MaintenanceSettings from './MaintenanceSettings.vue';

const parties: GraphEntity[] = [
  { id: 'p1', kind: 'person', name: 'Dana Lee', focus: '', revision: 1 },
  { id: 'p2', kind: 'person', name: 'Mira Chen', focus: '', revision: 1 },
];
const backup: BackupRecord = { id: 'b1', created_at: '2026-11-23T00:00:00Z', status: 'verified', manifest_sha256: 'a'.repeat(64), workspace_id: 'w1', format_version: 1 };
const snapshot: RestorePreview = {
  id: 'pv1', digest: 'b'.repeat(64), backup, current_cursor: '7', erasure_resurrections: 1,
  required_bytes: 1024,
};
const operation = { id: 'op1', kind: 'erase' as const, epoch: 'epoch-2', external_copies_requiring_disposal: 1 };
const erasure: ErasePreview = {
  id: 'ep1', digest: 'c'.repeat(64), domain_cursor: '7', affected_counts: { party: 1 },
  shared_sources: 0, owner_managed_backups: 1,
};

beforeEach(() => {
  vi.spyOn(parley, 'getMaintenanceSettings').mockResolvedValue({ backup_directory: '/backups', interval_hours: 24, retention_count: 14, revision: 1, last_successful_at: null });
  vi.spyOn(parley, 'queryDirectory').mockResolvedValue({ rows: parties, nextCursor: null, fence: '0' });
  vi.spyOn(parley, 'listBackups').mockResolvedValue({ items: [backup], last_successful_at: null });
  vi.spyOn(parley, 'updateMaintenanceSettings').mockResolvedValue({ backup_directory: '/backups', interval_hours: 48, retention_count: 14, revision: 2, last_successful_at: null });
  vi.spyOn(parley, 'createBackup').mockResolvedValue(backup);
  vi.spyOn(parley, 'previewRestore').mockResolvedValue(snapshot);
  vi.spyOn(parley, 'commitRestore').mockResolvedValue({ state_sha256: 'd'.repeat(64) });
  vi.spyOn(parley, 'previewFullImport').mockResolvedValue(snapshot);
  vi.spyOn(parley, 'commitFullImport').mockResolvedValue({ state_sha256: 'e'.repeat(64) });
  vi.spyOn(parley, 'previewPartyErasure').mockResolvedValue(erasure);
  vi.spyOn(parley, 'refreshSession').mockResolvedValue();
  vi.spyOn(parley, 'getMaintenanceOperation').mockResolvedValue({ ...operation, state: 'complete' });
  vi.spyOn(parley, 'eraseParty').mockResolvedValue({ ...operation, state: 'pending_restart' });
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
    const wrapper = mount(MaintenanceSettings);
    await flushPromises();
    expect(wrapper.text()).toContain('Verified backup');
    await wrapper.get('input[maxlength="4096"]').setValue('/new/backups');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(parley.updateMaintenanceSettings).toHaveBeenCalledWith(expect.objectContaining({ backup_directory: '/new/backups', expected_revision: 1 }), expect.any(String));
    // TD-10 W3: a destructive save stops at the approval inbox with the
    // intent frozen to the operation key; the retry must reuse that key so
    // the granted approval matches.
    vi.mocked(parley.updateMaintenanceSettings).mockRejectedValueOnce(new ApiError('approval required', 403, 'approval_required'));
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.text()).toContain('Owner approval required');
    // The refused call pinned the key; the retry reuses it so the granted
    // approval matches the frozen intent.
    const pinnedKey = vi.mocked(parley.updateMaintenanceSettings).mock.calls[1]![1] as string;
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(vi.mocked(parley.updateMaintenanceSettings).mock.calls[2]![1]).toBe(pinnedKey);
    expect(wrapper.text()).toContain('Backup destination and schedule saved.');
    await button(wrapper, 'Create verified backup now').trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('Verified backup b1 created.');
    vi.spyOn(parley, 'createFullExport').mockResolvedValue(new Blob(['zip']));
    expect(wrapper.get('[aria-label="Personal data export"]').text()).toContain('does not approve or release recipient-facing content');
    await button(wrapper, 'Download personal data export').trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('Personal export downloaded');
    wrapper.unmount();
  });

  it('previews and commits a verified restore only after acknowledgement', async () => {
    const wrapper = mount(MaintenanceSettings);
    await flushPromises();
    await button(wrapper, 'Verify restore preview').trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('1 erased records may return');
    await wrapper.get('article input[type="checkbox"]').setValue(true);
    await button(wrapper, 'Commit verified restore').trigger('click');
    await flushPromises();
    expect(parley.commitRestore).toHaveBeenCalledWith(snapshot, true, expect.any(String));
    expect(wrapper.text()).toContain('Restore published');
    wrapper.unmount();
  });

  it('validates an import archive before committing to an empty workspace', async () => {
    const wrapper = mount(MaintenanceSettings);
    await flushPromises();
    const file = new File(['zip'], 'export.zip', { type: 'application/zip' });
    const input = wrapper.get('input[accept=".zip,application/zip"]');
    Object.defineProperty(input.element, 'files', { configurable: true, value: [file] });
    await input.trigger('change');
    await button(wrapper, 'Validate import archive').trigger('click');
    await flushPromises();
    expect(parley.previewFullImport).toHaveBeenCalledWith(file);
    expect(wrapper.text()).toContain('Preview pv1');
    expect(button(wrapper, 'Commit full data import').attributes('disabled')).toBeDefined();
    await wrapper.get('article input[type="checkbox"]').setValue(true);
    await button(wrapper, 'Commit full data import').trigger('click');
    await flushPromises();
    expect(parley.commitFullImport).toHaveBeenCalledWith(snapshot, expect.any(String));
    expect(wrapper.text()).toContain('Personal data import published.');
    wrapper.unmount();
  });

  it('previews an erasure and completes it after a fresh session and cleanup', async () => {
    const wrapper = mount(MaintenanceSettings);
    await flushPromises();
    const panel = wrapper.get('section[aria-label="Erase a party"]');
    await panel.get('select').setValue('p1');
    await button(wrapper, 'Preview erasure').trigger('click');
    await flushPromises();
    expect(parley.previewPartyErasure).toHaveBeenCalledWith('p1');
    expect(panel.text()).toContain('Records affected: Dana Lee');
    await panel.get('select').setValue('p2');
    expect(panel.find('input').exists()).toBe(false);
    expect(panel.text()).not.toContain('Records affected:');
    await panel.get('select').setValue('p1');
    await button(wrapper, 'Preview erasure').trigger('click');
    await flushPromises();
    await panel.get('input').setValue('Dana Lee');
    await button(wrapper, 'Erase Dana Lee').trigger('click');
    await flushPromises();
    expect(parley.eraseParty).toHaveBeenCalledWith('p1', erasure, expect.any(String));
    expect(parley.refreshSession).toHaveBeenCalledWith(operation.epoch);
    expect(parley.getMaintenanceOperation).toHaveBeenCalledWith(operation.id);
    expect(wrapper.text()).toContain('Erasure completed');
    expect(wrapper.emitted('erased')).toBeDefined();
    wrapper.unmount();
  });
});
