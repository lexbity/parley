import { flushPromises, mount } from '@vue/test-utils';
import { describe, expect, it, vi } from 'vitest';
import Board, { type BoardService, type BoardSnapshot } from './Board.vue';

const initial: BoardSnapshot = {
  board_revision: 8,
  lists: [
    { id: 'list-first', name: 'Research', position: 0, revision: 3, archived: false },
    { id: 'list-second', name: 'Follow-ups', position: 1, revision: 6, archived: false },
  ],
  items: [
    { id: 'op-a', revision:1, list_id: 'list-first', position: 0, title: 'Onboarding review', status: 'open', participants: [{ id: 'dana', kind: 'person', name: 'Dana Lee' }], tags: ['hiring'], next_task: 'Review checklist', deadline: '2026-11-25', missing_material: true },
    { id: 'op-b', revision:1, list_id: 'list-second', position: 0, title: 'Platform handoff', status: 'open', participants: [{ id: 'eli', kind: 'person', name: 'Eli Chen' }], tags: ['platform'], next_task: null, deadline: null, missing_material: false },
  ],
};

function makeService(overrides: Partial<BoardService> = {}): BoardService {
  return {
    listOpportunities: vi.fn(async () => initial),
    getNotes: vi.fn(async (id) => ({ opportunity_id: id, markdown: '- [ ] Prepare', revision: 1 })),
    saveNotes: vi.fn(async (id, input) => ({ markdown: `${id}:${input.markdown}`, revision: 2 })),
    toggleTask: vi.fn(async (id, input) => ({ markdown: `${id}:${input.checked ? 'checked' : 'unchecked'}`, revision: 2 })),
    createOpportunity: vi.fn(async (input) => ({ id: 'new-opportunity', ...input })),
    createList: vi.fn(async () => initial),
    renameList: vi.fn(async () => initial),
    reorderLists: vi.fn(async () => initial),
    archiveList: vi.fn(async () => initial),
    moveCard: vi.fn(async () => initial),
    reorderCard: vi.fn(async () => initial),
    ...overrides,
  };
}

describe('S05 opportunity board', () => {
  it('projects the same canonical opportunities in board and table views', async () => {
    const wrapper = mount(Board, { props: { service: makeService(), workspaceEpoch: 'epoch-1' } });
    await flushPromises();
    expect(wrapper.text()).toContain('Onboarding review');
    expect(wrapper.text()).toContain('Platform handoff');
    await wrapper.get('button[aria-pressed="false"]').trigger('click');
    expect(wrapper.get('table').text()).toContain('Onboarding review');
    expect(wrapper.get('table').text()).toContain('Platform handoff');
    expect(wrapper.text()).toContain('Research');
    expect(wrapper.text()).toContain('Follow-ups');
    wrapper.unmount();
  });

  it('filters the shared projection by participant, tag, deadline and missing material', async () => {
    const wrapper = mount(Board, { props: { service: makeService(), workspaceEpoch: 'epoch-1' } });
    await flushPromises();
    const participantLabel = wrapper.findAll('label').find((label) => label.text().startsWith('Participant'));
    await participantLabel?.get('select').setValue('dana');
    expect(wrapper.text()).toContain('Onboarding review');
    expect(wrapper.text()).not.toContain('Platform handoff');
    const selects = wrapper.findAll('label');
    const tagLabel = selects.find((label) => label.text().startsWith('Tag'));
    await tagLabel?.get('select').setValue('hiring');
    const missingLabel = selects.find((label) => label.text().startsWith('Material'));
    await missingLabel?.get('select').setValue('missing');
    expect(wrapper.text()).toContain('Onboarding review');
    await missingLabel?.get('select').setValue('ready');
    expect(wrapper.text()).toContain('No opportunities match these filters.');
    wrapper.unmount();
  });

  it('moves a canonical card with expected board and list revisions, with no send operation', async () => {
    const service = makeService();
    const wrapper = mount(Board, { props: { service, workspaceEpoch: 'epoch-1' } });
    await flushPromises();
    const move = wrapper.get('#move-op-a');
    await move.setValue('list-second');
    await flushPromises();
    expect(service.moveCard).toHaveBeenCalledWith({
      opportunity_id: 'op-a', destination_list_id: 'list-second', anchor_opportunity_id: null,
      expected_board_revision: 8, expected_source_revision: 3, expected_destination_revision: 6,
    }, expect.any(String));
    expect(Object.keys(service)).not.toContain('markSent');
    expect(wrapper.text()).toContain('Status, dates, and contact history are unchanged.');
    wrapper.unmount();
  });

  it('creates an opportunity with selected participants and does not create an outbound task', async () => {
    const service = makeService();
    const wrapper = mount(Board, { props: { service, workspaceEpoch: 'epoch-1', participantOptions: [{ id: 'dana', kind: 'person', name: 'Dana Lee' }] } });
    await flushPromises();
    await wrapper.get('input[maxlength="160"]').setValue('New onboarding exchange');
    await wrapper.get('form.create-opportunity select[multiple]').setValue(['dana']);
    await wrapper.get('form.create-opportunity').trigger('submit');
    await flushPromises();
    expect(service.createOpportunity).toHaveBeenCalledWith({ title: 'New onboarding exchange', participant_ids: ['dana'] }, expect.any(String));
    expect(wrapper.text()).toContain('Opportunity created. No outbound task was created.');
    wrapper.unmount();
  });

  it('reconciles an unknown opportunity creation with the same frozen request and idempotency key', async () => {
    const createOpportunity = vi.fn()
      .mockRejectedValueOnce(new TypeError('connection lost'))
      .mockResolvedValueOnce({ id: 'new-opportunity' });
    const service = makeService({ createOpportunity });
    const wrapper = mount(Board, { props: { service, workspaceEpoch: 'epoch-1' } });
    await flushPromises();
    await wrapper.get('input[maxlength="160"]').setValue('One durable request');
    const form = wrapper.get('form.create-opportunity');
    await form.trigger('submit');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('status unknown');
    const inputs = createOpportunity.mock.calls[0];
    expect(inputs?.[0]).toEqual({ title: 'One durable request', participant_ids: [] });
    expect(wrapper.findAll('button').some((button) => button.text().includes('Retry opportunity creation'))).toBe(true);
    await form.trigger('submit');
    await flushPromises();
    expect(createOpportunity).toHaveBeenCalledTimes(2);
    expect(createOpportunity.mock.calls[1]).toEqual(inputs);
    wrapper.unmount();
  });

  it('requires a destination before archiving a list and provides keyboard list reordering', async () => {
    const service = makeService();
    const wrapper = mount(Board, { props: { service, workspaceEpoch: 'epoch-1' } });
    await flushPromises();
    const archive = wrapper.findAll('button').find((button) => button.text() === 'Archive list');
    await archive?.trigger('click');
    expect(service.archiveList).not.toHaveBeenCalled();
    expect(wrapper.get('[role="alert"]').text()).toContain('Choose a destination list');
    await wrapper.get('button[aria-label="Move Research later"]').trigger('click');
    expect(service.reorderLists).toHaveBeenCalledWith({ ordered_ids: ['list-second', 'list-first'], expected_revision: 8 }, expect.any(String));
    wrapper.unmount();
  });
});
