import { mount } from '@vue/test-utils';
import { describe, expect, it } from 'vitest';
import type { Dossier, GraphEntity } from '../../api/client';
import PartyInspector from './PartyInspector.vue';

const party: GraphEntity = { id: 'p1', kind: 'org', name: 'Fieldwork', focus: 'CSV tools', revision: 2, etag: '"e"' };
const dossier: Dossier = {
  entity: party,
  direct_links: [{ id: 'e1', from_id: 'p2', to_id: 'p1', verb: 'member_of', revision: 1, tier: 1, wording: 'Mira maintains the CSV tools.' }],
  files: [{ id: 'a1', filename: 'guide.pdf', content_type: 'application/pdf', size: 1024, sha256: 'a'.repeat(64), share_permission: false, reviewed_by_owner: true, created_at: '2026-11-22T00:00:00Z' }],
  opportunities: [{ id: 'o1', title: 'Onboarding exchange', role: 'primary' }],
  associated_resources: [{
    id: 'r1', title: 'Checklist', kind: 'proof', opportunity_id: 'o1', opportunity_title: 'Onboarding exchange',
    paths: [
      { via: 'opportunity', participant_id: 'p1', opportunity_id: 'o1', opportunity_title: 'Onboarding exchange' },
      { via: 'edge', participant_id: 'p1', edge_id: 'e9', verb: 'could_help' },
    ],
  }],
};

function mountInspector(props: Record<string, unknown> = {}) {
  return mount(PartyInspector, {
    props: { party: null, dossier: null, loading: false, rootPartyId: null, notesError: '', ...props },
  });
}

describe('PartyInspector', () => {
  it('shows the empty state without a selection', () => {
    const wrapper = mountInspector();
    expect(wrapper.text()).toContain('Select a party row or graph node');
  });

  it('renders the dossier sections and opens the card', () => {
    const wrapper = mountInspector({ party, dossier, rootPartyId: 'p0' });
    expect(wrapper.text()).toContain('Fieldwork');
    expect(wrapper.text()).toContain('CSV tools');
    expect(wrapper.text()).toContain('member_of');
    expect(wrapper.text()).toContain('guide.pdf');
    expect(wrapper.text()).toContain('Onboarding exchange');
    expect(wrapper.text()).toContain('Checklist');
    // Association paths show each hop with a deep link (amendment §1).
    expect(wrapper.text()).toContain('via Onboarding exchange');
    expect(wrapper.text()).toContain('via could help');
    expect(wrapper.text()).toContain('Open Fieldwork’s network');
    const button = wrapper.findAll('button').find((item) => item.text().includes('Open Fieldwork’s network'))!;
    button.trigger('click');
    expect(wrapper.emitted('make-root')![0]).toEqual(['p1']);
    wrapper.findAll('button').find((item) => item.text() === 'Open card')!.trigger('click');
    expect(wrapper.emitted('open-card')).toHaveLength(1);
  });

  it('does not offer root promotion when the root is the inspected party', () => {
    const wrapper = mountInspector({ party, dossier, rootPartyId: 'p1' });
    expect(wrapper.find('.outside-root').exists()).toBe(false);
  });

  it('does not offer root promotion without a root', () => {
    const wrapper = mountInspector({ party, dossier, rootPartyId: null });
    expect(wrapper.find('.outside-root').exists()).toBe(false);
  });
});
