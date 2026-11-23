import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type GraphEntity, type Opportunity } from '../../api/client';
import { CommandApiError, commands, type RunListItem, type RunStatus, type TranscriptPage } from '../../api/commands';
import DelegatedRuns from './DelegatedRuns.vue';

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const party: GraphEntity = { id: 'party-1', kind: 'person', name: 'Granted Party', focus: '', revision: 1 };
const opportunity = { id: 'opp-1', title: 'Granted Opportunity', state: 'open', revision: 1, participant_ids: [], source_variant_ids: [], material_ids: [] } as Opportunity;
const queued: RunListItem = { run_id: 'run-1', state: 'queued', revision: 1, deadline_at: '2026-11-23T10:00:00Z' };
const status: RunStatus = { run_id: 'run-1', state: 'running', revision: 2, deadline_at: queued.deadline_at, turn_count: 1, transcript_bytes: 42, wake_condition: null };
const started = { ...queued, state: 'queued' as const, granted_actions: ['party.read'], targets: [{ kind: 'party' as const, id: party.id }], phase: 'queued' };
const page: TranscriptPage = {
  items: [
    { turn_no: 1, speaker: 'owner', ordinal: 1, text: 'Summarise the granted party' },
    { turn_no: 2, speaker: 'assistant', ordinal: 0, text: 'Granted Party is ready.' },
  ],
  next_cursor: '323a30',
};

function setup(options: {
  parties?: GraphEntity[]; opportunities?: Opportunity[]; runs?: RunListItem[];
  runStatus?: RunStatus; transcript?: TranscriptPage; failList?: boolean; failTranscript?: boolean;
} = {}) {
  vi.spyOn(parley, 'connect').mockResolvedValue();
  vi.spyOn(parley, 'listParties').mockResolvedValue(options.parties ?? [party]);
  vi.spyOn(parley, 'listOpportunities').mockResolvedValue(options.opportunities ?? [opportunity]);
  vi.spyOn(parley, 'refreshSession').mockResolvedValue();
  const epoch = { value: 'epoch-1' };
  vi.spyOn(parley, 'workspaceEpoch', 'get').mockImplementation(() => epoch.value);
  const listRuns = vi.spyOn(commands, 'listRuns');
  if (options.failList) listRuns.mockRejectedValue(new Error('Run list unavailable'));
  else listRuns.mockResolvedValue({ items: options.runs ?? [], next_cursor: null });
  vi.spyOn(commands, 'getRun').mockResolvedValue(options.runStatus ?? status);
  const getRunTranscript = vi.spyOn(commands, 'getRunTranscript');
  if (options.failTranscript) getRunTranscript.mockRejectedValue(new Error('Transcript unavailable'));
  else getRunTranscript.mockResolvedValue(options.transcript ?? { items: [], next_cursor: null });
  vi.spyOn(commands, 'startRun').mockResolvedValue(started);
  vi.spyOn(commands, 'cancelRun').mockResolvedValue({ run_id: 'run-1', state: 'cancelled' });
  return { epoch, listRuns, getRunTranscript, wrapper: mount(DelegatedRuns) };
}

describe('DelegatedRuns U02 owner consent', () => {
  it('loads targets from the shell session and defaults to read-only grants', async () => {
    const { wrapper, listRuns } = setup();
    expect(wrapper.text()).toContain('Loading parties and opportunities…');
    expect(wrapper.text()).toContain('Loading delegated runs…');
    await flushPromises();
    expect(wrapper.find('[aria-label="Target Party: Granted Party"]').exists()).toBe(true);
    expect(wrapper.find('[aria-label="Target Opportunity: Granted Opportunity"]').exists()).toBe(true);
    expect((wrapper.get('[aria-label="Grant party.read"]').element as HTMLInputElement).checked).toBe(true);
    expect((wrapper.get('[aria-label="Grant opportunity.read"]').element as HTMLInputElement).checked).toBe(true);
    expect((wrapper.get('[aria-label="Grant opportunity.phase.edit"]').element as HTMLInputElement).checked).toBe(false);
    expect(wrapper.get('[aria-label="Start scoped run"]').attributes('disabled')).toBeDefined();
    expect(listRuns).toHaveBeenCalledWith({ limit: 20 });
    wrapper.unmount();
  });

  it('distinguishes loading, empty, no-match and failure states', async () => {
    const empty = setup({ parties: [], opportunities: [] });
    await flushPromises();
    expect(empty.wrapper.text()).toContain('No parties or opportunities are available to delegate.');
    expect(empty.wrapper.text()).toContain('No delegated runs yet.');
    await empty.wrapper.get('[aria-label="Filter available targets"]').setValue('granted');
    expect(empty.wrapper.text()).toContain('No parties or opportunities are available to delegate.');
    empty.wrapper.unmount();

    const filtered = setup({ runs: [queued] });
    await flushPromises();
    await filtered.wrapper.get('[aria-label="Filter available targets"]').setValue('missing');
    expect(filtered.wrapper.text()).toContain('No targets match this filter.');
    await filtered.wrapper.get('[aria-label="Filter runs by state"]').setValue('succeeded');
    expect(filtered.wrapper.text()).toContain('No runs match this state filter.');
    filtered.wrapper.unmount();

    const failed = setup({ failList: true });
    await flushPromises();
    expect(failed.wrapper.get('[role="alert"]').text()).toContain('Run list unavailable');
    failed.wrapper.unmount();
  });

  it('starts with explicit consent, opens the result and cancels with a separate key', async () => {
    const { wrapper } = setup();
    await flushPromises();
    await wrapper.get('[aria-label="Task for delegated run"]').setValue('Summarise the granted party');
    await wrapper.get('[aria-label="Target Party: Granted Party"]').setValue(true);
    await wrapper.get('[aria-label="Acknowledge scoped read grant"]').setValue(true);
    expect(wrapper.get('[aria-label="Start scoped run"]').attributes('disabled')).toBeUndefined();
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    const [input, startKey] = vi.mocked(commands.startRun).mock.calls[0]!;
    expect(input).toMatchObject({ task: 'Summarise the granted party', permissions: ['party.read', 'opportunity.read'], allow_created_opportunities: false });
    expect(input.targets).toEqual([{ kind: 'party', id: 'party-1' }]);
    expect(startKey).toBeTruthy();
    expect(wrapper.get('[role="status"]').text()).toContain('Run run-1 started');
    expect(wrapper.get('[data-run-id="run-1"]').attributes('data-state')).toBe('queued');

    await wrapper.get('[aria-label="Cancel run run-1"]').trigger('click');
    await flushPromises();
    const [runId, cancelKey] = vi.mocked(commands.cancelRun).mock.calls[0]!;
    expect(runId).toBe('run-1');
    expect(cancelKey).toBeTruthy();
    expect(cancelKey).not.toBe(startKey);
    expect(wrapper.get('[data-run-id="run-1"]').attributes('data-state')).toBe('cancelled');
    expect(wrapper.text()).toContain('Run run-1 was cancelled.');
    wrapper.unmount();
  });

  it('pages plain-text transcripts and never renders turn text as HTML', async () => {
    const injected: TranscriptPage = {
      items: [{ turn_no: 3, speaker: 'tool', ordinal: 1, text: '<img src=x onerror="alert(1)"> tool result' }],
      next_cursor: null,
    };
    const { wrapper, getRunTranscript } = setup({ runs: [queued], transcript: page });
    await flushPromises();
    await wrapper.get('[aria-label="View transcript run-1"]').trigger('click');
    await flushPromises();
    expect(wrapper.get('[role="region"]').text()).toContain('Granted Party is ready.');
    await wrapper.get('[aria-label="Next transcript page"]').trigger('click');
    await flushPromises();
    expect(getRunTranscript).toHaveBeenLastCalledWith('run-1', { limit: 2, cursor: '323a30' });

    getRunTranscript.mockResolvedValueOnce(injected);
    await wrapper.get('[aria-label="Previous transcript page"]').trigger('click');
    getRunTranscript.mockResolvedValue(injected);
    await wrapper.get('[aria-label="View transcript run-1"]').trigger('click');
    await flushPromises();
    expect(wrapper.find('img').exists()).toBe(false);
    expect(wrapper.get('[role="region"]').text()).toContain('<img src=x onerror="alert(1)"> tool result');
    wrapper.unmount();
  });

  it('reports a transcript failure without losing the run list', async () => {
    const { wrapper } = setup({ runs: [queued], failTranscript: true });
    await flushPromises();
    await wrapper.get('[aria-label="View transcript run-1"]').trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('Transcript unavailable');
    expect(wrapper.find('[data-run-id="run-1"]').exists()).toBe(true);
    wrapper.unmount();
  });

  it('stops polling and shows a stale alert when the epoch changes', async () => {
    vi.useFakeTimers();
    const { wrapper, listRuns, epoch } = setup({ runs: [queued] });
    await flushPromises();
    await vi.advanceTimersByTimeAsync(1000);
    expect(listRuns.mock.calls.length).toBeGreaterThan(1);
    epoch.value = 'epoch-2';
    await vi.advanceTimersByTimeAsync(1000);
    await flushPromises();
    expect(wrapper.get('[data-stale]').text()).toContain('epoch changed');
    const callsAtStale = listRuns.mock.calls.length;
    await vi.advanceTimersByTimeAsync(3000);
    await flushPromises();
    expect(listRuns.mock.calls.length).toBe(callsAtStale);
    await wrapper.get('[aria-label="Cancel run run-1"]').trigger('click');
    expect(commands.cancelRun).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('marks a workspace change event stale and reopens with a fresh session', async () => {
    const { wrapper, listRuns } = setup({ runs: [queued] });
    await flushPromises();
    window.dispatchEvent(new CustomEvent('parley:workspace-changed', { detail: { operation: 'restore', epoch: 'epoch-2' } }));
    await flushPromises();
    expect(wrapper.get('[data-stale]').text()).toContain('epoch changed');
    await wrapper.get('[aria-label="Reopen delegated runs with current session"]').trigger('click');
    await flushPromises();
    expect(parley.refreshSession).toHaveBeenCalled();
    expect(wrapper.find('[data-stale]').exists()).toBe(false);
    expect(listRuns.mock.calls.length).toBeGreaterThan(1);
    wrapper.unmount();
  });

  it('treats a rejected owner session as stale and reports other run errors', async () => {
    const { wrapper, listRuns } = setup({ runs: [] });
    await flushPromises();
    listRuns.mockRejectedValueOnce(new CommandApiError('Session expired', 403, 'TOKEN_INVALID'));
    await wrapper.get('[aria-label="Refresh runs"]').trigger('click');
    await flushPromises();
    expect(wrapper.get('[data-stale]').text()).toContain('session expired');
    wrapper.unmount();

    const failed = setup({ runs: [queued] });
    await flushPromises();
    vi.mocked(commands.getRun).mockRejectedValueOnce(new Error('Read failed'));
    await failed.wrapper.get('[aria-label="View transcript run-1"]').trigger('click');
    await flushPromises();
    expect(failed.wrapper.text()).toContain('Read failed');
    failed.wrapper.unmount();
  });
});
