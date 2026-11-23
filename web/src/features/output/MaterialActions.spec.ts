import { mount, flushPromises } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type Material, type ReleasedMaterial } from '../../api/client';
import MaterialActions from './MaterialActions.vue';

const sha = 'd6f36942493eec3600c9e18bccf0da8ffb3a21e936a298e85228513cec155934';
function material(overrides: Partial<Material> = {}): Material {
  return {
    id: 'm1', opportunity_id: 'o1', recipient_id: 'p1', recipient_name: 'Dana Lee', kind: 'outreach',
    version: 1, body: 'Hello Dana.', content_sha256: sha, status: 'draft', gate: null, attachment: null, ...overrides,
  };
}
const release: ReleasedMaterial = {
  release_id: 'r1', material_id: 'm1', version: 1, recipient_id: 'p1', body: 'Hello Dana.',
  content_sha256: sha, issued_at: '2026-11-23T00:00:00Z', attachment: null,
};
const soft = { id: 'S1 GROUNDING', severity: 'soft' as const, pass: false, detail: 'Add evidence', overridden: false };
const hard = { id: 'H3 GROUNDED', severity: 'hard' as const, pass: true, detail: '', overridden: false };

afterEach(() => vi.restoreAllMocks());

/** WebCrypto digest settles one native macrotask after Vue's promise flush. */
async function settle() {
  await flushPromises();
  await new Promise((resolve) => globalThis.setTimeout(resolve, 10));
  await flushPromises();
}

describe('MaterialActions', () => {
  it('evaluates the deterministic gate and approves only after a sufficient override reason', async () => {
    vi.spyOn(parley, 'evaluateMaterialGate').mockResolvedValue({ checks: [soft, hard], copy_enabled: true });
    const approve = vi.spyOn(parley, 'approveMaterial').mockResolvedValue(material({ status: 'approved' }));
    const wrapper = mount(MaterialActions, { props: { material: material() } });
    await flushPromises();
    expect(wrapper.text()).toContain('S1 GROUNDING');
    const approveButton = () => wrapper.findAll('button').find((button) => button.text().includes('Approve'));
    expect(approveButton()?.attributes('disabled')).toBeDefined();
    await wrapper.get('textarea').setValue('Reviewed evidence from the signed source.');
    expect(approveButton()?.attributes('disabled')).toBeDefined();
    await wrapper.findAll('button').find((button) => button.text() === 'Recalculate current gate report')!.trigger('click');
    await flushPromises();
    expect(approveButton()?.attributes('disabled')).toBeUndefined();
    await approveButton()!.trigger('click');
    await flushPromises();
    expect(approve).toHaveBeenCalledWith('m1', { 'S1 GROUNDING': 'Reviewed evidence from the signed source.' });
    expect(wrapper.emitted('updated')?.[0]).toBeDefined();
    wrapper.unmount();
  });

  it('releases an exact approved version, copies it and acknowledges delivery', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(globalThis.navigator, 'clipboard', { configurable: true, value: { writeText } });
    const releaseMaterial = vi.spyOn(parley, 'releaseMaterial').mockResolvedValue(release);
    const ack = vi.spyOn(parley, 'acknowledgeRelease').mockResolvedValue();
    const wrapper = mount(MaterialActions, { props: { material: material({ status: 'approved', gate: { checks: [hard], copy_enabled: true } }) } });
    await wrapper.findAll('button').find((button) => button.text() === 'Copy approved text')!.trigger('click');
    await settle();
    expect(releaseMaterial).toHaveBeenCalledWith('m1');
    expect(writeText).toHaveBeenCalledWith('Hello Dana.');
    expect(ack).toHaveBeenCalledWith('r1', true);
    expect(wrapper.emitted('delivered')?.[0]).toEqual([release]);
    expect(wrapper.text()).toContain('Mark sent remains a separate action');
    wrapper.unmount();
  });

  it('reconciles a pending acknowledgement and refuses a digest mismatch', async () => {
    const ack = vi.spyOn(parley, 'acknowledgeRelease')
      .mockRejectedValueOnce(new Error('ack unavailable'))
      .mockResolvedValue();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(globalThis.navigator, 'clipboard', { configurable: true, value: { writeText } });
    vi.spyOn(parley, 'releaseMaterial').mockResolvedValue(release);
    const wrapper = mount(MaterialActions, { props: { material: material({ status: 'approved', gate: { checks: [hard], copy_enabled: true } }) } });
    await wrapper.findAll('button').find((button) => button.text() === 'Copy approved text')!.trigger('click');
    await settle();
    expect(wrapper.text()).toContain('acknowledgement is pending');
    await wrapper.findAll('button').find((button) => button.text() === 'Reconcile original release acknowledgement')!.trigger('click');
    await settle();
    expect(ack).toHaveBeenCalledTimes(2);
    wrapper.unmount();

    vi.spyOn(parley, 'releaseMaterial').mockResolvedValue({ ...release, body: 'Tampered text.' });
    const second = mount(MaterialActions, { props: { material: material({ status: 'approved', gate: { checks: [hard], copy_enabled: true } }) } });
    await second.findAll('button').find((button) => button.text() === 'Copy approved text')!.trigger('click');
    await settle();
    expect(second.get('[role="alert"]').text()).toContain('integrity check');
    second.unmount();
  });
});
