import type { CommandContext, CommandContextValue } from '../../api/commands';

/**
 * Closed palette context (UCR-001 FR-7, §4.5). Only `route` and allowlisted
 * `selection.*` keys may cross the wire. A missing selection is an explicit
 * `false`, never an absent key, so server-side `when` predicates are stable.
 */

export const CONTEXT_KEY_ALLOWLIST = [
  'route',
  'selection.opportunity',
  'selection.party',
  'selection.material',
  'selection.resource',
  'selection.question',
] as const;

export type ContextKey = typeof CONTEXT_KEY_ALLOWLIST[number];

export interface ContextRoute {
  tab: string;
  location?: 'inbox' | 'templates' | 'workspace' | string;
  recordId?: string | null;
}

/** Logical selection: a present id means the record is selected in the UI. */
/** Authoritative phase data a Queue view contributes to palette intents. */
export interface QueuePaletteBinding {
  opportunityId: string | null;
  phaseRevision: number;
  pack: { id: string; version: string };
  stages: Array<{ value: string; label: string }>;
  originalEventId: string | null;
  /** Indexed tasks of the selected opportunity, from the authoritative notes read. */
  tasks: Array<{ value: string; label: string; byteStart: number; byteEnd: number; checked: boolean }>;
  /** Notes revision captured with the selected row. */
  notesRevision: number;
}

export interface ContextSelection {
  opportunity?: string | null;
  party?: string | null;
  material?: string | null;
  resource?: string | null;
  question?: string | null;
}

const ROUTE_NAMES: Record<string, string> = {
  resources: 'resources',
  parties: 'parties',
  leads: 'parties',
  queue: 'queue',
  output: 'output',
  settings: 'settings',
};

function present(value: string | null | undefined): boolean {
  return typeof value === 'string' && value.trim().length > 0;
}

/** Lowercase snake route token; deep locations get a dotted suffix. */
export function routeToken(route: ContextRoute): string {
  const base = ROUTE_NAMES[route.tab.toLowerCase()] ?? route.tab.toLowerCase().replace(/[^a-z0-9]+/g, '_');
  if (route.location === 'inbox' && base === 'parties') return 'parties.inbox';
  if (route.location === 'templates' && base === 'output') return 'output.templates';
  return base;
}

/**
 * Derive the closed context map. Unknown keys are dropped, boolean selection
 * flags are always present, and the result is bounded below the 16-key wire
 * limit even when callers supply extra fields.
 */
export function deriveContext(route: ContextRoute, selection: ContextSelection = {}): CommandContext {
  const context: CommandContext = { route: routeToken(route) };
  for (const key of CONTEXT_KEY_ALLOWLIST) {
    if (key === 'route') continue;
    const field = key.slice('selection.'.length) as keyof ContextSelection;
    context[key] = present(selection[field]) as CommandContextValue;
  }
  return context;
}

/** Stable fingerprint for context revalidation and intent freezing. */
export function contextFingerprint(context: CommandContext): string {
  return CONTEXT_KEY_ALLOWLIST
    .map((key) => `${key}=${String(context[key] ?? '')}`)
    .join('|');
}

/** True when a context value is an allowlisted key with a wire-safe shape. */
export function isAllowedContextKey(key: string): key is ContextKey {
  return (CONTEXT_KEY_ALLOWLIST as readonly string[]).includes(key);
}
