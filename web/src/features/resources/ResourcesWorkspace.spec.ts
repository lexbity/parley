import { mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushPromises } from '@vue/test-utils';
import { commands } from '../../api/commands';
import { ApiError, parley, type ResourceBoardSnapshot } from '../../api/client';
import { createCommandRegistry, installCommandRegistry, useCommandRegistry } from '../../app/commands/registry';
import ResourcesWorkspace from './ResourcesWorkspace.vue';

installCommandRegistry(createCommandRegistry(vi.fn()));

const emptyBoard: ResourceBoardSnapshot = {
  columns: [
    { id: 'inbox', title: 'Inbox', position: 1024, revision: 1, kind: 'system' },
    { id: 'developing', title: 'Developing', position: 2048, revision: 1, kind: 'owner' },
    { id: 'available', title: 'Available', position: 3072, revision: 1, kind: 'owner' },
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
    revision: 1,
    content_revision: 1,
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
  vi.spyOn(parley, 'listAttachments').mockResolvedValue([]);
  vi.spyOn(parley, 'moveResourceCard').mockRejectedValue(new Error('not moved'));
  vi.spyOn(parley, 'commitResourceCard').mockRejectedValue(new Error('not committed'));
}

describe('ResourcesWorkspace', () => {
  it('renders the three initial lists with add entry points on an empty board', async () => {
    stubBoard();
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const lanes = wrapper.findAll('section[aria-label="Resource board"] .lane');
    expect(lanes.length).toBe(3);
    expect(lanes[0]!.text()).toContain('Inbox');
    expect(lanes[1]!.text()).toContain('Developing');
    expect(lanes[2]!.text()).toContain('Available');
    expect(wrapper.findAll('input[aria-label^="Add card to"]').length).toBe(3);
    const boardHeader = wrapper.get('section[aria-label="Resource board"] header.panel-header');
    expect(boardHeader.findAll('button').map(button => button.text())).toEqual(['Find resource']);
    expect(wrapper.get('.new-list button').text()).toContain('Add another list');
    wrapper.unmount();
  });

  it('opens the shared editor from a column and commits a new card with placement', async () => {
    stubBoard();
    const commit = vi.spyOn(parley, 'commitResourceCard').mockResolvedValue({
      entity: {
        id: 'new-1', kind: 'resource', title: 'Workshop outline', subtype: 'note',
        body: {}, readiness: 'draft', revision: 1, content_revision: 1,
        notes: { markdown: 'Notes', revision: 1 }, etag: '"p1.e.new-1.1"',
      },
      placement: { column_id: 'developing', position: 2048, revision: 1 },
      created_related: null,
      event_cursor: 4,
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const lane = wrapper.get('section.lane[aria-label="Developing"]');
    await lane.get('.card-inline input').setValue('Workshop outline');
    await lane.findAll('button').find(button => button.text() === 'More options')!.trigger('click');
    const form = wrapper.get('form[aria-label="Resource card editor"]');
    expect((form.get('input[maxlength="240"]').element as HTMLInputElement).value).toBe('Workshop outline');
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
    const tile = wrapper.get('.tile-body');
    await tile.trigger('click');
    await flushPromises();
    expect(wrapper.get('.tile.selected')).toBeTruthy();
    const moveSelect = wrapper.get('select[aria-label^="Move"]');
    await moveSelect.setValue('available');
    await flushPromises();
    expect(move).toHaveBeenCalledWith(expect.objectContaining({ resource_id: 'card-1', destination_column_id: 'available' }), expect.any(String));
    wrapper.unmount();
  });

  it('keeps placement when editing a board card', async () => {
    stubBoard(oneCardBoard);
    vi.spyOn(parley, 'getEntityCard').mockResolvedValue({
      id: 'card-1', kind: 'resource', title: 'Importer checklist', subtype: 'artifact',
      body: {}, readiness: 'draft', revision: 1, content_revision: 1,
      notes: { markdown: '', revision: 1 }, etag: '"p1.e.card-1.1"',
    });
    const commit = vi.spyOn(parley, 'commitResourceCard').mockResolvedValue({
      entity: {
        id: 'card-1', kind: 'resource', title: 'Importer checklist v2', subtype: 'artifact',
        body: {}, readiness: 'draft', revision: 2, content_revision: 2,
        notes: { markdown: '', revision: 1 }, etag: '"p1.e.card-1.2"',
      },
      placement: { column_id: 'developing', position: 1024, revision: 2 },
      created_related: null,
      event_cursor: 5,
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    await wrapper.get('.tile-body').trigger('click');
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
      }),
      expect.any(String),
    );
    expect(commit.mock.calls[0]?.[0]).not.toHaveProperty('placement');
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
        content_revision: 1,
        revision: 1,
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
    await wrapper.get('.tile-body').trigger('click');
    await flushPromises();
    const inspector = wrapper.get('section[aria-label="Resource inspector"]');
    expect(inspector.text()).toContain('Available, but still ineligible');
    expect(inspector.text()).toContain('Missing typed fields: Result, Context, Time, Owner role');
    expect(inspector.text()).toContain('Mira Chen');
    const record = wrapper.findAll('button').find((button) => button.text() === 'View sent version');
    expect(record).toBeTruthy();
    wrapper.unmount();
  });

  it('offers three-way conflict resolution and keeps the draft on retry', async () => {
    stubBoard(oneCardBoard);
    const initialCard = {
      id: 'card-1', kind: 'resource', title: 'Importer checklist', subtype: 'artifact',
      body: {}, readiness: 'draft', revision: 1, content_revision: 1,
      notes: { markdown: '', revision: 1 }, etag: '"p1.e.card-1.1"',
    };
    const currentCard = {
      id: 'card-1', kind: 'resource', title: 'Theirs', subtype: 'artifact',
      body: {}, readiness: 'draft', revision: 2, content_revision: 2,
      notes: { markdown: 'theirs', revision: 2 }, etag: '"p1.e.card-1.2"',
    };
    vi.spyOn(parley, 'getEntityCard').mockResolvedValueOnce(initialCard).mockResolvedValueOnce(initialCard).mockResolvedValue(currentCard);
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
    await wrapper.get('.tile-body').trigger('click');
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

  it('archives the selected card through the inspector', async () => {
    stubBoard(oneCardBoard);
    const retire = vi.spyOn(parley, 'retireResourceCard').mockResolvedValue({
      entity: { id: 'card-1', kind: 'resource', readiness: 'retired', revision: 2 },
      affected_versions: [],
    });
    const confirmSpy = vi.spyOn(globalThis, 'confirm').mockReturnValue(true);
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    await wrapper.get('.tile-body').trigger('click');
    await flushPromises();
    const inspectorButtons = wrapper.findAll('section[aria-label="Resource inspector"] button');
    const archiveButton = inspectorButtons.find((button) => button.text() === 'Archive card')!;
    await archiveButton.trigger('click');
    await flushPromises();
    expect(confirmSpy).toHaveBeenCalled();
    expect(retire).toHaveBeenCalledTimes(1);
    expect((retire.mock.calls[0]![0] as string)).toBe('card-1');
    wrapper.unmount();
  });

  it('erases the selected card through the inspector', async () => {
    stubBoard(oneCardBoard);
    const erase = vi.spyOn(parley, 'eraseResourceCard').mockResolvedValue({ id: 'card-1', erased: true });
    vi.spyOn(globalThis, 'confirm').mockReturnValue(true);
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    await wrapper.get('.tile-body').trigger('click');
    await flushPromises();
    const inspectorButtons = wrapper.findAll('section[aria-label="Resource inspector"] button');
    await inspectorButtons.find((button) => button.text() === 'Delete card')!.trigger('click');
    await flushPromises();
    expect(erase).toHaveBeenCalledTimes(1);
    expect((erase.mock.calls[0]![0] as string)).toBe('card-1');
    wrapper.unmount();
  });

  it('shows off-board resources in the picker and offers placement to legacy rows', async () => {
    stubBoard({
      ...emptyBoard,
      off_board: [
        {
          resource_id: 'off-1', subtype: 'artifact', title: 'Fixture walkthrough',
          readiness: 'draft', revision: 1, content_revision: 1, legacy: false,
          counts: { files: 0, connections: 0, proposed: 0, deliveries: 0 },
          gaps: [],
          etag: '"p1.e.off-1.1"',
        },
        {
          resource_id: 'legacy-1', subtype: 'note', title: 'Legacy checklist idea',
          readiness: 'draft', revision: 1, content_revision: 1, legacy: true,
          counts: { files: 0, connections: 0, proposed: 0, deliveries: 0 },
          gaps: [],
          etag: '"p1.e.legacy-1.1"',
        },
      ],
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    await wrapper.get('header.panel-header button').trigger('click');
    const picker = wrapper.get('dialog[aria-labelledby="resource-picker-title"]');
    expect(picker.text()).toContain('Fixture walkthrough');
    expect(picker.text()).toContain('Off board');
    const legacyRow = picker.findAll('li').find((item) => item.text().includes('Legacy checklist idea'))!;
    expect(legacyRow.text()).toContain('Off board');
    expect(legacyRow.find('select').exists()).toBe(true);
    wrapper.unmount();
  });

  it('offers Save / Discard / Stay when switching with a dirty draft', async () => {
    stubBoard(oneCardBoard);
    vi.spyOn(parley, 'getEntityCard').mockResolvedValue({
      id: 'card-1', kind: 'resource', title: 'Importer checklist', subtype: 'artifact',
      body: {}, readiness: 'draft', revision: 1, content_revision: 1,
      notes: { markdown: '', revision: 1 }, etag: '"p1.e.card-1.1"',
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    await wrapper.get('.tile-body').trigger('click');
    await flushPromises();
    await wrapper.findAll('section[aria-label="Resource board"] button').find((button) => button.text() === 'Edit')!.trigger('click');
    await flushPromises();
    const form = wrapper.get('form[aria-label="Resource card editor"]');
    await form.get('input[maxlength="240"]').setValue('Uncommitted local edit');

    // Opening the quick-add editor keeps the draft and offers the three-way switch.
    await wrapper.findAll('button').find((button) => button.text() === 'More options')!.trigger('click');
    const prompt = wrapper.get('[role="alertdialog"]');
    expect(prompt.text()).toContain('Unsaved card draft');
    await prompt.findAll('button').find((button) => button.text() === 'Stay')!.trigger('click');
    expect((wrapper.get('form[aria-label="Resource card editor"]').get('input[maxlength="240"]').element as HTMLInputElement).value).toBe('Uncommitted local edit');

    // Discard completes the pending switch into a fresh create editor.
    await wrapper.findAll('button').find((button) => button.text() === 'More options')!.trigger('click');
    await wrapper.get('[role="alertdialog"]').findAll('button').find((button) => button.text() === 'Discard unsaved changes')!.trigger('click');
    await flushPromises();
    expect((wrapper.get('form[aria-label="Resource card editor"]').get('input[maxlength="240"]').element as HTMLInputElement).value).toBe('');
    wrapper.unmount();
  });

  it('shows the graph root in the header and never recenters on an ordinary edit', async () => {
    stubBoard({
      ...oneCardBoard,
      cards: [
        ...oneCardBoard.cards,
        {
          resource_id: 'card-2', subtype: 'note', title: 'Workshop outline',
          readiness: 'draft', revision: 1, content_revision: 1, placement: { column_id: 'inbox', position: 1024, revision: 1 },
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
      edges: [{ id: 'e1', from_id: 'card-1', to_id: 'card-2', verb: 'includes', revision: 1, quote: '', tier: 3, speaker_kind: 'owner', captured_at: '2026-11-22T00:00:00Z', occurrence_id: null, source_title: null, source_url: null, claim_id: null, owner_authored: true }],
      omitted_nodes: 0, omitted_edges: 0,
    });
    vi.spyOn(parley, 'getEntityCard').mockImplementation(async (id: string) => ({
      id, kind: 'resource', title: id === 'card-2' ? 'Workshop outline' : 'Importer checklist',
      subtype: id === 'card-2' ? 'note' : 'artifact',
      body: {}, readiness: 'draft', revision: 1, content_revision: 1,
      notes: { markdown: '', revision: 1 }, etag: `"p1.e.${id}.1"`,
    }));
    const commit = vi.spyOn(parley, 'commitResourceCard').mockResolvedValue({
      entity: { id: 'card-2', kind: 'resource', title: 'Workshop outline v2', subtype: 'note', body: {}, readiness: 'draft', revision: 2, content_revision: 2, notes: { markdown: '', revision: 1 }, etag: '"p1.e.card-2.2"' },
      placement: null, created_related: null, event_cursor: 6,
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    // Selecting the checklist tile roots the graph at it.
    const tileFor = (title: string) => wrapper.findAll('.tile-body').find((tile) => tile.text().includes(title))!;
    await tileFor('Importer checklist').trigger('click');
    await flushPromises();
    expect(wrapper.get('section[aria-label="Connections graph"] header.panel-header h2').text()).toBe('Importer checklist · connections');

    // Editing a neighboring graph node keeps the board selection and graph root.
    const neighbor = wrapper.get('ul[aria-label="Graph cards"] li:nth-child(2) button');
    await neighbor.trigger('click');
    await flushPromises();
    const form = wrapper.get('form[aria-label="Resource card editor"]');
    await form.get('input[maxlength="240"]').setValue('Workshop outline v2');
    await form.trigger('submit');
    await flushPromises();
    expect(commit).toHaveBeenCalledWith(
      expect.objectContaining({ edit: expect.objectContaining({ resource_id: 'card-2' }) }),
      expect.any(String),
    );
    expect(wrapper.get('section[aria-label="Connections graph"] header.panel-header h2').text()).toBe('Importer checklist · connections');
    wrapper.unmount();
  });

  it('adds a card from the always-visible list input', async () => {
    stubBoard();
    const commit = vi.spyOn(parley, 'commitResourceCard').mockResolvedValue({
      entity: { id: 'new-1', kind: 'resource', title: 'Workshop outline', subtype: 'artifact', body: {}, readiness: 'draft', revision: 1, content_revision: 1, notes: { markdown: '', revision: 1 }, etag: '"p1.e.new-1.1"' },
      placement: { column_id: 'developing', position: 2048, revision: 1 },
      created_related: null,
      event_cursor: 4,
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const lane = wrapper.get('section.lane[aria-label="Developing"]');
    await lane.get('.card-inline input').setValue('Workshop outline');
    await lane.get('form.card-inline').trigger('submit');
    await flushPromises();
    expect(commit).toHaveBeenCalledWith(
      expect.objectContaining({
        create: expect.objectContaining({ title: 'Workshop outline', placement: { column_id: 'developing' } }),
      }),
      expect.any(String),
    );
    expect((lane.get('.card-inline input').element as HTMLInputElement).value).toBe('');
    wrapper.unmount();
  });

  it('renames a list in place through the lane menu', async () => {
    stubBoard();
    const rename = vi.spyOn(parley, 'renameResourceColumn').mockResolvedValue({ id: 'developing', title: 'Studio', position: 2048, revision: 2, kind: 'owner' });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const lane = wrapper.get('section.lane[aria-label="Developing"]');
    await lane.get('.lane-menu-button').trigger('click');
    await lane.get('[role="menu"]').findAll('button').find((button) => button.text() === 'Rename list')!.trigger('click');
    await flushPromises();
    const input = lane.get('.lane-rename input');
    await input.setValue('Studio');
    await lane.get('form.lane-rename').trigger('submit');
    await flushPromises();
    expect(rename).toHaveBeenCalledWith('developing', { title: 'Studio', expected_revision: 1 }, expect.any(String));
    wrapper.unmount();
  });

  it('demands a typed confirmation before removing the system list', async () => {
    stubBoard();
    const remove = vi.spyOn(parley, 'removeResourceColumn').mockResolvedValue({ id: 'inbox', off_board_count: 0 });
    const confirmSpy = vi.spyOn(globalThis, 'confirm').mockReturnValue(true);
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const lane = wrapper.get('section.lane[aria-label="Inbox"]');
    await lane.get('.lane-menu-button').trigger('click');
    await lane.get('[role="menu"]').findAll('button').find((button) => button.text() === 'Remove list')!.trigger('click');
    await flushPromises();
    expect(confirmSpy).not.toHaveBeenCalled();
    const dialog = wrapper.get('dialog[aria-labelledby="system-remove-title"]');
    expect(dialog.text()).toContain('system list');
    const input = dialog.get('input[aria-label="Confirm list name"]');
    await input.setValue('wrong name');
    await dialog.get('form').trigger('submit');
    expect(remove).not.toHaveBeenCalled();
    expect(dialog.text()).toContain('Type the list name exactly');
    await input.setValue('Inbox');
    await dialog.get('form').trigger('submit');
    await flushPromises();
    expect(remove).toHaveBeenCalledWith('inbox', { expected_revision: 1 }, expect.any(String));
    wrapper.unmount();
  });

  it('collapses an empty list into a pill and expands it again', async () => {
    stubBoard();
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const lane = () => wrapper.get('section.lane[aria-label="Inbox"]');
    await lane().get('.lane-menu-button').trigger('click');
    await lane().get('[role="menu"]').findAll('button').find((button) => button.text() === 'Collapse')!.trigger('click');
    const pill = wrapper.get('button[aria-label="Expand empty list Inbox"]');
    expect(pill.text()).toContain('Inbox');
    expect(wrapper.find('section.lane[aria-label="Inbox"]').exists()).toBe(false);
    await pill.trigger('click');
    expect(wrapper.find('section.lane[aria-label="Inbox"]').exists()).toBe(true);
    wrapper.unmount();
  });

  it('edits a card title in place on the tile', async () => {
    stubBoard(oneCardBoard);
    vi.spyOn(parley, 'getEntityCard').mockResolvedValue({
      id: 'card-1', kind: 'resource', title: 'Importer checklist', subtype: 'artifact',
      body: {}, readiness: 'draft', revision: 3, content_revision: 3,
      notes: { markdown: '', revision: 4 }, etag: '"p1.e.card-1.3"',
    });
    const commit = vi.spyOn(parley, 'commitResourceCard').mockResolvedValue({
      entity: { id: 'card-1', kind: 'resource', title: 'Importer checklist v2', subtype: 'artifact', body: {}, readiness: 'draft', revision: 4, content_revision: 4, notes: { markdown: '', revision: 4 }, etag: '"p1.e.card-1.4"' },
      placement: { column_id: 'developing', position: 1024, revision: 2 },
      created_related: null,
      event_cursor: 6,
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const lane = wrapper.get('section.lane[aria-label="Developing"]');
    // A double-click on the title opens the in-place editor.
    await lane.get('button.tile-title').trigger('dblclick');
    await flushPromises();
    const input = lane.get('.tile-body input');
    await input.setValue('Importer checklist v2');
    await input.trigger('keydown.enter');
    await flushPromises();
    expect(commit).toHaveBeenCalledWith(
      expect.objectContaining({
        edit: expect.objectContaining({ resource_id: 'card-1', expected_revision: 3, expected_notes_rev: 4, title: 'Importer checklist v2' }),
      }),
      expect.any(String),
    );
    wrapper.unmount();
  });

  it('keeps a lane draft when its list is removed and offers it in another lane', async () => {
    const withoutDeveloping: ResourceBoardSnapshot = {
      ...emptyBoard,
      columns: [emptyBoard.columns[0]!, emptyBoard.columns[2]!],
    };
    vi.spyOn(parley, 'getResourceBoard').mockResolvedValueOnce(emptyBoard).mockResolvedValue(withoutDeveloping);
    vi.spyOn(parley, 'getGraph').mockResolvedValue({ nodes: [], edges: [], omitted_nodes: 0, omitted_edges: 0 });
    vi.spyOn(parley, 'getEntityCard').mockRejectedValue(new Error('not opened'));
    vi.spyOn(parley, 'getEntityCardDeliveries').mockResolvedValue([]);
    vi.spyOn(parley, 'listAttachments').mockResolvedValue([]);
    const remove = vi.spyOn(parley, 'removeResourceColumn').mockResolvedValue({ id: 'developing', off_board_count: 0 });
    vi.spyOn(globalThis, 'confirm').mockReturnValue(true);
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const developing = wrapper.get('section.lane[aria-label="Developing"]');
    await developing.get('.card-inline input').setValue('Displaced thought');
    await developing.get('.lane-menu-button').trigger('click');
    await developing.get('[role="menu"]').findAll('button').find((button) => button.text() === 'Remove list')!.trigger('click');
    await flushPromises();
    expect(remove).toHaveBeenCalled();
    expect(wrapper.get('.notice').text()).toContain('Displaced thought');
    const inbox = wrapper.get('section.lane[aria-label="Inbox"]');
    await inbox.get('.card-inline input').trigger('focus');
    expect((inbox.get('.card-inline input').element as HTMLInputElement).value).toBe('Displaced thought');
    expect(wrapper.find('.notice').exists()).toBe(false);
    wrapper.unmount();
  });

  it('keeps the lane draft and shows the error when a quick add fails', async () => {
    stubBoard();
    vi.spyOn(parley, 'commitResourceCard').mockRejectedValue(new ApiError('nope', 422, 'INVALID_REQUEST'));
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const lane = wrapper.get('section.lane[aria-label="Developing"]');
    await lane.get('.card-inline input').setValue('Doomed card');
    await lane.get('form.card-inline').trigger('submit');
    await flushPromises();
    expect(lane.get('.card-inline p[role="alert"]').text()).toBe('nope');
    expect((lane.get('.card-inline input').element as HTMLInputElement).value).toBe('Doomed card');
    wrapper.unmount();
  });

  it('closes the rename and refreshes when the list revision is stale', async () => {
    stubBoard();
    const rename = vi.spyOn(parley, 'renameResourceColumn').mockRejectedValue(new ApiError('stale', 412, 'STALE_REVISION'));
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const lane = wrapper.get('section.lane[aria-label="Developing"]');
    await lane.get('.lane-menu-button').trigger('click');
    await lane.get('[role="menu"]').findAll('button').find((button) => button.text() === 'Rename list')!.trigger('click');
    await flushPromises();
    const input = lane.get('.lane-rename input');
    await input.setValue('Studio');
    await lane.get('form.lane-rename').trigger('submit');
    await flushPromises();
    expect(rename).toHaveBeenCalled();
    expect(wrapper.find('form.lane-rename').exists()).toBe(false);
    wrapper.unmount();
  });

  it('closes the lane menu on an outside pointer press', async () => {
    stubBoard();
    const wrapper = mount(ResourcesWorkspace, { attachTo: document.body });
    await flushPromises();
    const lane = wrapper.get('section.lane[aria-label="Inbox"]');
    await lane.get('.lane-menu-button').trigger('click');
    expect(lane.find('[role="menu"]').exists()).toBe(true);
    wrapper.get('header.panel-header').element.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true }));
    await flushPromises();
    expect(lane.find('[role="menu"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it('surfaces an erase refusal without clearing the board', async () => {
    stubBoard(oneCardBoard);
    vi.spyOn(parley, 'eraseResourceCard').mockRejectedValue(new ApiError('delivered', 409, 'RESOURCE_DELIVERED'));
    vi.spyOn(globalThis, 'confirm').mockReturnValue(true);
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const tileActions = wrapper.get('.tile').findAll('button');
    await tileActions.find((button) => button.text() === 'Delete')!.trigger('click');
    await flushPromises();
    expect(wrapper.get('p.workspace-status[role="status"]').text()).toBe('');
    expect(wrapper.get('p.workspace-status.notice[role="alert"]').text()).toContain('delivered');
    wrapper.unmount();
  });

  it('cancels the in-place title edit when the title is unchanged', async () => {
    stubBoard(oneCardBoard);
    vi.spyOn(parley, 'getEntityCard').mockResolvedValue({
      id: 'card-1', kind: 'resource', title: 'Importer checklist', subtype: 'artifact',
      body: {}, readiness: 'draft', revision: 1, content_revision: 1,
      notes: { markdown: '', revision: 1 }, etag: '"p1.e.card-1.1"',
    });
    const commit = vi.spyOn(parley, 'commitResourceCard');
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const lane = wrapper.get('section.lane[aria-label="Developing"]');
    await lane.get('button.tile-title').trigger('dblclick');
    await flushPromises();
    await lane.get('.tile-body input').trigger('keydown.enter');
    await flushPromises();
    expect(commit).not.toHaveBeenCalled();
    expect(lane.find('button.tile-title').exists()).toBe(true);
    wrapper.unmount();
  });

  it('validates the in-place title and keeps editing on stale revisions', async () => {
    stubBoard(oneCardBoard);
    vi.spyOn(parley, 'getEntityCard').mockResolvedValue({
      id: 'card-1', kind: 'resource', title: 'Importer checklist', subtype: 'artifact',
      body: {}, readiness: 'draft', revision: 1, content_revision: 1,
      notes: { markdown: '', revision: 1 }, etag: '"p1.e.card-1.1"',
    });
    vi.spyOn(parley, 'commitResourceCard').mockRejectedValue(new ApiError('stale', 412, 'STALE_REVISION'));
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const lane = wrapper.get('section.lane[aria-label="Developing"]');
    await lane.get('button.tile-title').trigger('dblclick');
    await flushPromises();
    const input = lane.get('.tile-body input');
    await input.setValue('');
    await input.trigger('keydown.enter');
    expect(lane.get('.tile-body p[role="alert"]').text()).toBe('Enter a title.');
    await input.setValue('r'.repeat(241));
    await input.trigger('keydown.enter');
    expect(lane.get('.tile-body p[role="alert"]').text()).toBe('Keep the title within 240 characters.');
    await input.setValue('Fresh title');
    await input.trigger('keydown.enter');
    await flushPromises();
    expect(lane.get('.tile-body p[role="alert"]').text()).toBe('stale');
    expect(lane.find('.tile-body input').exists()).toBe(true);
    await input.trigger('keydown.esc');
    expect(lane.find('button.tile-title').exists()).toBe(true);
    wrapper.unmount();
  });

  it('adds and cancels the list composer', async () => {
    stubBoard();
    const create = vi.spyOn(parley, 'createResourceColumn').mockResolvedValue({ id: 'c-new', title: 'Workshop', position: 4096, revision: 1, kind: 'owner' });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    await wrapper.get('.new-list > button').trigger('click');
    const input = wrapper.get('.new-list input[aria-label="List name"]');
    await input.setValue('Workshop');
    await wrapper.get('.new-list form.board-composer').trigger('submit');
    await flushPromises();
    expect(create).toHaveBeenCalledWith({ title: 'Workshop' }, expect.any(String));
    await wrapper.get('.new-list > button').trigger('click');
    await wrapper.get('.new-list input[aria-label="List name"]').setValue('Cancelled');
    await wrapper.get('.new-list input[aria-label="List name"]').trigger('keydown.esc');
    expect(wrapper.find('.new-list form.board-composer').exists()).toBe(false);
    wrapper.unmount();
  });
});

describe('ResourcesWorkspace scoped U06 command', () => {
  it('exposes mark-ready for the selected resource with its frozen content revision', async () => {
    const board: ResourceBoardSnapshot = {
      ...emptyBoard,
      cards: [{
        resource_id: 'card-1', subtype: 'gift', title: 'Gift card', readiness: 'draft',
        revision: 2, content_revision: 2, placement: { column_id: 'developing', position: 1024, revision: 1 },
        counts: { files: 0, connections: 0, proposed: 0, deliveries: 0 },
        gaps: ['what'], etag: '"p1.e.card-1.2"',
      }],
    };
    stubBoard(board);
    const invoke = vi.spyOn(commands, 'invoke').mockResolvedValue({
      ok: true, command_version: 1, audit_id: 'a', replayed: false, dry_run: false,
      result: { id: 'card-1', readiness: 'ready', content_revision: 3 },
    });
    const wrapper = mount(ResourcesWorkspace, { props: { routeId: 'card-1' } });
    await flushPromises();
    const ctx = { context: { route: 'resources', 'selection.resource': true }, selection: { resource: 'card-1' } };
    const actions = useCommandRegistry().list(ctx);
    const markReady = actions.find((action) => action.id === 'ui.resources.mark_ready');
    expect(markReady).toBeTruthy();
    await markReady!.run({}, ctx);
    expect(invoke).toHaveBeenCalledWith(
      'resource.mark_ready',
      { version: 1, args: { resource_id: 'card-1', expected_content_revision: 2 }, dry_run: false },
      expect.any(String),
    );
    wrapper.unmount();
  });

  it('picking an off-board resource publishes the deep link for palette selection', async () => {
    stubBoard({
      ...emptyBoard,
      off_board: [{
        resource_id: 'off-1', subtype: 'gift', title: 'Off gift', readiness: 'draft',
        revision: 1, content_revision: 1, legacy: false,
        counts: { files: 0, connections: 0, proposed: 0, deliveries: 0 },
        gaps: [], etag: '"p1.e.off-1.1"',
      }],
    });
    const wrapper = mount(ResourcesWorkspace);
    await flushPromises();
    const findResource = wrapper.findAll('button').find((button) => button.text() === 'Find resource')!;
    await findResource.trigger('click');
    await flushPromises();
    const picker = wrapper.get('ul[aria-label="Resource picker"]');
    await picker.get('button').trigger('click');
    expect(wrapper.emitted('navigate')?.[0]?.[0]).toBe('/resources/off-1');
    wrapper.unmount();
  });
});
