import { getCurrentScope, onScopeDispose } from 'vue';
import { newOperationKey } from '../../api/client';
import type {
  CommandArgs, CommandContext, CommandDescriptor, CommandPrompt, CommandSuccess,
} from '../../api/commands';
import type { ContextSelection } from './context';

/**
 * Browser command registry (UCR-001 §9.1). The shell owns the one registry;
 * components register commands for the lifetime of their scope. Server
 * descriptor ids are immutable: the registry never edits, renames or wraps a
 * server descriptor, it merely places frozen copies alongside `ui.*` entries.
 */

export interface UiRunContext {
  context: CommandContext;
  selection: ContextSelection;
}

export interface UiCommand {
  id: `ui.${string}`;
  title: string;
  summary?: string;
  keywords?: string[];
  effect: 'read';
  /** Server descriptor id whose UI front door this command replaces in context. */
  replaces?: string;
  when?: (ctx: UiRunContext) => boolean;
  prompts?: CommandPrompt[];
  /** Text for the in-modal confirmation step of a write-fronting UI action. */
  confirmation?: string;
  run: (args: CommandArgs, ctx: UiRunContext) => void | Promise<void>;
}

export type Disposable = () => void;

export interface CommandRegistry {
  /** Shell navigation reused by built-in ui.* commands and guarded views. */
  navigate(path: string): void;
  register(command: UiCommand): Disposable;
  unregister(id: string): void;
  get(id: string): UiCommand | undefined;
  list(ctx: UiRunContext): UiCommand[];
  descriptors(ctx: UiRunContext): CommandDescriptor[];
  run(id: string, args: CommandArgs, ctx: UiRunContext): Promise<void>;
}

export function uiDescriptor(command: UiCommand): CommandDescriptor {
  return Object.freeze({
    id: command.id,
    version: 1,
    title: command.title,
    summary: command.summary ?? '',
    description: command.summary ?? '',
    keywords: Object.freeze([...(command.keywords ?? [])]) as unknown as string[],
    effect: 'read' as const,
    requires_permission: 'ui:none',
    surfaces: Object.freeze(['palette'] as const) as unknown as CommandDescriptor['surfaces'],
    args_schema: Object.freeze({ type: 'object', additionalProperties: false }) as CommandDescriptor['args_schema'],
    result_schema: Object.freeze({ type: 'object' }) as CommandDescriptor['result_schema'],
    supports_dry_run: false,
    palette: Object.freeze({
      context_fields: Object.freeze([]) as unknown as string[],
      prompts: Object.freeze([...(command.prompts ?? [])]) as unknown as CommandPrompt[],
    }),
  });
}

/** Freeze a server descriptor without mutating the object received from the API. */
export function freezeDescriptor(descriptor: CommandDescriptor): CommandDescriptor {
  return Object.freeze({
    ...descriptor,
    keywords: Object.freeze([...descriptor.keywords]) as unknown as string[],
    surfaces: Object.freeze([...descriptor.surfaces]) as unknown as CommandDescriptor['surfaces'],
    args_schema: Object.freeze({ ...descriptor.args_schema }) as CommandDescriptor['args_schema'],
    result_schema: Object.freeze({ ...descriptor.result_schema }) as CommandDescriptor['result_schema'],
    palette: Object.freeze({
      ...descriptor.palette,
      context_fields: Object.freeze([...descriptor.palette.context_fields]) as unknown as string[],
      prompts: Object.freeze(descriptor.palette.prompts.map((prompt) => Object.freeze({ ...prompt }))) as unknown as CommandPrompt[],
    }),
  });
}

/**
 * Merge server and browser commands into one palette list. Scoped UI actions
 * hide the server descriptor they replace; a server descriptor's id and text
 * are otherwise copied verbatim into a frozen object.
 */
export function mergeDescriptors(server: readonly CommandDescriptor[], ui: readonly UiCommand[]): CommandDescriptor[] {
  const replaced = new Set(ui.map((command) => command.replaces).filter((id): id is string => typeof id === 'string'));
  return [
    ...server.filter((descriptor) => !replaced.has(descriptor.id)).map(freezeDescriptor),
    ...ui.map(uiDescriptor),
  ];
}

function assertUiId(id: string): asserts id is `ui.${string}` {
  if (!/^ui\.[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$/.test(id) || id.includes('__')) {
    throw new Error(`Frontend command id must use the reserved ui. namespace: ${id}`);
  }
}

export function createCommandRegistry(navigate: (path: string) => void): CommandRegistry {
  const registered = new Map<string, UiCommand>();
  return {
    navigate,
    register(command) {
      assertUiId(command.id);
      if (registered.has(command.id)) throw new Error(`Frontend command already registered: ${command.id}`);
      registered.set(command.id, command);
      return () => { if (registered.get(command.id) === command) registered.delete(command.id); };
    },
    unregister(id) { registered.delete(id); },
    get(id) { return registered.get(id); },
    list(ctx) {
      return [...registered.values()].filter((command) => !command.when || command.when(ctx));
    },
    descriptors(ctx) {
      return this.list(ctx).map(uiDescriptor);
    },
    async run(id, args, ctx) {
      const command = registered.get(id);
      if (!command) throw new Error(`Unknown frontend command: ${id}`);
      if (command.when && !command.when(ctx)) throw new Error(`Frontend command is not available: ${id}`);
      await command.run(args, ctx);
    },
  };
}

/** Scoped registration: disposed with the calling component's effect scope. */
export function registerScoped(registry: CommandRegistry, commands: readonly UiCommand[]): Disposable {
  const registrations = commands.map((command) => registry.register(command));
  const dispose = () => { for (const unregister of registrations.reverse()) unregister(); };
  const scope = getCurrentScope();
  if (scope) onScopeDispose(dispose);
  return dispose;
}

const NAVIGATIONS: Array<{ suffix: string; title: string; path: string; keywords: string[] }> = [
  { suffix: 'open_resources', title: 'Open Resources', path: '/resources', keywords: ['inventory', 'cards'] },
  { suffix: 'open_parties', title: 'Open Parties', path: '/parties', keywords: ['leads', 'directory'] },
  { suffix: 'open_queue', title: 'Open Queue', path: '/queue', keywords: ['scenarios', 'sheet'] },
  { suffix: 'open_output', title: 'Open Output', path: '/output', keywords: ['materials', 'approved'] },
  { suffix: 'open_settings', title: 'Open Settings', path: '/settings', keywords: ['packs', 'provider'] },
  { suffix: 'open_inbox', title: 'Open capture inbox', path: '/parties/inbox', keywords: ['captures', 'sources'] },
  { suffix: 'open_templates', title: 'Open templates', path: '/output/templates', keywords: ['writing', 'templates'] },
];

/** The shell's seven built-in navigation commands. */
export function builtInNavigationCommands(navigate: (path: string) => void): UiCommand[] {
  return NAVIGATIONS.map((entry) => ({
    id: `ui.${entry.suffix}` as const,
    title: entry.title,
    summary: `Navigate to ${entry.title.toLocaleLowerCase()}.`,
    keywords: entry.keywords,
    effect: 'read' as const,
    run: () => { navigate(entry.path); },
  }));
}

export interface QueueActionInput {
  opportunityId: string;
  phaseRevision: number;
  packId: string;
  packVersion: string;
  originalEventId: string | null;
  /** Fingerprint of the selected record, revision and epoch. */
  intentFingerprint: string;
  invoke: (id: string, args: CommandArgs, key: string) => Promise<CommandSuccess>;
  onCommitted: (result: CommandSuccess, commandId: string) => void | Promise<void>;
}

const SET_ID = 'opportunity.set_phase';
const UNDO_ID = 'opportunity.undo_phase_change';

/**
 * Scoped Queue actions. Each dispatch freezes the authoritative record,
 * revision, pack identity, args and command version together with a fresh
 * operation key. Any change to the fingerprint creates a new intent and key.
 */
export function queuePhaseActions(input: QueueActionInput): UiCommand[] {
  let frozen: { fingerprint: string; key: string; args: Record<string, CommandArgs>; version: number } | null = null;
  function intent() {
    const fingerprint = `${input.intentFingerprint}|${input.opportunityId}|${input.phaseRevision}|${input.packId}|${input.packVersion}|${input.originalEventId ?? ''}`;
    if (!frozen || frozen.fingerprint !== fingerprint) {
      const common = {
        opportunity_id: input.opportunityId,
        expected_phase_revision: input.phaseRevision,
        expected_pack_id: input.packId,
        expected_pack_version: input.packVersion,
      };
      frozen = {
        fingerprint,
        key: newOperationKey(),
        version: 1,
        args: Object.freeze({
          [SET_ID]: Object.freeze({ ...common }),
          [UNDO_ID]: Object.freeze({ ...common, original_event_id: input.originalEventId ?? '' }),
        }),
      };
    }
    return frozen;
  }
  const dispatch = (
    id: string,
    args: (frozenArgs: Record<string, CommandArgs>, prompted: CommandArgs) => CommandArgs,
  ) => async (prompted: CommandArgs) => {
    const current = intent();
    const result = await input.invoke(id, args(current.args, prompted), current.key);
    await input.onCommitted(result, id);
  };
  return [
    {
      id: 'ui.queue.set_phase',
      title: 'Change opportunity phase',
      summary: 'Move the selected opportunity to a planning phase',
      keywords: ['queue', 'stage'],
      effect: 'read',
      replaces: SET_ID,
      prompts: [{ field: 'stage_id', picker: 'planning_stage', title: 'Planning phase' }],
      confirmation: 'Move the selected opportunity to this planning phase?',
      when: (ctx) => ctx.context.route === 'queue' && ctx.context['selection.opportunity'] === true,
      run: dispatch(SET_ID, (args, prompted) => ({ ...(args[SET_ID] ?? {}), ...prompted })),
    },
    {
      id: 'ui.queue.undo_phase_change',
      title: 'Undo phase change',
      summary: 'Revert the latest manual phase change',
      keywords: ['queue', 'stage', 'undo'],
      effect: 'read',
      replaces: UNDO_ID,
      confirmation: 'Undo the last recorded phase change for this opportunity?',
      when: (ctx) => ctx.context.route === 'queue' && ctx.context['selection.opportunity'] === true,
      run: dispatch(UNDO_ID, (args, prompted) => ({ ...(args[UNDO_ID] ?? {}), ...prompted })),
    },
  ];
}

let installed: CommandRegistry | null = null;

/** Install the shell-owned registry. Replacing it disposes the previous one. */
export function installCommandRegistry(registry: CommandRegistry): Disposable {
  const previous = installed;
  installed = registry;
  return () => { if (installed === registry) installed = previous; };
}

export function useCommandRegistry(): CommandRegistry {
  if (!installed) throw new Error('Command registry is not installed');
  return installed;
}
