import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError, parley, type ActivityItem, type ActivityPage } from '../api/client';
import { commands, type RunStatus, type TranscriptPage } from '../api/commands';
import AgentActivity from './AgentActivity.vue';

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const jobItem: ActivityItem = {
  kind: 'job',
  id: 'job-1',
  role: 'coach',
  status: 'queued',
  state_revision: 1,
  attempt_count: 0,
  progress: null,
  scope: { kind: 'weekly', id: '2026-W47' },
  error_code: null,
  review_ref: null,
  updated_at: '2026-11-24T10:00:00.000Z',
};

const runItem: ActivityItem = {
  kind: 'run',
  id: 'run-1',
  role: 'delegated',
  status: 'running',
  state_revision: 2,
  attempt_count: null,
  progress: { current: 1, total: 16 },
  scope: { kind: 'delegated', id: null },
  error_code: null,
  review_ref: null,
  updated_at: '2026-11-24T10:01:00.000Z',
};

const reviewRunItem: ActivityItem = {
  ...runItem,
  id: 'run-9',
  status: 'succeeded',
  review_ref: { kind: 'delegated_review', job_id: 'job-9', scope_kind: 'opportunity', scope_id: 'opp-1' },
};

function page(items: ActivityItem[], next: string | null = null): ActivityPage {
  return { items, next_cursor: next, epoch: 'epoch-1' };
}

const runStatus: RunStatus = {
  run_id: 'run-1', state: 'running', revision: 2, deadline_at: '2026-11-24T11:00:00Z',
  turn_count: 1, transcript_bytes: 42, wake_condition: null, profile_revision: 1,
  transcript_major: 1, wait_intent: null, review: null,
};

const transcriptPage: TranscriptPage = {
  items: [{ turn_no: 1, speaker: 'owner', ordinal: 1, text: 'Summarise the party' }],
  next_cursor: null,
};

function setup(options: { items?: ActivityItem[]; next?: string | null; failList?: boolean } = {}) {
  vi.spyOn(parley, 'workspaceEpoch', 'get').mockReturnValue('epoch-1');
  const getActivity = vi.spyOn(parley, 'getActivity');
  if (options.failList) getActivity.mockRejectedValue(new Error('Activity unavailable'));
  else getActivity.mockImplementation(async () => page(options.items ?? [jobItem, runItem], options.next ?? null));
  vi.spyOn(parley, 'getRoleJob').mockResolvedValue({
    id: 'job-1', role: 'coach', status: 'queued', result: null, last_error: null,
    scope_kind: 'owner', scope_id: 'weekly', review_ref: null,
  });
  vi.spyOn(commands, 'getRun').mockResolvedValue(runStatus);
  vi.spyOn(commands, 'getRunTranscript').mockResolvedValue(transcriptPage);
  vi.spyOn(commands, 'cancelRun').mockResolvedValue({ run_id: 'run-1', state: 'cancelled' });
  vi.spyOn(parley, 'cancelJob').mockResolvedValue();
  const navigate = vi.fn();
  const wrapper = mount(AgentActivity, { props: { navigate } });
  return { wrapper, getActivity, navigate };
}

describe('AgentActivity AR09 owner activity', () => {
  it('renders the mixed job/run union from the shared reconciler', async () => {
    const { wrapper } = setup();
    await flushPromises();
    expect(wrapper.get('[data-kind="job"][data-id="job-1"]').attributes('data-state')).toBe('queued');
    const runRow = wrapper.get('[data-kind="run"][data-id="run-1"]');
    expect(runRow.attributes('data-state')).toBe('running');
    expect(runRow.text()).toContain('Delegated run');
    expect(runRow.find('progress').attributes('value')).toBe('1');
    expect(runRow.find('progress').attributes('max')).toBe('16');
    expect(wrapper.find('[aria-label="Cancel run-1"]').exists()).toBe(true);
    expect(wrapper.find('[aria-label="Cancel job-1"]').exists()).toBe(true);
    wrapper.unmount();
  });

  it('reports a load failure while keeping the filter surface usable', async () => {
    const { wrapper } = setup({ failList: true });
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('Activity unavailable');
    wrapper.unmount();
  });

  it('refilters via kind/status/scope through the server read', async () => {
    const { wrapper, getActivity } = setup();
    await flushPromises();
    await wrapper.get('[aria-label="Filter by kind"]').setValue('run');
    await wrapper.get('[aria-label="Filter by status"]').setValue('running');
    await wrapper.get('[aria-label="Filter by scope"]').setValue('delegated');
    await flushPromises();
    const lastCall = getActivity.mock.calls.at(-1)![0] as Record<string, unknown>;
    expect(lastCall.kind).toBe('run');
    expect(lastCall.status).toBe('running');
    expect(lastCall.scope).toBe('delegated');
    expect(lastCall.limit).toBe(20);
    wrapper.unmount();
  });

  it('pages forward and back with the signed cursor', async () => {
    const { wrapper, getActivity } = setup({ items: [jobItem, runItem], next: 'cursor-2' });
    await flushPromises();
    const callsBeforeNext = getActivity.mock.calls.length;
    await wrapper.get('[aria-label="Next activity page"]').trigger('click');
    await flushPromises();
    // Deeper pages are a stable server snapshot: no new read happens until the
    // owner pages back to page one.
    expect(getActivity.mock.calls.length).toBe(callsBeforeNext);
    expect(wrapper.find('[data-kind="job"][data-id="job-1"]').exists()).toBe(true);
    await wrapper.get('[aria-label="Previous activity page"]').trigger('click');
    await flushPromises();
    const prevCall = getActivity.mock.calls.at(-1)![0] as Record<string, unknown>;
    expect(prevCall.cursor).toBeUndefined();
    wrapper.unmount();
  });

  it('cancels a job and a run through the real cancel operations', async () => {
    const { wrapper } = setup();
    await flushPromises();
    await wrapper.get('[aria-label="Cancel job-1"]').trigger('click');
    await flushPromises();
    expect(parley.cancelJob).toHaveBeenCalledWith('job-1');
    expect(wrapper.get('[role="status"]').text()).toContain('Job job-1 was cancelled.');
    await wrapper.get('[aria-label="Cancel run-1"]').trigger('click');
    await flushPromises();
    expect(commands.cancelRun).toHaveBeenCalledWith('run-1', expect.any(String));
    expect(wrapper.get('[role="status"]').text()).toContain('Run run-1 was cancelled.');
    wrapper.unmount();
  });

  it('surfaces a terminal conflict on cancel', async () => {
    vi.spyOn(parley, 'cancelJob').mockImplementationOnce(() => Promise.reject(new ApiError('Terminal', 409, 'JOB_TERMINAL')));
    const { wrapper } = setup();
    await flushPromises();
    await wrapper.get('[aria-label="Cancel job-1"]').trigger('click');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('already finished');
    wrapper.unmount();
  });

  it('expands job and run detail including plain-text transcript paging', async () => {
    const { wrapper } = setup();
    await flushPromises();
    await wrapper.get('[aria-label="View detail for job-1"]').trigger('click');
    await flushPromises();
    const jobDetail = wrapper.get('[role="region"][aria-label="Detail for job-1"]');
    expect(jobDetail.text()).toContain('Status: queued');
    await wrapper.get('[aria-label="View detail for run-1"]').trigger('click');
    await flushPromises();
    const runDetail = wrapper.get('[role="region"][aria-label="Detail for run-1"]');
    expect(runDetail.text()).toContain('Turn 1 of at most 16');
    expect(runDetail.text()).toContain('Summarise the party');
    expect(runDetail.find('img').exists()).toBe(false);
    wrapper.unmount();
  });

  it('opens the typed review destination for a reviewable item', async () => {
    const { wrapper, navigate } = setup({ items: [reviewRunItem] });
    await flushPromises();
    await wrapper.get('[aria-label="Open review for run-9"]').trigger('click');
    expect(navigate).toHaveBeenCalledWith('/output/opp-1');
    wrapper.unmount();
  });

  it('marks the view stale when the session/epoch fence trips', async () => {
    const { wrapper, getActivity } = setup({ items: [runItem] });
    await flushPromises();
    // A rejected read from the session boundary (401/403/409) fences the
    // view instead of retrying forever.
    getActivity.mockImplementation(async () => {
      throw new ApiError('Session changed', 403, 'TOKEN_INVALID');
    });
    window.dispatchEvent(new CustomEvent('parley:command-committed', { detail: {} }));
    await flushPromises();
    expect(wrapper.get('[data-stale]').text()).toContain('session changed');
    wrapper.unmount();
  });
});
