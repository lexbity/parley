import { mount, flushPromises } from '@vue/test-utils';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parley, type GraphEntity, type GraphSnapshot, type SavedSource } from '../../api/client';
import MarkdownEditor from '../../shared/MarkdownEditor.vue';
import RelationshipEditor from './RelationshipEditor.vue';

const person: GraphEntity = { id: 'p1', kind: 'person', name: 'Dana Lee', focus: 'Hiring', revision: 4 };
const org: GraphEntity = { id: 'o1', kind: 'org', name: 'Northline', focus: '', revision: 2 };
const nodes = [person, org] as GraphSnapshot['nodes'];
const sources: SavedSource[] = [{ content_id: 'content-1234567890', variant_id: 'v1', occurrence_id: 'occ-1', normalized_sha256: 'a', raw_sha256: 'b' }];

beforeEach(() => {
  vi.spyOn(parley, 'supports').mockReturnValue(true);
  vi.spyOn(parley, 'queryDirectory').mockResolvedValue({ rows: [org], nextCursor: '1', fence: '1' });
  vi.spyOn(parley, 'createRelatedEntity').mockResolvedValue({ id: 'new-1', kind: 'org', name: 'New Org', focus: '', revision: 1 });
  vi.spyOn(parley, 'saveEdge').mockResolvedValue();
  vi.spyOn(parley, 'stageEntityAttachment').mockImplementation((_file, onProgress) => {
    onProgress(50);
    onProgress(100);
    return Promise.resolve('upload-1');
  });
});

afterEach(() => { vi.restoreAllMocks(); });

function props(overrides: Record<string, unknown> = {}) {
  return { root: 'p1', selected: 'p1', nodes, sources, ...overrides };
}

async function fillCommon(wrapper: ReturnType<typeof mount>, sourceIndex = 4) {
  await wrapper.get('form').findAll('select')[sourceIndex]!.setValue('occ-1');
  await wrapper.get('textarea[required]').setValue('Dana joined Northline.');
}

describe('RelationshipEditor', () => {
  it('builds a source-backed related entity from a typed relationship', async () => {
    const wrapper = mount(RelationshipEditor, { props: props() });
    await flushPromises();
    await wrapper.get('input[maxlength="240"]').setValue('New Org');
    expect(wrapper.get('.sentence').text()).toContain('Dana Lee member of');
    await fillCommon(wrapper);
    wrapper.getComponent(MarkdownEditor).vm.$emit('update:modelValue', '**Owner note**');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(parley.createRelatedEntity).toHaveBeenCalledWith(expect.objectContaining({
      kind: 'org', name: 'New Org', related_entity_id: 'p1', expected_parent_revision: 4,
      verb: 'member_of', occurrence_id: 'occ-1', quote: 'Dana joined Northline.', notes: '**Owner note**', view_id: 'p1',
    }), expect.any(String));
    expect(wrapper.emitted('saved')?.[0]).toEqual(['new-1']);
    expect(wrapper.emitted('cancel')).toBeUndefined();
    wrapper.unmount();
  });

  it('searches the directory and links an existing card', async () => {
    vi.useFakeTimers();
    const wrapper = mount(RelationshipEditor, { props: props() });
    await flushPromises();
    await wrapper.get('form').findAll('select')[0]!.setValue('link');
    await wrapper.get('input[type="search"]').setValue('North');
    await vi.advanceTimersByTimeAsync(200);
    await flushPromises();
    expect(parley.queryDirectory).toHaveBeenCalledWith(expect.objectContaining({ kind: 'org', q: 'North' }));
    const selects = wrapper.get('form').findAll('select');
    await selects[3]!.setValue('o1');
    await fillCommon(wrapper, 5);
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(parley.saveEdge).toHaveBeenCalledWith(null, expect.objectContaining({ from_id: 'p1', to_id: 'o1', expected_from_revision: 4 }), expect.any(String));
    expect(wrapper.emitted('saved')?.[0]).toEqual(['o1']);
    wrapper.unmount();
    vi.useRealTimers();
  });

  it('restores a correction into link mode and reports save failures', async () => {
    vi.useFakeTimers();
    const correction = {
      id: 'edge-1', from_id: 'p1', to_id: 'o1', verb: 'member_of', revision: 3, occurrence_id: 'occ-1',
      owner_authored: false, quote: 'Dana joined Northline.', tier: 2, speaker_kind: 'owner',
      captured_at: '2026-11-23T00:00:00Z', source_title: null, source_url: null,
    } as GraphSnapshot['edges'][number];
    const wrapper = mount(RelationshipEditor, { props: props({ correction, selected: null }) });
    await flushPromises();
    await vi.advanceTimersByTimeAsync(200);
    expect(wrapper.text()).toContain('Correct relationship');
    await fillCommon(wrapper);
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(parley.saveEdge).toHaveBeenCalledWith('edge-1', expect.objectContaining({ expected_revision: 3 }), expect.any(String));

    vi.mocked(parley.saveEdge).mockRejectedValueOnce(new Error('edge changed'));
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('edge changed');
    wrapper.unmount();
    vi.useRealTimers();
  });

  it('stages a private file through the shared child control', async () => {
    const wrapper = mount(RelationshipEditor, { props: props() });
    const file = new File(['proof'], 'proof.pdf', { type: 'application/pdf' });
    const input = wrapper.get('input[type="file"]');
    Object.defineProperty(input.element, 'files', { configurable: true, value: [file] });
    await input.trigger('change');
    await flushPromises();
    expect(parley.stageEntityAttachment).toHaveBeenCalledWith(file, expect.any(Function));
    expect(wrapper.text()).toContain('proof.pdf');
    wrapper.unmount();
  });
});
