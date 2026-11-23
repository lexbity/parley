import { mount, flushPromises } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type GraphEntity, type GraphSnapshot } from '../../api/client';
import ClaimCorrection from './ClaimCorrection.vue';

const parties: GraphEntity[] = [
  { id: 'p1', kind: 'person', name: 'Dana Lee', focus: 'Hiring', revision: 1 },
];

function edge(overrides: Partial<GraphSnapshot['edges'][number]> = {}): GraphSnapshot['edges'][number] {
  return {
    id: 'e1', from_id: 'p1', to_id: 'o1', verb: 'offers_to', revision: 1, occurrence_id: 'occ-1',
    owner_authored: false, quote: 'Dana offered a case study', tier: 2, speaker_kind: 'owner',
    captured_at: '2026-11-23T00:00:00Z', source_title: null, source_url: null,
    claim_id: 'c1', claim_revision: 3, speaker_entity_id: null, ...overrides,
  } as GraphSnapshot['edges'][number];
}

afterEach(() => vi.restoreAllMocks());

describe('ClaimCorrection', () => {
  it('reuses the operation key for an unchanged intent and re-keys after an edit', async () => {
    const revise = vi.spyOn(parley, 'reviseClaim').mockResolvedValue();
    const wrapper = mount(ClaimCorrection, { props: { edge: edge(), parties } });
    await wrapper.get('textarea').setValue('Dana offered a proof document');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(revise).toHaveBeenCalledWith('c1', expect.objectContaining({
      expected_revision: 3, wording: 'Dana offered a proof document', speaker_kind: 'owner', speaker_entity_id: null,
    }), expect.any(String));
    const firstKey = revise.mock.calls[0]![2];
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(revise.mock.calls[1]![2]).toBe(firstKey);
    await wrapper.get('select').setValue('party');
    await wrapper.get('form').findAll('select')[1]!.setValue('p1');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(revise.mock.calls[2]![2]).not.toBe(firstKey);
    expect(revise.mock.calls[2]![1]).toMatchObject({ speaker_kind: 'party', speaker_entity_id: 'p1' });
    expect(wrapper.emitted('saved')).toHaveLength(3);
    wrapper.unmount();
  });

  it('shows a revision failure and offers cancel', async () => {
    vi.spyOn(parley, 'reviseClaim').mockRejectedValue(new Error('claim changed'));
    const wrapper = mount(ClaimCorrection, { props: { edge: edge(), parties } });
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('claim changed');
    await wrapper.findAll('button').find((button) => button.text() === 'Cancel')!.trigger('click');
    expect(wrapper.emitted('cancel')).toHaveLength(1);
    const incomplete = mount(ClaimCorrection, { props: { edge: edge({ claim_id: null, claim_revision: null }), parties } });
    await incomplete.get('form').trigger('submit');
    expect(parley.reviseClaim).toHaveBeenCalledTimes(1);
    incomplete.unmount();
    wrapper.unmount();
  });
});
