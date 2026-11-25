import { mount, flushPromises } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type Material, type OpportunitySummary, type ReleasedMaterial, type Resource, type WritingTemplate } from '../../api/client';
import OutputWorkspace from './OutputWorkspace.vue';

afterEach(() => vi.restoreAllMocks());

const party = { id: 'person-1', kind: 'person' as const, name: 'Dana Lee' };
const opportunity: OpportunitySummary = {
  id: 'opp-1', title: 'Onboarding', state: 'open', revision: 1, participants: [party],
  tags: [], next_task: null, next_deadline: null, missing_material: true,
};
const sha = 'd6f36942493eec3600c9e18bccf0da8ffb3a21e936a298e85228513cec155934';
function material(overrides: Partial<Material> = {}): Material {
  return {
    id: 'm1', opportunity_id: 'opp-1', recipient_id: 'person-1', recipient_name: 'Dana Lee', kind: 'outreach',
    version: 1, body: 'Hello Dana.', content_sha256: sha, status: 'draft', gate: null, attachment: null, ...overrides,
  };
}
const gift: Resource = { id: 'r1', kind: 'gift', title: 'First-week checklist', body: { what: 'the first-week checklist' }, readiness: 'ready', revision: 8, etag: 'e1' };
const activeTemplate: WritingTemplate = {
  id: 'template-1', family_id: 'family-1', name: 'Field introduction', version: 2, status: 'active',
  body: 'Hi {{their_name:name}},\nI can offer {{gift:guide}}.\nWould {{ask:walkthrough}} be useful?',
  sentence_rules: ['R1', 'R2'], stage_class: 'prepared', channel: 'email', parent_version_id: null,
};
const release: ReleasedMaterial = {
  release_id: 'r1', material_id: 'm1', version: 1, recipient_id: 'person-1', body: 'Hello Dana.',
  content_sha256: sha, issued_at: '2026-11-23T00:00:00Z', attachment: null,
};
const soft = { id: 'S1 GIVE_FIRST', severity: 'soft' as const, pass: false, detail: 'Include a real gift', overridden: false };
const hard = { id: 'H3 GROUNDED', severity: 'hard' as const, pass: true, detail: '', overridden: false };

function stubAuthoring(overrides: {
  templates?: WritingTemplate[];
  embedding?: { model: string; model_digest: string | null; calibrated_kinds: string[] };
} = {}) {
  vi.spyOn(parley, 'listWritingTemplates').mockResolvedValue(overrides.templates ?? []);
  vi.spyOn(parley, 'listTemplateFamilies').mockResolvedValue([{ id: 'family-1', name: 'One ask' }]);
  vi.spyOn(parley, 'listVoiceSamples').mockResolvedValue([]);
  vi.spyOn(parley, 'getEmbeddingModel').mockResolvedValue(overrides.embedding ?? { model: '', model_digest: null, calibrated_kinds: [] });
  vi.spyOn(parley, 'getWritingInterests').mockResolvedValue([]);
  vi.spyOn(parley, 'getWritingExchange').mockResolvedValue({ current: null, versions: [] });
  vi.spyOn(parley, 'getWritingMatch').mockRejectedValue(new Error('no completed match'));
  vi.spyOn(parley, 'listThreads').mockResolvedValue([]);
}

function stubOpportunities(list: OpportunitySummary[] = [opportunity]) {
  vi.spyOn(parley, 'listOpportunitiesPage').mockImplementation(async (filters = {}) => {
    const q = (filters.q ?? '').trim().toLocaleLowerCase();
    const rows = q ? list.filter(item => item.title.toLocaleLowerCase().includes(q)) : list;
    return { items: rows.slice(0, filters.limit ?? 50), nextCursor: null };
  });
  vi.spyOn(parley, 'getOpportunity').mockImplementation(async (id) => {
    const found = list.find(item => item.id === id);
    return found ? {
      id: found.id, title: found.title, state: found.state, revision: found.revision,
      participant_ids: found.participants.map(party => party.id), source_variant_ids: [], material_ids: [],
    } : (() => { throw new Error('not found'); })();
  });
  vi.spyOn(parley, 'getEntity').mockImplementation(async (id) => {
    const found = list.flatMap(item => item.participants).find(party => party.id === id);
    if (!found) throw new Error('not found');
    return { id: found.id, kind: found.kind, name: found.name, focus: '', revision: 1 };
  });
}

function mountWorkspace(props: Partial<InstanceType<typeof OutputWorkspace>['$props']> = {}, list: OpportunitySummary[] = [opportunity]) {
  stubOpportunities(list);
  const savedOutputs = (props.outputs ?? []) as Material[];
  vi.spyOn(parley, 'getMaterial').mockImplementation(async (id) => {
    const found = savedOutputs.find(item => item.id === id);
    if (!found) throw new Error('not found');
    return found;
  });
  return mount(OutputWorkspace, {
    props: {
      resources: [], outputs: [],
      active: true, recordId: null, location: null, ...props,
    },
  });
}

/** WebCrypto digest settles one native macrotask after Vue's promise flush. */
async function settle() {
  await flushPromises();
  await new Promise((resolve) => globalThis.setTimeout(resolve, 10));
  await flushPromises();
}

async function openTool(wrapper: ReturnType<typeof mount>, group: string, command: string) {
  await wrapper.findAll('button').find((button) => button.text() === group)!.trigger('click');
  await wrapper.findAll('button').find((button) => button.text().includes(command))!.trigger('click');
  await flushPromises();
}

describe('OutputWorkspace', () => {
  it('binds interests and an explicit fairness attestation to the selected recipient', async () => {
    stubAuthoring();
    let savedInterests: Array<Record<string, unknown>> = [];
    let exchangeSaved = false;
    const add = vi.spyOn(parley, 'addWritingInterest').mockImplementation(async (_opportunity, input) => {
      const item = {
        id: input.side === 'them' ? 'i1' : 'i2', side: input.side, statement: input.statement,
        knowledge_kind: input.knowledge_kind, check_suggestion: input.check_suggestion ?? null,
        claim_id: null, claim_revision: null, claim: null, revision: 1, status: 'active',
      };
      savedInterests = [...savedInterests, item];
      return item as Awaited<ReturnType<typeof parley.addWritingInterest>>;
    });
    const save = vi.spyOn(parley, 'saveWritingExchange').mockImplementation(async () => { exchangeSaved = true; return { version: 1 }; });
    const attest = vi.spyOn(parley, 'attestWritingExchange').mockResolvedValue();
    vi.spyOn(parley, 'getWritingInterests').mockImplementation(async () => savedInterests as Awaited<ReturnType<typeof parley.getWritingInterests>>);
    vi.spyOn(parley, 'getWritingExchange').mockImplementation(async () => ({
      current: exchangeSaved ? { version: 1, they_get: 'A checklist', i_get: 'A useful conversation', fairness_attested: false } : null,
      versions: [],
    }));
    const wrapper = mountWorkspace();
    await flushPromises();

    await openTool(wrapper, 'Context', 'Interests and exchange');
    const dialog = wrapper.get('dialog');
    const textareas = dialog.findAll('textarea');
    await textareas[0]!.setValue('Reduce onboarding friction');
    await textareas[1]!.setValue('Find a useful role');
    await textareas[2]!.setValue('A checklist');
    await textareas[3]!.setValue('A useful conversation');
    await dialog.findAll('button').find((button) => button.text() === 'Save exchange version')!.trigger('click');
    await flushPromises();
    expect(add).toHaveBeenCalledWith('opp-1', expect.objectContaining({ recipient_id: 'person-1', side: 'them', knowledge_kind: 'hypothesis', check_suggestion: expect.any(String) }));
    expect(add).toHaveBeenCalledWith('opp-1', expect.objectContaining({ recipient_id: 'person-1', side: 'me', knowledge_kind: 'owner_report' }));
    expect(save).toHaveBeenCalledWith('opp-1', expect.objectContaining({ recipient_id: 'person-1', they_get: 'A checklist', i_get: 'A useful conversation' }));

    // The refreshed current exchange is unattested; attestation stays explicit.
    const attestDialog = wrapper.get('dialog');
    await attestDialog.get('input[type="checkbox"]').setValue(true);
    await attestDialog.findAll('button').find((button) => button.text() === 'Attest saved exchange')!.trigger('click');
    await flushPromises();
    expect(attest).toHaveBeenCalledWith('opp-1', 'person-1');
    wrapper.unmount();
  });

  it('creates, reviews, and activates an immutable template version from the library', async () => {
    const template: WritingTemplate = { ...activeTemplate, status: 'draft' };
    stubAuthoring();
    vi.spyOn(parley, 'listWritingTemplates')
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([{ ...template, status: 'approved' }])
      .mockResolvedValueOnce([{ ...template, status: 'active' }]);
    const createVersion = vi.spyOn(parley, 'createWritingTemplateVersion').mockResolvedValue(template);
    const approve = vi.spyOn(parley, 'approveWritingTemplate').mockResolvedValue();
    const activate = vi.spyOn(parley, 'activateWritingTemplate').mockResolvedValue();
    const wrapper = mountWorkspace();
    await flushPromises();

    await openTool(wrapper, 'Insert', 'Template library');
    const dialog = wrapper.get('dialog');
    await dialog.findAll('button').find((button) => button.text() === 'Save as new draft version')!.trigger('click');
    await flushPromises();
    expect(createVersion).toHaveBeenCalledWith('family-1', expect.objectContaining({
      name: 'One ask', body: expect.stringContaining('{{gift:guide}}'), sentence_rules: [[], ['R1'], ['R2']], proof_fallback: 'omit_sentence',
    }));
    await dialog.findAll('button').find((button) => button.text() === 'Owner approve version')!.trigger('click');
    await flushPromises();
    await dialog.findAll('button').find((button) => button.text() === 'Activate validated version')!.trigger('click');
    await flushPromises();
    expect(approve).toHaveBeenCalledWith('template-1');
    expect(activate).toHaveBeenCalledWith('template-1');
    wrapper.unmount();
  });

  it('keeps matching, compose and release unavailable offline without hiding the manual path', async () => {
    stubAuthoring();
    const wrapper = mountWorkspace();
    await flushPromises();

    expect(wrapper.text()).toContain('This scope has no assessment yet');
    await openTool(wrapper, 'Context', 'Matching and calibration');
    const matchingDialog = wrapper.get('dialog');
    expect(wrapper.text()).toContain('Without a matching model calibration, the system cannot rank resources.');
    expect(matchingDialog.findAll('button').find((button) => button.text() === 'Run matching')?.attributes('disabled')).toBeDefined();
    expect(matchingDialog.findAll('button').find((button) => button.text() === 'Choose manually')?.attributes('disabled')).toBeUndefined();

    await openTool(wrapper, 'Review', 'Generate a proposal');
    expect(wrapper.get('dialog').findAll('button').find((button) => button.text() === 'Generate')?.attributes('disabled')).toBeDefined();

    await openTool(wrapper, 'Review', 'Copy or download approved text');
    expect(wrapper.text()).toContain('Save the current material and approve that exact version in Gate checks to enable release.');
    expect(wrapper.findAll('button').find((button) => button.text() === 'Copy approved text')?.attributes('disabled')).toBeDefined();
    wrapper.unmount();
  });

  it('saves the working draft with template fills and owner-reviewed sentence bindings', async () => {
    stubAuthoring({ templates: [activeTemplate], embedding: { model: 'local', model_digest: 'digest-1', calibrated_kinds: ['gift'] } });
    vi.spyOn(parley, 'getWritingExchange').mockResolvedValue({ current: { version: 1, fairness_attested: true }, versions: [] });
    const createMaterial = vi.spyOn(parley, 'createMaterial').mockResolvedValue(material());
    const wrapper = mountWorkspace({ resources: [gift] });
    await flushPromises();

    await openTool(wrapper, 'Insert', 'Template and fills');
    const dialog = wrapper.get('dialog');
    await dialog.get('select').setValue('template-1');
    await flushPromises();
    const fillSelect = dialog.findAll('select').at(-1)!;
    await fillSelect.setValue('r1');
    await dialog.findAll('input').at(-1)!.setValue('a short walkthrough');
    await dialog.findAll('button').find((button) => button.text() === 'Apply selections')!.trigger('click');
    await flushPromises();

    const editor = wrapper.get('#ow-message');
    await editor.setValue('Hi Dana Lee,\nI can offer the first-week checklist.\nWould a short walkthrough be useful?');
    const save = wrapper.findAll('button').find((button) => button.text() === 'Save draft')!;
    expect(save.attributes('disabled')).toBeUndefined();
    await save.trigger('click');
    await flushPromises();
    expect(createMaterial).toHaveBeenCalledWith('opp-1', expect.objectContaining({
      recipient_id: 'person-1', kind: 'outreach', template_version_id: 'template-1', exchange_version: 1,
      slot_fills: [
        { slot: 'their_name:name', value: 'Dana Lee' },
        { slot: 'gift:guide', value: 'the first-week checklist', resource_id: 'r1' },
        { slot: 'ask:walkthrough', value: 'a short walkthrough' },
      ],
      sentence_bindings: [
        expect.objectContaining({ sentence_index: 0, claim_type: 'generic' }),
        expect.objectContaining({ sentence_index: 1, claim_type: 'offer', ref_kind: 'resource', ref_id: 'r1', ref_revision: 8 }),
        expect.objectContaining({ sentence_index: 2, claim_type: 'ask', ref_kind: 'ladder', ask_rung: 1 }),
      ],
    }));
    wrapper.unmount();
  });

  it('keeps a generated proposal scoped to its opportunity and refuses to apply it elsewhere', async () => {
    const secondOpportunity = { ...opportunity, id: 'opp-2', title: 'Second onboarding', participants: [{ id: 'person-2', kind: 'person' as const, name: 'Mina Patel' }] };
    stubAuthoring({ templates: [activeTemplate], embedding: { model: 'local', model_digest: 'digest-1', calibrated_kinds: ['gift'] } });
    vi.spyOn(parley, 'getWritingExchange').mockResolvedValue({ current: { version: 1, fairness_attested: true }, versions: [] });
    vi.spyOn(parley, 'composeWritingMaterial').mockResolvedValue({ job_id: 'job-b', role: 'composer' });
    vi.spyOn(parley, 'getRoleJob').mockResolvedValue({ status: 'succeeded', result: { body: 'generated proposal' } } as unknown as Awaited<ReturnType<typeof parley.getRoleJob>>);
    const wrapper = mountWorkspace({
      resources: [gift],
    }, [opportunity, secondOpportunity]);
    await flushPromises();

    await openTool(wrapper, 'Insert', 'Template and fills');
    let dialog = wrapper.get('dialog');
    await dialog.get('select').setValue('template-1');
    await flushPromises();
    await dialog.findAll('select').at(-1)!.setValue('r1');
    await dialog.findAll('input').at(-1)!.setValue('a short walkthrough');
    await dialog.findAll('button').find((button) => button.text() === 'Apply selections')!.trigger('click');
    await flushPromises();

    await openTool(wrapper, 'Review', 'Generate a proposal');
    await wrapper.get('dialog').findAll('button').find((button) => button.text() === 'Generate')!.trigger('click');
    await flushPromises();
    expect((wrapper.get('dialog').get('textarea').element as HTMLTextAreaElement).value).toContain('generated proposal');

    // The proposal belongs to opp-1: another scope may not apply it (F-FR-1).
    await wrapper.findAll('select')[0]!.setValue('opp-2');
    await flushPromises();
    await openTool(wrapper, 'Review', 'Generate a proposal');
    dialog = wrapper.get('dialog');
    const apply = dialog.findAll('button').find((button) => button.text() === 'Apply to draft')!;
    expect(apply.attributes('disabled')).toBeDefined();
    await apply.trigger('click');
    await flushPromises();
    expect((wrapper.get('#ow-message').element as HTMLTextAreaElement).value).toBe('');

    await wrapper.findAll('select')[0]!.setValue('opp-1');
    await flushPromises();
    await openTool(wrapper, 'Review', 'Generate a proposal');
    const applyCurrent = wrapper.get('dialog').findAll('button').find((button) => button.text() === 'Apply to draft')!;
    expect(applyCurrent.attributes('disabled')).toBeUndefined();
    await applyCurrent.trigger('click');
    await flushPromises();
    expect((wrapper.get('#ow-message').element as HTMLTextAreaElement).value).toBe('generated proposal');
    wrapper.unmount();
  });

  it('evaluates the deterministic gate and approves only after a sufficient override reason', async () => {
    stubAuthoring();
    vi.spyOn(parley, 'evaluateMaterialGate')
      .mockResolvedValueOnce({ checks: [soft, hard], copy_enabled: true })
      .mockResolvedValueOnce({ checks: [{ ...soft, overridden: true }, hard], copy_enabled: true });
    const approve = vi.spyOn(parley, 'approveMaterial').mockResolvedValue(material({ status: 'approved' }));
    const wrapper = mountWorkspace({ outputs: [material()] });
    await flushPromises();

    await wrapper.findAll('button').find((button) => button.text().startsWith('Drafts'))!.trigger('click');
    await wrapper.findAll('button').find((button) => button.text().includes('Outreach'))!.trigger('click');
    await flushPromises();
    await openTool(wrapper, 'Review', 'Run gate checks');
    await settle();
    expect(wrapper.text()).toContain('S1 GIVE_FIRST');
    expect(parley.evaluateMaterialGate).toHaveBeenCalledWith('m1', {});
    const approveButton = () => wrapper.findAll('button').find((button) => button.text() === 'Approve exact version');
    expect(approveButton()?.attributes('disabled')).toBeDefined();
    await wrapper.get('dialog').get('textarea').setValue('Reviewed evidence from the signed source.');
    expect(approveButton()?.attributes('disabled')).toBeDefined();
    await wrapper.findAll('button').find((button) => button.text() === 'Recalculate current gate report')!.trigger('click');
    await settle();
    expect(parley.evaluateMaterialGate).toHaveBeenCalledWith('m1', { 'S1 GIVE_FIRST': 'Reviewed evidence from the signed source.' });
    expect(approveButton()?.attributes('disabled')).toBeUndefined();
    await approveButton()!.trigger('click');
    await flushPromises();
    expect(approve).toHaveBeenCalledWith('m1', { 'S1 GIVE_FIRST': 'Reviewed evidence from the signed source.' });
    expect(wrapper.text()).toContain('Approved exact material v1');
    wrapper.unmount();
  });

  it('releases an exact approved version, copies it and acknowledges delivery', async () => {
    stubAuthoring();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(globalThis.navigator, 'clipboard', { configurable: true, value: { writeText } });
    const releaseMaterial = vi.spyOn(parley, 'releaseMaterial').mockResolvedValue(release);
    const ack = vi.spyOn(parley, 'acknowledgeRelease').mockResolvedValue();
    const wrapper = mountWorkspace({ outputs: [material({ status: 'approved', gate: { checks: [hard], copy_enabled: true } })] });
    await flushPromises();

    await wrapper.findAll('button').find((button) => button.text().startsWith('Drafts'))!.trigger('click');
    await wrapper.findAll('button').find((button) => button.text().includes('Outreach'))!.trigger('click');
    await flushPromises();
    await openTool(wrapper, 'Review', 'Copy or download approved text');
    await wrapper.findAll('button').find((button) => button.text() === 'Copy approved text')!.trigger('click');
    await settle();
    expect(releaseMaterial).toHaveBeenCalledWith('m1');
    expect(writeText).toHaveBeenCalledWith('Hello Dana.');
    expect(ack).toHaveBeenCalledWith('r1', true);
    expect(wrapper.text()).toContain('Mark sent remains a separate action');
    wrapper.unmount();
  });

  it('reconciles a pending acknowledgement and refuses a digest mismatch', async () => {
    stubAuthoring();
    const ack = vi.spyOn(parley, 'acknowledgeRelease')
      .mockRejectedValueOnce(new Error('ack unavailable'))
      .mockResolvedValue();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(globalThis.navigator, 'clipboard', { configurable: true, value: { writeText } });
    vi.spyOn(parley, 'releaseMaterial').mockResolvedValue(release);
    const wrapper = mountWorkspace({ outputs: [material({ status: 'approved', gate: { checks: [hard], copy_enabled: true } })] });
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text().startsWith('Drafts'))!.trigger('click');
    await wrapper.findAll('button').find((button) => button.text().includes('Outreach'))!.trigger('click');
    await flushPromises();
    await openTool(wrapper, 'Review', 'Copy or download approved text');
    await wrapper.findAll('button').find((button) => button.text() === 'Copy approved text')!.trigger('click');
    await settle();
    expect(wrapper.text()).toContain('acknowledgement is pending');
    await wrapper.findAll('button').find((button) => button.text() === 'Reconcile original release acknowledgement')!.trigger('click');
    await settle();
    expect(ack).toHaveBeenCalledTimes(2);
    wrapper.unmount();

    vi.spyOn(parley, 'releaseMaterial').mockResolvedValue({ ...release, body: 'Tampered text.' });
    const second = mountWorkspace({ outputs: [material({ status: 'approved', gate: { checks: [hard], copy_enabled: true } })] });
    await flushPromises();
    await second.findAll('button').find((button) => button.text().startsWith('Drafts'))!.trigger('click');
    await second.findAll('button').find((button) => button.text().includes('Outreach'))!.trigger('click');
    await flushPromises();
    await openTool(second, 'Review', 'Copy or download approved text');
    await second.findAll('button').find((button) => button.text() === 'Copy approved text')!.trigger('click');
    await settle();
    expect(second.get('[role="alert"]').text()).toContain('integrity check');
    second.unmount();
  });

  it('opens a saved version from a deep link and shows its history', async () => {
    stubAuthoring();
    const saved = material({ status: 'approved', body: 'Saved wording.' });
    const wrapper = mountWorkspace({ outputs: [saved], recordId: 'm1' });
    await flushPromises();
    expect(wrapper.get('dialog').text()).toContain('Version history');
    expect((wrapper.get('dialog').get('textarea').element as HTMLTextAreaElement).value).toBe('Saved wording.');
    expect((wrapper.get('#ow-message').element as HTMLTextAreaElement).value).toBe('Saved wording.');
    expect(wrapper.text()).toContain('Approved · v1');
    wrapper.unmount();
  });
});
