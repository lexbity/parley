import { mount, flushPromises } from '@vue/test-utils';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError, parley, type PartyImportPreview, type PartyImportReceipt, type PartyImportStage } from '../../api/client';
import PartyImport from './PartyImport.vue';

const stage: PartyImportStage = { stage_id: 'stage-1', headers: ['Name', 'Focus', 'Extra'], row_count: 1, max_rows: 100, max_bytes: 1000 };
const preview: PartyImportPreview = {
  preview_id: 'pv-1', digest: 'a'.repeat(64), create_count: 1, skip_count: 0, error_count: 0,
  rows: [{ source_record: 1, name: 'Dana Lee', focus: 'Hiring', notes: 'A note', duplicates: [], decision: 'create' }],
};
const receipt: PartyImportReceipt = { batch_id: 'batch-1', created: [{ source_record: 1, party_id: 'p1' }], skipped_count: 0 };

beforeEach(() => {
  vi.spyOn(parley, 'supports').mockReturnValue(true);
  vi.spyOn(parley, 'workspaceId', 'get').mockReturnValue('workspace-1');
  vi.spyOn(parley, 'workspaceEpoch', 'get').mockReturnValue('epoch-1');
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value(this: HTMLDialogElement) { this.setAttribute('open', ''); } });
  Object.defineProperty(HTMLDialogElement.prototype, 'close', { configurable: true, value(this: HTMLDialogElement) { this.removeAttribute('open'); } });
  globalThis.sessionStorage.clear();
});

afterEach(() => { vi.restoreAllMocks(); globalThis.sessionStorage.clear(); });

function mountImport() {
  return mount(PartyImport, { attachTo: globalThis.document.body });
}

describe('PartyImport', () => {
  it('stages CSV, previews mapped rows, decides duplicates and commits with one key', async () => {
    const stageImport = vi.spyOn(parley, 'stagePartyImport').mockResolvedValue(stage);
    const candidate = { ...preview.rows[0]!, duplicates: ['Dana Lee (existing)'] };
    const duplicateReview = { ...preview, create_count: 0, rows: [{ ...candidate, decision: 'review' as const }] };
    const skipped = { ...preview, create_count: 0, skip_count: 1, rows: [{ ...candidate, decision: 'skip' as const }] };
    const approved = { ...preview, rows: [candidate] };
    const previewImport = vi.spyOn(parley, 'previewPartyImport')
      .mockResolvedValueOnce(duplicateReview).mockResolvedValueOnce(skipped).mockResolvedValue(approved);
    const commit = vi.spyOn(parley, 'commitPartyImport').mockResolvedValue(receipt);
    const wrapper = mountImport();
    await wrapper.get('button').trigger('click');
    await wrapper.get('textarea').setValue('Name,Focus\nDana Lee,Hiring');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(stageImport).toHaveBeenCalled();
    expect(wrapper.text()).toContain('Skipped columns · 2 · Focus, 3 · Extra');
    await wrapper.findAll('button').find((button) => button.text() === 'Preview mapped rows')!.trigger('click');
    await flushPromises();
    expect(previewImport).toHaveBeenCalledWith('stage-1', expect.objectContaining({ name_column: 0, party_type: 'person' }), expect.any(String));
    const decision = wrapper.get('fieldset.record-review select:has(option[value="distinct"])');
    await decision.setValue('skip');
    await flushPromises();
    expect(previewImport).toHaveBeenLastCalledWith('stage-1', expect.objectContaining({ excluded_records: [1] }), expect.any(String));
    await decision.setValue('distinct');
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Import 1 party')!.trigger('click');
    await flushPromises();
    expect(previewImport).toHaveBeenLastCalledWith('stage-1', expect.objectContaining({ distinct_records: [1], excluded_records: [] }), expect.any(String));
    expect(commit).toHaveBeenCalledWith('stage-1', expect.objectContaining({ preview_id: 'pv-1', digest: preview.digest }), expect.any(String));
    expect(wrapper.text()).toContain('1 created · 0 skipped');
    await wrapper.findAll('button').find((button) => button.text() === 'View imported parties')!.trigger('click');
    expect(wrapper.emitted('viewImported')?.[0]).toEqual([receipt]);
    wrapper.unmount();
  });

  it('restores and clears a pending commit across reopenings', async () => {
    const wrapper = mountImport();
    const pending = { stageId: 'stage-1', preview_id: 'pv-1', digest: preview.digest, key: 'key-1' };
    globalThis.sessionStorage.setItem('parley.party-import.workspace-1.epoch-1', JSON.stringify(pending));
    await wrapper.get('button').trigger('click');
    expect(wrapper.text()).toContain('Check the receipt for your submitted import.');
    await wrapper.findAll('button').find((button) => button.text() === 'Close')!.trigger('click');
    await wrapper.findAll('button')[0]!.trigger('click');
    expect(wrapper.text()).toContain('Check the receipt for your submitted import.');
    wrapper.unmount();
  });

  it('surfaces staging, preview and commit failures with terminal recovery', async () => {
    const wrapper = mountImport();
    vi.spyOn(parley, 'stagePartyImport').mockRejectedValueOnce(new Error('stage failed')).mockResolvedValue(stage);
    await wrapper.get('button').trigger('click');
    await wrapper.get('textarea').setValue('Name\nDana');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('stage failed');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.text()).toContain('Map 1 data records');

    vi.spyOn(parley, 'previewPartyImport').mockRejectedValue(new Error('preview failed'));
    await wrapper.findAll('button').find((button) => button.text() === 'Preview mapped rows')!.trigger('click');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('preview failed');

    vi.spyOn(parley, 'previewPartyImport').mockResolvedValue(preview);
    await wrapper.findAll('button').find((button) => button.text() === 'Preview mapped rows')!.trigger('click');
    await flushPromises();
    vi.spyOn(parley, 'commitPartyImport').mockRejectedValue(new ApiError('stage expired', 410, 'GONE'));
    await wrapper.findAll('button').find((button) => button.text() === 'Import 1 party')!.trigger('click');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('stage expired');
    expect(wrapper.text()).toContain('Read columns');
    wrapper.unmount();
  });
});
