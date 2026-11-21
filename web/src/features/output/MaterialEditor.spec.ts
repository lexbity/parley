import { flushPromises, mount } from '@vue/test-utils';
import { describe, expect, it, vi } from 'vitest';
import MaterialEditor, {
  type CurrentRelease,
  type MaterialApproval,
  type MaterialGate,
  type MaterialRecord,
  type MaterialRecipient,
} from './MaterialEditor.vue';

const recipient: MaterialRecipient = { id: 'person-dana', kind: 'person', name: 'Dana Lee' };
const material: MaterialRecord = {
  id: 'material-1',
  kind: 'outreach',
  versionId: 'version-1',
  revision: 3,
  body: 'I can share a checklist.',
  status: 'draft',
  updatedAt: '2026-11-21T12:00:00Z',
  sentences: [{ text: 'I can share a checklist.', claims: [{ type: 'offer', ref_kind: 'gift', ref_id: 'gift-1' }], rules: ['R1'] }],
  slotFills: { gift: 'A checklist' },
  protectedTokens: ['A checklist'],
};
const blockedGate: MaterialGate = {
  evaluatedAt: '2026-11-21T12:01:00Z',
  hardFailures: [{ id: 'H3 GROUNDED', detail: 'Sentence 1 needs eligible evidence.' }],
  softFailures: [
    { id: 'S5 TONE_LINT', detail: 'Review the tone finding.' },
    { id: 'S8 DAILY_CAP', detail: 'The daily suggestion cap was reached.' },
  ],
  copyEnabled: false,
};
const cleanGate: MaterialGate = {
  evaluatedAt: '2026-11-21T12:02:00Z',
  hardFailures: [],
  softFailures: [],
  copyEnabled: true,
};
const softGate: MaterialGate = { ...blockedGate, hardFailures: [] };
const approval: MaterialApproval = { versionId: 'version-1', recipientId: recipient.id, approvedAt: '2026-11-21T12:03:00Z' };
const release: CurrentRelease = {
  grantId: 'grant-1', versionId: 'version-1', recipientId: recipient.id, digest: 'sha256:approved', body: 'Approved exact bytes.',
};

function mountEditor(overrides: Record<string, unknown> = {}) {
  return mount(MaterialEditor, {
    props: {
      opportunityId: 'opportunity-1',
      opportunityTitle: 'Northline onboarding',
      recipient,
      material,
      gate: blockedGate,
      claimReferences: [{ kind: 'gift', id: 'gift-1', label: 'Onboarding checklist', tier: 3 }],
      approval: null,
      currentRelease: null,
      ...overrides,
    },
  });
}

function button(wrapper: ReturnType<typeof mount>, label: string) {
  const found = wrapper.findAll('button').find((candidate) => candidate.text().trim() === label);
  if (!found) throw new Error('Button not found: ' + label);
  return found;
}

describe('S03 material editor', () => {
  it('keeps the recipient visible, displays server hard and soft verdicts, and requires individual reasons', async () => {
    const wrapper = mountEditor();
    expect(wrapper.get('[aria-label="Named recipient"]').text()).toContain('Dana Lee');
    expect(wrapper.text()).toContain('H3 GROUNDED');
    expect(wrapper.text()).toContain('S5 TONE_LINT');
    expect(wrapper.text()).toContain('S8 DAILY_CAP');
    expect((button(wrapper, 'Approve this version').element as HTMLButtonElement).disabled).toBe(true);
    await wrapper.setProps({ gate: softGate });
    await wrapper.findAll('.soft-failure textarea')[0]!.setValue('Too short');
    expect((button(wrapper, 'Approve this version').element as HTMLButtonElement).disabled).toBe(true);
    await wrapper.findAll('.soft-failure textarea')[0]!.setValue('Review the tone');
    await wrapper.findAll('.soft-failure textarea')[1]!.setValue('This is still important');
    expect((button(wrapper, 'Approve this version').element as HTMLButtonElement).disabled).toBe(false);
    await button(wrapper, 'Approve this version').trigger('click');
    expect(wrapper.emitted('approve')?.[0]?.[0]).toEqual({
      materialId: 'material-1',
      versionId: 'version-1',
      expectedRevision: 3,
      overrideReasons: { 'S5 TONE_LINT': 'Review the tone', 'S8 DAILY_CAP': 'This is still important' },
    });
    expect(wrapper.text()).toContain('Preview remains available for owner review. It is not release content.');
    expect(wrapper.text()).toContain('Copying text does not mark it sent or start a reply timer.');
    wrapper.unmount();
  });

  it('saves a new immutable version with sentence claims, selected rules, slot fills and protected tokens', async () => {
    const wrapper = mountEditor({ gate: cleanGate });
    await wrapper.get('#material-body').setValue('I can share a checklist. Is that useful?');
    await wrapper.get('#claim-type-0-0').setValue('offer');
    await wrapper.get('#claim-ref-0-0').setValue('gift:gift-1');
    await wrapper.get('#slot-value-0').setValue('A checklist');
    await button(wrapper, 'Save as a new version').trigger('click');
    expect(wrapper.emitted('saveVersion')?.[0]?.[0]).toEqual({
      materialId: 'material-1',
      expectedRevision: 3,
      body: 'I can share a checklist. Is that useful?',
      sentences: [
        { text: 'I can share a checklist.', claims: [{ type: 'offer', ref_kind: 'gift', ref_id: 'gift-1' }], rules: ['R1'] },
        { text: 'Is that useful?', claims: [], rules: [] },
      ],
      slotFills: { gift: 'A checklist' },
      protectedTokens: ['A checklist'],
    });
    wrapper.unmount();
  });

  it('shows base, mine and current version on conflict and requires an explicit choice', async () => {
    const wrapper = mountEditor();
    await wrapper.get('#material-body').setValue('My unsaved wording.');
    await wrapper.setProps({ material: { ...material, versionId: 'version-2', revision: 4, body: 'Server wording.' } });
    expect(wrapper.get('[role="alert"]').text()).toContain('This material changed while you were editing');
    expect(wrapper.text()).toContain('Your base version · version-1');
    expect(wrapper.text()).toContain('I can share a checklist.');
    expect(wrapper.text()).toContain('My unsaved wording.');
    expect(wrapper.text()).toContain('Server wording.');
    await button(wrapper, 'Create a new version from my text').trigger('click');
    expect(wrapper.emitted('saveVersion')?.at(-1)?.[0]).toMatchObject({ materialId: 'material-1', expectedRevision: 4, body: 'My unsaved wording.' });
    wrapper.unmount();
  });

  it('binds request and clipboard confirmation to the approved recipient and exact version', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    const wrapper = mountEditor({ gate: cleanGate, approval });
    await button(wrapper, 'Prepare approved copy').trigger('click');
    expect(wrapper.emitted('requestCopy')?.[0]?.[0]).toEqual({
      materialId: 'material-1', versionId: 'version-1', recipientId: recipient.id,
    });
    await wrapper.setProps({ currentRelease: release });
    await button(wrapper, 'Copy approved text').trigger('click');
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith(release.body);
    const firstAck = wrapper.emitted('acknowledgeCopy')?.[0]?.[0] as { grantId: string; success: boolean; operationKey: string };
    expect(firstAck).toMatchObject({ grantId: release.grantId, success: true });
    await wrapper.setProps({ currentRelease: { ...release, acknowledgementState: 'unknown' } });
    await button(wrapper, 'Retry copy acknowledgement').trigger('click');
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(wrapper.emitted('acknowledgeCopy')?.[1]?.[0]).toEqual(firstAck);
    await wrapper.setProps({ currentRelease: { ...release, acknowledgementState: 'recorded' } });
    expect(wrapper.text()).toContain('Copy confirmed by the clipboard API and acknowledged.');
    wrapper.unmount();
  });

  it('reports clipboard rejection without claiming delivery and rejects stale release bindings', async () => {
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: vi.fn().mockRejectedValue(new Error('denied')) } });
    const staleRelease = { ...release, recipientId: 'another-recipient' };
    const wrapper = mountEditor({ gate: cleanGate, approval: { ...approval, recipientId: 'another-recipient' }, currentRelease: staleRelease });
    expect(wrapper.text()).toContain('Approval belongs to an earlier version or recipient');
    expect(wrapper.findAll('button').some((button) => button.text() === 'Copy approved text')).toBe(false);
    await button(wrapper, 'Prepare approved copy').trigger('click');
    expect(wrapper.emitted('requestCopy')).toBeUndefined();
    await wrapper.setProps({ approval });
    await button(wrapper, 'Prepare approved copy').trigger('click');
    expect(wrapper.emitted('requestCopy')).toHaveLength(1);
    await wrapper.setProps({ approval, currentRelease: release });
    await button(wrapper, 'Copy approved text').trigger('click');
    await flushPromises();
    expect(wrapper.emitted('acknowledgeCopy')?.[0]?.[0]).toMatchObject({ grantId: release.grantId, success: false });
    expect(wrapper.text()).toContain('No delivery or reply timer was recorded.');
    wrapper.unmount();
  });
});
