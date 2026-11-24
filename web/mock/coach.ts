type Row = Record<string, unknown>;

export const PACK_DOCUMENTS = [
  {
    id: 'job_search', name: 'Job search', version: '1.0.0',
    document: `id: job_search\nname: Job search\nversion: 1.0.0\nvocabulary:\n  party_noun: hiring contact\n  win_noun: role\nstages:\n  - { name: Target, class: identified }\n  - { name: Researched, class: prepared }\n  - { name: Contacted, class: touched }\nask_ladder:\n  - { rung: 1, weight: 1, label: one-line question }\n  - { rung: 2, weight: 2, label: short review }\ncadence:\n  max_touches: 3\nlimits:\n  daily_send_cap: 12`,
  },
  {
    id: 'lead_gen', name: 'Client lead generation', version: '1.0.0',
    document: `id: lead_gen\nname: Client lead generation\nversion: 1.0.0\nvocabulary:\n  party_noun: prospect\n  win_noun: engagement\nstages:\n  - { name: Identified, class: identified }\n  - { name: Briefed, class: prepared }\n  - { name: Contacted, class: touched }\nask_ladder:\n  - { rung: 1, weight: 1, label: yes/no question }\n  - { rung: 2, weight: 2, label: review }\ncadence:\n  max_touches: 3\nlimits:\n  daily_send_cap: 15`,
  },
];

export function buildDailyQueue(opportunities: Row[], threads: Row[], materials: Row[], cap: number, asOf: string, packId: string, packVersion: string) {
  const rows = opportunities.map((opportunity) => {
    const thread = threads.find((item) => item.opportunity_id === opportunity.id && ['waiting', 'conversing'].includes(String(item.state)));
    const opportunityMaterials = materials.filter((item) => item.opportunity_id === opportunity.id);
    const blocked = opportunityMaterials.length === 0 || !opportunityMaterials.some((item) => item.status === 'approved');
    const type = thread && thread.state === 'waiting' ? 'followup_due' : blocked ? 'blocked' : 'ready_to_send';
    const title = typeof opportunity.title === 'string' ? opportunity.title : 'Untitled opportunity';
    return {
      id: `${String(opportunity.id)}:${thread?.id ?? 'work'}`,
      opportunity_id: String(opportunity.id), title, type,
      party_id: Array.isArray(opportunity.participant_ids) ? String(opportunity.participant_ids[0] ?? '') || null : null,
      party_name: null,
      due_at: typeof thread?.next_action_at === 'string' ? thread.next_action_at : null,
      reason: thread?.state === 'waiting' ? 'A reply window is open; review the thread before the next touch.' : blocked ? 'No approved material is ready for this opportunity.' : 'Review the currently approved material and its release gate.',
      blocking_reason: blocked ? 'Create or approve material for the selected participant.' : null,
      remediation_target: blocked ? { kind: 'opportunity', id: String(opportunity.id) } : { kind: 'opportunity', id: String(opportunity.id) },
      stage_weight: Number(opportunity.stage_weight ?? 0),
    };
  }).filter((item) => item.type !== 'blocked' || item.blocking_reason !== null);
  rows.sort((a, b) => (a.due_at ?? '9999').localeCompare(b.due_at ?? '9999') || b.stage_weight - a.stage_weight || a.opportunity_id.localeCompare(b.opportunity_id) || a.id.localeCompare(b.id));
  const blocked = rows.filter((item) => item.type === 'blocked');
  const actionable = rows.filter((item) => item.type !== 'blocked').slice(0, cap);
  const totalActionable = rows.filter((item) => item.type !== 'blocked').length;
  const visible = [...actionable, ...blocked].map((item) => {
    const visibleItem = { ...item };
    delete (visibleItem as Partial<typeof item>).stage_weight;
    return visibleItem;
  });
  return { as_of: asOf, pack_id: packId, pack_version: packVersion, daily_cap: cap, actionable_count: totalActionable, deferred_count: totalActionable - actionable.length, items: visible };
}

export function wilson(k: number, n: number) {
  if (n === 0) return { rate: null, lower: null, upper: null, state: 'insufficient_observations' as const };
  const z = 1.96;
  const p = k / n;
  const d = 1 + z * z / n;
  const center = (p + z * z / (2 * n)) / d;
  const half = z * Math.sqrt((p * (1 - p) / n) + (z * z / (4 * n * n))) / d;
  return { rate: p, lower: Math.max(0, center - half), upper: Math.min(1, center + half), state: 'measured' as const };
}

export function buildInsights(sends: Row[], outcomes: Map<string, Row>, asOf: string, eventCursor: string) {
  const firstTouches = sends.filter((send) => Number(send.touch_no) === 1);
  const effectiveClass = (send: Row) => String(outcomes.get(String(send.id))?.class ?? 'no_outcome');
  const replied = (send: Row) => !['no_outcome', 'no_reply'].includes(effectiveClass(send));
  const advances = firstTouches.filter((send) => ['rung_accepted', 'call_scheduled'].includes(effectiveClass(send)));
  const measure = (k: number, n: number) => {
    const { lower, upper } = wilson(k, n);
    return { k, n, interval_95: lower === null ? null : { lower_95: lower, upper_95: upper } };
  };
  const strata = new Map<string, Row[]>();
  for (const send of firstTouches) {
    if (!Array.isArray(send.segment_ids_snapshot)) continue;
    for (const id of send.segment_ids_snapshot) {
      if (typeof id === 'string') strata.set(id, [...(strata.get(id) ?? []), send]);
    }
  }
  const counts = new Map<string, number>();
  for (const outcome of outcomes.values()) {
    const kind = String(outcome.class);
    counts.set(kind, (counts.get(kind) ?? 0) + 1);
  }
  return {
    as_of: asOf,
    event_cursor: eventCursor,
    effective_outcomes: Array.from(counts).sort(([a], [b]) => a.localeCompare(b))
      .map(([kind, count]) => ({ class: kind, count })),
    rates: [
      { metric: 'reply_rate', ...measure(firstTouches.filter(replied).length, firstTouches.length) },
      { metric: 'advance_rate', ...measure(advances.length, firstTouches.length) },
    ],
    strata: Array.from(strata).sort(([a], [b]) => a.localeCompare(b))
      .map(([segment_id, matches]) => ({ segment_id, reply_rate: measure(matches.filter(replied).length, matches.length) })),
    comparisons: [],
  };
}

export function weekPeriod(date: Date): string {
  const thursday = new Date(Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate()));
  thursday.setUTCDate(thursday.getUTCDate() + 3 - ((thursday.getUTCDay() + 6) % 7));
  const year = thursday.getUTCFullYear();
  const yearStart = Date.UTC(year, 0, 1);
  const week = Math.ceil(((thursday.getTime() - yearStart) / 86_400_000 + 1) / 7);
  return `${year}-W${String(week).padStart(2, '0')}`;
}

export function isWeekPeriod(value: unknown): value is string {
  if (typeof value !== 'string') return false;
  const match = /^(\d{4})-W(0[1-9]|[1-4][0-9]|5[0-3])$/.exec(value);
  return Boolean(match && weekPeriod(new Date(`${match[1]}-12-28T00:00:00Z`)) >= value);
}

export function validatePackDocument(document: string) {
  const lines = document.split(/\r?\n/);
  const findLine = (key: string) => lines.findIndex((line) => new RegExp(`^${key}:`).test(line));
  const errors: Array<{ line: number | null; field: string | null; message: string }> = [];
  for (const field of ['id', 'name', 'version', 'stages', 'ask_ladder', 'cadence', 'limits']) {
    if (findLine(field) < 0) errors.push({ line: null, field, message: `Required pack field “${field}” is missing.` });
  }
  const idLine = findLine('id');
  const versionLine = findLine('version');
  const id = idLine >= 0 ? /^\s*id:\s*([^\s#]+)/.exec(lines[idLine]!)?.[1] ?? null : null;
  const version = versionLine >= 0 ? /^\s*version:\s*([^\s#]+)/.exec(lines[versionLine]!)?.[1] ?? null : null;
  return { valid: errors.length === 0, pack_id: id, version, normalized_document: errors.length === 0 ? document.replace(/\r\n/g, '\n').trimEnd() + '\n' : null, errors: errors.map((error) => ({ ...error, line: error.line === null && error.field ? findLine(error.field) >= 0 ? findLine(error.field) + 1 : null : error.line })) };
}
