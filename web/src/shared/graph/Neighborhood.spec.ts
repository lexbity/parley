import { flushPromises, mount } from '@vue/test-utils';
import { describe, expect, it, vi } from 'vitest';
import { parley, type Neighborhood } from '../../api/client';
import NeighborhoodView from './Neighborhood.vue';

vi.mock('@xyflow/vue', () => ({ VueFlow: { name: 'VueFlow', props: ['nodes', 'edges'], template: '<div />' } }));

const graph: Neighborhood = {
  focus_id: 'person-1',
  nodes: [
    { id: 'person-1', kind: 'person', name: 'Dana', descriptor: 'Northline', aliases: [], tags: [], revision: 1 },
    { id: 'org-1', kind: 'org', name: 'Northline', descriptor: '', aliases: [], tags: [], revision: 1 },
  ],
  edges: [{
    id: 'edge-1', from_id: 'person-1', to_id: 'org-1', verb: 'member_of', revision: 1,
    claim_id: 'claim-1', claim_revision: 1, wording: 'Dana will join onboarding.', approved_wording: null,
    tier: 1, speaker_kind: 'party', speaker_entity_id: 'person-1', occurrence_id: 'source-1',
    source_start: 0, source_end: 25, normalization_version: 1, source_title: 'Hiring update', source_url: null,
  }],
  positions: [{ entity_id: 'person-1', x: 10, y: 20, pinned: true, revision: 1 }],
  omitted_nodes: 2,
  omitted_edges: 1,
};

describe('Neighborhood evidence and layout', () => {
  it('inspects, corrects, refreshes, opens source and persists new and moved positions', async () => {
    const correct = vi.spyOn(parley, 'correctClaim').mockResolvedValue({ claim_id: 'claim-1', revision: 2, approved_wording: 'Dana will collaborate on onboarding.' });
    const layout = vi.spyOn(parley, 'saveLayout').mockResolvedValue();
    const wrapper = mount(NeighborhoodView, { props: { focusId: 'person-1', graph } });
    const flow = wrapper.findComponent({ name: 'VueFlow' });
    expect(flow.props('nodes')).toHaveLength(2);
    expect(flow.props('nodes')[0].position).toEqual({ x: 10, y: 20 });
    expect(flow.props('nodes')[1].position).toEqual({ x: 300, y: 70 });
    expect(wrapper.text()).toContain('2 additional nodes and 1 additional relationships are omitted');
    await flow.vm.$emit('edge-click', {}, { id: 'edge-1' });
    await flushPromises();
    await wrapper.get('.relationship-list button').trigger('click');
    expect(wrapper.text()).toContain('Speaker: party · Dana · T1');
    expect(wrapper.text()).toContain('Hiring update');
    await wrapper.findAll('button').find((button) => button.text() === 'Open original source')!.trigger('click');
    expect(wrapper.emitted('source')).toEqual([['source-1']]);
    await wrapper.get('textarea').setValue('Dana will collaborate on onboarding.');
    await wrapper.findAll('button').find((button) => button.text() === 'Save correction')!.trigger('click');
    await flushPromises();
    expect(correct).toHaveBeenCalledWith('claim-1', 1, 'Dana will collaborate on onboarding.');
    expect(wrapper.emitted('refresh')).toHaveLength(1);
    await flow.vm.$emit('node-drag-stop', {}, { id: 'org-1', position: { x: 320, y: 90 } });
    await flushPromises();
    await flow.vm.$emit('node-drag-stop', {}, { id: 'person-1', position: { x: 15, y: 25 } });
    await flushPromises();
    expect(layout).toHaveBeenCalledTimes(2);
    expect(layout).toHaveBeenNthCalledWith(1, { view_scope: 'party', view_id: 'person-1', entity_id: 'org-1', expected_revision: 0, x: 320, y: 90 });
    expect(layout).toHaveBeenNthCalledWith(2, { view_scope: 'party', view_id: 'person-1', entity_id: 'person-1', expected_revision: 1, x: 15, y: 25 });
    wrapper.unmount();
  });

  it('keeps an empty neighborhood readable and reports correction and layout failures', async () => {
    const empty: Neighborhood = { focus_id: 'person-1', nodes: [], edges: [], positions: [], omitted_nodes: 0, omitted_edges: 0 };
    const wrapper = mount(NeighborhoodView, { props: { focusId: 'person-1', graph: empty } });
    expect(wrapper.text()).toContain('No saved relationships in this neighborhood');
    await wrapper.setProps({ graph: graph });
    await wrapper.get('.relationship-list button').trigger('click');
    vi.spyOn(parley, 'correctClaim').mockRejectedValue(new Error('revision conflict'));
    await wrapper.get('textarea').setValue('Corrected wording');
    await wrapper.findAll('button').find((button) => button.text() === 'Save correction')!.trigger('click');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toBe('revision conflict');
    vi.spyOn(parley, 'saveLayout').mockRejectedValue(new Error('layout conflict'));
    const flow = wrapper.findComponent({ name: 'VueFlow' });
    await flow.vm.$emit('node-drag-stop', {}, { id: 'org-1', position: { x: 321, y: 90 } });
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('layout conflict');
    expect(wrapper.emitted('refresh')).toHaveLength(1);
    wrapper.unmount();
  });
});
