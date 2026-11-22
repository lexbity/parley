import { mount, flushPromises, type VueWrapper } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { EditorView } from '@codemirror/view';
import { parley, type BoardSnapshot, type GraphEntity, type OpportunityNotes } from '../../api/client';
import QueueWorkspace from './QueueWorkspace.vue';
import MarkdownEditor from '../../shared/MarkdownEditor.vue';

afterEach(() => vi.restoreAllMocks());

function notesView(wrapper: VueWrapper): EditorView {
  const view = (wrapper.getComponent(MarkdownEditor).vm as unknown as { getView(): EditorView | null }).getView();
  if (!view) throw new Error('CodeMirror editor was not mounted');
  return view;
}

// The rendered preview also renders checkboxes for task items, but they are
// disabled and labelled; the card's interactive toggle has neither.
function taskToggle(wrapper: VueWrapper) {
  const box = wrapper.findAll('input[type="checkbox"]').find((box) => box.attributes('aria-label') === undefined);
  if (!box) throw new Error('card task checkbox not found');
  return box;
}

const parties: GraphEntity[] = [{ id: 'p1', kind: 'person', name: 'Dana Lee', revision: 1 }];
const taskHash = 'c'.repeat(64);
const notes: OpportunityNotes = {
  opportunity_id: 'o1', markdown: '🧭\n- [ ] Review checklist\n', revision: 1, next_task: 'Review checklist',
  tasks: [{ byte_start: 5, byte_end: 29, line_sha256: taskHash, checked: false, text: 'Review checklist' }],
};
const opportunity = { id: 'o1', title: 'Onboarding', state: 'open', revision: 1, participant_ids: ['p1'], source_variant_ids: [], material_ids: [] };
const board: BoardSnapshot = {
  lists: [
    { id: 'a', title: 'Today', position: 0, archived: false, revision: 1 },
    { id: 'b', title: 'Later', position: 1, archived: false, revision: 1 },
  ],
  cards: [{ opportunity, opportunity_id: 'o1', list_id: 'a', position: 0, placement_revision: 1, next_task: 'Review checklist' }],
};

function stub(boardValue: BoardSnapshot = board) {
  vi.spyOn(parley, 'connect').mockResolvedValue();
  vi.spyOn(parley, 'getBoard').mockResolvedValue(boardValue);
  vi.spyOn(parley, 'createOpportunity').mockResolvedValue(opportunity);
  vi.spyOn(parley, 'createBoardList').mockResolvedValue({ list: boardValue.lists[0]!, board_revision: 1 });
  vi.spyOn(parley, 'renameBoardList').mockResolvedValue(boardValue.lists[0]!);
  vi.spyOn(parley, 'reorderBoardLists').mockResolvedValue(boardValue.lists);
  vi.spyOn(parley, 'archiveBoardList').mockResolvedValue({ archived_list: { ...boardValue.lists[0]!, archived: true }, destination_list: boardValue.lists[1]!, moved: [] });
  vi.spyOn(parley, 'moveOpportunityPlacement').mockResolvedValue({ opportunity_id: 'o1', list_id: 'b', position: 0, placement_revision: 2, source_list_revision: 2, destination_list_revision: 2 });
  vi.spyOn(parley, 'getOpportunityNotes').mockResolvedValue(notes);
  vi.spyOn(parley, 'listThreads').mockResolvedValue([]);
  vi.spyOn(parley, 'updateOpportunityNotes').mockResolvedValue({ ...notes, revision: 2 });
  vi.spyOn(parley, 'toggleOpportunityTask').mockResolvedValue({ ...notes, markdown: '🧭\n- [x] Review checklist\n', revision: 2, next_task: null, tasks: [{ ...notes.tasks[0]!, checked: true }] });
  return mount(QueueWorkspace, { props: { parties, outputs: [] } });
}

describe('QueueWorkspace S05 board and card behavior', () => {
  it('creates, renames, reorders, moves, and archives lists through revision-aware commands', async () => {
    const wrapper = stub(); await flushPromises();
    expect(wrapper.text()).toContain('Today');
    expect(wrapper.text()).toContain('Next task: Review checklist');
    const createList = vi.mocked(parley.createBoardList);
    await wrapper.get('input[required][maxlength="120"]').setValue('Friday');
    const listForm = wrapper.findAll('form').find((form) => form.text().includes('Custom board lists'))!;
    await listForm.trigger('submit'); await flushPromises();
    expect(createList).toHaveBeenCalled();
    const rename = wrapper.findAll('button').find((button) => button.text() === 'Save list name')!;
    await wrapper.get('input[aria-label="Rename Today"]').setValue('Focus');
    await rename.trigger('click'); await flushPromises();
    expect(parley.renameBoardList).toHaveBeenCalledWith('a', { title: 'Focus', expected_revision: 1 });
    await wrapper.get('select[aria-label="Move Onboarding to list"]').setValue('b');
    await wrapper.findAll('button').find((button) => button.text() === 'Move card')!.trigger('click'); await flushPromises();
    expect(parley.moveOpportunityPlacement).toHaveBeenCalledWith('o1', expect.objectContaining({ list_id: 'b', expected_placement_revision: 1 }));
    await wrapper.get('button[aria-label="Move Later up"]').trigger('click'); await flushPromises();
    expect(parley.reorderBoardLists).toHaveBeenCalledWith({ ordered_ids: ['b', 'a'], expected_revisions: { a: 1, b: 1 } });
    await wrapper.get('select[aria-label="Archive destination for Today"]').setValue('b');
    await wrapper.findAll('button').find((button) => button.text() === 'Archive list')!.trigger('click'); await flushPromises();
    expect(parley.archiveBoardList).toHaveBeenCalledWith('a', { destination_list_id: 'b', expected_list_revision: 1, expected_destination_revision: 1 });
    wrapper.unmount();
  });

  it('keeps Markdown and rendered text in sync and toggles only the backend-addressed task', async () => {
    const wrapper = stub(); await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Open card')!.trigger('click'); await flushPromises();
    expect(parley.getOpportunityNotes).toHaveBeenCalledWith('o1');
    const view = notesView(wrapper);
    const edited = '🧭\n- [ ] Review checklist\n\n```md\n- [ ] example\n```\n';
    const savedNotes = { ...notes, markdown: edited, revision: 2 };
    vi.mocked(parley.updateOpportunityNotes).mockResolvedValueOnce(savedNotes);
    vi.mocked(parley.toggleOpportunityTask).mockResolvedValueOnce({
      ...savedNotes,
      markdown: '🧭\n- [x] Review checklist\n\n```md\n- [ ] example\n```\n',
      revision: 3,
      next_task: null,
      tasks: [{ ...notes.tasks[0]!, checked: true }],
    });
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: edited } });
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Save notes')!.trigger('click'); await flushPromises();
    expect(parley.updateOpportunityNotes).toHaveBeenCalledWith('o1', edited, 1, expect.any(String));
    const preview = wrapper.get('.markdown-preview');
    expect(preview.text()).toContain('🧭');
    expect(preview.findAll('input[type="checkbox"]')).toHaveLength(1);
    expect(preview.get('pre').text()).toBe('- [ ] example');
    const checkbox = taskToggle(wrapper);
    await checkbox.setValue(true); await flushPromises();
    expect(parley.toggleOpportunityTask).toHaveBeenCalledWith('o1', {
      notes_revision: 2, byte_start: 5, byte_end: 29, line_sha256: taskHash, checked: true,
    });
    const previewAfter = wrapper.get('.markdown-preview');
    expect(previewAfter.findAll('input[type="checkbox"]').filter((box) => (box.element as HTMLInputElement).checked)).toHaveLength(1);
    expect(previewAfter.get('pre').text()).toBe('- [ ] example');
    wrapper.unmount();
  });

  it('blocks card task toggles while the canonical notes buffer is dirty and reports stale writes', async () => {
    const wrapper = stub(); await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Open card')!.trigger('click'); await flushPromises();
    const view = notesView(wrapper);
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: 'local changes' } });
    await flushPromises();
    await taskToggle(wrapper).trigger('change');
    expect(parley.toggleOpportunityTask).not.toHaveBeenCalled();
    expect(taskToggle(wrapper).attributes('disabled')).toBeDefined();
    vi.mocked(parley.updateOpportunityNotes).mockRejectedValueOnce(new Error('notes changed elsewhere'));
    await wrapper.findAll('button').find((button) => button.text() === 'Save notes')!.trigger('click'); await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('notes changed elsewhere');
    await wrapper.findAll('button').find((button) => button.text() === 'Discard edits and reload')!.trigger('click'); await flushPromises();
    expect(notesView(wrapper).state.doc.toString()).toBe(notes.markdown);
    wrapper.unmount();
  });
});
