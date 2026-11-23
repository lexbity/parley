import { describe, expect, it } from 'vitest';
import { rankCommands, subsequenceSkips, type RankableCommand } from './search';

const items: RankableCommand[] = [
  { id: 'z.undo', title: 'Undo phase change', keywords: ['queue', 'stage'] },
  { id: 'a.set', title: 'Change opportunity phase', keywords: ['queue'] },
  { id: 'b.exact', title: 'Phase', keywords: [] },
  { id: 'c.prefix', title: 'Phase editor', keywords: [] },
  { id: 'd.token', title: 'Queue: set phase', keywords: [] },
  { id: 'e.subsequence', title: 'Publish assessment help', keywords: [] },
];

describe('rankCommands', () => {
  it('orders exact title, prefix, token prefix then subsequence', () => {
    const ranked = rankCommands(items, 'phase').map((item) => item.id);
    expect(ranked[0]).toBe('b.exact');
    expect(ranked[1]).toBe('c.prefix');
    expect(ranked.slice(2, 4)).toEqual(['d.token', 'a.set'].sort());
    expect(ranked).toContain('e.subsequence');
  });

  it('prefers fewer skipped characters inside the subsequence class', () => {
    expect(subsequenceSkips('Change opportunity phase', 'chp')).toBeGreaterThanOrEqual(0);
    expect(subsequenceSkips('Phase editor', 'ped')).toBeLessThan(subsequenceSkips('Publish assessment help', 'ped') ?? 99);
    const ranked = rankCommands([
      { id: 'tight', title: 'phase editor' },
      { id: 'loose', title: 'publish assessment help' },
    ], 'ped').map((item) => item.id);
    expect(ranked).toEqual(['tight']);
  });

  it('uses session recency only as a tie-break and ID as final order', () => {
    const tie: RankableCommand[] = [
      { id: 'a', title: 'Phase one' },
      { id: 'b', title: 'Phase two' },
    ];
    expect(rankCommands(tie, 'phase').map((item) => item.id)).toEqual(['a', 'b']);
    expect(rankCommands(tie, 'phase', { b: 3 }).map((item) => item.id)).toEqual(['b', 'a']);
  });

  it('normalizes search text without changing the source title', () => {
    const source = { id: 'unicode', title: 'Café Phase' };
    expect(rankCommands([source], ' CAFÉ  phase  ')).toEqual([source]);
    expect(source.title).toBe('Café Phase');
  });

  it('returns no match marker for a missing subsequence', () => {
    expect(subsequenceSkips('phase', 'xyz')).toBeNull();
  });
});
