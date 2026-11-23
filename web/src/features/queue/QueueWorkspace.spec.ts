import { mount, flushPromises } from '@vue/test-utils';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parley, type GraphEntity, type QueueSheetResponse, type SetPhaseResult, type SheetPhase } from '../../api/client';
import { commands } from '../../api/commands';
import { sessionEventReader } from '../../app/commands/events';
import { createCommandRegistry, installCommandRegistry, useCommandRegistry } from '../../app/commands/registry';
import QueueWorkspace from './QueueWorkspace.vue';

beforeEach(() => {
  installCommandRegistry(createCommandRegistry(vi.fn()));
  vi.spyOn(commands, 'query').mockResolvedValue({ contract_major: 1, manifest_version: 'sha256:test', truncated: false, commands: [] });
});

afterEach(() => {
  vi.restoreAllMocks();
  globalThis.sessionStorage.clear();
});

const phases = [
  { stage_id: 'exploring', label: 'Exploring', class: 'identified', kind: 'planning' as const, pack_id: 'job_search', pack_version: '1.0.0', count: 1 },
  { stage_id: 'preparing', label: 'Preparing', class: 'prepared', kind: 'planning' as const, pack_id: 'job_search', pack_version: '1.0.0', count: 1 },
  { stage_id: 'in_conversation', label: 'In conversation', class: 'responded', kind: 'planning' as const, pack_id: 'job_search', pack_version: '1.0.0', count: 0 },
];

const parties: GraphEntity[] = [{ id: 'p1', kind: 'person', name: 'Dana Lee', revision: 1 }];

function row(overrides: Partial<QueueSheetResponse['rows'][number]> = {}): QueueSheetResponse['rows'][number] {
  return {
    opportunity_id: 'o1', title: 'Onboarding', lifecycle: 'open', close_reason: null, pack_id: 'job_search', pack_version: '1.0.0', phase_revision: 1, phase: null,
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
  vi.spyOn(parley, 'getEvents').mockResolvedValue({ items: [], next_cursor: '0', observed_cursor: '0' });
  vi.spyOn(parley, 'getWritingExchange').mockResolvedValue({ current: null, versions: [] });
  vi.spyOn(parley, 'getWritingInterests').mockResolvedValue([]);
  vi.spyOn(parley, 'listThreads').mockResolvedValue([]);
  vi.spyOn(parley, 'getQueueSheet').mockImplementation(async (filters: { search?: string; phase?: string; sort?: 'due' | 'name' } = {}) => {
    let rows = sheetResponse.rows.map((item) => ({ ...item, phase: phaseState.get(item.opportunity_id) ?? item.phase, phase_revision: phaseState.get(item.opportunity_id)?.revision ?? item.phase_revision }));
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
    return { row: { ...row({ phase: next, phase_revision: nextRev }), opportunity_id: id }, event_id: input.original_event_id ?? `ev-${id}-${input.target_stage_id}`, phase_revision: nextRev, reverted: !!input.original_event_id };
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
    }, expect.any(String));
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

  it('retries a phase change with the same intent after the response is lost', async () => {
    const wrapper = stub();
    await flushPromises();
    const change = vi.mocked(parley.setOpportunityPhase);
    change.mockRejectedValueOnce(new TypeError('Connection lost'));

    await wrapper.get('select[aria-label="Phase for Onboarding"]').setValue('preparing');
    await flushPromises();
    expect(wrapper.get('select[aria-label="Phase for Onboarding"]').attributes('disabled')).toBeDefined();
    await wrapper.findAll('button').find(button => button.text() === 'Retry phase change')!.trigger('click');
    await flushPromises();

    expect(change.mock.calls[1]).toEqual(change.mock.calls[0]);
    expect(wrapper.get('select[aria-label="Phase for Onboarding"]').element).toMatchObject({ value: 'preparing' });
    wrapper.unmount();
  });

  it('expires an unconfirmed phase change after the workspace epoch changes', async () => {
    const epoch = vi.spyOn(parley, 'workspaceEpoch', 'get').mockReturnValue('first-epoch');
    const wrapper = stub();
    await flushPromises();
    const change = vi.mocked(parley.setOpportunityPhase);
    change.mockRejectedValueOnce(new TypeError('Connection lost'));
    await wrapper.get('select[aria-label="Phase for Onboarding"]').setValue('preparing');
    await flushPromises();
    epoch.mockReturnValue('second-epoch');
    await wrapper.findAll('button').find(button => button.text() === 'Retry phase change')!.trigger('click');
    await flushPromises();
    expect(change).toHaveBeenCalledTimes(1);
    expect(wrapper.text()).toContain('The workspace changed. Review the current phase before editing.');
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
    }), expect.any(String));
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
  it('restores the saved search, filters, sort and selection after reconnect', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'workspaceEpoch', 'get').mockReturnValue('epoch-1');
    globalThis.sessionStorage.setItem('parley.queue.epoch-1', JSON.stringify({
      query: 'fixture', phaseFilter: '', sortKey: 'name', selectedId: 'o2',
    }));
    const sheet = vi.spyOn(parley, 'getQueueSheet').mockResolvedValue({
      ...sheetResponse,
      rows: [sheetResponse.rows[1]!, sheetResponse.rows[0]!],
    });
    vi.spyOn(parley, 'getDailyQueue').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', pack_id: 'job_search', pack_version: '1.0.0', daily_cap: 12, actionable_count: 0, deferred_count: 0, items: [] });
    vi.spyOn(parley, 'getInsights').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', period_start: '2026-11-14', period_end: '2026-11-21', pack_id: 'job_search', pack_version: '1.0.0', sample_size: 0, metrics: [], comparisons: [], gaps: [], questions: [] });
    const wrapper = mount(QueueWorkspace, { props: { parties, outputs: [] } });
    await flushPromises();
    expect((wrapper.get('input[aria-label="Find scenario"]').element as HTMLInputElement).value).toBe('fixture');
    expect(sheet.mock.calls[0]?.[0]).toMatchObject({ search: 'fixture', sort: 'name' });
    // The restored selection survives because the row is still visible.
    expect(wrapper.get('button.open[aria-pressed="true"]').text()).toBe('Fieldwork fixture');
    wrapper.unmount();
  });

  it('never offers Unmapped or terminal lifecycle stages as phase targets', async () => {
    const wrapper = stub(); await flushPromises();
    vi.mocked(parley.getQueueSheet).mockResolvedValue({
      ...sheetResponse,
      phases: [
        ...phases,
        { stage_id: '__unmapped__', label: 'Unmapped', class: 'unmapped', kind: 'planning', pack_id: '', pack_version: '', count: 1 },
        { stage_id: 'closed', label: 'Closed', class: 'terminal', kind: 'terminal', pack_id: '', pack_version: '', count: 0 },
      ],
    });
    await wrapper.get('select[aria-label="Order"]').setValue('name');
    await flushPromises();
    const targets = wrapper.get('select[aria-label="Phase for Onboarding"]').findAll('option').map((option) => option.text());
    expect(targets).toEqual(['Unmapped', 'Exploring', 'Preparing', 'In conversation']);
    expect(targets).not.toContain('Closed');
    wrapper.unmount();
  });

  it('labels action dates in the owner timezone', async () => {
    const now = new Date();
    const iso = (offsetDays: number) => {
      const day = new Date(now.getFullYear(), now.getMonth(), now.getDate() + offsetDays);
      return `${day.getFullYear()}-${String(day.getMonth() + 1).padStart(2, '0')}-${String(day.getDate()).padStart(2, '0')}`;
    };
    const dated = {
      ...sheetResponse,
      rows: [
        row({ opportunity_id: 'o1', next_action: { kind: 'task', label: 'Confirm the recipient', reason: 'Checklist ready.', date: iso(0), thread_id: null, recipient_id: null, recipient_name: null } }),
        row({ opportunity_id: 'o2', next_action: { kind: 'task', label: 'Review the outline', reason: 'Workshop planned.', date: '2026-05-04', thread_id: null, recipient_id: null, recipient_name: null } }),
      ],
    };
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'getQueueSheet').mockResolvedValue(dated);
    vi.spyOn(parley, 'getDailyQueue').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', pack_id: 'job_search', pack_version: '1.0.0', daily_cap: 12, actionable_count: 0, deferred_count: 0, items: [] });
    vi.spyOn(parley, 'getInsights').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', period_start: '2026-11-14', period_end: '2026-11-21', pack_id: 'job_search', pack_version: '1.0.0', sample_size: 0, metrics: [], comparisons: [], gaps: [], questions: [] });
    const wrapper = mount(QueueWorkspace, { props: { parties, outputs: [] } });
    await flushPromises();
    const cells = wrapper.findAll('td.date');
    expect(cells[0]!.text()).toContain('Today');
    expect(cells[0]!.classes()).toContain('due');
    expect(cells[1]!.text()).toContain('May 4');
    wrapper.unmount();
  });

  it('appends the next page through the continuation cursor', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    const sheet = vi.spyOn(parley, 'getQueueSheet')
      .mockResolvedValueOnce({ ...sheetResponse, rows: [sheetResponse.rows[0]!], total: 2, next_cursor: '1' })
      .mockResolvedValueOnce({ ...sheetResponse, rows: sheetResponse.rows, total: 2, next_cursor: null });
    void sheet;
    vi.spyOn(parley, 'getDailyQueue').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', pack_id: 'job_search', pack_version: '1.0.0', daily_cap: 12, actionable_count: 0, deferred_count: 0, items: [] });
    vi.spyOn(parley, 'getInsights').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', period_start: '2026-11-14', period_end: '2026-11-21', pack_id: 'job_search', pack_version: '1.0.0', sample_size: 0, metrics: [], comparisons: [], gaps: [], questions: [] });
    const wrapper = mount(QueueWorkspace, { props: { parties, outputs: [] } });
    await flushPromises();
    expect(wrapper.findAll('tbody tr[data-opportunity-id]')).toHaveLength(1);
    await wrapper.findAll('button').find((button) => button.text() === 'Load more')!.trigger('click');
    await flushPromises();
    expect(wrapper.findAll('tbody tr[data-opportunity-id]')).toHaveLength(2);
    wrapper.unmount();
  });

  it('freezes pack identity and revision in a scoped palette command and applies its receipt', async () => {
    const wrapper = stub(); await flushPromises();
    await wrapper.get('button[data-open="o1"]').trigger('click');
    await flushPromises();
    const result = {
      ok: true as const, command_version: 1, audit_id: 'aud-1', replayed: false, dry_run: false,
      result: { opportunity_id: 'o1', phase_revision: 5, previous_stage_id: null, stage_id: 'preparing', event_id: 'ev-9', event_cursor: '12' },
    };
    const event = { cursor: '12', event_id: 'ev-9', entity_type: 'opportunity', entity_id: 'o1', revision: 5, event_type: 'opportunity.phase_changed', occurred_at: '2026-11-20T09:00:00Z', payload: { operation: 'manual', to_stage_id: 'preparing' } };
    const invoke = vi.spyOn(commands, 'invoke').mockImplementation(async () => {
      const changed = row({ phase: { ...phases[1]!, revision: 5 }, phase_revision: 5 });
      vi.mocked(parley.getQueueSheet).mockResolvedValue({ ...sheetResponse, rows: [changed, sheetResponse.rows[1]!] });
      vi.mocked(parley.getEvents).mockResolvedValue({ items: [event], next_cursor: '12', observed_cursor: '12' });
      return result;
    });
    const ctx = { context: { route: 'queue', 'selection.opportunity': true }, selection: { opportunity: 'o1' } };
    await useCommandRegistry().run('ui.queue.set_phase', { stage_id: 'preparing' }, ctx);
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith('opportunity.set_phase', expect.objectContaining({
      version: 1,
      args: expect.objectContaining({ opportunity_id: 'o1', expected_pack_id: 'job_search', expected_pack_version: '1.0.0', stage_id: 'preparing' }),
    }), expect.any(String));
    expect(wrapper.text()).toContain('moved to Preparing');
    expect(wrapper.emitted('binding')?.at(-1)?.[0]).toMatchObject({ opportunityId: 'o1', originalEventId: 'ev-9' });
    wrapper.unmount();
  });

  it('reports a conflict when a command result names a row that is no longer visible', async () => {
    const wrapper = stub(); await flushPromises();
    await wrapper.get('button[data-open="o1"]').trigger('click');
    await flushPromises();
    vi.spyOn(commands, 'invoke').mockResolvedValue({
      ok: true, command_version: 1, audit_id: 'aud-2', replayed: false, dry_run: false,
      result: { opportunity_id: 'missing', phase_revision: 2, previous_stage_id: null, stage_id: 'preparing', event_id: null, event_cursor: '1' },
    });
    await useCommandRegistry().run('ui.queue.set_phase', { stage_id: 'preparing' }, {
      context: { route: 'queue', 'selection.opportunity': true }, selection: { opportunity: 'o1' },
    });
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('no longer visible');
    wrapper.unmount();
  });

  it('preserves a dirty phase edit when an external event arrives during a save', async () => {
    let hooks!: Parameters<typeof sessionEventReader.start>[0];
    vi.spyOn(sessionEventReader, 'start').mockImplementation((value) => { hooks = value; });
    let finish!: (value: SetPhaseResult) => void;
    const wrapper = stub();
    vi.mocked(parley.setOpportunityPhase).mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    await flushPromises();
    await wrapper.get('select[aria-label="Phase for Onboarding"]').setValue('preparing');
    await flushPromises();
    hooks.onEvents({ items: [{ cursor: '1', event_id: 'external', entity_type: 'opportunity', entity_id: 'o1', revision: 2, event_type: 'opportunity.phase_changed', occurred_at: '2026-11-20T09:00:00Z', payload: {} }], next_cursor: '1', observed_cursor: '1' });
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('in flight');
    finish({ row: sheetResponse.rows[0]!, event_id: 'ev-1', phase_revision: 2, reverted: false });
    await flushPromises();
    wrapper.unmount();
  });

  it('shows proposed exchange, source passages, follow-ups and history in the inspector', async () => {
    const wrapper = stub(); await flushPromises();
    vi.spyOn(parley, 'getWritingExchange').mockResolvedValue({
      current: { version: 1, they_get: 'Annotated setup instructions', i_get: 'Maintainer feedback' },
      versions: [],
    });
    vi.spyOn(parley, 'getWritingInterests').mockResolvedValue([
      { id: 'i1', side: 'them', statement: 'Clearer quickstart', knowledge_kind: 'observation', check_suggestion: null, claim_id: 'c1', claim_revision: 1, claim: { claim_id: 'c1', revision: 1, wording: 'We incorporated the setup edits into the quickstart.', tier: 1, speaker_kind: 'party' }, revision: 1, status: 'active' },
    ]);
    const eventPage = {
      items: [{ cursor: '1', event_id: 'e1', entity_type: 'opportunity', entity_id: 'o1', revision: 1, event_type: 'opportunity.created', occurred_at: '2026-11-20T09:00:00Z', payload: {} }],
      next_cursor: '1', observed_cursor: '1',
    };
    vi.spyOn(parley, 'getEvents').mockImplementation(async (cursor = '0') => cursor === '0' ? eventPage : { items: [], next_cursor: cursor, observed_cursor: '1' });
    vi.spyOn(parley, 'listThreads').mockResolvedValue([{
      id: 't1', opportunity_id: 'o1', recipient_id: 'p1', state: 'waiting', current_touch: 1,
      expires_at: null, next_action_at: '2026-11-25T09:00:00Z', sent_at: null, updated_at: '2026-11-21T09:00:00Z', sends: [],
    }]);
    await wrapper.findAll('button.open')[1]!.trigger('click');
    await flushPromises();
    const inspector = wrapper.get('aside.inspector');
    expect(inspector.text()).toContain('PROPOSED EXCHANGE');
    expect(inspector.text()).toContain('They get · Annotated setup instructions');
    expect(inspector.text()).toContain('I hope to get · Maintainer feedback');
    expect(inspector.text()).toContain('SOURCE PASSAGES');
    expect(inspector.text()).toContain('We incorporated the setup edits into the quickstart.');
    expect(inspector.text()).toContain('party · T1');
    expect(inspector.text()).toContain('thread waiting');
    expect(inspector.text()).toContain('Scenario history (1)');
    wrapper.unmount();
  });
});
