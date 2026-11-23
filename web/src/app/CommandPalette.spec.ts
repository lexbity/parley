import { mount, flushPromises } from '@vue/test-utils';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { commands, type CommandDescriptor, type CommandSuccess } from '../api/commands';
import { deriveContext } from './commands/context';
import { createCommandRegistry, installCommandRegistry, useCommandRegistry } from './commands/registry';
import CommandPalette from './CommandPalette.vue';

const descriptor: CommandDescriptor = {
  id: 'ui.queue.set_phase',
  version: 1,
  title: 'Change opportunity phase',
  summary: 'Move an open opportunity to a planning phase',
  description: 'Moves a record.',
  keywords: ['queue'],
  effect: 'read',
  requires_permission: '',
  surfaces: ['palette'],
  args_schema: { type: 'object' },
  result_schema: { type: 'object' },
  supports_dry_run: false,
  palette: { context_fields: [], prompts: [{ field: 'stage_id', picker: 'planning_stage', title: 'Planning phase' }] },
};

const receipt: CommandSuccess = {
  ok: true, command_version: 1, audit_id: 'aud-1', replayed: false, dry_run: false,
  result: { opportunity_id: 'o1', stage_id: 'preparing', phase_revision: 2 },
};

beforeEach(() => {
  const navigate = vi.fn();
  const registry = createCommandRegistry(navigate);
  registry.register({
    id: 'ui.queue.set_phase', title: descriptor.title, summary: descriptor.summary, effect: 'read',
    prompts: descriptor.palette.prompts, confirmation: 'Apply this phase change?', run: vi.fn(),
  });
  installCommandRegistry(registry);
});

afterEach(() => { vi.restoreAllMocks(); });

function useServerPath() { useCommandRegistry().unregister('ui.queue.set_phase'); }

function mountPalette(overrides: Record<string, unknown> = {}) {
  const wrapper = mount(CommandPalette, {
    props: {
      open: true,
      descriptors: [descriptor],
      context: deriveContext({ tab: 'Queue' }, { opportunity: 'o1' }),
      selection: { opportunity: 'o1' },
      pickerOptions: { planning_stage: [{ value: 'preparing', label: 'Preparing' }] },
      intentFingerprint: 'epoch-1|o1|1',
      ...overrides,
    },
    attachTo: globalThis.document.body,
  });
  return wrapper;
}

describe('CommandPalette', () => {
  it('opens a labelled modal with a combobox, owned listbox and active option', async () => {
    const wrapper = mountPalette();
    await flushPromises();
    const dialog = wrapper.get('dialog.command-palette');
    expect(dialog.attributes('aria-labelledby')).toBe('command-palette-title');
    const input = wrapper.get('[role="combobox"]');
    expect(input.attributes('aria-expanded')).toBe('true');
    expect(input.attributes('aria-controls')).toBe('command-palette-listbox');
    expect(input.attributes('aria-activedescendant')).toBe('command-palette-option-0');
    expect(wrapper.get('[role="listbox"]').attributes('id')).toBe('command-palette-listbox');
    expect(wrapper.get('[role="option"]').text()).toContain('Change opportunity phase');
    wrapper.unmount();
  });

  it('moves the active descendant with arrows, ignores IME keys and selects with Enter', async () => {
    const wrapper = mountPalette({ descriptors: [descriptor, { ...descriptor, id: 'ui.queue.undo_phase_change', title: 'Undo phase change', palette: { context_fields: [], prompts: [] } }] });
    await flushPromises();
    const input = wrapper.get('[role="combobox"]');
    await input.trigger('compositionstart');
    await input.trigger('keydown', { key: 'ArrowDown' });
    expect(input.attributes('aria-activedescendant')).toBe('command-palette-option-0');
    await input.trigger('compositionend');
    await input.trigger('keydown', { key: 'ArrowDown' });
    expect(input.attributes('aria-activedescendant')).toBe('command-palette-option-1');
    await input.trigger('keydown', { key: 'ArrowUp' });
    expect(input.attributes('aria-activedescendant')).toBe('command-palette-option-0');
    await input.trigger('keydown', { key: 'Enter' });
    expect(wrapper.text()).toContain('Planning phase');
    expect(wrapper.get('button.primary').attributes('disabled')).toBeDefined();
    wrapper.unmount();
  });

  it('collects a typed prompt, confirms, freezes a key and emits the receipt', async () => {
    useServerPath();
    const invoke = vi.spyOn(commands, 'invoke').mockResolvedValue(receipt);
    const wrapper = mountPalette();
    await flushPromises();
    await wrapper.get('[role="combobox"]').trigger('keydown', { key: 'Enter' });
    await wrapper.get('label select').setValue('preparing');
    expect(wrapper.get('button.primary').attributes('disabled')).toBeUndefined();
    await wrapper.get('button.primary').trigger('click');
    expect(wrapper.text()).toContain('Run “Change opportunity phase”?');
    const key = (wrapper.vm as unknown as { intentKey: () => string }).intentKey();
    expect(key).toMatch(/^[0-9a-f-]+$/);
    await wrapper.get('button.primary').trigger('click');
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith('ui.queue.set_phase', {
      version: 1, args: { stage_id: 'preparing' }, dry_run: false,
    }, key);
    expect(wrapper.emitted('committed')?.[0]).toEqual([receipt]);
    expect(wrapper.text()).toContain('Command completed');
    wrapper.unmount();
  });

  it('reports a conflict when the frozen selection or epoch changes before dispatch', async () => {
    useServerPath();
    const invoke = vi.spyOn(commands, 'invoke').mockResolvedValue(receipt);
    const wrapper = mountPalette();
    await flushPromises();
    await wrapper.get('[role="combobox"]').trigger('keydown', { key: 'Enter' });
    await wrapper.get('label select').setValue('preparing');
    await wrapper.get('button.primary').trigger('click');
    const first = (wrapper.vm as unknown as { intentKey: () => string }).intentKey();
    await wrapper.setProps({ intentFingerprint: 'epoch-2|o1|2' });
    await flushPromises();
    expect((wrapper.vm as unknown as { intentKey: () => string }).intentKey()).toBe(first);
    await wrapper.get('button.primary').trigger('click');
    await flushPromises();
    expect(invoke).not.toHaveBeenCalled();
    expect(wrapper.get('[role="alert"]').text()).toContain('changed after this command was prepared');
    wrapper.unmount();
  });

  it('reports a command failure inside the modal without losing the prompt', async () => {
    useServerPath();
    vi.spyOn(commands, 'invoke').mockRejectedValue(new Error('Phase revision changed'));
    const wrapper = mountPalette();
    await flushPromises();
    await wrapper.get('[role="combobox"]').trigger('keydown', { key: 'Enter' });
    await wrapper.get('label select').setValue('preparing');
    await wrapper.get('button.primary').trigger('click');
    await wrapper.get('button.primary').trigger('click');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('Phase revision changed');
    await wrapper.findAll('button').find((button) => button.text() === 'Back')!.trigger('click');
    expect(wrapper.text()).toContain('Planning phase');
    wrapper.unmount();
  });

  it('closes on Escape and restores focus to the opener or the workspace heading', async () => {
    const trigger = globalThis.document.createElement('button');
    globalThis.document.body.appendChild(trigger);
    trigger.focus();
    const wrapper = mountPalette();
    await flushPromises();
    await wrapper.get('[role="combobox"]').trigger('keydown', { key: 'Escape' });
    expect(wrapper.emitted('close')).toHaveLength(1);
    await wrapper.setProps({ open: false });
    await flushPromises();
    expect(globalThis.document.activeElement).toBe(trigger);

    // A removed trigger falls back to the active workspace heading.
    const heading = globalThis.document.createElement('h1');
    heading.id = 'fallback-heading';
    globalThis.document.body.appendChild(heading);
    const second = mountPalette({ fallbackFocusSelector: '#fallback-heading' });
    await flushPromises();
    trigger.remove();
    await second.setProps({ open: false });
    await flushPromises();
    expect(globalThis.document.activeElement).toBe(heading);
    heading.remove();
    second.unmount();
    wrapper.unmount();
  });

  it('traps Tab inside the modal and closes on a native cancel event', async () => {
    const wrapper = mountPalette();
    await flushPromises();
    const dialog = wrapper.get('dialog.command-palette');
    await dialog.trigger('keydown', { key: 'Tab' });
    await dialog.trigger('keydown', { key: 'Tab', shiftKey: true });
    await dialog.trigger('cancel');
    expect(wrapper.emitted('close')).toHaveLength(1);
    wrapper.unmount();
  });

  it('runs a registered UI command directly when it has no prompts', async () => {
    const run = vi.fn().mockResolvedValue(undefined);
    useCommandRegistry().register({
      id: 'ui.queue.undo_phase_change', title: 'Undo phase change', effect: 'read',
      confirmation: 'Undo this phase change?', run,
    });
    const plain = { ...descriptor, id: 'ui.queue.undo_phase_change', title: 'Undo phase change', palette: { context_fields: [], prompts: [] } };
    const wrapper = mountPalette({ descriptors: [plain] });
    await flushPromises();
    await wrapper.get('[role="option"]').trigger('click');
    expect(wrapper.text()).toContain('Undo this phase change?');
    await wrapper.get('button.primary').trigger('click');
    await flushPromises();
    expect(run).toHaveBeenCalledWith({}, expect.objectContaining({ context: expect.any(Object), selection: expect.any(Object) }));
    expect(wrapper.text()).toContain('Undo phase change completed');
    wrapper.unmount();
  });

  it('accepts free-text prompt input when no typed picker is available', async () => {
    useServerPath();
    const invoke = vi.spyOn(commands, 'invoke').mockResolvedValue(receipt);
    const freeText = { ...descriptor, palette: { context_fields: [], prompts: [{ field: 'stage_id', picker: 'custom', title: 'Stage id' }] } };
    const wrapper = mountPalette({ descriptors: [freeText] });
    await flushPromises();
    await wrapper.get('[role="combobox"]').trigger('keydown', { key: 'Enter' });
    await wrapper.get('input[aria-label="Stage id"]').setValue('preparing');
    await wrapper.get('button.primary').trigger('click');
    await wrapper.get('button.primary').trigger('click');
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith(descriptor.id, expect.objectContaining({ args: { stage_id: 'preparing' } }), expect.any(String));
    wrapper.unmount();
  });

  it('renders empty, loading and no-match dataset states as text only', async () => {
    const empty = mountPalette({ descriptors: [] });
    await flushPromises();
    expect(empty.text()).toContain('No commands are available here.');
    empty.unmount();
    const noMatch = mountPalette();
    await flushPromises();
    await noMatch.get('[role="combobox"]').setValue('nothing matches this');
    expect(noMatch.text()).toContain('No matching commands.');
    expect(noMatch.html()).not.toContain('<script');
    noMatch.unmount();
  });
});
