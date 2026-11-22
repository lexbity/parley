import { mount, flushPromises, type VueWrapper } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { EditorView } from '@codemirror/view';
import { parley, type BoardSnapshot, type GraphEntity, type OpportunityNotes } from '../../api/client';
import QueueWorkspace from './QueueWorkspace.vue';
import MarkdownEditor from '../../shared/MarkdownEditor.vue';

let clipboardDescriptor: PropertyDescriptor | undefined;
afterEach(() => {
  vi.restoreAllMocks();
  if (clipboardDescriptor) Object.defineProperty(navigator, 'clipboard', clipboardDescriptor);
  else Reflect.deleteProperty(navigator, 'clipboard');
  clipboardDescriptor = undefined;
});

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
const opportunity = { id: 'o1', title: 'Onboarding', state: 'open', revision: 1, participant_ids: ['p1'], source_variant_ids: [], material_ids: [], tags: ['tested'] };
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
  vi.spyOn(parley, 'getDailyQueue').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', pack_id: 'job_search', pack_version: '1.0.0', daily_cap: 12, actionable_count: 0, deferred_count: 0, items: [] });
  vi.spyOn(parley, 'getInsights').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', period_start: '2026-11-14', period_end: '2026-11-21', pack_id: 'job_search', pack_version: '1.0.0', sample_size: 0, metrics: [], comparisons: [], gaps: [], questions: [] });
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
  vi.spyOn(parley, 'setOpportunityTags').mockResolvedValue({ id: 'o1', title: 'Onboarding', state: 'open', revision: 2, tags: [] });
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

  it('refreshes board and thread projections when the contact panel records a response', async () => {
    const material = { id: 'm1', opportunity_id: 'o1', recipient_id: 'p1', recipient_name: 'Dana Lee', kind: 'outreach' as const, version: 1, body: 'Hello.', content_sha256: 'a'.repeat(64), status: 'approved' as const, gate: null, attachment: null };
    const thread = { id: 't1', opportunity_id: 'o1', recipient_id: 'p1', state: 'waiting' as const, current_touch: 1, expires_at: '2026-11-28T08:00:00Z', next_action_at: null, sent_at: '2026-11-21T08:00:00Z', updated_at: '2026-11-21T08:00:00Z', sends: [{ id: 's1', touch_no: 1, primary_material_id: 'm1', primary_version_no: 1, recipient_id: 'p1', content_sha256: 'a'.repeat(64), sent_at: '2026-11-21T08:00:00Z', response_deadline: '2026-11-28T08:00:00Z', deadline_zone: 'UTC', capacity_charged: false, outcome: null }] };
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'getBoard').mockResolvedValue(board);
    vi.spyOn(parley, 'getOpportunityNotes').mockResolvedValue(notes);
    vi.spyOn(parley, 'listThreads').mockResolvedValue([thread]);
    vi.spyOn(parley, 'recordOutcome').mockResolvedValue({ id: 'outcome-1', send_id: 's1', class: 'reply_positive', rung_accepted: null, received_at: '2026-11-22T08:00:00Z', recorded_at: '2026-11-22T08:00:00Z', note: null });
    const wrapper = mount(QueueWorkspace, { props: { parties, outputs: [material] } });
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Open card')!.trigger('click');
    await flushPromises();
    expect(parley.listThreads).toHaveBeenCalledTimes(1);
    const record = wrapper.findAll('button').find((button) => button.text().includes('Record response for touch 1'))!;
    await record.trigger('click');
    await flushPromises();
    expect(parley.recordOutcome).toHaveBeenCalledWith('s1', expect.objectContaining({ class: 'reply_positive' }));
    // The panel's change event re-reads both projections for the same card.
    expect(parley.listThreads).toHaveBeenCalledTimes(2);
    expect(parley.getBoard).toHaveBeenCalledTimes(2);
    wrapper.unmount();
  });

  it('quarantines dirty notes after a workspace restore and requires copy or discard before reload', async () => {
    const wrapper = stub(); await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Open card')!.trigger('click'); await flushPromises();
    const view = notesView(wrapper);
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: 'unacknowledged private notes' } });
    const clipboardWrite = vi.fn().mockResolvedValue(undefined);
    clipboardDescriptor = Object.getOwnPropertyDescriptor(navigator, 'clipboard');
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: clipboardWrite } });
    const refreshSession = vi.spyOn(parley, 'refreshSession').mockResolvedValue();
    globalThis.dispatchEvent(new globalThis.CustomEvent('parley:workspace-changed', { detail: { operation: 'restore' } }));
    await flushPromises();
    expect(wrapper.get('[role="alertdialog"]').get('textarea[aria-label="Quarantined notes text"]').element).toMatchObject({ value: 'unacknowledged private notes' });
    expect(parley.updateOpportunityNotes).not.toHaveBeenCalled();
    await wrapper.findAll('button').find((button) => button.text() === 'Copy quarantined notes')!.trigger('click'); await flushPromises();
    expect(clipboardWrite).toHaveBeenCalledWith('unacknowledged private notes');
    await wrapper.findAll('button').find((button) => button.text() === 'Discard local notes and load current workspace')!.trigger('click'); await flushPromises();
    expect(refreshSession).toHaveBeenCalledOnce();
    expect(parley.updateOpportunityNotes).not.toHaveBeenCalled();
    wrapper.unmount();
  });
});

describe('QueueWorkspace F-FR-1 open-card race', () => {
  it('keeps the last-clicked card when an earlier notes response resolves later', async () => {
    const opportunityB = { id: 'o2', title: 'Onboarding B', state: 'open', revision: 1, participant_ids: ['p1'], source_variant_ids: [], material_ids: [], tags: [] };
    const boardTwo: BoardSnapshot = {
      lists: [{ id: 'a', title: 'Today', position: 0, archived: false, revision: 1 }],
      cards: [
        { opportunity, opportunity_id: 'o1', list_id: 'a', position: 0, placement_revision: 1, next_task: 'Review checklist' },
        { opportunity: opportunityB, opportunity_id: 'o2', list_id: 'a', position: 1, placement_revision: 1, next_task: null },
      ],
    };
    function deferred<T>() {
      let resolve!: (value: T) => void;
      const promise = new Promise<T>((done) => { resolve = done; });
      return { promise, resolve };
    }
    const first = deferred<OpportunityNotes>();
    const second = deferred<OpportunityNotes>();
    vi.spyOn(parley, 'getOpportunityNotes')
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(() => second.promise);
    const wrapper = stub(boardTwo);
    await flushPromises();

    // Two route-driven opens race: o1 then o2, with o1's notes resolving
    // last and o2's resolving first. The stale o1 response must be dropped.
    await wrapper.setProps({ routeId: 'o1' });
    await flushPromises();
    await wrapper.setProps({ routeId: 'o2' });
    await flushPromises();
    second.resolve({ ...notes, opportunity_id: 'o2', markdown: 'B notes\n' });
    await flushPromises();
    first.resolve({ ...notes, markdown: 'A notes\n' });
    await flushPromises();

    expect(wrapper.text()).toContain('Onboarding B');
    expect(notesView(wrapper).state.doc.toString()).toContain('B notes');
    wrapper.unmount();
  });
});

describe('QueueWorkspace FE-09 filters and sort', () => {
  it('edits normalized opportunity tags and updates the card revision', async () => {
    const wrapper = stub(); await flushPromises();
    const tagInput = wrapper.get('input[aria-label="Edit tags for Onboarding"]');
    await tagInput.setValue('Onboarding, tested, Onboarding');
    await wrapper.findAll('button').find((button) => button.text() === 'Save tags')!.trigger('click');
    await flushPromises();
    expect(parley.setOpportunityTags).toHaveBeenCalledWith('o1', { expected_revision: 1, tags: ['onboarding', 'tested'] });
    expect(wrapper.text()).toContain('Tags saved for Onboarding.');
    wrapper.unmount();
  });

  it('filters the table and the board by an applied tag', async () => {
    const opportunityB = { id: 'o2', title: 'Onboarding B', state: 'open', revision: 1, participant_ids: ['p1'], source_variant_ids: [], material_ids: [], tags: [] };
    const boardTwo: BoardSnapshot = {
      lists: [{ id: 'a', title: 'Today', position: 0, archived: false, revision: 1 }],
      cards: [
        { opportunity, opportunity_id: 'o1', list_id: 'a', position: 0, placement_revision: 1, next_task: 'Review checklist' },
        { opportunity: opportunityB, opportunity_id: 'o2', list_id: 'a', position: 1, placement_revision: 1, next_task: null },
      ],
    };
    const wrapper = stub(boardTwo); await flushPromises();
    await wrapper.get('input[aria-label="Filter by tag"]').setValue('tested');
    await wrapper.findAll('button').find((button) => button.text() === 'Apply to board and table')!.trigger('click');
    await flushPromises();
    const rows = wrapper.findAll('tbody tr[data-opportunity-id]');
    expect(rows).toHaveLength(1);
    expect(rows[0]!.attributes('data-opportunity-id')).toBe('o1');
    expect(wrapper.get('[aria-label="Opportunity board"]').text()).toContain('Onboarding');
    expect(wrapper.get('[aria-label="Opportunity board"]').text()).not.toContain('Onboarding B');
    // Clearing the filter restores both views.
    await wrapper.get('input[aria-label="Filter by tag"]').setValue('');
    await wrapper.findAll('button').find((button) => button.text() === 'Apply to board and table')!.trigger('click');
    await flushPromises();
    expect(wrapper.findAll('tbody tr[data-opportunity-id]')).toHaveLength(2);
    wrapper.unmount();
  });

  it('sorts the table by next task with actionable rows first', async () => {
    const opportunityB = { id: 'o2', title: 'Onboarding B', state: 'open', revision: 1, participant_ids: ['p1'], source_variant_ids: [], material_ids: [], tags: [] };
    const boardTwo: BoardSnapshot = {
      lists: [{ id: 'a', title: 'Today', position: 0, archived: false, revision: 1 }],
      cards: [
        { opportunity, opportunity_id: 'o1', list_id: 'a', position: 0, placement_revision: 1, next_task: 'Review checklist' },
        { opportunity: opportunityB, opportunity_id: 'o2', list_id: 'a', position: 1, placement_revision: 1, next_task: null },
      ],
    };
    const wrapper = stub(boardTwo); await flushPromises();
    await wrapper.get('select[aria-label="Sort opportunities"]').setValue('next_task');
    await wrapper.findAll('button').find((button) => button.text() === 'Apply to board and table')!.trigger('click');
    await flushPromises();
    const rows = wrapper.findAll('tbody tr[data-opportunity-id]');
    expect(rows[0]!.attributes('data-opportunity-id')).toBe('o1');
    expect(rows[1]!.attributes('data-opportunity-id')).toBe('o2');
    wrapper.unmount();
  });
});
