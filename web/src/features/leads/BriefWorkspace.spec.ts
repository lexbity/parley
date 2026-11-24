import { mount, flushPromises } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type BriefItem, type GraphEntity } from '../../api/client';
import BriefWorkspace from './BriefWorkspace.vue';

afterEach(() => vi.restoreAllMocks());

const dana: GraphEntity = { id: 'p1', kind: 'person', name: 'Dana Lee', revision: 1 };
const miles: GraphEntity = { id: 'p2', kind: 'person', name: 'Miles Kim', revision: 1 };

function item(scope: string): BriefItem {
  return {
    id: `item-${scope}`,
    scope_kind: 'party',
    scope_id: scope,
    kind: 'fact',
    text: `${scope} brief item`,
    detail: {},
    tier: 1,
    quote: null,
    check_suggestion: null,
    speaker_kind: 'owner',
    status: 'proposed',
    revision: 1,
    supersedes_id: null,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

describe('BriefWorkspace', () => {
  it('discards a delayed brief load that resolves after a newer scope selection', async () => {
    const first = deferred<BriefItem[]>();
    const second = deferred<BriefItem[]>();
    vi.spyOn(parley, 'getBriefItems')
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(() => second.promise);
    vi.spyOn(parley, 'listFluency').mockResolvedValue([]);
    vi.spyOn(parley, 'listAgentChats').mockResolvedValue([]);
    vi.spyOn(parley, 'listSegments').mockResolvedValue([]);

    const wrapper = mount(BriefWorkspace, { props: { parties: [dana, miles] } });
    await flushPromises();

    // Open party 1 then quickly switch to party 2; the party-1 response
    // resolves last and must be discarded (F-FR-1).
    await wrapper.findAll('select')[1]!.setValue('p1');
    await flushPromises();
    await wrapper.findAll('select')[1]!.setValue('p2');
    await flushPromises();

    second.resolve([item('p2')]);
    await flushPromises();
    first.resolve([item('p1')]);
    await flushPromises();

    expect(wrapper.text()).toContain('p2 brief item');
    expect(wrapper.text()).not.toContain('p1 brief item');
    wrapper.unmount();
  });
});

describe('BriefWorkspace AR04 shared reconciler', () => {
  it('generates a brief and observes its durable job to a reviewable state', async () => {
    vi.useFakeTimers();
    vi.spyOn(parley, 'getBriefItems').mockResolvedValue([item('p1')]);
    vi.spyOn(parley, 'listFluency').mockResolvedValue([]);
    vi.spyOn(parley, 'listAgentChats').mockResolvedValue([]);
    vi.spyOn(parley, 'listSegments').mockResolvedValue([]);
    vi.spyOn(parley, 'generateBrief').mockResolvedValue({ job_id: 'j1' });
    vi.spyOn(parley, 'getRoleJob').mockResolvedValue({ id: 'j1', role: 'briefer', status: 'succeeded', result: { item_count: 1 }, last_error: null, review_ref: null });

    const wrapper = mount(BriefWorkspace, { props: { parties: [dana, miles] } });
    await flushPromises();
    await wrapper.findAll('select')[1]!.setValue('p1');
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Generate brief proposals')!.trigger('click');
    await flushPromises();
    await vi.advanceTimersByTimeAsync(2100);
    await flushPromises();

    expect(wrapper.text()).toContain('ready for owner review');
    expect(wrapper.text()).toContain('p1 brief item');
    wrapper.unmount();
    vi.useRealTimers();
  });

  it('records a scoped research turn and refreshes the chat from the job result', async () => {
    vi.useFakeTimers();
    const chat = { id: 'c1', role: 'briefer', scope_kind: 'party', scope_id: 'p1', asset_kind: null, revision: 1, active_job_id: null, turns: [], questions: [], candidates: [] };
    const summary = { id: 'c1', role: 'briefer' as const, scope_kind: 'party' as const, scope_id: 'p1', asset_kind: null, revision: 2, last_turn: null, updated_at: '', active_job_id: 'j2' };
    vi.spyOn(parley, 'getBriefItems').mockResolvedValue([item('p1')]);
    vi.spyOn(parley, 'listFluency').mockResolvedValue([]);
    vi.spyOn(parley, 'listAgentChats').mockResolvedValue([summary]);
    vi.spyOn(parley, 'listSegments').mockResolvedValue([]);
    vi.spyOn(parley, 'createAgentChat').mockResolvedValue(chat as never);
    vi.spyOn(parley, 'addAgentTurn').mockResolvedValue({ chat_id: 'c1', turn_id: 't1', job_id: 'j2', revision: 2 });
    vi.spyOn(parley, 'getRoleJob').mockResolvedValue({ id: 'j2', role: 'briefer', status: 'succeeded', result: { response: 'ok' }, last_error: null, review_ref: null });
    vi.spyOn(parley, 'getAgentChat').mockResolvedValue({ ...chat, revision: 2, turns: [{ id: 't1', turn_no: 1, speaker: 'owner', text: 'Question', job_id: null, created_at: '' }] } as never);

    const wrapper = mount(BriefWorkspace, { props: { parties: [dana, miles] } });
    await flushPromises();
    await wrapper.findAll('select')[1]!.setValue('p1');
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Start scoped chat')!.trigger('click');
    await flushPromises();
    await wrapper.get('textarea[aria-label="Question"]').setValue('What should we clarify?');
    await wrapper.get('form.chat-composer').trigger('submit');
    await flushPromises();
    await vi.advanceTimersByTimeAsync(2100);
    await flushPromises();

    expect(parley.addAgentTurn).toHaveBeenCalled();
    expect(wrapper.text()).toContain('Your scoped question is saved');
    expect(wrapper.text()).toContain('You:');
    wrapper.unmount();
    vi.useRealTimers();
  });
});
