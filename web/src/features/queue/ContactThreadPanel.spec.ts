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
});
