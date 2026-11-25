import { mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type Dossier, type GraphEntity } from '../../api/client';
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

const exchangeVersion = {
  version: 1,
  they_get: 'A ready-to-use checklist',
  i_get: 'Field feedback',
  fairness_attested: true,
  attestation_digest: 'd'.repeat(64),
  attested_at: '2026-11-25T10:00:00Z',
  interest_ids: [],
  interests: [
    { id: 'i1', side: 'them', statement: 'One onboarding checklist', knowledge_kind: 'hypothesis' },
    { id: 'i2', side: 'me', statement: 'A field test', knowledge_kind: 'owner_report' },
  ],
};

function exchangeResponse(current: Record<string, unknown> | null) {
  return { current, versions: current ? [current] : [] };
}

async function expandMutualValue(wrapper: ReturnType<typeof mount>) {
  const details = wrapper.find('details[data-testid="mutual-value"]');
  (details.element as HTMLDetailsElement).open = true;
  await details.trigger('toggle');
}

function mountInspector(props: Record<string, unknown> = {}) {
  return mount(PartyInspector, {
    props: { party: null, dossier: null, loading: false, rootPartyId: null, notesError: '', ...props },
  });
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe('PartyInspector', () => {
  it('shows the empty state without a selection', () => {
    const wrapper = mountInspector();
    expect(wrapper.text()).toContain('Select a party row or graph node');
  });

  it('renders the dossier sections and opens the card', () => {
    const wrapper = mountInspector({ party, dossier, rootPartyId: 'p0' });
    expect(wrapper.text()).toContain('Fieldwork');
    expect(wrapper.text()).toContain('CSV tools');
    expect(wrapper.text()).toContain('member of');
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

  it('fetches the exchange only on expand and renders the attested latest version', async () => {
    const spy = vi.spyOn(parley, 'getWritingExchange').mockResolvedValue(exchangeResponse(exchangeVersion));
    const wrapper = mountInspector({ party, dossier, rootPartyId: 'p0' });
    expect(spy).not.toHaveBeenCalled();
    await expandMutualValue(wrapper);
    await vi.waitFor(() => expect(wrapper.text()).toContain('They get: A ready-to-use checklist'));
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy).toHaveBeenCalledWith('o1', 'p1');
    expect(wrapper.text()).toContain('You get: Field feedback');
    expect(wrapper.text()).toContain('attested');
    expect(wrapper.text()).toContain('hypothesis (their side): One onboarding checklist');
    expect(wrapper.text()).toContain('owner report (your side): A field test');
  });

  it('marks the latest unattested version as needing attestation', async () => {
    vi.spyOn(parley, 'getWritingExchange').mockResolvedValue(exchangeResponse({ ...exchangeVersion, fairness_attested: false, attested_at: null }));
    const wrapper = mountInspector({ party, dossier, rootPartyId: 'p0' });
    await expandMutualValue(wrapper);
    await vi.waitFor(() => expect(wrapper.text()).toContain('needs attestation'));
    expect(wrapper.text()).not.toContain('attested Nov');
  });

  it('shows the empty state with an Output link when no exchange exists', async () => {
    vi.spyOn(parley, 'getWritingExchange').mockResolvedValue(exchangeResponse(null));
    const wrapper = mountInspector({ party, dossier, rootPartyId: 'p0' });
    await expandMutualValue(wrapper);
    await vi.waitFor(() => expect(wrapper.text()).toContain('No exchange recorded yet.'));
    const link = wrapper.findAll('button').find((item) => item.text() === 'Set one in Output')!;
    link.trigger('click');
    expect(wrapper.emitted('navigate')![0]).toEqual(['/output']);
  });

  it('shows an error line and recovers through the retry action', async () => {
    const spy = vi.spyOn(parley, 'getWritingExchange');
    spy.mockRejectedValueOnce(new Error('offline'));
    const wrapper = mountInspector({ party, dossier, rootPartyId: 'p0' });
    await expandMutualValue(wrapper);
    await vi.waitFor(() => expect(wrapper.text()).toContain('Exchanges could not be loaded.'));
    spy.mockResolvedValue(exchangeResponse(exchangeVersion));
    await wrapper.findAll('button').find((item) => item.text() === 'Retry')!.trigger('click');
    await vi.waitFor(() => expect(wrapper.text()).toContain('They get: A ready-to-use checklist'));
  });

  it('resets and refetches when the inspected party changes while expanded', async () => {
    const spy = vi.spyOn(parley, 'getWritingExchange').mockResolvedValue(exchangeResponse(exchangeVersion));
    const wrapper = mountInspector({ party, dossier, rootPartyId: 'p1' });
    await expandMutualValue(wrapper);
    await vi.waitFor(() => expect(wrapper.text()).toContain('They get: A ready-to-use checklist'));
    const party2: GraphEntity = { id: 'p2', kind: 'person', name: 'Mira', focus: '', revision: 1, etag: '"e2"' };
    const dossier2: Dossier = { entity: party2, direct_links: [], files: [], opportunities: [{ id: 'o9', title: 'Second conversation', role: 'participant' }], associated_resources: [] };
    spy.mockClear();
    spy.mockResolvedValue(exchangeResponse({ ...exchangeVersion, they_get: 'Second exchange text' }));
    await wrapper.setProps({ party: party2, dossier: dossier2 });
    await vi.waitFor(() => expect(wrapper.text()).toContain('Second exchange text'));
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy).toHaveBeenCalledWith('o9', 'p2');
  });

  it('omits opportunities without an exchange and keeps the count on the summary', async () => {
    vi.spyOn(parley, 'getWritingExchange').mockResolvedValue(exchangeResponse(exchangeVersion));
    const wrapper = mountInspector({ party, dossier, rootPartyId: 'p0' });
    expect(wrapper.find('summary').text()).not.toContain('(');
    await expandMutualValue(wrapper);
    await vi.waitFor(() => expect(wrapper.find('summary').text()).toContain('Mutual value (1)'));
  });
});
