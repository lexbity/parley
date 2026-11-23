import { mount } from '@vue/test-utils';
import { describe, expect, it } from 'vitest';
import type { GraphEntity } from '../../api/client';
import PartySheet from './PartySheet.vue';

const party: GraphEntity = { id: 'p1', kind: 'org', name: 'Fieldwork', focus: 'CSV tools', revision: 1, etag: '"e"' };
const other: GraphEntity = { id: 'p2', kind: 'person', name: 'Mira Chen', focus: '', revision: 1, etag: '"e"' };

function mountSheet(props: Record<string, unknown> = {}) {
  return mount(PartySheet, {
    props: {
      parties: [party, other],
      rootPartyId: 'p1',
      selectedRecordId: 'p1',
      query: '',
      kind: 'all',
      sort: 'name',
      focusEdit: null,
      focusDraft: '',
      busy: false,
      ...props,
    },
  });
}

describe('PartySheet', () => {
  it('lists every party row with type and focus columns', () => {
    const wrapper = mountSheet();
    const rows = wrapper.findAll('tbody tr');
    expect(rows).toHaveLength(2);
    expect(rows[0]!.text()).toContain('Fieldwork');
    expect(rows[0]!.text()).toContain('Organization');
    expect(rows[0]!.text()).toContain('CSV tools');
    expect(rows[1]!.text()).toContain('Person');
    expect(rows[1]!.text()).toContain('Add focus');
  });

  it('emits selection on row click and tracks the root row', async () => {
    const wrapper = mountSheet();
    await wrapper.findAll('.row-party')[1]!.trigger('click');
    expect(wrapper.emitted('select-row')![0]).toEqual(['p2']);
    expect(wrapper.findAll('tbody tr')[0]!.classes()).toContain('root');
    expect(wrapper.findAll('tbody tr')[1]!.classes()).not.toContain('root');
  });

  it('filters by query and kind and sorts by focus', async () => {
    const wrapper = mountSheet({ query: 'mira' });
    expect(wrapper.findAll('tbody tr')).toHaveLength(1);
    await wrapper.setProps({ query: '', kind: 'org' });
    expect(wrapper.findAll('tbody tr')).toHaveLength(1);
    await wrapper.setProps({ kind: 'all', sort: 'focus' });
    expect(wrapper.findAll('tbody tr')[0]!.text()).toContain('Mira Chen');
  });

  it('edits a focus cell through events', async () => {
    const wrapper = mountSheet({ focusEdit: 'p2', focusDraft: 'Maintenance' });
    const input = wrapper.get('.focus-input');
    await input.setValue('Maintenance');
    await wrapper.findAll('.focus-input')[0]!.trigger('keydown.enter');
    expect(wrapper.emitted('save-focus')![0]).toEqual(['p2']);
    await wrapper.findAll('.focus-input')[0]!.trigger('keydown.esc');
    expect(wrapper.emitted('cancel-focus')).toHaveLength(1);
  });
});
