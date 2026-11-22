/* global InputEvent, Range */
import { flushPromises, mount } from '@vue/test-utils';
import { webcrypto } from 'node:crypto';
import { describe, expect, it, vi } from 'vitest';
import NotesEditor, { type NotesService, type NotesSnapshot } from './NotesEditor.vue';

const initial = '- [ ] Répéter\n1. [x] Répéter\n  - [ ] Nested task\n```md\n- [ ] This is code\n```';

Object.defineProperty(Range.prototype, 'getClientRects', { configurable: true, value: () => [] });
Object.defineProperty(Range.prototype, 'getBoundingClientRect', { configurable: true, value: () => ({ x: 0, y: 0, top: 0, right: 0, bottom: 0, left: 0, width: 0, height: 0, toJSON: () => ({}) }) });

function makeService(overrides: Partial<NotesService> = {}): NotesService {
  return {
    getNotes: vi.fn(async (id) => ({ opportunity_id: id, markdown: initial, revision: 4 })),
    saveNotes: vi.fn(async (id, input) => ({ markdown: `${id}:${input.markdown}`, revision: 5 })),
    toggleTask: vi.fn(async (id, input) => ({
      markdown: `${id}:${input.start_byte}:${input.end_byte}`,
      revision: 5,
    })),
    ...overrides,
  };
}

function getExposedParser(wrapper: ReturnType<typeof mount>): (source: string) => Array<{ lineIndex: number; startByte: number; endByte: number; checked: boolean; ordered: boolean }> {
  const exposed = wrapper.vm.$.exposed as { parseTasks: (source: string) => Array<{ lineIndex: number; startByte: number; endByte: number; checked: boolean; ordered: boolean }> } | null;
  if (!exposed) throw new Error('NotesEditor test surface was not exposed.');
  return exposed.parseTasks;
}

describe('S05 NotesEditor', () => {
  it('parses nested unordered and ordered task markers, ignores fenced examples, and returns UTF-8 byte offsets', () => {
    vi.stubGlobal('crypto', webcrypto);
    const wrapper = mount(NotesEditor, { props: { opportunityId: 'op-1', service: makeService(), workspaceEpoch: 'epoch-1' } });
    const parsed = getExposedParser(wrapper)(initial);
    expect(parsed.map((task) => [task.lineIndex, task.checked, task.ordered])).toEqual([
      [0, false, false], [1, true, true], [2, false, false],
    ]);
    const firstMarker = new TextEncoder().encode('- ');
    expect(parsed[0]?.startByte).toBe(firstMarker.length);
    expect(parsed[0]?.endByte).toBe(parsed[0]!.startByte + 3);
    expect(parsed[1]?.startByte).toBe(new TextEncoder().encode('- [ ] Répéter\n1. ').length);
    wrapper.unmount();
    vi.unstubAllGlobals();
  });

  it('loads canonical notes, identifies duplicated task labels by source line, and submits exact marker bytes and line digest', async () => {
    vi.stubGlobal('crypto', webcrypto);
    const service = makeService({
      toggleTask: vi.fn(async (id, input) => ({
        markdown: id ? initial.replace('1. [x] Répéter', `1. [${input.checked ? 'x' : ' '}] Répéter`) : initial,
        revision: 5,
      })),
    });
    const wrapper = mount(NotesEditor, { props: { opportunityId: 'op-1', service, workspaceEpoch: 'epoch-1' } });
    await flushPromises();
    const checks = wrapper.findAll('.task-list input[type="checkbox"]');
    expect(checks).toHaveLength(3);
    await checks[1]!.setValue(false);
    await flushPromises();
    const call = vi.mocked(service.toggleTask).mock.calls[0];
    expect(call?.[0]).toBe('op-1');
    expect(call?.[1]).toMatchObject({
      expected_revision: 4,
      start_byte: new TextEncoder().encode('- [ ] Répéter\n1. ').length,
      end_byte: new TextEncoder().encode('- [ ] Répéter\n1. [x]').length,
      checked: false,
    });
    const lineHash = await webcrypto.subtle.digest('SHA-256', new TextEncoder().encode('1. [x] Répéter'));
    expect(call?.[1].line_hash).toBe(Array.from(new Uint8Array(lineHash), (byte) => byte.toString(16).padStart(2, '0')).join(''));
    expect(wrapper.text()).toContain('This is code');
    expect(wrapper.find('img').exists()).toBe(false);
    wrapper.unmount();
    vi.unstubAllGlobals();
  });

  it('preserves a local edit after a 412 and requires a reviewed accept or reapply choice', async () => {
    vi.stubGlobal('crypto', webcrypto);
    const service = makeService({
      saveNotes: vi.fn(async () => { const error = new Error('stale') as Error & { status: number }; error.status = 412; throw error; }),
      getNotes: vi.fn(async (id): Promise<NotesSnapshot> => ({ opportunity_id: id, markdown: 'Other session text.', revision: 5 })),
    });
    const wrapper = mount(NotesEditor, { props: { opportunityId: 'op-conflict', service, workspaceEpoch: 'epoch-1' } });
    await flushPromises();
    const cm = wrapper.get('.cm-content').element;
    for (let index = 0; index < 100; index += 1) {
      cm.textContent = `My current text ${index}`;
      cm.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: '.' }));
    }
    await vi.waitFor(() => expect(vi.mocked(service.saveNotes)).toHaveBeenCalled());
    await flushPromises();
    expect(service.saveNotes).toHaveBeenCalledTimes(1);
    expect(wrapper.get('[role="alert"]').text()).toContain('Notes changed in another session');
    expect(wrapper.text()).toContain('My current text 99');
    expect(wrapper.text()).toContain('Other session text.');
    expect(wrapper.findAll('button').some((button) => button.text().includes('Review and reapply'))).toBe(true);
    wrapper.unmount();
    vi.unstubAllGlobals();
  });

  it('does not render raw HTML from the Markdown source as live elements', async () => {
    const service = makeService({ getNotes: vi.fn(async (id) => ({ opportunity_id: id, markdown: '<img src=x onerror=alert(1)>', revision: 1 })) });
    const wrapper = mount(NotesEditor, { props: { opportunityId: 'op-safe', service, workspaceEpoch: 'epoch-1' } });
    await flushPromises();
    expect(wrapper.find('img').exists()).toBe(false);
    expect(wrapper.text()).toContain('<img src=x onerror=alert(1)>');
    wrapper.unmount();
  });
});
