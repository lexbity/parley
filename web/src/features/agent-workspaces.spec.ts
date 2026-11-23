import { mount } from '@vue/test-utils';
import { flushPromises } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type AgentChat, type BriefItem, type GraphEntity } from '../api/client';
import InterviewerWorkspace from './resources/InterviewerWorkspace.vue';
import BriefWorkspace from './leads/BriefWorkspace.vue';
import AgentPolicySettings from './settings/AgentPolicySettings.vue';

afterEach(() => vi.restoreAllMocks());

const candidate = { id: 'candidate-1', asset_kind: 'proof', fields: { what_i_did: 'Improved onboarding' }, owner_quotes: { what_i_did: { quote: 'I improved onboarding.' } }, missing_fields: ['result_value', 'period'], state: 'proposed', resource_id: null };
const chat: AgentChat = { id: 'chat-1', role: 'interviewer', scope_kind: 'owner', scope_id: null, asset_kind: 'proof', revision: 2, active_job_id: null, turns: [], questions: [], candidates: [candidate] };
const party: GraphEntity = { id: 'party-1', kind: 'person', name: 'Dana Lee', revision: 1 };

describe('InterviewerWorkspace', () => {
  it('opens an owner chat, exposes the fixed bank, and confirms an editable candidate explicitly', async () => {
    vi.spyOn(parley, 'getQuestionBank').mockResolvedValue({ version_id: 'bank-v1', content: { questions: { proof: [{ id: 'result', text: 'What changed?' }], gift: [], artifact: [] } } });
    vi.spyOn(parley, 'listAgentChats').mockResolvedValue([]);
    vi.spyOn(parley, 'createAgentChat').mockResolvedValue(chat);
    vi.spyOn(parley, 'getAgentChat').mockResolvedValueOnce(chat).mockResolvedValueOnce({ ...chat, candidates: [{ ...candidate, state: 'saved', resource_id: 'resource-1' }] });
    const question = vi.spyOn(parley, 'setInterviewerQuestion').mockResolvedValue();
    const confirm = vi.spyOn(parley, 'confirmInterviewerCandidate').mockResolvedValue();
    const wrapper = mount(InterviewerWorkspace);
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Start Interviewer')!.trigger('click');
    await flushPromises();
    expect(parley.createAgentChat).toHaveBeenCalled();
    expect(wrapper.text()).toContain('What changed?');
    await wrapper.findAll('button').find((button) => button.text() === 'Ask')!.trigger('click');
    expect(question).toHaveBeenCalledWith('chat-1', 'result', 'ask');
    await wrapper.get('input[maxlength="240"]').setValue('Onboarding proof');
    const confirmButton = wrapper.findAll('button').find((button) => button.text() === 'Confirm and save');
    await confirmButton!.trigger('click');
    expect(confirm).toHaveBeenCalledWith('candidate-1', { title: 'Onboarding proof', fields: candidate.fields, expected_chat_revision: 2 }, expect.any(String));
    wrapper.unmount();
  });

  it('keeps owner edits when a background refresh returns fresh candidate defaults', async () => {
    vi.spyOn(parley, 'getQuestionBank').mockResolvedValue({ version_id: 'bank-v1', content: { questions: { proof: [{ id: 'result', text: 'What changed?' }], gift: [], artifact: [] } } });
    vi.spyOn(parley, 'listAgentChats').mockResolvedValue([]);
    vi.spyOn(parley, 'createAgentChat').mockResolvedValue(chat);
    const refreshed = { ...chat, candidates: [{ ...candidate, fields: { what_i_did: 'Refreshed default' } }] };
    vi.spyOn(parley, 'getAgentChat').mockResolvedValueOnce(chat).mockResolvedValueOnce(refreshed);
    vi.spyOn(parley, 'setInterviewerQuestion').mockResolvedValue();
    const wrapper = mount(InterviewerWorkspace);
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Start Interviewer')!.trigger('click');
    await flushPromises();
    await wrapper.get('input[maxlength="240"]').setValue('Owner proof title');
    await wrapper.findAll('button').find((button) => button.text() === 'Ask')!.trigger('click');
    await flushPromises();
    expect((wrapper.get('input[maxlength="240"]').element as HTMLInputElement).value).toBe('Owner proof title');
    wrapper.unmount();
  });

  it('does not attach a completing job of a previous chat to the current chat', async () => {
    vi.useFakeTimers();
    try {
      const secondChat: AgentChat = { id: 'chat-2', role: 'interviewer', scope_kind: 'owner', scope_id: null, asset_kind: 'proof', revision: 1, active_job_id: null, turns: [], questions: [], candidates: [] };
      vi.spyOn(parley, 'getQuestionBank').mockResolvedValue({ version_id: 'bank-v1', content: { questions: { proof: [{ id: 'result', text: 'What changed?' }], gift: [], artifact: [] } } });
      vi.spyOn(parley, 'listAgentChats').mockResolvedValue([
        { id: 'chat-1', role: 'interviewer', scope_kind: 'owner', scope_id: null, asset_kind: 'proof', revision: 2, last_turn: 'First answer', updated_at: '2026-11-22T10:00:00Z', active_job_id: null },
        { id: 'chat-2', role: 'interviewer', scope_kind: 'owner', scope_id: null, asset_kind: 'proof', revision: 1, last_turn: null, updated_at: '2026-11-22T10:00:00Z', active_job_id: null },
      ]);
      vi.spyOn(parley, 'createAgentChat').mockResolvedValue(chat);
      vi.spyOn(parley, 'getAgentChat').mockImplementation(async (id: string) => (id === 'chat-2' ? secondChat : chat));
      vi.spyOn(parley, 'addAgentTurn').mockResolvedValue({ chat_id: 'chat-1', turn_id: 'turn-1', job_id: 'job-1', revision: 1 });
      vi.spyOn(parley, 'getRoleJob').mockResolvedValue({ id: 'job-1', role: 'interviewer', status: 'failed', result: null, last_error: 'simulated failure' });
      const wrapper = mount(InterviewerWorkspace);
      await vi.advanceTimersByTimeAsync(0);
      await wrapper.findAll('button').find((button) => button.text() === 'Start Interviewer')!.trigger('click');
      await flushPromises();
      await wrapper.get('textarea').setValue('First answer');
      await wrapper.get('form').trigger('submit');
      await flushPromises();
      // Switch to the second chat while the first chat's job is still pending.
      await wrapper.findAll('select')[1]!.setValue('chat-2');
      await wrapper.findAll('button').find((button) => button.text() === 'Open saved chat')!.trigger('click');
      await flushPromises();
      await vi.advanceTimersByTimeAsync(1000);
      await flushPromises();
      expect(wrapper.text()).not.toContain('simulated failure');
      wrapper.unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it('clears the answer buffer when switching to another saved chat', async () => {
    const secondChat: AgentChat = { id: 'chat-2', role: 'interviewer', scope_kind: 'owner', scope_id: null, asset_kind: 'proof', revision: 1, active_job_id: null, turns: [], questions: [], candidates: [] };
    vi.spyOn(parley, 'getQuestionBank').mockResolvedValue({ version_id: 'bank-v1', content: { questions: { proof: [{ id: 'result', text: 'What changed?' }], gift: [], artifact: [] } } });
    vi.spyOn(parley, 'listAgentChats').mockResolvedValue([
      { id: 'chat-1', role: 'interviewer', scope_kind: 'owner', scope_id: null, asset_kind: 'proof', revision: 2, last_turn: 'First answer', updated_at: '2026-11-22T10:00:00Z', active_job_id: null },
      { id: 'chat-2', role: 'interviewer', scope_kind: 'owner', scope_id: null, asset_kind: 'proof', revision: 1, last_turn: null, updated_at: '2026-11-22T10:00:00Z', active_job_id: null },
    ]);
    vi.spyOn(parley, 'createAgentChat').mockResolvedValue(chat);
    vi.spyOn(parley, 'getAgentChat').mockImplementation(async (id: string) => (id === 'chat-2' ? secondChat : chat));
    const wrapper = mount(InterviewerWorkspace);
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Start Interviewer')!.trigger('click');
    await flushPromises();
    await wrapper.get('textarea').setValue('Draft answer text');
    await wrapper.findAll('select')[1]!.setValue('chat-2');
    await wrapper.findAll('button').find((button) => button.text() === 'Open saved chat')!.trigger('click');
    await flushPromises();
    expect((wrapper.get('textarea').element as HTMLTextAreaElement).value).toBe('');
    wrapper.unmount();
  });
});

describe('BriefWorkspace', () => {
  it('loads a scoped party brief and records a tier-2 correction as an owner decision', async () => {
    const item: BriefItem = { id: 'brief-1', scope_kind: 'party', scope_id: party.id, kind: 'jargon', text: 'Example term', detail: { plain_meaning: 'Example' }, tier: 2, quote: null, check_suggestion: 'Check recent posts', speaker_kind: 'model_prior', status: 'proposed', revision: 1, supersedes_id: null };
    vi.spyOn(parley, 'getBriefItems').mockResolvedValue([item]);
    vi.spyOn(parley, 'listSegments').mockResolvedValue([]);
    vi.spyOn(parley, 'listFluency').mockResolvedValue([{ term: item.text, status: 'unknown', revision: 1 }]);
    vi.spyOn(parley, 'listAgentChats').mockResolvedValue([]);
    const decide = vi.spyOn(parley, 'decideBriefItem').mockResolvedValue();
    const updateFluency = vi.spyOn(parley, 'updateFluency').mockResolvedValue({ term: item.text, status: 'fluent', revision: 2 });
    const wrapper = mount(BriefWorkspace, { props: { parties: [party] } });
    await wrapper.get('label:nth-of-type(2) select').setValue(party.id);
    await vi.waitFor(() => expect(wrapper.text()).toContain('Tendency, not rule'));
    await wrapper.get('select[aria-label="Your vocabulary fluency"]').setValue('fluent');
    expect(updateFluency).toHaveBeenCalledWith(item.text, 'fluent', 1);
    await wrapper.get('input').setValue('Correct description');
    await wrapper.findAll('button').find((button) => button.text() === 'Correct')!.trigger('click');
    expect(decide).toHaveBeenCalledWith('brief-1', { decision: 'correct', expected_revision: 1, corrected_text: 'Correct description' });
    wrapper.unmount();
  });
});

describe('AgentPolicySettings', () => {
  it('versions prompt and bank changes using their current version IDs', async () => {
    vi.spyOn(parley, 'getAgentPrompt').mockResolvedValue({ role: 'interviewer', version_id: 'prompt-v1', version: 1, content: 'Default', sha256: 'a'.repeat(64) });
    vi.spyOn(parley, 'getQuestionBank').mockResolvedValue({ version_id: 'bank-v1', content: { questions: { proof: [], gift: [], artifact: [] } } });
    const updatePrompt = vi.spyOn(parley, 'updateAgentPrompt').mockResolvedValue();
    const updateBank = vi.spyOn(parley, 'updateQuestionBank').mockResolvedValue();
    const wrapper = mount(AgentPolicySettings);
    await vi.waitFor(() => expect(wrapper.text()).toContain('prompt-v1'));
    await wrapper.findAll('textarea')[0]!.setValue('Owner version');
    await wrapper.findAll('button')[0]!.trigger('click');
    await wrapper.findAll('textarea')[1]!.setValue('{"questions":{"proof":[],"gift":[],"artifact":[]}}');
    await wrapper.findAll('button')[1]!.trigger('click');
    expect(updatePrompt).toHaveBeenCalledWith('interviewer', 'prompt-v1', 'Owner version');
    expect(updateBank).toHaveBeenCalledWith('bank-v1', { questions: { proof: [], gift: [], artifact: [] } });
    wrapper.unmount();
  });
});
