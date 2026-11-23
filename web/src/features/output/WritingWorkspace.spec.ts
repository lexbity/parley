import { mount, flushPromises } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type GraphEntity, type Opportunity, type WritingTemplate } from '../../api/client';
import WritingWorkspace from './WritingWorkspace.vue';

afterEach(() => vi.restoreAllMocks());

const party: GraphEntity = { id: 'person-1', kind: 'person', name: 'Dana Lee', revision: 1 };
const opportunity: Opportunity = { id: 'opp-1', title: 'Onboarding', state: 'open', revision: 1, participant_ids: [party.id], source_variant_ids: [], material_ids: [] };
const template: WritingTemplate = { id: 'template-1', family_id: 'family-1', name: 'One ask', version: 1, status: 'draft', body: '{{their_name:name}},\nI can offer {{gift:guide}}.\nWould a review help {{ask:next_step}}?', sentence_rules: ['R1', 'R2'], stage_class: 'identified', channel: 'email', parent_version_id: null };

function setup() {
  vi.spyOn(parley, 'listWritingTemplates').mockResolvedValue([]);
  vi.spyOn(parley, 'listTemplateFamilies').mockResolvedValue([]);
  vi.spyOn(parley, 'getEmbeddingModel').mockResolvedValue({ model: '', model_digest: null, calibrated_kinds: [] });
  vi.spyOn(parley, 'getWritingInterests').mockResolvedValue([]);
  vi.spyOn(parley, 'getWritingExchange').mockResolvedValue({ current: null, versions: [] });
  return mount(WritingWorkspace, { props: { opportunities: [opportunity], parties: [party], resources: [], outputs: [] } });
}

describe('WritingWorkspace', () => {
  it('binds interests and an explicit fairness attestation to the selected recipient', async () => {
    const wrapper = setup(); await flushPromises();
    const add = vi.spyOn(parley, 'addWritingInterest').mockImplementation(async (_opportunity, input) => ({ id: input.side === 'them' ? 'i1' : 'i2', side: input.side, statement: input.statement, knowledge_kind: input.knowledge_kind, check_suggestion: input.check_suggestion ?? null, claim_id: null, claim_revision: null, claim: null, revision: 1, status: 'active' }));
    let exchangeSaved = false;
    const save = vi.spyOn(parley, 'saveWritingExchange').mockImplementation(async () => { exchangeSaved = true; return { version: 1 }; });
    const attest = vi.spyOn(parley, 'attestWritingExchange').mockResolvedValue();
    vi.spyOn(parley, 'getWritingInterests').mockResolvedValueOnce([]).mockResolvedValue([{ id: 'i1', side: 'them', statement: 'Reduce onboarding friction', knowledge_kind: 'hypothesis', check_suggestion: 'Verify', claim_id: null, claim_revision: null, claim: null, revision: 1, status: 'active' }, { id: 'i2', side: 'me', statement: 'Find a useful role', knowledge_kind: 'owner_report', check_suggestion: null, claim_id: null, claim_revision: null, claim: null, revision: 1, status: 'active' }]);
    vi.spyOn(parley, 'getWritingExchange').mockImplementation(async () => ({ current: exchangeSaved ? { version: 1, they_get: 'A checklist', i_get: 'A useful conversation', fairness_attested: false } : null, versions: [] }));
    await wrapper.get('select').setValue('opp-1'); await wrapper.findAll('select')[1]!.setValue('person-1'); await flushPromises();
    await wrapper.findAll('form')[0]!.get('textarea').setValue('Reduce onboarding friction'); await wrapper.findAll('form')[0]!.trigger('submit'); await flushPromises();
    expect(add).toHaveBeenCalledWith('opp-1', expect.objectContaining({ recipient_id: 'person-1', side: 'them', knowledge_kind: 'hypothesis', check_suggestion: expect.any(String) }));
    await wrapper.findAll('form')[1]!.get('textarea').setValue('Find a useful role'); await wrapper.findAll('form')[1]!.trigger('submit'); await flushPromises();
    await wrapper.findAll('textarea')[2]!.setValue('A checklist'); await wrapper.findAll('textarea')[3]!.setValue('A useful conversation');
    await wrapper.findAll('button').find((item) => item.text() === 'Save exchange version')!.trigger('click'); await flushPromises();
    expect(save).toHaveBeenCalledWith('opp-1', expect.objectContaining({ recipient_id: 'person-1', interest_ids: ['i1', 'i2'] }));
    await wrapper.findAll('button').find((item) => item.text() === 'Attest both sides benefit')!.trigger('click');
    expect(attest).toHaveBeenCalledWith('opp-1', 'person-1');
    wrapper.unmount();
  });

  it('creates, reviews, and activates an immutable template version and blocks model actions without calibration', async () => {
    const createFamily = vi.spyOn(parley, 'createTemplateFamily').mockResolvedValue({ id: 'family-1', name: 'One ask' });
    const createVersion = vi.spyOn(parley, 'createWritingTemplateVersion').mockResolvedValue(template);
    vi.spyOn(parley, 'listWritingTemplates').mockResolvedValueOnce([]).mockResolvedValueOnce([{ ...template, status: 'approved' }]).mockResolvedValueOnce([{ ...template, status: 'active' }]);
    const approve = vi.spyOn(parley, 'approveWritingTemplate').mockResolvedValue();
    const activate = vi.spyOn(parley, 'activateWritingTemplate').mockResolvedValue();
    const wrapper = setup(); await flushPromises();
    await wrapper.findAll('button').find((item) => item.text() === 'Save as new draft version')!.trigger('click'); await flushPromises();
    expect(createFamily).toHaveBeenCalledWith('One ask'); expect(createVersion).toHaveBeenCalledWith('family-1', expect.objectContaining({ name: 'One ask', body: expect.stringContaining('{{gift:guide}}'), sentence_rules: [[], ['R1'], ['R2']] }));
    await wrapper.findAll('button').find((item) => item.text() === 'Owner approve version')!.trigger('click'); await flushPromises();
    await wrapper.findAll('button').find((item) => item.text() === 'Activate validated version')!.trigger('click'); await flushPromises();
    expect(approve).toHaveBeenCalledWith('template-1'); expect(activate).toHaveBeenCalledWith('template-1');
    expect(wrapper.text()).toContain('calibrated kinds: none');
    expect(wrapper.findAll('button').find((item) => item.text() === 'Run exact matcher')?.attributes('disabled')).toBeDefined();
    wrapper.unmount();
  });

  it('keeps Composer unavailable offline and never offers release for an unsaved proposal', async () => {
    const wrapper = setup(); await flushPromises();
    expect(wrapper.text()).toContain('Without a matching model calibration, the system cannot rank resources.');
    expect(wrapper.findAll('button').find((item) => item.text() === 'Generate recipient-bound proposal')?.attributes('disabled')).toBeDefined();
    expect(wrapper.findAll('button').some((item) => item.text().includes('Release'))).toBe(false);
    wrapper.unmount();
  });

  it('refuses to save a composer proposal generated for one opportunity under another', async () => {
    const secondOpportunity: Opportunity = { id: 'opp-2', title: 'Second onboarding', state: 'open', revision: 1, participant_ids: [party.id], source_variant_ids: [], material_ids: [] };
    vi.spyOn(parley, 'listWritingTemplates').mockResolvedValue([]);
    vi.spyOn(parley, 'listTemplateFamilies').mockResolvedValue([]);
    vi.spyOn(parley, 'listVoiceSamples').mockResolvedValue([]);
    vi.spyOn(parley, 'getEmbeddingModel').mockResolvedValue({ model: 'local', model_digest: 'digest-1', calibrated_kinds: ['gift'] });
    vi.spyOn(parley, 'getWritingInterests').mockResolvedValue([]);
    vi.spyOn(parley, 'getWritingExchange').mockResolvedValue({ current: null, versions: [] });
    vi.spyOn(parley, 'startMatching').mockResolvedValue({ job_id: 'job-a', role: 'matcher' });
    vi.spyOn(parley, 'getRoleJob').mockResolvedValue({ status: 'succeeded', result: { body: 'stale proposal body' } } as unknown as Awaited<ReturnType<typeof parley.getRoleJob>>);
    vi.spyOn(parley, 'getWritingMatch').mockResolvedValue({ id: 'match-a', opportunity_id: 'opp-1', recipient_id: party.id, embed_model_digest: 'digest-1', calibration_json: '{}', shortlist_json: '[]', why_job_id: null } as Awaited<ReturnType<typeof parley.getWritingMatch>>);
    const save = vi.spyOn(parley, 'saveWritingDraft').mockResolvedValue({ material_id: 'mat-1' });
    const wrapper = mount(WritingWorkspace, { props: { opportunities: [opportunity, secondOpportunity], parties: [party], resources: [], outputs: [] } });
    await flushPromises();

    await wrapper.findAll('select')[0]!.setValue('opp-1'); await wrapper.findAll('select')[1]!.setValue(party.id); await flushPromises();
    await wrapper.findAll('button').find((item) => item.text() === 'Run exact matcher')!.trigger('click'); await flushPromises();
    expect(wrapper.text()).toContain('stale proposal body');

    // Switch to a different opportunity. The proposal belongs to opp-1 and
    // must not be savable under opp-2 (F-FR-1).
    await wrapper.findAll('select')[0]!.setValue('opp-2'); await flushPromises();
    await wrapper.findAll('button').find((item) => item.text() === 'Save proposal as draft')!.trigger('click'); await flushPromises();
    expect(save).not.toHaveBeenCalled();
    expect(wrapper.text()).toContain('belongs to');
    wrapper.unmount();
  });
});
