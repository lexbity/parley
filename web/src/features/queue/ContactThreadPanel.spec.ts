import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type Material, type ReleasedMaterial, type SendRecord } from '../../api/client';
import ContactThreadPanel from './ContactThreadPanel.vue';

afterEach(() => vi.restoreAllMocks());

const material: Material = {
  id: 'mat-1', opportunity_id: 'opp-1', recipient_id: 'party-1', recipient_name: 'Dana Lee', kind: 'outreach',
  version: 3, body: 'Hello.', content_sha256: 'a'.repeat(64), status: 'approved', gate: null, attachment: null,
};
const delivered: ReleasedMaterial = {
  release_id: 'release-1', material_id: 'mat-1', version: 3, recipient_id: 'party-1', body: 'Hello.',
  content_sha256: 'a'.repeat(64), issued_at: '2026-11-21T12:00:00Z', attachment: null,
};
const send: SendRecord = {
  id: 'send-1', thread_id: 'thread-1', touch_no: 1, primary_material_id: 'mat-1', primary_version_no: 3,
  recipient_id: 'party-1', content_sha256: 'a'.repeat(64), sent_at: '2026-11-21T12:00:00Z',
  response_deadline: '2026-11-29T08:00:00Z', deadline_zone: 'America/Vancouver', next_action_at: null, capacity_charged: false,
};

describe('ContactThreadPanel S07', () => {
  it('requires the acknowledged exact release and retries an unknown send with the frozen key and thread', async () => {
    vi.spyOn(parley, 'listThreads').mockResolvedValue([]);
    const sendOnThread = vi.spyOn(parley, 'sendOnThread')
      .mockRejectedValueOnce(new TypeError('connection dropped'))
      .mockResolvedValueOnce(send);
    const wrapper = mount(ContactThreadPanel, { props: { opportunityId: 'opp-1', material, delivered: null, threads: [] } });
    await flushPromises();
    const markSent = wrapper.get('button').element as HTMLButtonElement;
    expect(markSent.disabled).toBe(true);
    await wrapper.setProps({ delivered });
    await flushPromises();
    await wrapper.get('input[type="date"]').setValue('2026-11-29');
    await wrapper.get('button').trigger('click');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('connection dropped');
    expect(wrapper.text()).toContain('Retry original Mark sent');
    const first = sendOnThread.mock.calls[0]!;
    await wrapper.get('button').trigger('click');
    await flushPromises();
    const second = sendOnThread.mock.calls[1]!;
    expect(second[0]).toBe(first[0]);
    expect(second[2]).toBe(first[2]);
    expect(second[1]).toEqual(first[1]);
    expect(second[1]).toMatchObject({ opportunity_id: 'opp-1', recipient_id: 'party-1', primary: { release_id: 'release-1', version: 3 } });
    expect(wrapper.text()).toContain('Touch 1 recorded');
    wrapper.unmount();
  });

  it('does not let a changed material version reuse an acknowledged older release', async () => {
    vi.spyOn(parley, 'listThreads').mockResolvedValue([]);
    const wrapper = mount(ContactThreadPanel, { props: { opportunityId: 'opp-1', material, delivered, threads: [] } });
    await flushPromises();
    await wrapper.setProps({ material: { ...material, version: 4, content_sha256: 'b'.repeat(64) } });
    await flushPromises();
    expect((wrapper.get('button').element as HTMLButtonElement).disabled).toBe(true);
    expect(wrapper.text()).toContain('exact version and recipient');
    wrapper.unmount();
  });

  it('attaches a response to a historical send from the served thread history after reload', async () => {
    const historySend = {
      id: 'send-9', touch_no: 1, primary_material_id: 'mat-1', primary_version_no: 3, recipient_id: 'party-1',
      content_sha256: 'a'.repeat(64), sent_at: '2026-11-21T12:00:00Z', response_deadline: '2026-11-29T08:00:00Z',
      deadline_zone: 'America/Vancouver', next_action_at: null, capacity_charged: false, outcome: null,
    };
    const thread = {
      id: 'thread-1', opportunity_id: 'opp-1', recipient_id: 'party-1', state: 'waiting' as const, current_touch: 1,
      expires_at: '2026-11-29T08:00:00Z', next_action_at: null, sent_at: '2026-11-21T12:00:00Z',
      updated_at: '2026-11-21T12:00:00Z', sends: [historySend],
    };
    const recordOutcome = vi.spyOn(parley, 'recordOutcome')
      .mockResolvedValue({ id: 'outcome-9', send_id: 'send-9', class: 'reply_positive', rung_accepted: null, received_at: '2026-11-22T10:00:00Z', recorded_at: '2026-11-22T10:00:00Z', note: null });
    const wrapper = mount(ContactThreadPanel, { props: { opportunityId: 'opp-1', material, delivered: null, threads: [thread] } });
    await flushPromises();
    expect(wrapper.text()).toContain('Touch 1 · sent');
    expect(wrapper.text()).toContain('Responding to touch 1');
    const record = wrapper.findAll('button').find((button) => button.text().includes('Record response for touch 1'))!;
    await record.trigger('click');
    await flushPromises();
    expect(recordOutcome).toHaveBeenCalledWith('send-9', expect.objectContaining({ class: 'reply_positive' }));
    wrapper.unmount();
  });

  it('requires a recorded reason of at least ten characters before clearing do-not-contact', async () => {
    const declinedThread = {
      id: 'thread-d', opportunity_id: 'opp-1', recipient_id: 'party-1', state: 'closed_declined' as const, current_touch: 1,
      expires_at: null, next_action_at: null, sent_at: '2026-11-21T12:00:00Z',
      updated_at: '2026-11-21T12:00:00Z', sends: [],
    };
    const clearDnc = vi.spyOn(parley, 'clearPartyDnc')
      .mockResolvedValue({ party_id: 'party-1', state: 'open', do_not_contact: false, highest_rung: 0, touch_count: 1, revision: 2 });
    const wrapper = mount(ContactThreadPanel, { props: { opportunityId: 'opp-1', material, delivered: null, threads: [declinedThread] } });
    await flushPromises();
    const clearButton = wrapper.findAll('button').find((button) => button.text().includes('Clear do-not-contact'))!;
    expect((clearButton.element as HTMLButtonElement).disabled).toBe(true);
    const reason = wrapper.get('input[type="text"]');
    await reason.setValue('short');
    await flushPromises();
    expect((clearButton.element as HTMLButtonElement).disabled).toBe(true);
    await reason.setValue('The recipient explicitly asked to be contacted again after the pilot.');
    await flushPromises();
    expect((clearButton.element as HTMLButtonElement).disabled).toBe(false);
    await clearButton.trigger('click');
    await flushPromises();
    expect(clearDnc).toHaveBeenCalledWith('party-1', expect.objectContaining({ reason: expect.stringMatching(/pilot/) }));
    expect(wrapper.get('[role="status"]').text()).toContain('cleared with a recorded reason');
    wrapper.unmount();
  });

  it('surfaces a failed clearance as an alert and keeps the reason input', async () => {
    const declinedThread = {
      id: 'thread-d', opportunity_id: 'opp-1', recipient_id: 'party-1', state: 'closed_declined' as const, current_touch: 1,
      expires_at: null, next_action_at: null, sent_at: '2026-11-21T12:00:00Z',
      updated_at: '2026-11-21T12:00:00Z', sends: [],
    };
    vi.spyOn(parley, 'clearPartyDnc').mockRejectedValue(new Error('party has no suppression to clear'));
    const wrapper = mount(ContactThreadPanel, { props: { opportunityId: 'opp-1', material, delivered: null, threads: [declinedThread] } });
    await flushPromises();
    const reason = wrapper.get('input[type="text"]');
    await reason.setValue('The counterparty asked to be re-contacted after a review.');
    await flushPromises();
    const clearButton = wrapper.findAll('button').find((button) => button.text().includes('Clear do-not-contact'))!;
    await clearButton.trigger('click');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('no suppression to clear');
    expect((reason.element as HTMLInputElement).value).toContain('re-contacted');
    wrapper.unmount();
  });

  it('offers each unanswered touch and attaches a response to the chosen historical send', async () => {
    const firstSend = {
      id: 'send-1', touch_no: 1, primary_material_id: 'mat-1', primary_version_no: 3, recipient_id: 'party-1',
      content_sha256: 'a'.repeat(64), sent_at: '2026-11-21T12:00:00Z', response_deadline: '2026-11-29T08:00:00Z',
      deadline_zone: 'America/Vancouver', next_action_at: null, capacity_charged: false, outcome: null,
    };
    const secondSend = {
      id: 'send-2', touch_no: 2, primary_material_id: 'mat-1', primary_version_no: 4, recipient_id: 'party-1',
      content_sha256: 'b'.repeat(64), sent_at: '2026-11-24T12:00:00Z', response_deadline: '2026-12-01T08:00:00Z',
      deadline_zone: 'America/Vancouver', next_action_at: null, capacity_charged: false, outcome: null,
    };
    const thread = {
      id: 'thread-1', opportunity_id: 'opp-1', recipient_id: 'party-1', state: 'waiting' as const, current_touch: 2,
      expires_at: '2026-12-01T08:00:00Z', next_action_at: null, sent_at: '2026-11-24T12:00:00Z',
      updated_at: '2026-11-24T12:00:00Z', sends: [firstSend, secondSend],
    };
    const recordOutcome = vi.spyOn(parley, 'recordOutcome')
      .mockResolvedValue({ id: 'outcome-2', send_id: 'send-2', class: 'reply_neutral', rung_accepted: null, received_at: '2026-11-25T10:00:00Z', recorded_at: '2026-11-25T10:00:00Z', note: null });
    const wrapper = mount(ContactThreadPanel, { props: { opportunityId: 'opp-1', material, delivered: null, threads: [thread] } });
    await flushPromises();
    const select = wrapper.get('select#response-send');
    expect(select.findAll('option')).toHaveLength(2);
    await select.setValue('send-2');
    await flushPromises();
    expect(wrapper.text()).toContain('Responding to touch 2');
    const record = wrapper.findAll('button').find((button) => button.text().includes('Record response for touch 2'))!;
    await record.trigger('click');
    await flushPromises();
    expect(recordOutcome).toHaveBeenCalledWith('send-2', expect.objectContaining({ class: 'reply_positive' }));
    wrapper.unmount();
  });

  it('shows a stateless empty state when no thread matches the opportunity and recipient', async () => {
    const wrapper = mount(ContactThreadPanel, { props: { opportunityId: 'opp-9', material, delivered: null, threads: [] } });
    await flushPromises();
    expect(wrapper.text()).toContain('No send has been recorded');
    wrapper.unmount();
  });

  it('records a resolution with both sides’ benefit assessments', async () => {
    const thread = {
      id: 'thread-1', opportunity_id: 'opp-1', recipient_id: 'party-1', state: 'waiting' as const, current_touch: 1,
      expires_at: '2026-11-29T08:00:00Z', next_action_at: null, sent_at: '2026-11-21T12:00:00Z',
      updated_at: '2026-11-21T12:00:00Z', sends: [],
    };
    const recordResolution = vi.spyOn(parley, 'recordResolution')
      .mockResolvedValue({ id: 'resolution-1', opportunity_id: 'opp-1', thread_id: null, result: 'mutual_win', they_benefited: 'yes', i_benefited: 'yes', noted_at: '2026-11-25T10:00:00Z' });
    const wrapper = mount(ContactThreadPanel, { props: { opportunityId: 'opp-1', material, delivered: null, threads: [thread] } });
    await flushPromises();
    const selects = wrapper.findAll('fieldset').at(-1)!.findAll('select');
    await selects[0]!.setValue('mutual_win');
    await selects[1]!.setValue('yes');
    await selects[2]!.setValue('yes');
    await flushPromises();
    const button = wrapper.findAll('button').find((item) => item.text().includes('Record resolution'))!;
    await button.trigger('click');
    await flushPromises();
    expect(recordResolution).toHaveBeenCalledWith('opp-1', { result: 'mutual_win', they_benefited: 'yes', i_benefited: 'yes' });
    wrapper.unmount();
  });

  it('freezes the original Mark sent request when its result is unknown', async () => {
    vi.spyOn(parley, 'listThreads').mockResolvedValue([]);
    const sendOnThread = vi.spyOn(parley, 'sendOnThread').mockRejectedValue(new TypeError('connection dropped'));
    const wrapper = mount(ContactThreadPanel, { props: { opportunityId: 'opp-1', material, delivered, threads: [] } });
    await flushPromises();
    await wrapper.get('input[type="date"]').setValue('2026-11-29');
    await wrapper.get('button').trigger('click');
    await flushPromises();
    expect(sendOnThread).toHaveBeenCalledTimes(1);
    // While the previous send result is unknown the deadline input is frozen,
    // so a new send cannot be issued with changed parameters: the retry button
    // re-issues the exact original key and request.
    expect((wrapper.get('input[type="date"]').element as HTMLInputElement).disabled).toBe(true);
    expect(wrapper.text()).toContain('Retry original Mark sent');
    await wrapper.get('button').trigger('click');
    await flushPromises();
    expect(sendOnThread).toHaveBeenCalledTimes(2);
    expect(sendOnThread.mock.calls[1]![0]).toBe(sendOnThread.mock.calls[0]![0]);
    expect(sendOnThread.mock.calls[1]![2]).toBe(sendOnThread.mock.calls[0]![2]);
    wrapper.unmount();
  });

  it('records a rung-accepted outcome with its accepted rung', async () => {
    const historySend = {
      id: 'send-9', touch_no: 1, primary_material_id: 'mat-1', primary_version_no: 3, recipient_id: 'party-1',
      content_sha256: 'a'.repeat(64), sent_at: '2026-11-21T12:00:00Z', response_deadline: '2026-11-29T08:00:00Z',
      deadline_zone: 'America/Vancouver', next_action_at: null, capacity_charged: false, outcome: null,
    };
    const thread = {
      id: 'thread-1', opportunity_id: 'opp-1', recipient_id: 'party-1', state: 'waiting' as const, current_touch: 1,
      expires_at: '2026-11-29T08:00:00Z', next_action_at: null, sent_at: '2026-11-21T12:00:00Z',
      updated_at: '2026-11-21T12:00:00Z', sends: [historySend],
    };
    const recordOutcome = vi.spyOn(parley, 'recordOutcome')
      .mockResolvedValue({ id: 'outcome-9', send_id: 'send-9', class: 'rung_accepted', rung_accepted: 2, received_at: '2026-11-22T10:00:00Z', recorded_at: '2026-11-22T10:00:00Z', note: null });
    const wrapper = mount(ContactThreadPanel, { props: { opportunityId: 'opp-1', material, delivered: null, threads: [thread] } });
    await flushPromises();
    await wrapper.get('select').setValue('rung_accepted');
    await flushPromises();
    await wrapper.get('input[type="number"]').setValue('2');
    await wrapper.get('input[type="datetime-local"]').setValue('2026-11-22T10:00');
    await wrapper.get('input[maxlength="1000"]').setValue('call agreed');
    await flushPromises();
    const record = wrapper.findAll('button').find((button) => button.text().includes('Record response for touch 1'))!;
    await record.trigger('click');
    await flushPromises();
    expect(recordOutcome).toHaveBeenCalledWith('send-9', expect.objectContaining({ class: 'rung_accepted', rung_accepted: 2, note: 'call agreed' }));
    wrapper.unmount();
  });

  it('reopens a timed-out thread with an explicit owner action', async () => {
    const timedOut = {
      id: 'thread-1', opportunity_id: 'opp-1', recipient_id: 'party-1', state: 'timed_out' as const, current_touch: 1,
      expires_at: '2026-11-29T08:00:00Z', next_action_at: null, sent_at: '2026-11-21T12:00:00Z',
      updated_at: '2026-11-29T08:00:00Z', sends: [],
    };
    const reopenThread = vi.spyOn(parley, 'reopenThread')
      .mockResolvedValue({ ...timedOut, state: 'waiting', expires_at: null, next_action_at: null, updated_at: '2026-11-30T08:00:00Z' });
    const wrapper = mount(ContactThreadPanel, { props: { opportunityId: 'opp-1', material, delivered: null, threads: [timedOut] } });
    await flushPromises();
    const button = wrapper.findAll('button').find((item) => item.text().includes('Reopen this thread'))!;
    await button.trigger('click');
    await flushPromises();
    expect(reopenThread).toHaveBeenCalledWith('thread-1');
    expect(wrapper.get('[role="status"]').text()).toContain('Thread reopened by explicit owner action');
    wrapper.unmount();
  });

  it('renders a recorded outcome and the session send receipt text', async () => {
    const recordedSend = {
      id: 'send-1', touch_no: 1, primary_material_id: 'mat-1', primary_version_no: 3, recipient_id: 'party-1',
      content_sha256: 'a'.repeat(64), sent_at: '2026-11-21T12:00:00Z', response_deadline: '2026-11-29T08:00:00Z',
      deadline_zone: 'America/Vancouver', next_action_at: null, capacity_charged: false,
      outcome: { id: 'outcome-1', send_id: 'send-1', class: 'reply_positive', rung_accepted: null, received_at: '2026-11-22T10:00:00Z', recorded_at: '2026-11-22T10:00:00Z', note: null },
    };
    const thread = {
      id: 'thread-1', opportunity_id: 'opp-1', recipient_id: 'party-1', state: 'conversing' as const, current_touch: 1,
      expires_at: null, next_action_at: null, sent_at: '2026-11-21T12:00:00Z',
      updated_at: '2026-11-22T10:00:00Z', sends: [recordedSend],
    };
    const wrapper = mount(ContactThreadPanel, { props: { opportunityId: 'opp-1', material, delivered: null, threads: [thread] } });
    await flushPromises();
    expect(wrapper.text()).toContain('reply positive');
    expect(wrapper.text()).not.toContain('no response recorded');
    expect(wrapper.findAll('button').some((button) => button.text().includes('Record response'))).toBe(false);
    wrapper.unmount();
  });
});
