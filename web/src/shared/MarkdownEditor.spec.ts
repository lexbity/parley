import { EditorView } from '@codemirror/view';
import { mount } from '@vue/test-utils';
import { describe, expect, it } from 'vitest';
import MarkdownEditor from './MarkdownEditor.vue';

describe('MarkdownEditor', () => {
  it('emits document changes and replaces external values without echoing them', async () => {
    const wrapper = mount(MarkdownEditor, { props: { modelValue: '# Initial', label: 'Owner notes' } });
    const root = (wrapper.element as HTMLElement).shadowRoot!;
    const editor = root.querySelector<HTMLElement>('.cm-editor')!;
    const view = EditorView.findFromDOM(editor)!;
    view.dispatch({ changes: { from: view.state.doc.length, insert: '\n\n**Bold**' } });
    expect(wrapper.emitted('update:modelValue')).toEqual([['# Initial\n\n**Bold**']]);
    await wrapper.setProps({ modelValue: 'Restored draft' });
    expect(view.state.doc.toString()).toBe('Restored draft');
    expect(wrapper.emitted('update:modelValue')).toHaveLength(1);
    wrapper.unmount();
  });

  it('updates the textbox label and read-only state while mounted', async () => {
    const wrapper = mount(MarkdownEditor, { props: { modelValue: 'Note', label: 'Owner notes' } });
    const textbox = (wrapper.element as HTMLElement).shadowRoot!.querySelector('[role="textbox"]')!;
    expect(textbox.getAttribute('aria-label')).toBe('Owner notes');
    expect(textbox.getAttribute('contenteditable')).toBe('true');
    await wrapper.setProps({ label: 'Saved notes', readonly: true });
    expect(textbox.getAttribute('aria-label')).toBe('Saved notes');
    expect(textbox.getAttribute('aria-readonly')).toBe('true');
    expect(textbox.getAttribute('contenteditable')).toBe('false');
    await wrapper.setProps({ readonly: false });
    expect(textbox.getAttribute('contenteditable')).toBe('true');
    wrapper.unmount();
  });
});
