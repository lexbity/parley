import { mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushPromises } from '@vue/test-utils';
import { parley, type BoardOpportunity, type BoardSnapshot, type ExtractorJob, type GraphEntity, type Material, type OpportunitySummary, type ProviderConfig, type SavedSource } from '../api/client';
import { commands } from '../api/commands';
import AppShell from './AppShell.vue';
import { tabs } from './tabs';

afterEach(() => { vi.restoreAllMocks(); window.history.replaceState(null, '', '/'); });

const configuredProvider: ProviderConfig = {
  endpoint: 'http://127.0.0.1:11434', model: '', num_ctx: 8192, num_predict: 1024,
  locale: 'en', profile_revision: 3, pairing: null,
  structured: { status: 'unqualified', diagnostic: null, checked_at: null, probe_id: null },
  embed: { status: 'unqualified', diagnostic: null, checked_at: null, probe_id: null },
};

function stubWorkspace(parties: GraphEntity[] = [], sources: SavedSource[] = [], board: BoardSnapshot = {
  lists: [{ id: 'list-default', title: 'Opportunities', position: 0, archived: false, revision: 1 }], cards: [],
}): void {
  vi.spyOn(parley, 'connect').mockResolvedValue();
  vi.spyOn(commands, 'query').mockResolvedValue({ contract_major: 1, manifest_version: 'sha256:test', truncated: false, commands: [] });
  vi.spyOn(parley, 'getEvents').mockResolvedValue({ items: [], next_cursor: '0', observed_cursor: '0' });
  vi.spyOn(parley, 'queryDirectory').mockResolvedValue({ rows: parties, nextCursor: null, fence: '0' });
  vi.spyOn(parley, 'listResources').mockResolvedValue([]);
  vi.spyOn(parley, 'listSources').mockResolvedValue(sources);
  vi.spyOn(parley, 'listOpportunitiesPage').mockResolvedValue({ items: [], nextCursor: null });
  vi.spyOn(parley, 'getOpportunity').mockRejectedValue(new Error('no opportunity'));
  vi.spyOn(parley, 'getEntity').mockRejectedValue(new Error('no entity'));
  vi.spyOn(parley, 'listOutputs').mockResolvedValue([]);
  vi.spyOn(parley, 'listWritingTemplates').mockResolvedValue([]);
  vi.spyOn(parley, 'listTemplateFamilies').mockResolvedValue([]);
  vi.spyOn(parley, 'listVoiceSamples').mockResolvedValue([]);
  vi.spyOn(parley, 'getEmbeddingModel').mockResolvedValue({ model: '', model_digest: null, profile_revision: 3, calibrated_kinds: [], readiness: { status: 'unqualified', diagnostic: null, checked_at: null, probe_id: null } });
  vi.spyOn(parley, 'getWritingInterests').mockResolvedValue([]);
  vi.spyOn(parley, 'getWritingExchange').mockResolvedValue({ current: null, versions: [] });
  vi.spyOn(parley, 'getWritingMatch').mockRejectedValue(new Error('no completed match'));
  vi.spyOn(parley, 'listThreads').mockResolvedValue([]);
  vi.spyOn(parley, 'listCapacity').mockResolvedValue([]);
  vi.spyOn(parley, 'getBoard').mockResolvedValue(board);
  vi.spyOn(parley, 'getQueueSheet').mockResolvedValue({ generation: '1', total: 0, rows: [], phases: [], cursor: null, next_cursor: null });
  vi.spyOn(parley, 'setOpportunityPhase').mockResolvedValue({ row: { opportunity_id: '', title: '', lifecycle: 'open', close_reason: null, pack_id: 'job_search', pack_version: '1.0.0', phase_revision: 1, phase: null, participants: [], next_action: { kind: 'choose', label: 'Choose a next action', reason: '', date: null, thread_id: null, recipient_id: null, recipient_name: null }, other_action_count: 0 }, event_id: 'ev', phase_revision: 1, reverted: false });
  vi.spyOn(parley, 'getDailyQueue').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', pack_id: 'job_search', pack_version: '1.0.0', daily_cap: 12, actionable_count: 0, deferred_count: 0, items: [] });
  vi.spyOn(parley, 'getInsights').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', event_cursor: '0', effective_outcomes: [], rates: [], strata: [], comparisons: [] });
  vi.spyOn(parley, 'getOpportunityNotes').mockResolvedValue({ opportunity_id: '', markdown: '', revision: 1, next_task: null, tasks: [] });
  vi.spyOn(parley, 'getProvider').mockResolvedValue(configuredProvider);
  vi.spyOn(parley, 'getAdminOverview').mockResolvedValue({ as_of: '2026-11-21T12:00:00Z', workspace_epoch: 'epoch-test', event_cursor: '0', last_verified_backup: null, backups: [], parties: parties.map((party) => ({ id: party.id, kind: party.kind as 'person' | 'org', name: party.name, descriptor: null })) });
  vi.spyOn(parley, 'getQuestionBank').mockResolvedValue({ version_id: 'question-bank-v1', content: { questions: { proof: [], gift: [], artifact: [] } } });
  vi.spyOn(parley, 'listAgentChats').mockResolvedValue([]);
  vi.spyOn(parley, 'getGraph').mockResolvedValue({ nodes: [], edges: [], omitted_nodes: 0, omitted_edges: 0 });
  vi.spyOn(parley, 'getResourceBoard').mockResolvedValue({
    columns: [
      { id: 'inbox', title: 'Inbox', position: 1024, revision: 1, kind: 'system' },
      { id: 'developing', title: 'Developing', position: 2048, revision: 1, kind: 'owner' },
      { id: 'available', title: 'Available', position: 3072, revision: 1, kind: 'owner' },
    ],
    cards: [],
    off_board: [],
    next_cursor: null,
    event_cursor: 0,
  });
  vi.spyOn(parley, 'getEntityCard').mockRejectedValue(new Error('no selection'));
  vi.spyOn(parley, 'commitResourceCard').mockRejectedValue(new Error('not used'));
  vi.spyOn(parley, 'moveResourceCard').mockRejectedValue(new Error('not used'));
  vi.spyOn(parley, 'getDossier').mockRejectedValue(new Error('no selection'));
  vi.spyOn(parley, 'getEntityNotes').mockRejectedValue(new Error('no selection'));
  vi.spyOn(parley, 'listSegments').mockResolvedValue([]);
  vi.spyOn(parley, 'listFluency').mockResolvedValue([]);
  vi.spyOn(parley, 'getAgentPrompt').mockResolvedValue({ role: 'interviewer', version_id: 'interviewer-v1', version: 1, content: 'Default', sha256: 'a'.repeat(64) });
  vi.spyOn(parley, 'updateAgentPrompt').mockResolvedValue();
  vi.spyOn(parley, 'updateQuestionBank').mockResolvedValue();
}

function partiesSection(wrapper: ReturnType<typeof mount>) {
  return wrapper.get('section[aria-labelledby="parties-heading"]');
}

describe('AppShell', () => {
  it('renders the mandated destination set with the Activity surface', () => {
    expect([...tabs]).toEqual(['Resources', 'Parties', 'Queue', 'Output', 'Settings', 'Activity']);
    stubWorkspace();
    const wrapper = mount(AppShell);
    const labels = wrapper.findAll('nav[aria-label="Main navigation"] a').map((link) => link.text());
    expect(labels).toEqual(['Resources', 'Parties', 'Queue', 'Output', 'Settings', 'Activity']);
    expect(wrapper.get('h1').text()).toBe('Resources');
    wrapper.unmount();
  });

  it('protects the resource card draft when switching tabs', async () => {
    stubWorkspace();
    const wrapper = mount(AppShell);
    await flushPromises();
    const board = wrapper.get('section[aria-label="Resource board"]');
    await board.findAll('button').find(button => button.text() === 'More options')!.trigger('click');
    const editor = wrapper.get('form[aria-label="Resource card editor"]');
    const titleInput = editor.get('label:has(input) input');
    await titleInput.setValue('unsent resource title');
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(2)').trigger('click');
    expect(wrapper.get('h1').text()).toBe('Resources');
    const prompt = wrapper.get('[role="alertdialog"]');
    await prompt.findAll('button').find(button => button.text() === 'Stay')!.trigger('click');
    expect((wrapper.get('form[aria-label="Resource card editor"] input').element as HTMLInputElement).value)
      .toBe('unsent resource title');
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(2)').trigger('click');
    await wrapper.get('[role="alertdialog"]').findAll('button').find(button => button.text() === 'Discard unsaved changes')!.trigger('click');
    expect(wrapper.get('h1').text()).toBe('Parties');
    wrapper.unmount();
  });

  it('creates a party without an opportunity and saves source-only captures', async () => {
    stubWorkspace([], []);
    const createEntity = vi.spyOn(parley, 'createEntity')
      .mockResolvedValue({ id: 'p1', kind: 'person', name: 'Dana Lee', focus: '', revision: 1 });
    const saveSource = vi.spyOn(parley, 'saveSource').mockResolvedValue({
      content_id: 'c1', variant_id: 'v1', occurrence_id: 'o1', normalized_sha256: 'a', raw_sha256: 'b',
    });
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(2)').trigger('click');
    const parties = partiesSection(wrapper);
    await parties.findAll('button').find((button) => button.text() === 'Add party')!.trigger('click');
    await parties.get('form.add-party select').setValue('person');
    await parties.get('form.add-party input[required]').setValue('Dana Lee');
    await parties.get('form.add-party').trigger('submit');
    await flushPromises();
    expect(createEntity).toHaveBeenCalledWith({ kind: 'person', name: 'Dana Lee', focus: '' }, expect.any(String));
    expect(wrapper.text()).toContain('Party created. No opportunity, edge or outreach task was created');
    await parties.get('#leads-inbox textarea').setValue('Hiring update with an unresolved Dana mention.');
    await parties.get('#leads-inbox').trigger('submit');
    await flushPromises();
    expect(saveSource).toHaveBeenCalledWith({ text: 'Hiring update with an unresolved Dana mention.', title: null, url: null }, expect.any(String));
    expect(wrapper.text()).toContain('No person was inferred or linked');
    wrapper.unmount();
  });

  it('saves loopback provider settings and reports explicit model health', async () => {
    stubWorkspace();
    const update = vi.spyOn(parley, 'updateProvider').mockResolvedValue();
    const health = vi.spyOn(parley, 'checkProvider').mockResolvedValue({ available: true, model: 'local:latest', models: ['local:latest'], message: 'Local model is available' });
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(5)').trigger('click');
    await wrapper.get('section[aria-labelledby="settings-heading"] input[required][autocomplete="off"]').setValue('http://127.0.0.1:11434');
    await wrapper.get('section[aria-labelledby="settings-heading"]').findAll('input[required][autocomplete="off"]')[1]!.setValue('local:latest');
    const numericInputs = wrapper.findAll('input[type="number"]');
    await numericInputs[0]!.setValue('16384');
    await numericInputs[1]!.setValue('2048');
    await wrapper.get('input[maxlength="40"]').setValue('en');
    await wrapper.get('section[aria-labelledby="settings-heading"] form').trigger('submit');
    await flushPromises();
    expect(update).toHaveBeenCalledWith({ endpoint: 'http://127.0.0.1:11434', model: 'local:latest', num_ctx: 16384, num_predict: 2048, locale: 'en', expected_profile_revision: 3 });
    await wrapper.findAll('#settings-provider button').find((button) => button.text() === 'Check local model readiness')!.trigger('click');
    await flushPromises();
    expect(health).toHaveBeenCalledOnce();
    expect(wrapper.text()).toContain('Local model is available');
    wrapper.unmount();
  });

  it('reopens a durable capture review, requires a subject, and records proposal decisions', async () => {
    const party: GraphEntity = { id: 'p1', kind: 'person', name: 'Dana Lee', revision: 1 };
    const source: SavedSource = { content_id: 'c1', variant_id: 'v1', occurrence_id: 'o1', normalized_sha256: 'a', raw_sha256: 'b' };
    const opportunity: OpportunitySummary = {
      id: 'opp1', title: 'Onboarding', state: 'open', revision: 1, participants: [{ id: party.id, name: party.name, kind: 'person' }],
      tags: [], next_task: null, next_deadline: null, missing_material: true,
    };
    const job: ExtractorJob = {
      id: 'job1', role: 'extractor', status: 'succeeded', current_attempt: 1, max_attempts: 3,
      progress_current: 1, progress_total: 1, injection_suspected: true, last_error_code: null, last_error: null,
      source_occurrence_id: 'o1', proposal_batch_id: 'batch1', proposals: [
        { id: 'signal', kind: 'signal', status: 'pending', candidate: { statement: 'Dana will help onboarding.', quote: 'Dana will help onboarding.', mention_name: 'Dana Lee', mention_kind: 'person' } },
        { id: 'interest', kind: 'interest', status: 'pending', candidate: { statement: 'Onboarding is expanding.', quote: 'Onboarding is expanding.', mention_name: null, mention_kind: null } },
      ],
    };
    stubWorkspace([party], [source]);
    vi.spyOn(parley, 'listOpportunitiesPage').mockResolvedValue({ items: [opportunity], nextCursor: null });
    const openReview = vi.spyOn(parley, 'getCaptureProposals').mockResolvedValue(job);
    const resolve = vi.spyOn(parley, 'resolveProposals').mockResolvedValue({ status: 'partially_applied' });
    vi.spyOn(parley, 'getExtractorJob').mockResolvedValue(job);
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(2)').trigger('click');
    await flushPromises();
    await wrapper.findAll('button').find((element) => element.text() === 'Review proposals')!.trigger('click');
    await flushPromises();
    expect(openReview).toHaveBeenCalledWith('o1');
    expect(wrapper.text()).toContain('prompt-injection text');
    expect(wrapper.text()).toContain('T1 source evidence');
    await wrapper.get('section[aria-label="Capture review"] article button').trigger('click');
    expect(wrapper.text()).toContain('Choose the person or organization this claim is about');
    const selects = wrapper.get('section[aria-label="Capture review"]').findAll('select');
    await selects[0]!.setValue('p1');
    await selects[1]!.setValue('opp1');
    const proposalCards = wrapper.get('section[aria-label="Capture review"]').findAll('article');
    await proposalCards[0]!.findAll('button')[0]!.trigger('click');
    await flushPromises();
    expect(resolve).toHaveBeenCalledWith('batch1', [{ item_id: 'signal', action: 'confirm', target_entity_id: 'p1' }], 'opp1');
    await proposalCards[1]!.findAll('button')[2]!.trigger('click');
    await flushPromises();
    expect(resolve).toHaveBeenLastCalledWith('batch1', [{ item_id: 'interest', action: 'source_only' }], 'opp1');
    expect(wrapper.text()).toContain('Proposal source only');
    await proposalCards[0]!.findAll('button')[1]!.trigger('click');
    await flushPromises();
    expect(resolve).toHaveBeenLastCalledWith('batch1', [{ item_id: 'signal', action: 'unresolved' }], 'opp1');
    await proposalCards[1]!.findAll('button')[3]!.trigger('click');
    await flushPromises();
    expect(resolve).toHaveBeenLastCalledWith('batch1', [{ item_id: 'interest', action: 'dismiss' }], 'opp1');
    wrapper.unmount();
  });

  it('keeps capture durable when extraction cannot queue or fails, and handles review refresh errors', async () => {
    stubWorkspace();
    const saved = { content_id: 'c2', variant_id: 'v2', occurrence_id: 'o2', normalized_sha256: 'a', raw_sha256: 'b', job_status: 'not_queued' as const };
    const save = vi.spyOn(parley, 'saveSource').mockResolvedValueOnce(saved).mockRejectedValueOnce(new Error('capture denied'));
    const getReview = vi.spyOn(parley, 'getCaptureProposals').mockRejectedValue(new Error('review not ready'));
    const wrapper = mount(AppShell); await flushPromises();
    vi.spyOn(parley, 'listSources').mockResolvedValue([saved]);
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(2)').trigger('click');
    const captureForm = wrapper.get('form.capture');
    await captureForm.get('textarea').setValue('A source without queue capacity.');
    await captureForm.trigger('submit'); await flushPromises();
    expect(save).toHaveBeenCalledTimes(1);
    expect(wrapper.text()).toContain('Extractor is unavailable or at capacity');
    await wrapper.findAll('button').find((button) => button.text() === 'Review proposals')!.trigger('click'); await flushPromises();
    expect(getReview).toHaveBeenCalledWith('o2');
    expect(wrapper.get('[role="alert"]').text()).toContain('review not ready');
    await captureForm.get('textarea').setValue('Failed source post.');
    await captureForm.trigger('submit'); await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('capture denied');
    wrapper.unmount();
  });

  it('polls a durable job to completion and records cancellation of a queued job', async () => {
    vi.useFakeTimers();
    const queued: ExtractorJob = {
      id: 'job2', role: 'extractor', status: 'queued', current_attempt: 0, max_attempts: 3,
      progress_current: 0, progress_total: 1, injection_suspected: false, last_error_code: null, last_error: null,
      source_occurrence_id: 'o2', proposal_batch_id: null, proposals: [],
    };
    const completed: ExtractorJob = { ...queued, status: 'succeeded', current_attempt: 1, progress_current: 1, proposal_batch_id: 'batch2', proposals: [] };
    const cancelled: ExtractorJob = { ...queued, status: 'cancelled', last_error: 'Cancelled by owner' };
    const source: SavedSource = { content_id: 'c2', variant_id: 'v2', occurrence_id: 'o2', normalized_sha256: 'a', raw_sha256: 'b', job_id: 'job2' };
    stubWorkspace();
    vi.spyOn(parley, 'listSources').mockResolvedValue([source]);
    vi.spyOn(parley, 'saveSource').mockResolvedValue(source);
    // The initial read stages the full job; the session reconciler then polls
    // the typed role projection and performs one bounded terminal fetch.
    const getJob = vi.spyOn(parley, 'getExtractorJob')
      .mockResolvedValueOnce(queued)
      .mockResolvedValueOnce(completed)
      .mockResolvedValueOnce(cancelled);
    const roleJob = (status: string) => ({ id: 'job2', role: 'extractor', status, result: null, last_error: null, review_ref: null });
    vi.spyOn(parley, 'getRoleJob')
      .mockResolvedValueOnce(roleJob('queued'))
      .mockResolvedValueOnce(roleJob('succeeded'));
    vi.spyOn(parley, 'getCaptureProposals').mockResolvedValue(queued);
    const cancel = vi.spyOn(parley, 'cancelJob').mockResolvedValue();
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(2)').trigger('click');
    await wrapper.get('form.capture textarea').setValue('A durable local capture.');
    await wrapper.get('form.capture').trigger('submit');
    await flushPromises();
    expect(getJob).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(2000);
    await flushPromises();
    expect(getJob).toHaveBeenCalledTimes(2);
    expect(wrapper.text()).toContain('Job succeeded');
    await wrapper.findAll('button').find((button) => button.text() === 'Review proposals')!.trigger('click');
    await flushPromises();
    await wrapper.get('section[aria-label="Capture review"] button').trigger('click');
    await flushPromises();
    expect(cancel).toHaveBeenCalledWith('job2');
    expect(wrapper.text()).toContain('Extractor cancellation was recorded');
    expect(getJob).toHaveBeenCalledTimes(3);
    wrapper.unmount();
    vi.useRealTimers();
  });

  it('surfaces manual recovery paths when polling, review, cancellation, or decisions fail', async () => {
    vi.useFakeTimers();
    const queued: ExtractorJob = {
      id: 'job3', role: 'extractor', status: 'queued', current_attempt: 0, max_attempts: 3,
      progress_current: 0, progress_total: 1, injection_suspected: false, last_error_code: null, last_error: null,
      source_occurrence_id: 'o3', proposal_batch_id: 'batch3', proposals: [
        { id: 'proposal3', kind: 'signal', status: 'pending', candidate: { statement: 'A hiring signal.', quote: 'A hiring signal.', mention_name: null, mention_kind: null } },
      ],
    };
    const source: SavedSource = { content_id: 'c3', variant_id: 'v3', occurrence_id: 'o3', normalized_sha256: 'a', raw_sha256: 'b', job_id: 'job3' };
    stubWorkspace();
    vi.spyOn(parley, 'listSources').mockResolvedValue([source]);
    vi.spyOn(parley, 'saveSource').mockResolvedValue(source);
    vi.spyOn(parley, 'getExtractorJob').mockResolvedValueOnce(queued).mockRejectedValueOnce('poll unavailable');
    vi.spyOn(parley, 'getRoleJob').mockRejectedValue('poll unavailable');
    vi.spyOn(parley, 'getCaptureProposals').mockRejectedValueOnce('review unavailable').mockResolvedValue(queued);
    vi.spyOn(parley, 'cancelJob').mockRejectedValue('cancel unavailable');
    vi.spyOn(parley, 'resolveProposals').mockRejectedValue('decision unavailable');
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(2)').trigger('click');
    await wrapper.get('form.capture textarea').setValue('A source whose local job needs manual review.');
    await wrapper.get('form.capture').trigger('submit');
    await flushPromises();
    expect(wrapper.text()).toContain('Could not refresh Extractor review');
    const reviewButton = () => wrapper.findAll('button').find((button) => button.text() === 'Review proposals')!;
    await reviewButton().trigger('click'); await flushPromises();
    expect(wrapper.text()).toContain('There is no Extractor review for this source yet');
    await reviewButton().trigger('click'); await flushPromises();
    await wrapper.get('section[aria-label="Capture review"] button').trigger('click'); await flushPromises();
    expect(wrapper.text()).toContain('Could not cancel this Extractor job');
    await wrapper.get('section[aria-label="Capture review"] article button:nth-of-type(3)').trigger('click'); await flushPromises();
    expect(wrapper.text()).toContain('Could not save the review decision');
    wrapper.unmount();
    vi.useRealTimers();
  });

  it('submits pairing identity with the observed revision and follows a durable probe', async () => {
    stubWorkspace();
    const update = vi.spyOn(parley, 'updateProvider').mockResolvedValue();
    const poll = vi.mocked(parley.getProvider);
    const probe = { status: 'queued' as const, diagnostic: null, checked_at: null, probe_id: 'probe-1' };
    vi.spyOn(parley, 'checkProvider').mockResolvedValue({
      available: false, model: 'fixture', models: [], message: 'Checking', structured: probe,
    });
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(5)').trigger('click');
    await flushPromises();
    const form = wrapper.get('#settings-provider form');
    await form.get('fieldset input[type="checkbox"]').setValue(true);
    const fields = form.findAll('fieldset input[required]');
    await fields[0]!.setValue('0.23.1');
    await fields[1]!.setValue('a'.repeat(64));
    await fields[2]!.setValue('b'.repeat(64));
    await fields[3]!.setValue('32');
    await form.trigger('submit');
    await flushPromises();
    expect(update).toHaveBeenCalledWith(expect.objectContaining({
      expected_profile_revision: 3,
      pairing: { provider_version: '0.23.1', model_digest: 'a'.repeat(64), tokenizer_digest: 'b'.repeat(64), template_tokens: 32 },
    }));
    poll.mockResolvedValueOnce({
      ...configuredProvider,
      structured: { ...probe, status: 'probe_succeeded', checked_at: '2026-11-23T12:00:00Z' },
    });
    await form.findAll('button').find((button) => button.text() === 'Check local model readiness')!.trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('Structured-output check passed');
    expect(wrapper.text()).not.toContain('Structured output is ready for Coach.');
    wrapper.unmount();
  });

  it('reports provider load, save, and health failures in Settings', async () => {
    stubWorkspace();
    vi.spyOn(parley, 'getProvider').mockRejectedValueOnce(new Error('settings unavailable'));
    const update = vi.spyOn(parley, 'updateProvider').mockRejectedValue(new Error('save denied'));
    const health = vi.spyOn(parley, 'checkProvider')
      .mockResolvedValueOnce({ available: false, model: 'missing', models: [], message: 'Local model is unavailable' })
      .mockRejectedValueOnce('health exception');
    const wrapper = mount(AppShell); await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(5)').trigger('click');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('settings unavailable');
    await wrapper.findAll('#settings-provider button').find((button) => button.text() === 'Reload saved profile')!.trigger('click');
    await flushPromises();
    await wrapper.get('section[aria-labelledby="settings-heading"] form').trigger('submit'); await flushPromises();
    expect(update).toHaveBeenCalledOnce();
    expect(wrapper.get('[role="alert"]').text()).toContain('save denied');
    await wrapper.findAll('#settings-provider button').find((button) => button.text() === 'Check local model readiness')!.trigger('click'); await flushPromises();
    expect(health).toHaveBeenCalledOnce();
    expect(wrapper.get('[role="alert"]').text()).toContain('Local model is unavailable');
    await wrapper.findAll('#settings-provider button').find((button) => button.text() === 'Check local model readiness')!.trigger('click'); await flushPromises();
    expect(wrapper.text()).toContain('Could not check the local model');
    expect(health).toHaveBeenCalledTimes(2);
    wrapper.unmount();
  });

  it('creates a selected-participant opportunity and presents shared Output records', async () => {
    const created: BoardOpportunity = { id: 'o1', title: 'Onboarding exchange', state: 'open', revision: 1, participant_ids: ['p1'], source_variant_ids: [], material_ids: ['m1'], tags: [] };
    const queued: Material = { id: 'm1', opportunity_id: 'o1', recipient_id: 'p1', recipient_name: 'Dana Lee', kind: 'outreach', version: 1, body: 'Hello Dana.', content_sha256: 'a'.repeat(64), status: 'draft', gate: null, attachment: null };
    stubWorkspace([{ id: 'p1', kind: 'person', name: 'Dana Lee', revision: 1 }], [], {
      lists: [{ id: 'list-default', title: 'Opportunities', position: 0, archived: false, revision: 1 }],
      cards: [{ opportunity: created, opportunity_id: 'o1', list_id: 'list-default', position: 0, placement_revision: 1, next_task: null }],
    });
    vi.mocked(parley.getQueueSheet).mockResolvedValue({
      generation: '1', total: 1,
      rows: [{
        opportunity_id: 'o1', title: 'Onboarding exchange', lifecycle: 'open', close_reason: null, pack_id: 'job_search', pack_version: '1.0.0', phase_revision: 1, phase: null,
        participants: [{ id: 'p1', name: 'Dana Lee', kind: 'person', role: 'primary' }],
        next_action: { kind: 'choose', label: 'Choose a next action', reason: '', date: null, thread_id: null, recipient_id: null, recipient_name: null },
        other_action_count: 0,
      }],
      phases: [], cursor: null, next_cursor: null,
    });
    vi.spyOn(parley, 'listOutputs').mockResolvedValue([queued]);
    vi.spyOn(parley, 'listOpportunitiesPage').mockResolvedValue({ items: [{ id: 'o1', title: 'Onboarding exchange', state: 'open', revision: 1, participants: [{ id: 'p1', kind: 'person', name: 'Dana Lee' }], tags: [], next_task: null, next_deadline: null, missing_material: true }], nextCursor: null });
    vi.spyOn(parley, 'getOpportunity').mockResolvedValue({ id: 'o1', title: 'Onboarding exchange', state: 'open', revision: 1, participant_ids: ['p1'], source_variant_ids: [], material_ids: [] });
    vi.spyOn(parley, 'getEntity').mockResolvedValue({ id: 'p1', kind: 'person', name: 'Dana Lee', focus: '', revision: 1 });
    const createOpportunity = vi.spyOn(parley, 'createOpportunity').mockResolvedValue(created);
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(3)').trigger('click');
    const queueSection = wrapper.get('section[aria-labelledby="queue-heading"]');
    await queueSection.get('input[maxlength="500"]').setValue('Onboarding exchange');
    await queueSection.get('select[required]').setValue('p1');
    await queueSection.get('form').trigger('submit');
    await flushPromises();
    expect(createOpportunity).toHaveBeenCalledWith('Onboarding exchange', 'p1');
    expect(wrapper.text()).toContain('Opportunity created');
    await wrapper.get('button[data-open="o1"]').trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('Review outreach v1 in Output');
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(4)').trigger('click');
    await flushPromises();
    expect(wrapper.find('[aria-label="Output workspace"]').exists()).toBe(true);
    await wrapper.findAll('button').find((button) => button.text().startsWith('Drafts'))!.trigger('click');
    expect(wrapper.text()).toContain('Outreach');
    expect(wrapper.text()).toContain('v1');
    expect(wrapper.text()).toContain('Dana Lee');
    wrapper.unmount();
  });

  it('reports opportunity creation errors and copies a released approved version', async () => {
    const approved: Material = { id: 'm1', opportunity_id: 'o1', recipient_id: 'p1', recipient_name: 'Dana Lee', kind: 'outreach', version: 1, body: 'Hi Dana.', content_sha256: 'c42a55643d4c6762196d5271afcb9f8ed6beb3a828f1b201a321cfcb6b89aad2', status: 'approved', gate: null, attachment: null };
    stubWorkspace([{ id: 'p1', kind: 'person', name: 'Dana Lee', revision: 1 }]);
    vi.spyOn(parley, 'listOpportunitiesPage').mockResolvedValue({ items: [{ id: 'o1', title: 'Onboarding', state: 'open', revision: 1, participants: [{ id: 'p1', kind: 'person', name: 'Dana Lee' }], tags: [], next_task: null, next_deadline: null, missing_material: false }], nextCursor: null });
    vi.spyOn(parley, 'getOpportunity').mockResolvedValue({ id: 'o1', title: 'Onboarding', state: 'open', revision: 1, participant_ids: ['p1'], source_variant_ids: [], material_ids: [] });
    vi.spyOn(parley, 'getEntity').mockResolvedValue({ id: 'p1', kind: 'person', name: 'Dana Lee', focus: '', revision: 1 });
    vi.spyOn(parley, 'listOutputs').mockResolvedValue([approved]);
    vi.spyOn(parley, 'createOpportunity').mockRejectedValueOnce(new Error('opportunity denied'));
    const release = vi.spyOn(parley, 'releaseMaterial').mockResolvedValue({ release_id: 'r1', material_id: 'm1', version: 1, recipient_id: 'p1', body: 'Hi Dana.', content_sha256: approved.content_sha256, issued_at: '2026-11-21T12:00:00Z', attachment: null });
    const acknowledge = vi.spyOn(parley, 'acknowledgeRelease').mockResolvedValue();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(globalThis.navigator, 'clipboard', { configurable: true, value: { writeText } });
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(3)').trigger('click');
    const queueSection = wrapper.get('section[aria-labelledby="queue-heading"]');
    await queueSection.get('input[maxlength="500"]').setValue('Onboarding');
    await queueSection.get('select[required]').setValue('p1');
    await queueSection.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.text()).toContain('opportunity denied');
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(4)').trigger('click');
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text().startsWith('Drafts'))!.trigger('click');
    await wrapper.findAll('button').find((button) => button.text().includes('Outreach'))!.trigger('click');
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Review')!.trigger('click');
    await wrapper.findAll('button').find((button) => button.text().includes('Copy or download approved text'))!.trigger('click');
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Copy approved text')!.trigger('click');
    expect(release).toHaveBeenCalledWith('m1');
    await vi.waitFor(() => expect(writeText).toHaveBeenCalledWith('Hi Dana.'));
    expect(acknowledge).toHaveBeenCalledWith('r1', true);
    expect(wrapper.text()).toContain('Mark sent remains a separate action');
    wrapper.unmount();
  });

  it('records failed clipboard handoff after issuance and keeps unapproved copy disabled', async () => {
    const approved: Material = { id: 'm2', opportunity_id: 'o1', recipient_id: 'p1', recipient_name: 'Dana Lee', kind: 'outreach', version: 2, body: 'Hi Dana.', content_sha256: 'c42a55643d4c6762196d5271afcb9f8ed6beb3a828f1b201a321cfcb6b89aad2', status: 'approved', gate: null, attachment: null };
    const draft: Material = { ...approved, id: 'm3', version: 1, status: 'draft', gate: null, attachment: null };
    stubWorkspace([{ id: 'p1', kind: 'person', name: 'Dana Lee', revision: 1 }]);
    vi.spyOn(parley, 'listOpportunitiesPage').mockResolvedValue({ items: [{ id: 'o1', title: 'Onboarding', state: 'open', revision: 1, participants: [{ id: 'p1', kind: 'person', name: 'Dana Lee' }], tags: [], next_task: null, next_deadline: null, missing_material: false }], nextCursor: null });
    vi.spyOn(parley, 'getOpportunity').mockResolvedValue({ id: 'o1', title: 'Onboarding', state: 'open', revision: 1, participant_ids: ['p1'], source_variant_ids: [], material_ids: [] });
    vi.spyOn(parley, 'getEntity').mockResolvedValue({ id: 'p1', kind: 'person', name: 'Dana Lee', focus: '', revision: 1 });
    vi.spyOn(parley, 'listOutputs').mockResolvedValue([approved, draft]);
    vi.spyOn(parley, 'evaluateMaterialGate').mockResolvedValue({ checks: [], copy_enabled: true });
    vi.spyOn(parley, 'releaseMaterial').mockResolvedValue({ release_id: 'r2', material_id: 'm2', version: 2, recipient_id: 'p1', body: 'Hi Dana.', content_sha256: approved.content_sha256, issued_at: '2026-11-21T12:00:00Z', attachment: null });
    const acknowledge = vi.spyOn(parley, 'acknowledgeRelease').mockResolvedValue();
    Object.defineProperty(globalThis.navigator, 'clipboard', { configurable: true, value: { writeText: vi.fn().mockRejectedValue(new Error('clipboard unavailable')) } });
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(4)').trigger('click');
    await flushPromises();
    expect(parley.evaluateMaterialGate).not.toHaveBeenCalled();
    // The draft version opens through the library, and its gate review runs
    // the deterministic report for the exact saved version.
    await wrapper.findAll('button').find((button) => button.text().startsWith('Drafts'))!.trigger('click');
    await wrapper.findAll('button').filter((button) => button.text().includes('Outreach') && button.text().includes('v1'))[0]!.trigger('click');
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Review')!.trigger('click');
    await wrapper.findAll('button').find((button) => button.text().includes('Run gate checks'))!.trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('Approve exact version');
    expect(parley.evaluateMaterialGate).toHaveBeenCalledWith('m3', {});
    // The approved version releases through the same editor toolbar.
    await wrapper.findAll('button').find((button) => button.text().startsWith('Drafts'))!.trigger('click');
    await wrapper.findAll('button').filter((button) => button.text().includes('Outreach') && button.text().includes('v2'))[0]!.trigger('click');
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Review')!.trigger('click');
    await wrapper.findAll('button').find((button) => button.text().includes('Copy or download approved text'))!.trigger('click');
    await flushPromises();
    const copyButton = wrapper.findAll('button').find((button) => button.text() === 'Copy approved text')!;
    expect(copyButton.attributes('disabled')).toBeUndefined();
    await copyButton.trigger('click');
    // The failure ack runs after the rejected clipboard promise settles, one
    // microtask cycle behind the other work flushed above; wait for it.
    await vi.waitFor(() => expect(acknowledge).toHaveBeenCalledWith('r2', false));
    expect(wrapper.text()).toContain('clipboard unavailable');
    wrapper.unmount();
  });

  it('skips large collection refetches on tab switches when no events occurred', async () => {
    stubWorkspace();
    const listSources = vi.spyOn(parley, 'listSources');
    const listOutputs = vi.spyOn(parley, 'listOutputs');
    const getEvents = vi.spyOn(parley, 'getEvents');
    const wrapper = mount(AppShell);
    await flushPromises();

    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(2)').trigger('click');
    await flushPromises();
    expect(listSources).toHaveBeenCalledTimes(1);

    // Returning to a tab with no new events must not re-read its collections.
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(3)').trigger('click');
    await flushPromises();
    expect(listOutputs).toHaveBeenCalledTimes(1);
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(2)').trigger('click');
    await flushPromises();
    expect(listSources).toHaveBeenCalledTimes(1);
    expect(getEvents).toHaveBeenCalled();

    // A new event invalidates the cached tab on the next activation.
    getEvents.mockResolvedValueOnce({
      items: [{ cursor: '9', event_id: 'e9', entity_type: 'entity', entity_id: 'p1', revision: 2, event_type: 'entity.created', occurred_at: '2026-11-24T00:00:00Z', payload: {} }],
      next_cursor: '9', observed_cursor: '9',
    });
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(3)').trigger('click');
    await flushPromises();
    expect(listOutputs).toHaveBeenCalledTimes(2);
    wrapper.unmount();
  });

  it('reports a failed workspace connection instead of fetching directory data', async () => {
    const connect = vi.spyOn(parley, 'connect').mockRejectedValue(new Error('session refused'));
    const queryDirectory = vi.spyOn(parley, 'queryDirectory').mockResolvedValue({ rows: [], nextCursor: null, fence: '0' });
    vi.spyOn(parley, 'listResources').mockResolvedValue([]);
    vi.spyOn(parley, 'listSources').mockResolvedValue([]);
    vi.spyOn(parley, 'listOutputs').mockResolvedValue([]);
    const wrapper = mount(AppShell);
    await flushPromises();
    expect(connect).toHaveBeenCalled();
    expect(wrapper.text()).toContain('session refused');
    expect(queryDirectory).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('opens the palette with mod+k and closes it with Escape', async () => {
    stubWorkspace();
    const wrapper = mount(AppShell, { attachTo: globalThis.document.body });
    await flushPromises();
    globalThis.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', ctrlKey: true, bubbles: true }));
    await flushPromises();
    const dialog = wrapper.get('dialog.command-palette');
    expect(dialog.attributes('open')).toBeDefined();
    expect(dialog.get('[role="combobox"]').attributes('aria-expanded')).toBe('true');
    await dialog.get('[role="combobox"]').trigger('keydown', { key: 'Escape' });
    await flushPromises();
    expect(wrapper.get('dialog.command-palette').attributes('open')).toBeUndefined();
    wrapper.unmount();
  });

  it('leaves mod+k to a focused editable field and to an open dialog', async () => {
    stubWorkspace();
    const wrapper = mount(AppShell, { attachTo: globalThis.document.body });
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(3)').trigger('click');
    await flushPromises();
    const search = wrapper.get('input[aria-label="Find scenario"]');
    (search.element as HTMLInputElement).focus();
    search.element.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', ctrlKey: true, bubbles: true }));
    await flushPromises();
    expect(wrapper.get('dialog.command-palette').attributes('open')).toBeUndefined();

    const blocking = globalThis.document.createElement('div');
    blocking.setAttribute('role', 'alertdialog');
    blocking.textContent = 'Another dialog owns the chord';
    globalThis.document.body.appendChild(blocking);
    (search.element as HTMLInputElement).blur();
    globalThis.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', ctrlKey: true, bubbles: true }));
    await flushPromises();
    expect(wrapper.get('dialog.command-palette').attributes('open')).toBeUndefined();
    blocking.remove();
    wrapper.unmount();
  });

  it('restores focus to the workspace heading when the palette trigger is removed', async () => {
    stubWorkspace();
    const wrapper = mount(AppShell, { attachTo: globalThis.document.body });
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(3)').trigger('click');
    await flushPromises();
    const trigger = wrapper.get('button[aria-expanded="false"]');
    (trigger.element as HTMLButtonElement).focus();
    globalThis.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', ctrlKey: true, bubbles: true }));
    await flushPromises();
    expect(wrapper.get('dialog.command-palette').attributes('open')).toBeDefined();
    trigger.element.remove();
    await wrapper.get('dialog.command-palette [role="combobox"]').trigger('keydown', { key: 'Escape' });
    await flushPromises();
    expect((globalThis.document.activeElement as HTMLElement | null)?.tagName).toBe('H1');
    expect(globalThis.document.activeElement?.textContent).toContain('Queue');
    wrapper.unmount();
  });

  it('reports directory and capture failures without losing the other destinations', async () => {
    stubWorkspace([], []);
    vi.spyOn(parley, 'createEntity').mockRejectedValueOnce(new Error('write denied'));
    vi.spyOn(parley, 'saveSource').mockRejectedValueOnce(new Error('capture denied'));
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] a:nth-child(2)').trigger('click');
    const parties = partiesSection(wrapper);
    await parties.findAll('button').find((button) => button.text() === 'Add party')!.trigger('click');
    await parties.get('form.add-party input[required]').setValue('Noah');
    await parties.get('form.add-party').trigger('submit');
    await flushPromises();
    expect(wrapper.text()).toContain('write denied');
    await parties.get('#leads-inbox textarea').setValue('A source');
    await parties.get('#leads-inbox').trigger('submit');
    await flushPromises();
    expect(wrapper.text()).toContain('capture denied');
    const labels = wrapper.findAll('nav[aria-label="Main navigation"] a').map((link) => link.text());
    expect(labels).toEqual(['Resources', 'Parties', 'Queue', 'Output', 'Settings', 'Activity']);
    wrapper.unmount();
  });
});
