import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type PackSettings, type PackValidation } from '../../api/client';
import Packs from './Packs.vue';

afterEach(() => vi.restoreAllMocks());

const packDocument = 'id: job_search\nname: Job search\nversion: 1.0.0\nstages:\n  - { name: Target, class: identified }\nask_ladder:\n  - { rung: 1, weight: 1 }\ncadence:\n  max_touches: 3\nlimits:\n  daily_send_cap: 12\n';
const settings: PackSettings = { revision: 3, active_pack_id: 'job_search', active_pack_version: '1.0.0', active_document: packDocument, available_packs: [{ id: 'job_search', name: 'Job search', version: '1.0.0', document: packDocument }, { id: 'lead_gen', name: 'Lead generation', version: '1.0.0', document: packDocument.replaceAll('job_search', 'lead_gen').replaceAll('Job search', 'Lead generation') }] };

describe('Packs S10', () => {
  it('retains the active version when the server reports line-level validation failures', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'getPackSettings').mockResolvedValue(settings);
    vi.spyOn(parley, 'validatePack').mockResolvedValue({ valid: false, pack_id: null, version: null, normalized_document: null, errors: [{ line: 5, field: 'stages', message: 'Map every stage to one canonical class.' }] });
    const activate = vi.spyOn(parley, 'activatePack');
    const wrapper = mount(Packs);
    await flushPromises();
    await wrapper.get('button').trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('Line 5 · stages');
    expect(wrapper.text()).toContain('The previously active pack remains in use.');
    expect(activate).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('requires a stage mapping before switching pack versions and activates only the validated bytes', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'getPackSettings').mockResolvedValue(settings).mockResolvedValueOnce(settings);
    const validation: PackValidation = { valid: true, pack_id: 'lead_gen', version: '1.0.1', normalized_document: packDocument.replace('id: job_search', 'id: lead_gen').replace('version: 1.0.0', 'version: 1.0.1'), errors: [] };
    vi.spyOn(parley, 'validatePack').mockResolvedValue(validation);
    const activated = { ...settings, revision: 4, active_pack_id: 'lead_gen', active_pack_version: '1.0.1', active_document: validation.normalized_document! };
    const activate = vi.spyOn(parley, 'activatePack').mockResolvedValue(activated);
    const wrapper = mount(Packs);
    await flushPromises();
    await wrapper.get('select').setValue('lead_gen@1.0.0');
    await wrapper.get('textarea').setValue(validation.normalized_document!);
    await wrapper.findAll('button').find((button) => button.text() === 'Validate pack')!.trigger('click');
    await flushPromises();
    const activateButton = wrapper.findAll('button').find((button) => button.text() === 'Activate validated version')!;
    expect((activateButton.element as HTMLButtonElement).disabled).toBe(true);
    await wrapper.get('textarea[aria-label="Stage mapping JSON"]').setValue('{"Briefed":"prepared"}');
    expect((activateButton.element as HTMLButtonElement).disabled).toBe(false);
    await activateButton.trigger('click');
    await flushPromises();
    expect(activate).toHaveBeenCalledWith({ pack_id: 'lead_gen', version: '1.0.1', document: validation.normalized_document, expected_settings_revision: 3, stage_mapping: { Briefed: 'prepared' } });
    expect(wrapper.text()).toContain('Historical sends retain their original pack and gate snapshot.');
    wrapper.unmount();
  });
});
