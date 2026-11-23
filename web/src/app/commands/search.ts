/**
 * Palette search order (UCR-001 §5.2): exact title, title prefix, token
 * prefix, ordered subsequence with fewest skipped characters, session-only
 * recency, then ID. Search text is normalized; result titles are never
 * rewritten, only compared.
 */

export interface RankableCommand {
  id: string;
  title: string;
  keywords?: string[];
  summary?: string;
}

export type Recency = Record<string, number>;

function normalize(text: string): string {
  return text.toLocaleLowerCase().normalize('NFKC').trim().replace(/\s+/g, ' ');
}

/** Fewest skipped characters for an ordered subsequence, or null if absent. */
export function subsequenceSkips(text: string, query: string): number | null {
  const source = [...normalize(text)];
  const target = [...normalize(query)];
  if (target.length === 0) return 0;
  let position = 0;
  let skipped = 0;
  let started = false;
  for (const char of target) {
    while (position < source.length && source[position] !== char) {
      if (started) skipped += 1;
      position += 1;
    }
    if (position >= source.length) return null;
    started = true;
    position += 1;
  }
  return skipped;
}

function rank(item: RankableCommand, query: string): readonly [number, number] | null {
  const title = normalize(item.title);
  if (title === query) return [0, 0];
  if (title.startsWith(query)) return [1, 0];
  const tokens = [
    ...title.split(/[\s._-]+/).filter(Boolean),
    ...(item.keywords ?? []).map(normalize).flatMap((keyword) => keyword.split(/[\s._-]+/).filter(Boolean)),
  ];
  if (tokens.some((token) => token.startsWith(query))) return [2, 0];
  const skipped = subsequenceSkips(title, query);
  if (skipped !== null) return [3, skipped];
  return null;
}

/**
 * Stable ranking. Recency only breaks ties inside the same match class; it is
 * session memory and never persisted. A stable sort keeps ID as final order.
 */
export function rankCommands<T extends RankableCommand>(items: readonly T[], query: string, recency: Recency = {}): T[] {
  const needle = normalize(query);
  return items
    .map((item, index) => ({ item, index, score: rank(item, needle) }))
    .filter((entry): entry is { item: T; index: number; score: readonly [number, number] } => entry.score !== null)
    .sort((left, right) => {
      if (left.score[0] !== right.score[0]) return left.score[0] - right.score[0];
      if (left.score[1] !== right.score[1]) return left.score[1] - right.score[1];
      const recent = (recency[right.item.id] ?? 0) - (recency[left.item.id] ?? 0);
      if (recent !== 0) return recent;
      return left.item.id.localeCompare(right.item.id) || left.index - right.index;
    })
    .map((entry) => entry.item);
}
