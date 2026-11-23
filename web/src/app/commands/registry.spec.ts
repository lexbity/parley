import { describe, expect, it, vi } from 'vitest';
import type { CommandDescriptor } from '../../api/commands';
import { deriveContext } from './context';
import {
  builtInNavigationCommands, createCommandRegistry, freezeDescriptor, installCommandRegistry,
  mergeDescriptors, queuePhaseActions, registerScoped, uiDescriptor, useCommandRegistry,
} from './registry';

const serverDescriptor: CommandDescriptor = {
  id: 'opportunity.set_phase',
  version: 1,
  title: 'Change opportunity phase',
  summary: 'Move an open opportunity to a planning phase',
  description: 'Moves a record.',
  keywords: ['queue'],
  effect: 'write',
  requires_permission: 'opportunity:phase',
  surfaces: ['palette', 'agent'],
  agent_tier: 2,
  args_schema: { type: 'object' },
  result_schema: { type: 'object' },
  supports_dry_run: false,
  when: 'route == "queue" && selection.opportunity',
  palette: { context_fields: ['route'], prompts: [{ field: 'stage_id', picker: 'planning_stage', title: 'Planning phase' }] },
};

describe('createCommandRegistry', () => {
  it('registers the seven built-in navigation commands with the shell', () => {
    const navigate = vi.fn();
    const registry = createCommandRegistry(navigate);
    registerScoped(registry, builtInNavigationCommands(navigate));
    const ids = registry.list({ context: deriveContext({ tab: 'Resources' }), selection: {} }).map((item) => item.id);
    expect(ids).toEqual([
      'ui.open_resources', 'ui.open_parties', 'ui.open_queue', 'ui.open_output',
      'ui.open_settings', 'ui.open_inbox', 'ui.open_templates',
    ]);
    const run = registry.get('ui.open_queue')!;
    void run.run({}, { context: { route: 'resources' }, selection: {} });
    expect(navigate).toHaveBeenCalledWith('/queue');
  });

  it('supports scoped disposal and rejects server ids or duplicate registrations', () => {
    const registry = createCommandRegistry(vi.fn());
    const command = { id: 'ui.temporary' as const, title: 'Temporary', effect: 'read' as const, run: vi.fn() };
    const dispose = registerScoped(registry, [command]);
    expect(registry.get('ui.temporary')).toBe(command);
    dispose();
    expect(registry.get('ui.temporary')).toBeUndefined();
    expect(() => registry.register({ ...command, id: 'opportunity.set_phase' as `ui.${string}` })).toThrow(/ui\. namespace/);
    expect(() => registry.register({ ...command, id: 'ui.bad__id' as `ui.${string}` })).toThrow(/ui\. namespace/);
    registry.register(command);
    expect(() => registry.register(command)).toThrow(/already registered/);
  });

  it('evaluates when predicates and refuses to run unavailable commands', async () => {
    const registry = createCommandRegistry(vi.fn());
    const run = vi.fn();
    registry.register({
      id: 'ui.queue.only', title: 'Queue only', effect: 'read', run,
      when: (ctx) => ctx.context.route === 'queue' && ctx.context['selection.opportunity'] === true,
    });
    const elsewhere = { context: deriveContext({ tab: 'Resources' }), selection: {} };
    const queue = { context: deriveContext({ tab: 'Queue' }, { opportunity: 'o1' }), selection: { opportunity: 'o1' } };
    expect(registry.list(elsewhere)).toEqual([]);
    expect(registry.list(queue)).toHaveLength(1);
    await expect(registry.run('ui.queue.only', {}, elsewhere)).rejects.toThrow(/not available/);
    await registry.run('ui.queue.only', {}, queue);
    expect(run).toHaveBeenCalledOnce();
    await expect(registry.run('ui.missing', {}, queue)).rejects.toThrow(/Unknown frontend command/);
    registry.unregister('ui.queue.only');
    expect(registry.list(queue)).toEqual([]);
  });
});

describe('descriptor merging', () => {
  it('freezes server descriptors without mutating the original', () => {
    const frozen = freezeDescriptor(serverDescriptor);
    expect(Object.isFrozen(frozen)).toBe(true);
    expect(Object.isFrozen(frozen.palette.prompts)).toBe(true);
    expect(frozen.title).toBe(serverDescriptor.title);
    expect(frozen.keywords).toEqual(serverDescriptor.keywords);
    expect(frozen.keywords).not.toBe(serverDescriptor.keywords);
    expect(() => { (frozen as { title: string }).title = 'changed'; }).toThrow();
    expect(serverDescriptor.title).toBe('Change opportunity phase');
  });

  it('keeps descriptor ids immutable and hides only the replaced server command', () => {
    const other = { ...serverDescriptor, id: 'opportunity.undo_phase_change', title: 'Undo phase change' };
    const untouched = { ...serverDescriptor, id: 'ui.navigate', title: 'Navigate' };
    const ui = createCommandRegistry(vi.fn());
    const action = queuePhaseActions({
      opportunityId: 'o1', phaseRevision: 3, packId: 'job_search', packVersion: '1.0.0',
      originalEventId: 'ev-1', intentFingerprint: 'epoch-1',
      invoke: vi.fn().mockResolvedValue({ ok: true }), onCommitted: vi.fn(),
    });
    for (const command of action) ui.register(command);
    const merged = mergeDescriptors([serverDescriptor, other, untouched], action);
    expect(merged.map((item) => item.id)).toEqual([
      'ui.navigate', 'ui.queue.set_phase', 'ui.queue.undo_phase_change',
    ]);
    expect(merged[0]).not.toBe(untouched);
    expect(ui.descriptors({
      context: { route: 'queue', 'selection.opportunity': true },
      selection: { opportunity: 'o1' },
    })[0]?.id.startsWith('ui.')).toBe(true);
  });

  it('builds a frozen ui descriptor with its prompts', () => {
    const descriptor = uiDescriptor({ id: 'ui.prompt', title: 'Prompt', effect: 'read', prompts: [{ field: 'stage_id', picker: 'planning_stage', title: 'Stage' }], run: vi.fn() });
    expect(descriptor.palette.prompts[0]?.field).toBe('stage_id');
    expect(Object.isFrozen(descriptor.palette)).toBe(true);
  });
});

describe('queuePhaseActions', () => {
  function setup(fingerprint = 'epoch-1') {
    const invoke = vi.fn().mockResolvedValue({ ok: true, result: { stage_id: 'preparing' } });
    const onCommitted = vi.fn();
    const actions = queuePhaseActions({
      opportunityId: 'o1', phaseRevision: 2, packId: 'job_search', packVersion: '1.0.0',
      originalEventId: 'ev-1', intentFingerprint: fingerprint, invoke, onCommitted,
    });
    return { actions, invoke, onCommitted };
  }

  it('freezes args, revision, version and a fresh key per distinct intent', async () => {
    const { actions, invoke, onCommitted } = setup();
    const ctx = { context: deriveContext({ tab: 'Queue' }, { opportunity: 'o1' }), selection: { opportunity: 'o1' } };
    await actions[0]!.run({ stage_id: 'preparing' }, ctx);
    const firstKey = invoke.mock.calls[0]![2] as string;
    expect(invoke.mock.calls[0]![0]).toBe('opportunity.set_phase');
    expect(invoke.mock.calls[0]![1]).toMatchObject({
      opportunity_id: 'o1', expected_phase_revision: 2, expected_pack_id: 'job_search', expected_pack_version: '1.0.0', stage_id: 'preparing',
    });
    expect(onCommitted).toHaveBeenCalledOnce();
    await actions[0]!.run({ stage_id: 'preparing' }, ctx);
    expect(invoke.mock.calls[1]![2]).toBe(firstKey);
    await actions[0]!.run({ stage_id: 'in_conversation' }, ctx);
    expect(invoke.mock.calls[2]![2]).not.toBe(firstKey);
    const changed = setup('epoch-2');
    await changed.actions[0]!.run({ stage_id: 'preparing' }, ctx);
    expect(changed.invoke.mock.calls[0]![2]).not.toBe(firstKey);
  });

  it('carries the exact original event id into an undo', async () => {
    const { actions, invoke } = setup();
    await actions[1]!.run({}, { context: { route: 'queue' }, selection: { opportunity: 'o1' } });
    expect(invoke.mock.calls[0]![1]).toMatchObject({ original_event_id: 'ev-1' });
    expect(actions[0]!.prompts?.[0]?.picker).toBe('planning_stage');
    expect(actions[1]!.confirmation).toContain('Undo');
  });
});

describe('registry installation', () => {
  it('installs one shell registry for useCommandRegistry', () => {
    const registry = createCommandRegistry(vi.fn());
    const dispose = installCommandRegistry(registry);
    expect(useCommandRegistry()).toBe(registry);
    dispose();
    expect(() => useCommandRegistry()).toThrow(/not installed/);
  });
});
