import { mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushPromises } from '@vue/test-utils';
import { ApiError, parley, type ResourceBoardSnapshot } from '../../api/client';
import ResourcesWorkspace from './ResourcesWorkspace.vue';

const emptyBoard: ResourceBoardSnapshot = {
  columns: [
    { id: 'inbox', title: 'Inbox', position: 1024, revision: 1 },
    { id: 'developing', title: 'Developing', position: 2048, revision: 1 },
    { id: 'available', title: 'Available', position: 3072, revision: 1 },
  ],
  cards: [],
  off_board: [],
  next_cursor: null,
  event_cursor: 0,
};

const oneCardBoard: ResourceBoardSnapshot = {
  ...emptyBoard,
  cards: [{
    resource_id: 'card-1',
    subtype: 'artifact',
    title: 'Importer checklist',
    readiness: 'draft',
    placement: { column_id: 'developing', position: 1024, revision: 1 },
    counts: { files: 0, connections: 0, proposed: 0, deliveries: 0 },
    gaps: [],
    etag: '"p1.e.card-1.1"',
  }],
};

afterEach(() => vi.restoreAllMocks());

function stubBoard(board: ResourceBoardSnapshot = emptyBoard, graph = { nodes: [], edges: [], omitted_nodes: 0, omitted_edges: 0 }) {
  vi.spyOn(parley, 'getResourceBoard').mockResolvedValue(board);
  vi.spyOn(parley, 'getGraph').mockResolvedValue(graph);
  vi.spyOn(parley, 'getEntityCard').mockRejectedValue(new Error('not opened'));
  vi.spyOn(parley, 'getEntityCardDeliveries').mockResolvedValue([]);
  vi.spyOn(parley, 'moveResourceCard').mockRejectedValue(new Error('not moved'));
  vi.spyOn(parley, 'commitResourceCard').mockRejectedValue(new Error('not committed'));
}

describe('ResourcesWorkspace', () => {
  it('renders the three fixed columns with add entry points on an empty board', async () => {
    stubBoard();
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const lanes = wrapper.findAll('section[aria-label="Resource board"] .lane');
    expect(lanes.length).toBe(3);
    expect(lanes[0]!.text()).toContain('Inbox');
    expect(lanes[1]!.text()).toContain('Developing');
    expect(lanes[2]!.text()).toContain('Available');
    expect(wrapper.findAll('button.add-card').length).toBe(3);
    // The column-local entry is the board's only creation entry (spec §1).
    const boardHeader = wrapper.get('section[aria-label="Resource board"] header.pane-bar');
    expect(boardHeader.findAll('button').length).toBe(0);
    wrapper.unmount();
  });

  it('opens the shared editor from a column and commits a new card with placement', async () => {
    stubBoard();
    const commit = vi.spyOn(parley, 'commitResourceCard').mockResolvedValue({
      entity: {
        id: 'new-1', kind: 'resource', title: 'Workshop outline', subtype: 'note',
        body: {}, readiness: 'draft', revision: 1,
        notes: { markdown: 'Notes', revision: 1 }, etag: '"p1.e.new-1.1"',
      },
      placement: { column_id: 'developing', position: 2048, revision: 1 },
      created_related: null,
      event_cursor: 4,
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    await wrapper.findAll('button.add-card')[1]!.trigger('click');
    const form = wrapper.get('form[aria-label="Resource card editor"]');
    expect(form).toBeTruthy();
    await form.get('input[maxlength="240"]').setValue('Workshop outline');
    await form.get('select').setValue('note');
    await form.trigger('submit');
    await flushPromises();
    expect(commit).toHaveBeenCalledWith(
      expect.objectContaining({
        create: expect.objectContaining({
          title: 'Workshop outline',
          subtype: 'note',
          placement: { column_id: 'developing', before_resource_id: null },
        }),
      }),
      expect.any(String),
    );
    // Save closes the editor and selects the created card.
    expect(wrapper.find('form[aria-label="Resource card editor"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it('selects a tile and moves it to another column through the accessible control', async () => {
    const board = oneCardBoard;
    stubBoard(board);
    const move = vi.spyOn(parley, 'moveResourceCard').mockResolvedValue({
      placement: { column_id: 'available', position: 1024, revision: 2 },
      source_column_revision: 2,
      destination_column_revision: 2,
    });
    vi.spyOn(parley, 'getGraph').mockResolvedValue({
      nodes: [{ id: 'card-1', kind: 'resource', name: 'Importer checklist', focus: '', revision: 2, x: 0, y: 0, position_revision: 1 }],
      edges: [], omitted_nodes: 0, omitted_edges: 0,
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const tile = wrapper.get('button.tile-body');
    await tile.trigger('click');
    await flushPromises();
    expect(wrapper.get('.tile.active')).toBeTruthy();
    const moveSelect = wrapper.get('select[aria-label^="Move"]');
    await moveSelect.setValue('available');
    await flushPromises();
    expect(move).toHaveBeenCalledWith(expect.objectContaining({ resource_id: 'card-1', destination_column_id: 'available' }));
    wrapper.unmount();
  });

  it('keeps placement when editing a board card', async () => {
    stubBoard(oneCardBoard);
    vi.spyOn(parley, 'getEntityCard').mockResolvedValue({
      id: 'card-1', kind: 'resource', title: 'Importer checklist', subtype: 'artifact',
      body: {}, readiness: 'draft', revision: 1,
      notes: { markdown: '', revision: 1 }, etag: '"p1.e.card-1.1"',
    });
    const commit = vi.spyOn(parley, 'commitResourceCard').mockResolvedValue({
      entity: {
        id: 'card-1', kind: 'resource', title: 'Importer checklist v2', subtype: 'artifact',
        body: {}, readiness: 'draft', revision: 2,
        notes: { markdown: '', revision: 1 }, etag: '"p1.e.card-1.2"',
      },
      placement: { column_id: 'developing', position: 1024, revision: 2 },
      created_related: null,
      event_cursor: 5,
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    await wrapper.get('button.tile-body').trigger('click');
    await flushPromises();
    const boardButtons = wrapper.findAll('section[aria-label="Resource board"] button');
    const editButton = boardButtons.find((button) => button.text() === 'Edit')!;
    await editButton.trigger('click');
    await flushPromises();
    const form = wrapper.get('form[aria-label="Resource card editor"]');
    const selects = form.findAll('select');
    expect((selects[selects.length - 1]!.element as HTMLSelectElement).value).toBe('developing');
    await form.get('input[maxlength="240"]').setValue('Importer checklist v2');
    await form.trigger('submit');
    await flushPromises();
    expect(commit).toHaveBeenCalledWith(
      expect.objectContaining({
        edit: expect.objectContaining({ resource_id: 'card-1' }),
        placement: { column_id: 'developing', before_resource_id: null },
      }),
      expect.any(String),
    );
    wrapper.unmount();
  });

  it('surfaces typed gaps and delivery records in the inspector', async () => {
    const board: ResourceBoardSnapshot = {
      ...emptyBoard,
      cards: [{
        resource_id: 'card-1',
        subtype: 'proof',
        title: 'Unfinished proof',
        readiness: 'draft',
        placement: { column_id: 'available', position: 1024, revision: 1 },
        counts: { files: 0, connections: 0, proposed: 0, deliveries: 2 },
        gaps: ['Result', 'Context', 'Time', 'Owner role'],
        etag: '"p1.e.card-1.1"',
      }],
    };
    stubBoard(board);
    vi.spyOn(parley, 'getEntityCardDeliveries').mockResolvedValue([{
      send_id: 's1', sent_at: '2026-11-18T09:00:00Z',
      recipient_id: 'p1', recipient_name: 'Mira Chen',
      opportunity_id: 'o1', opportunity_title: 'Northline review',
      material_id: 'm1', material_version: 1, material_kind: 'outreach',
    }]);
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    await wrapper.get('button.tile-body').trigger('click');
    await flushPromises();
    const inspector = wrapper.get('section[aria-label="Resource inspector"]');
    expect(inspector.text()).toContain('Available, but still ineligible');
    expect(inspector.text()).toContain('Missing typed fields: Result, Context, Time, Owner role');
    expect(inspector.text()).toContain('Mira Chen');
    const record = wrapper.findAll('button').find((button) => button.text() === 'Record delivery…');
    expect(record).toBeTruthy();
    wrapper.unmount();
  });

  it('offers three-way conflict resolution and keeps the draft on retry', async () => {
    stubBoard(oneCardBoard);
    const initialCard = {
      id: 'card-1', kind: 'resource', title: 'Importer checklist', subtype: 'artifact',
      body: {}, readiness: 'draft', revision: 1,
      notes: { markdown: '', revision: 1 }, etag: '"p1.e.card-1.1"',
    };
    const currentCard = {
      id: 'card-1', kind: 'resource', title: 'Theirs', subtype: 'artifact',
      body: {}, readiness: 'draft', revision: 2,
      notes: { markdown: 'theirs', revision: 2 }, etag: '"p1.e.card-1.2"',
    };
    vi.spyOn(parley, 'getEntityCard').mockResolvedValueOnce(initialCard).mockResolvedValueOnce(currentCard);
    let commits = 0;
    vi.spyOn(parley, 'commitResourceCard').mockImplementation(async () => {
      commits += 1;
      if (commits === 1) throw new ApiError('stale', 412, 'STALE_REVISION');
      return {
        entity: { ...currentCard, title: 'Mine kept' },
        placement: { column_id: 'developing', position: 1024, revision: 2 },
        created_related: null,
        affected_versions: [],
        event_cursor: 9,
      };
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    await wrapper.get('button.tile-body').trigger('click');
    await flushPromises();
    const boardButtons = wrapper.findAll('section[aria-label="Resource board"] button');
    const editButton = boardButtons.find((button) => button.text() === 'Edit')!;
    await editButton.trigger('click');
    await flushPromises();
    const form = wrapper.get('form[aria-label="Resource card editor"]');
    await form.get('input[maxlength="240"]').setValue('Mine kept');
    await form.trigger('submit');
    await flushPromises();
    const conflictBox = wrapper.get('.conflict-box');
    expect(conflictBox.text()).toContain('Base');
    expect(conflictBox.text()).toContain('Mine kept');
    expect(conflictBox.text()).toContain('Theirs');
    const retryButton = conflictBox.findAll('button').find((button) => button.text() === 'Keep my edits and retry')!;
    await retryButton.trigger('click');
    await form.trigger('submit');
    await flushPromises();
    expect(commits).toBe(2);
    expect(wrapper.find('form[aria-label="Resource card editor"]').exists()).toBe(false);
    expect(wrapper.get('.workspace-status').text()).toContain('Card saved.');
    wrapper.unmount();
  });

  it('retires the selected card through the inspector', async () => {
    stubBoard(oneCardBoard);
    const retire = vi.spyOn(parley, 'retireResourceCard').mockResolvedValue({
      entity: { id: 'card-1', kind: 'resource', readiness: 'retired', revision: 2 },
      affected_versions: [],
    });
    const confirmSpy = vi.spyOn(globalThis, 'confirm').mockReturnValue(true);
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    await wrapper.get('button.tile-body').trigger('click');
    await flushPromises();
    const inspectorButtons = wrapper.findAll('section[aria-label="Resource inspector"] button');
    const retireButton = inspectorButtons.find((button) => button.text() === 'Retire card')!;
    await retireButton.trigger('click');
    await flushPromises();
    expect(confirmSpy).toHaveBeenCalled();
    expect(retire).toHaveBeenCalledTimes(1);
    expect((retire.mock.calls[0]![0] as string)).toBe('card-1');
    wrapper.unmount();
  });

  it('shows off-board resources in the picker and offers placement to legacy rows', async () => {
    stubBoard({
      ...emptyBoard,
      off_board: [
        {
          resource_id: 'off-1', subtype: 'artifact', title: 'Fixture walkthrough',
          readiness: 'draft', legacy: false,
          counts: { files: 0, connections: 0, proposed: 0, deliveries: 0 },
          gaps: [],
          etag: '"p1.e.off-1.1"',
        },
        {
          resource_id: 'legacy-1', subtype: 'note', title: 'Legacy checklist idea',
          readiness: 'draft', legacy: true,
          counts: { files: 0, connections: 0, proposed: 0, deliveries: 0 },
          gaps: [],
          etag: '"p1.e.legacy-1.1"',
        },
      ],
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const picker = wrapper.get('details.picker');
    expect(picker.text()).toContain('Fixture walkthrough');
    expect(picker.text()).toContain('off board');
    const legacyRow = picker.findAll('li').find((item) => item.text().includes('Legacy checklist idea'))!;
    expect(legacyRow.text()).toContain('legacy (off board)');
    expect(legacyRow.find('select').exists()).toBe(true);
    wrapper.unmount();
  });

  it('offers Save / Discard / Stay when switching with a dirty draft', async () => {
    stubBoard(oneCardBoard);
    vi.spyOn(parley, 'getEntityCard').mockResolvedValue({
      id: 'card-1', kind: 'resource', title: 'Importer checklist', subtype: 'artifact',
      body: {}, readiness: 'draft', revision: 1,
      notes: { markdown: '', revision: 1 }, etag: '"p1.e.card-1.1"',
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    await wrapper.get('button.tile-body').trigger('click');
    await flushPromises();
    await wrapper.findAll('section[aria-label="Resource board"] button').find((button) => button.text() === 'Edit')!.trigger('click');
    await flushPromises();
    const form = wrapper.get('form[aria-label="Resource card editor"]');
    await form.get('input[maxlength="240"]').setValue('Uncommitted local edit');

    // Opening another card keeps the draft and offers the three-way switch.
    await wrapper.findAll('section[aria-label="Resource board"] button').find((button) => button.text() === 'Edit')!.trigger('click');
    const prompt = wrapper.get('[role="alertdialog"]');
    expect(prompt.text()).toContain('Unsaved card draft');
    await prompt.findAll('button').find((button) => button.text() === 'Stay')!.trigger('click');
    expect((wrapper.get('input[maxlength="240"]').element as HTMLInputElement).value).toBe('Uncommitted local edit');

    // Discard clears the draft and completes the pending switch.
    await wrapper.findAll('section[aria-label="Resource board"] button').find((button) => button.text() === 'Edit')!.trigger('click');
    await wrapper.get('[role="alertdialog"]').findAll('button').find((button) => button.text() === 'Discard unsaved changes')!.trigger('click');
    await flushPromises();
    expect((wrapper.get('input[maxlength="240"]').element as HTMLInputElement).value).toBe('Importer checklist');
    wrapper.unmount();
  });

  it('shows the graph root in the header and never recenters on an ordinary edit', async () => {
    stubBoard({
      ...oneCardBoard,
      cards: [
        ...oneCardBoard.cards,
        {
          resource_id: 'card-2', subtype: 'note', title: 'Workshop outline',
          readiness: 'draft', placement: { column_id: 'inbox', position: 1024, revision: 1 },
          counts: { files: 0, connections: 0, proposed: 0, deliveries: 0 },
          gaps: [], etag: '"p1.e.card-2.1"',
        },
      ],
    });
    vi.spyOn(parley, 'getGraph').mockResolvedValue({
      nodes: [
        { id: 'card-1', kind: 'resource', name: 'Importer checklist', focus: '', revision: 1, x: 0, y: 0, position_revision: 0 },
        { id: 'card-2', kind: 'resource', name: 'Workshop outline', focus: '', revision: 1, x: 10, y: 0, position_revision: 0 },
      ],
      edges: [{ id: 'e1', from_id: 'card-1', to_id: 'card-2', verb: 'includes', revision: 1, quote: '', tier: 3, speaker_kind: 'owner', captured_at: '2026-11-22T00:00:00Z', occurrence_id: null, source_title: null, source_url: null, claim_id: null, state: 'active' }],
      omitted_nodes: 0, omitted_edges: 0,
    });
    vi.spyOn(parley, 'getEntityCard').mockImplementation(async (id: string) => ({
      id, kind: 'resource', title: id === 'card-2' ? 'Workshop outline' : 'Importer checklist',
      subtype: id === 'card-2' ? 'note' : 'artifact',
      body: {}, readiness: 'draft', revision: 1,
      notes: { markdown: '', revision: 1 }, etag: `"p1.e.${id}.1"`,
    }));
    const commit = vi.spyOn(parley, 'commitResourceCard').mockResolvedValue({
      entity: { id: 'card-2', kind: 'resource', title: 'Workshop outline v2', subtype: 'note', body: {}, readiness: 'draft', revision: 2, notes: { markdown: '', revision: 1 }, etag: '"p1.e.card-2.2"' },
      placement: null, created_related: null, event_cursor: 6,
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    // Selecting the checklist tile roots the graph at it.
    const tileFor = (title: string) => wrapper.findAll('button.tile-body').find((tile) => tile.text().includes(title))!;
    await tileFor('Importer checklist').trigger('click');
    await flushPromises();
    expect(wrapper.get('section[aria-label="Connections graph"] header.pane-bar h3').text()).toBe('Importer checklist · connections');

    // Edit the outline card via its tile Edit button; root stays on the selection.
    await tileFor('Importer checklist').trigger('click');
    await flushPromises();
    const outlineTile = wrapper.findAll('.tile').find((tile) => tile.text().includes('Workshop outline'))!;
    await outlineTile.findAll('button').find((button) => button.text() === 'Edit')!.trigger('click');
    await flushPromises();
    const form = wrapper.get('form[aria-label="Resource card editor"]');
    await form.get('input[maxlength="240"]').setValue('Workshop outline v2');
    await form.trigger('submit');
    await flushPromises();
    expect(commit).toHaveBeenCalledWith(
      expect.objectContaining({ edit: expect.objectContaining({ resource_id: 'card-2' }) }),
      expect.any(String),
    );
    expect(wrapper.get('section[aria-label="Connections graph"] header.pane-bar h3').text()).toBe('Importer checklist · connections');
    wrapper.unmount();
  });
});
