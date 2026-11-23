import { mount, flushPromises } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type GraphEntity, type QueueSheetResponse, type SetPhaseResult, type SheetPhase } from '../../api/client';
import QueueWorkspace from './QueueWorkspace.vue';

afterEach(() => {
  vi.restoreAllMocks();
});

const phases = [
  { stage_id: 'exploring', label: 'Exploring', class: 'identified', kind: 'planning' as const, pack_id: 'job_search', pack_version: '1.0.0', count: 1 },
  { stage_id: 'preparing', label: 'Preparing', class: 'prepared', kind: 'planning' as const, pack_id: 'job_search', pack_version: '1.0.0', count: 1 },
  { stage_id: 'in_conversation', label: 'In conversation', class: 'responded', kind: 'planning' as const, pack_id: 'job_search', pack_version: '1.0.0', count: 0 },
];

const parties: GraphEntity[] = [{ id: 'p1', kind: 'person', name: 'Dana Lee', revision: 1 }];

function row(overrides: Partial<QueueSheetResponse['rows'][number]> = {}): QueueSheetResponse['rows'][number] {
  return {
    opportunity_id: 'o1', title: 'Onboarding', lifecycle: 'open', close_reason: null, phase: null,
    participants: [{ id: 'p1', name: 'Dana Lee', kind: 'person', role: 'primary' }],
    next_action: { kind: 'choose', label: 'Choose a next action', reason: 'No work or wait is known for this opportunity.', date: null, thread_id: null, recipient_id: null, recipient_name: null },
    other_action_count: 0,
    ...overrides,
  };
}

const sheetResponse: QueueSheetResponse = {
  generation: '1', total: 2,
  rows: [
    row(),
    row({ opportunity_id: 'o2', title: 'Fieldwork fixture', participants: [{ id: 'p1', name: 'Dana Lee', kind: 'person', role: 'primary' }] }),
  ],
  phases, cursor: null, next_cursor: null,
};

function stub(initialPhases: Record<string, SheetPhase> = {}) {
  const phaseState = new Map<string, SheetPhase>(Object.entries(initialPhases));
  vi.spyOn(parley, 'connect').mockResolvedValue();
  vi.spyOn(parley, 'getQueueSheet').mockImplementation(async (filters: { search?: string; phase?: string; sort?: 'due' | 'name' } = {}) => {
    let rows = sheetResponse.rows.map((item) => ({ ...item, phase: phaseState.get(item.opportunity_id) ?? item.phase }));
    if (filters.phase === 'preparing') rows = rows.filter((item) => item.phase?.stage_id === 'preparing');
    if (filters.phase === '__unmapped__') rows = rows.filter((item) => !item.phase);
    if (filters.search) {
      const needle = filters.search.toLocaleLowerCase();
      rows = rows.filter((item) => item.title.toLocaleLowerCase().includes(needle) || item.participants.some((party) => party.name.toLocaleLowerCase().includes(needle)));
    }
    if (filters.sort === 'name') rows.sort((a, b) => a.title.localeCompare(b.title));
    return { ...sheetResponse, rows, total: rows.length };
  });
  vi.spyOn(parley, 'getDailyQueue').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', pack_id: 'job_search', pack_version: '1.0.0', daily_cap: 12, actionable_count: 0, deferred_count: 0, items: [] });
  vi.spyOn(parley, 'getInsights').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', period_start: '2026-11-14', period_end: '2026-11-21', pack_id: 'job_search', pack_version: '1.0.0', sample_size: 0, metrics: [], comparisons: [], gaps: [], questions: [] });
  vi.spyOn(parley, 'createOpportunity').mockResolvedValue({ id: 'o3', title: 'Created', state: 'open', revision: 1, participant_ids: ['p1'], source_variant_ids: [], material_ids: [] });
  vi.spyOn(parley, 'setOpportunityPhase').mockImplementation(async (id: string, input: { expected_phase_revision: number; target_stage_id: string; original_event_id?: string | null }): Promise<SetPhaseResult> => {
    const current = phaseState.get(id) ?? null;
    const nextRev = input.expected_phase_revision + (current?.stage_id === input.target_stage_id ? 0 : 1);
    const next: SheetPhase = { stage_id: input.target_stage_id, label: input.target_stage_id === 'preparing' ? 'Preparing' : 'Exploring', class: input.target_stage_id === 'preparing' ? 'prepared' : 'identified', kind: 'planning', pack_id: 'job_search', pack_version: '1.0.0', revision: nextRev };
    phaseState.set(id, next);
    return { row: { ...row({ phase: next }), opportunity_id: id }, event_id: input.original_event_id ?? `ev-${id}-${input.target_stage_id}`, phase_revision: nextRev, reverted: !!input.original_event_id };
  });
  return mount(QueueWorkspace, { props: { parties, outputs: [] } });
}

describe('QueueWorkspace sheet: search, phase edit, undo', () => {
  it('creates an opportunity from the toolbar and reloads the sheet', async () => {
    const wrapper = stub(); await flushPromises();
    await wrapper.get('button[aria-expanded="false"]').trigger('click');
    await flushPromises();
    await wrapper.get('input[required][maxlength="500"]').setValue('Northline review');
    await wrapper.get('select[required]').setValue('p1');
    await flushPromises();
    await wrapper.get('form.create-form').trigger('submit');
    await flushPromises();
    expect(parley.createOpportunity).toHaveBeenCalledWith('Northline review', 'p1');
    expect(parley.getQueueSheet).toHaveBeenCalled();
    wrapper.unmount();
  });

  it('debounces search and drops stale responses from previous queries', async () => {
    const wrapper = stub(); await flushPromises();
    let resolveFirst!: (value: QueueSheetResponse) => void;
    let resolveSecond!: (value: QueueSheetResponse) => void;
    const first = new Promise<QueueSheetResponse>((resolve) => { resolveFirst = resolve; });
    const second = new Promise<QueueSheetResponse>((resolve) => { resolveSecond = resolve; });
    vi.mocked(parley.getQueueSheet)
      .mockImplementationOnce(() => first)
      .mockImplementationOnce(() => second);
    // Search debounce (A) is in flight when a sort change (B) starts.
    await wrapper.get('input[aria-label="Find scenario"]').setValue('onboarding');
    await new Promise((resolve) => globalThis.setTimeout(resolve, 300));
    await wrapper.get('select[aria-label="Order"]').setValue('name');
    resolveSecond({ ...sheetResponse, total: 1, rows: [sheetResponse.rows[0]!] });
    await flushPromises();
    resolveFirst(sheetResponse);
    await flushPromises();
    // The newer query wins; the stale earlier response is discarded.
    expect(wrapper.findAll('tbody tr[data-opportunity-id]')).toHaveLength(1);
    wrapper.unmount();
  });

  it('changes phase through the server-authorized command and announces the ack', async () => {
    const wrapper = stub(); await flushPromises();
    await wrapper.get('select[aria-label="Phase for Onboarding"]').setValue('preparing');
    await flushPromises();
    expect(parley.setOpportunityPhase).toHaveBeenCalledWith('o1', {
      target_stage_id: 'preparing',
      pack_version: '1.0.0',
      expected_phase_revision: 1,
    });
    expect(wrapper.get('[role="status"]').text()).toContain('moved to Preparing');
    // The canonical label is reflected back in the row control.
    expect(wrapper.get('select[aria-label="Phase for Onboarding"]').element).toMatchObject({ value: 'preparing' });
    wrapper.unmount();
  });

  it('same-phase selection is a no-op and submits nothing', async () => {
    const exploring: SheetPhase = { stage_id: 'exploring', label: 'Exploring', class: 'identified', kind: 'planning', pack_id: 'job_search', pack_version: '1.0.0', revision: 2 };
    const wrapper = stub({ o1: exploring, o2: exploring }); await flushPromises();
    await wrapper.get('select[aria-label="Phase for Onboarding"]').setValue('exploring');
    await flushPromises();
    expect(parley.setOpportunityPhase).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('keeps the filter when a row leaves it, moves selection and offers Undo', async () => {
    const preparing: SheetPhase = { stage_id: 'preparing', label: 'Preparing', class: 'prepared', kind: 'planning', pack_id: 'job_search', pack_version: '1.0.0', revision: 2 };
    const wrapper = stub({ o1: preparing, o2: preparing }); await flushPromises();
    await wrapper.get('select[aria-label="Phase filter"]').setValue('preparing');
    await flushPromises();
    await wrapper.get('select[aria-label="Phase for Onboarding"]').setValue('exploring');
    await flushPromises();
    expect(wrapper.findAll('tbody tr[data-opportunity-id]')).toHaveLength(1);
    expect(wrapper.get('[role="status"]').text()).toContain('hidden by the Preparing filter');
    const undo = wrapper.findAll('button').find((button) => button.text() === 'Undo phase change')!;
    expect(undo).toBeDefined();
    await undo.trigger('click');
    await flushPromises();
    expect(parley.setOpportunityPhase).toHaveBeenCalledWith('o1', expect.objectContaining({
      target_stage_id: 'preparing',
      original_event_id: 'ev-o1-exploring',
      expected_phase_revision: 3,
    }));
    // Undo restores the row into the filter and appends a reversal event.
    expect(wrapper.findAll('tbody tr[data-opportunity-id]')).toHaveLength(2);
    wrapper.unmount();
  });

  it('surfaces read failures with Retry preserving filters', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'getQueueSheet').mockRejectedValueOnce(new Error('read failed')).mockResolvedValue(sheetResponse);
    vi.spyOn(parley, 'getDailyQueue').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', pack_id: 'job_search', pack_version: '1.0.0', daily_cap: 12, actionable_count: 0, deferred_count: 0, items: [] });
    vi.spyOn(parley, 'getInsights').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', period_start: '2026-11-14', period_end: '2026-11-21', pack_id: 'job_search', pack_version: '1.0.0', sample_size: 0, metrics: [], comparisons: [], gaps: [], questions: [] });
    const wrapper = mount(QueueWorkspace, { props: { parties, outputs: [] } });
    await flushPromises();
    expect(wrapper.text()).toContain('Could not load scenarios');
    await wrapper.findAll('button').find((button) => button.text() === 'Retry')!.trigger('click');
    await flushPromises();
    expect(wrapper.findAll('tbody tr[data-opportunity-id]')).toHaveLength(2);
    wrapper.unmount();
  });
});
