import { flushPromises, mount } from '@vue/test-utils';
import { describe, expect, it, vi } from 'vitest';
import CaptureReview, { type CaptureReviewBatch, type CaptureReviewService } from './CaptureReview.vue';

const source = {
  occurrence_id: 'occurrence-shared',
  content_id: 'content-shared',
  title: 'Northline hiring update',
  url: 'https://northline.example/hiring',
  text: 'The next engineer will work with Dana on onboarding.',
  raw_sha256: 'd'.repeat(64),
  captured_at: '2026-11-21T12:00:00Z',
};

const proposals: CaptureReviewBatch['proposals'] = [
  {
    id: 'proposal-hiring', action: 'create', target_kind: 'claim', target_id: null,
    target_label: 'Hiring signal', candidate_targets: [], statement: 'Northline is hiring an engineer.',
    quote: 'The next engineer', quote_start: 0, quote_end: 17, tier: 1, ambiguity: null,
    unresolved: false, candidate: { statement: 'Northline is hiring an engineer.' }, target_revision: null, review_state: 'proposed',
  },
  {
    id: 'proposal-dana', action: 'link', target_kind: 'person', target_id: null,
    target_label: 'Dana Lee (unresolved mention)', candidate_targets: [], statement: 'A Dana mention appears in the source.',
    quote: 'Dana', quote_start: 36, quote_end: 40, tier: 1,
    ambiguity: 'The text does not identify which Dana.', unresolved: true, candidate: null, target_revision: null, review_state: 'proposed',
  },
];

function batch(overrides: Partial<CaptureReviewBatch> = {}): CaptureReviewBatch {
  return { batch_id: 'batch-1', revision: 4, status: 'ok', injection_suspected:false, source, proposals, job_id: 'job-1', ...overrides };
}

function service(overrides: Partial<CaptureReviewService> = {}): CaptureReviewService {
  return {
    stageCapture: vi.fn().mockResolvedValue(batch()),
    getCaptureReview: vi.fn().mockResolvedValue(batch()),
    cancelCaptureJob: vi.fn().mockResolvedValue(batch({ status: 'cancelled' })),
    applyCaptureReview: vi.fn().mockResolvedValue({ batch_id: 'batch-1', revision: 5, status: 'ok', source, committed: [{ proposal_id: 'proposal-hiring', review_state: 'confirmed' }] }),
    saveCaptureSourceOnly: vi.fn().mockResolvedValue({ batch_id: 'batch-1', revision: 5, status: 'source_only', source, committed: proposals.map((proposal) => ({ proposal_id: proposal.id, review_state: 'source_only' as const })) }),
    ...overrides,
  };
}

const opportunities = [{ id: 'opportunity-onboarding', title: 'Onboarding' }, { id: 'opportunity-hiring', title: 'Hiring' }];

describe('CaptureReview', () => {
  it('shows a visible warning when deterministic capture screening suspects instructions', async () => {
    const api=service({stageCapture:vi.fn().mockResolvedValue(batch({injection_suspected:true}))});
    const wrapper = mount(CaptureReview, { props: { service:api, opportunities } });
    await wrapper.get('textarea').setValue(source.text);
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('resembling instructions');
    wrapper.unmount();
  });
  it('stages a source for two opportunities and applies only the owner-selected proposal', async () => {
    const api = service();
    const wrapper = mount(CaptureReview, { props: { service: api, opportunities } });
    await wrapper.get('textarea').setValue(source.text);
    await wrapper.findAll('input[type="checkbox"]')[0]!.setValue(true);
    await wrapper.findAll('input[type="checkbox"]')[1]!.setValue(true);
    await wrapper.get('form').trigger('submit');
    await flushPromises();

    expect(api.stageCapture).toHaveBeenCalledWith({
      text: source.text, title: null, url: null, opportunity_ids: ['opportunity-onboarding', 'opportunity-hiring'],
    });
    expect(wrapper.get('[aria-label="Staged source review"]').text()).toContain(source.text);
    expect(wrapper.get('[data-proposal-id="proposal-dana"]').text()).toContain('identity unresolved');
    expect(wrapper.get('[data-proposal-id="proposal-dana"]').text()).toContain('The text does not identify which Dana.');

    await wrapper.get('[data-proposal-id="proposal-hiring"] select').setValue('confirm');
    await wrapper.findAll('button').find((button) => button.text() === 'Apply selected decisions')!.trigger('click');
    await flushPromises();
    expect(api.applyCaptureReview).toHaveBeenCalledWith('batch-1', 4, [
      { proposal_id: 'proposal-hiring', action: 'confirm', target_id: null, corrected_statement: null },
    ], expect.any(String));
    expect(wrapper.text()).toContain('Unselected proposals remain pending');
    expect(wrapper.get('[data-proposal-id="proposal-hiring"]').text()).toContain('Owner decision saved: confirmed');
    expect(wrapper.get('[data-proposal-id="proposal-dana"]').text()).toContain('identity unresolved');
    expect(wrapper.emitted('linked')).toEqual([['batch-1', source.occurrence_id]]);
    wrapper.unmount();
  });

  it('keeps choices after a stale-revision refusal and lets the owner correct one proposal', async () => {
    const api = service({ applyCaptureReview: vi.fn().mockRejectedValue(new Error('stale parent revision')) });
    const wrapper = mount(CaptureReview, { props: { service: api, opportunities } });
    await wrapper.get('textarea').setValue(source.text);
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    const card = wrapper.get('[data-proposal-id="proposal-hiring"]');
    await card.find('select').setValue('correct');
    await card.get('input').setValue('The owner corrected hiring wording.');
    await wrapper.findAll('button').find((button) => button.text() === 'Apply selected decisions')!.trigger('click');
    await flushPromises();
    expect(api.applyCaptureReview).toHaveBeenCalledWith('batch-1', 4, [
      { proposal_id: 'proposal-hiring', action: 'correct', target_id: null, corrected_statement: 'The owner corrected hiring wording.' },
    ], expect.any(String));
    expect(wrapper.get('[role="alert"]').text()).toBe('stale parent revision');
    expect((card.find('input').element as HTMLInputElement).value).toBe('The owner corrected hiring wording.');
    wrapper.unmount();
  });

  it('cancels without applying proposals and preserves the staged source', async () => {
    const api = service({
      stageCapture: vi.fn().mockResolvedValue(batch({ status: 'running', proposals: [] })),
      cancelCaptureJob: vi.fn().mockResolvedValue(batch({ status: 'cancelled', proposals: [] })),
    });
    const wrapper = mount(CaptureReview, { props: { service: api, opportunities } });
    await wrapper.get('textarea').setValue(source.text);
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Cancel extraction')!.trigger('click');
    await flushPromises();
    expect(api.cancelCaptureJob).toHaveBeenCalledWith('batch-1', 'job-1');
    expect(api.applyCaptureReview).not.toHaveBeenCalled();
    expect(wrapper.get('[aria-label="Staged source review"]').text()).toContain(source.text);
    expect(wrapper.text()).toContain('No proposal was applied');
    wrapper.unmount();
  });

  it('offers source-only save after provider failure without auto-applying anything', async () => {
    const failed = batch({ status: 'failed', proposals: [] });
    const api = service({ stageCapture: vi.fn().mockResolvedValue(failed) });
    const wrapper = mount(CaptureReview, { props: { service: api, opportunities } });
    await wrapper.get('textarea').setValue(source.text);
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.text()).toContain('Extraction failed');
    expect(wrapper.text()).toContain('save it alone');
    await wrapper.findAll('button').find((button) => button.text() === 'Save source only')!.trigger('click');
    await flushPromises();
    expect(api.saveCaptureSourceOnly).toHaveBeenCalledWith('batch-1', 4, expect.any(String));
    expect(api.applyCaptureReview).not.toHaveBeenCalled();
    expect(wrapper.text()).toContain('Source saved without applying any proposals.');
    expect(wrapper.emitted('linked')).toEqual([['batch-1', source.occurrence_id]]);
    wrapper.unmount();
  });

  it('retains paste text and never presents proposals when staging fails', async () => {
    const api = service({ stageCapture: vi.fn().mockRejectedValue(new Error('model unavailable')) });
    const wrapper = mount(CaptureReview, { props: { service: api, opportunities } });
    await wrapper.get('textarea').setValue(source.text);
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toBe('model unavailable');
    expect((wrapper.get('textarea').element as HTMLTextAreaElement).value).toBe(source.text);
    expect(wrapper.find('[aria-label="Proposed changes"]').exists()).toBe(false);
    expect(api.applyCaptureReview).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('hands an unresolved proposal to chat with its source context without applying it', async () => {
    const api = service();
    const wrapper = mount(CaptureReview, { props: { service: api, opportunities } });
    await wrapper.get('textarea').setValue(source.text);
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    await wrapper.get('[data-proposal-id="proposal-dana"] button').trigger('click');
    expect(wrapper.emitted('chat')).toEqual([[
      { batchId: 'batch-1', occurrenceId: source.occurrence_id, proposalId: 'proposal-dana', quote: 'Dana', targetLabel: 'Dana Lee (unresolved mention)' },
    ]]);
    expect(api.applyCaptureReview).not.toHaveBeenCalled();
    wrapper.unmount();
  });
});
