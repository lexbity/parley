import { describe, expect, it } from 'vitest';
import {
  CONTEXT_KEY_ALLOWLIST, contextFingerprint, deriveContext, isAllowedContextKey, routeToken,
} from './context';

describe('deriveContext', () => {
  it('derives a closed queue context with an explicit opportunity flag', () => {
    const context = deriveContext(
      { tab: 'Queue', location: 'workspace', recordId: 'o1' },
      { opportunity: 'o1' },
    );
    expect(context).toEqual({
      route: 'queue',
      'selection.opportunity': true,
      'selection.party': false,
      'selection.material': false,
      'selection.resource': false,
      'selection.question': false,
    });
    expect(Object.keys(context).length).toBeLessThanOrEqual(16);
  });

  it('uses false for missing and blank selections', () => {
    const context = deriveContext({ tab: 'Parties' }, { opportunity: null, party: '   ', material: undefined });
    expect(context['selection.opportunity']).toBe(false);
    expect(context['selection.party']).toBe(false);
    expect(context['selection.material']).toBe(false);
    expect(context.route).toBe('parties');
  });

  it('maps deep locations to dotted route tokens', () => {
    expect(routeToken({ tab: 'Parties', location: 'inbox' })).toBe('parties.inbox');
    expect(routeToken({ tab: 'Output', location: 'templates' })).toBe('output.templates');
    expect(routeToken({ tab: 'Unknown Tab' })).toBe('unknown_tab');
  });

  it('drops unknown selection keys and reports only allowlisted context keys', () => {
    const selection = { opportunity: 'o1', secret: 'hidden' } as unknown as Parameters<typeof deriveContext>[1];
    const context = deriveContext({ tab: 'Queue' }, selection);
    expect(Object.keys(context).every(isAllowedContextKey)).toBe(true);
    expect(JSON.stringify(context)).not.toContain('hidden');
    expect(isAllowedContextKey('selection.secret')).toBe(false);
    expect(CONTEXT_KEY_ALLOWLIST).not.toContain('selection.secret');
  });

  it('fingerprints context deterministically and notices selection changes', () => {
    const first = deriveContext({ tab: 'Queue' }, { opportunity: 'o1' });
    const same = deriveContext({ tab: 'Queue' }, { opportunity: 'o1' });
    const changed = deriveContext({ tab: 'Queue' }, { opportunity: null });
    expect(contextFingerprint(first)).toBe(contextFingerprint(same));
    expect(contextFingerprint(first)).not.toBe(contextFingerprint(changed));
  });
});
