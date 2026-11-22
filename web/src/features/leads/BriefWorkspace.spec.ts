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
