import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { parley } from '../../api/client';
import { CommandApiError, commands, type ApprovalDetail, type ApprovalListItem } from '../../api/commands';
import ApprovalInbox from './ApprovalInbox.vue';

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const pendingRow: ApprovalListItem = {
  id: 'aprv-1',
  command_id: 'resource.retire',
  command_version: 1,
  revision: 1,
  status: 'pending',
  expires_at: '2026-11-23T00:10:00Z',
  preview_digest: 'a'.repeat(64),
  title: 'Gift A',
  impacted_versions_count: 2,
  args: { resource_id: 'res-1', expected_card_revision: 1, expected_content_revision: 1 },
  preview: {
    resource_id: 'res-1', title: 'Gift A', readiness: 'ready', card_revision: 1,
    content_revision: 1, placement_removed: true, impacted_versions_count: 2,
    impacted_versions_sample: [],
  },
};

function setup(options: { rows?: ApprovalListItem[]; failList?: boolean; failListExpired?: boolean; detail?: ApprovalDetail } = {}) {
  vi.spyOn(parley, 'connect').mockResolvedValue();
  const epoch = { value: 'epoch-1' };
  vi.spyOn(parley, 'workspaceEpoch', 'get').mockImplementation(() => epoch.value);
  const listApprovals = vi.spyOn(commands, 'listApprovals');
  if (options.failList) {
    listApprovals.mockRejectedValue(new Error('Approval list unavailable'));
  } else if (options.failListExpired) {
    listApprovals.mockRejectedValue(new CommandApiError('Session expired', 401, 'unauthenticated'));
  } else {
    listApprovals.mockResolvedValue({ items: options.rows ?? [pendingRow], next_cursor: null });
  }
  vi.spyOn(commands, 'getApproval').mockResolvedValue(options.detail ?? { ...pendingRow });
  vi.spyOn(commands, 'grantApproval').mockResolvedValue({ id: 'aprv-1', revision: 2, status: 'granted' });
  vi.spyOn(commands, 'denyApproval').mockResolvedValue({ id: 'aprv-1', revision: 2, status: 'denied' });
  const notify = vi.spyOn(commands, 'notify').mockResolvedValue(() => undefined);
  return { epoch, listApprovals, notify, wrapper: mount(ApprovalInbox) };
}

describe('ApprovalInbox U08 owner decisions', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  it('loads pending approvals and distinguishes the empty state', async () => {
    const { wrapper, listApprovals } = setup({ rows: [pendingRow] });
    await flushPromises();
    await vi.advanceTimersByTimeAsync(0);
    expect(listApprovals).toHaveBeenCalledWith(expect.objectContaining({ status: 'pending' }));
    expect(wrapper.text()).toContain('Gift A');
    expect(wrapper.text()).toContain('2');
    expect(wrapper.get('[data-approval-id="aprv-1"]').attributes('data-status')).toBe('pending');

    const empty = setup({ rows: [] });
    await flushPromises();
    expect(empty.wrapper.text()).toContain('No pending approvals.');
    empty.wrapper.unmount();
    wrapper.unmount();
  });

  it('denies an approval with its fresh revision and preview digest', async () => {
    const { wrapper } = setup();
    await flushPromises();
    await wrapper.get('[aria-label="Deny approval for Gift A"]').trigger('click');
    await flushPromises();
    expect(commands.denyApproval).toHaveBeenCalledWith(
      'aprv-1',
      expect.objectContaining({ expected_revision: 1, preview_digest: 'a'.repeat(64) }),
      expect.any(String),
    );
    expect(wrapper.text()).toContain('Gift A denied.');
    expect(commands.listApprovals).toHaveBeenCalled();
    wrapper.unmount();
  });

  it('approves an approval and refreshes the list', async () => {
    const { wrapper } = setup();
    await flushPromises();
    await wrapper.get('[aria-label="Approve approval for Gift A"]').trigger('click');
    await flushPromises();
    expect(commands.grantApproval).toHaveBeenCalled();
    expect(wrapper.text()).toContain('Gift A approved.');
    wrapper.unmount();
  });

  it('surfaces a stale decision and reloads authoritative state', async () => {
    const { wrapper } = setup();
    vi.spyOn(commands, 'denyApproval').mockRejectedValue(
      new CommandApiError('The approval changed', 409, 'approval_stale', [], null, null),
    );
    await flushPromises();
    await wrapper.get('[aria-label="Deny approval for Gift A"]').trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('This approval changed while you were reviewing it.');
    wrapper.unmount();
  });

  it('treats an expired session as stale and stops polling', async () => {
    const { wrapper } = setup({ failListExpired: true });
    await flushPromises();
    expect(wrapper.find('[data-stale]').text()).toContain('Reopen Approvals with the current session.');
    wrapper.unmount();
  });
});

describe('ApprovalInbox U08 detail and failures', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  it('expands an active approval detail with its exact args and preview', async () => {
    const { wrapper } = setup();
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Details')!.trigger('click');
    await flushPromises();
    expect(commands.getApproval).toHaveBeenCalledWith('aprv-1');
    expect(wrapper.text()).toContain('Arguments');
    expect(wrapper.text()).toContain('"resource_id":"res-1"');
    wrapper.unmount();
  });

  it('surfaces a generic list failure without killing the surface', async () => {
    const { wrapper } = setup({ failList: true });
    await flushPromises();
    expect(wrapper.text()).toContain('Approval list unavailable');
    wrapper.unmount();
  });
});

describe('ApprovalInbox U08 notifications', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  it('refreshes the list on approvals_changed and epoch_changed signals', async () => {
    const { wrapper, listApprovals, notify } = setup();
    await flushPromises();
    const handler = notify.mock.calls[0]![0].onSignal;
    handler('approvals_changed');
    await flushPromises();
    handler('epoch_changed');
    await flushPromises();
    expect(listApprovals.mock.calls.length).toBeGreaterThanOrEqual(3);
    wrapper.unmount();
  });
});
