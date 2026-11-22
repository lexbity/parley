export type AdminRow = Record<string, unknown>;
export type AdminOperation = 'restore' | 'erase_party' | 'erase_workspace';
export type AdminBackupRecord = { id: string; created_at: string; schema_version: number; workspace_epoch: string; size_bytes: number; integrity: 'verified' | 'unknown'; snapshot: Record<string, unknown> };

function records<T>(value: Map<string, T> | T[]): T[] {
  return value instanceof Map ? [...value.values()] : value;
}

function count(value: Map<string, unknown> | unknown[]): number {
  return value instanceof Map ? value.size : value.length;
}

export function adminOverview(input: { asOf: string; epoch: string; cursor: string; backups: AdminBackupRecord[]; parties: AdminRow[] }) {
  const backups = input.backups.map((backup) => ({ id: backup.id, created_at: backup.created_at, schema_version: backup.schema_version, workspace_epoch: backup.workspace_epoch, size_bytes: backup.size_bytes, integrity: backup.integrity }));
  return {
    as_of: input.asOf,
    workspace_epoch: input.epoch,
    event_cursor: input.cursor,
    last_verified_backup: backups.find((backup) => backup.integrity === 'verified') ?? null,
    backups,
    parties: input.parties.map((party) => ({ id: String(party.id), kind: party.kind, name: String(party.name), descriptor: typeof party.descriptor === 'string' ? party.descriptor : null })),
  };
}

export function adminImpact(operation: AdminOperation, state: Record<string, unknown>, partyId?: string): { impact: Record<string, number>; preserved_shared: Record<string, number> } {
  const opportunities = records(state.opportunities as Map<string, AdminRow> | AdminRow[]);
  const relevantOpportunities = operation === 'erase_party'
    ? opportunities.filter((item) => Array.isArray(item.participant_ids) && item.participant_ids.includes(partyId))
    : opportunities;
  const owned = (key: string, field: string) => records(state[key] as Map<string, AdminRow> | AdminRow[]).filter((item) => relevantOpportunities.some((opportunity) => opportunity.id === item[field]));
  if (operation === 'erase_workspace') {
    return {
      impact: Object.fromEntries(Object.entries(state).filter(([, value]) => value instanceof Map || Array.isArray(value)).map(([key, value]) => [key, count(value as Map<string, unknown> | unknown[])])),
      preserved_shared: {},
    };
  }
  if (operation === 'restore') {
    const backup = state.restore_backup as Record<string, unknown> | undefined;
    const snapshot = backup?.snapshot as Record<string, unknown> | undefined;
    const collections = snapshot?.collections as Record<string, unknown> | undefined;
    return {
      impact: { ...Object.fromEntries(Object.entries(collections ?? {}).filter(([, value]) => value instanceof Map || Array.isArray(value)).map(([key, value]) => [key, count(value as Map<string, unknown> | unknown[])])), owner_settings: 1 },
      preserved_shared: {},
    };
  }
  const sends = owned('sends', 'opportunity_id');
  const threads = records(state.threads as Map<string, AdminRow> | AdminRow[]).filter((item) => relevantOpportunities.some((opportunity) => opportunity.id === item.opportunity_id) || item.recipient_id === partyId);
  const materials = records(state.materialRecords as Map<string, AdminRow> | AdminRow[]).filter((item) => relevantOpportunities.some((opportunity) => opportunity.id === item.opportunity_id) || item.recipient_id === partyId);
  const captures = records(state.captures as Map<string, AdminRow> | AdminRow[]);
  const linkedVariantIds = new Set(relevantOpportunities.flatMap((opportunity) => Array.isArray(opportunity.source_variant_ids) ? opportunity.source_variant_ids.map(String) : []));
  const opportunityIds = new Set(relevantOpportunities.map((opportunity) => String(opportunity.id)));
  const sharedVariantIds = new Set(opportunities.filter((opportunity) => !opportunityIds.has(String(opportunity.id))).flatMap((opportunity) => Array.isArray(opportunity.source_variant_ids) ? opportunity.source_variant_ids.map(String) : []).filter((id) => linkedVariantIds.has(id)));
  const relatedNotes = state.notesByOpportunity as Map<string, unknown>;
  const partyEdges = records(state.graphEdges as Map<string, AdminRow> | AdminRow[]).filter((edge) => edge.from_entity_id === partyId || edge.to_entity_id === partyId);
  return {
    impact: {
      parties: 1,
      party_edges: partyEdges.length,
      opportunities: relevantOpportunities.length,
      contact_threads: threads.length,
      sent_touches: sends.filter((send) => send.recipient_id === partyId || threads.some((thread) => thread.id === send.thread_id)).length,
      materials: materials.length,
      attachments: records(state.uploads as Map<string, AdminRow> | AdminRow[]).filter((item) => materials.some((material) => material.id === item.material_id) || item.recipient_id === partyId).length,
      related_notes: [...relatedNotes.keys()].filter((id) => opportunityIds.has(id)).length,
    },
    preserved_shared: { independent_sources: captures.filter((capture) => !linkedVariantIds.has(String(capture.variant_id))).length, linked_sources: [...linkedVariantIds].length, shared_sources: sharedVariantIds.size },
  };
}

export function adminPreview(input: {
  id: string; operation: AdminOperation; epoch: string; cursor: string; expiresAt: string; digest: string;
  impact: Record<string, number>; preservedShared: Record<string, number>; backup?: AdminBackupRecord | null; party?: AdminRow | null;
}) {
  const backup = input.backup ? { id: input.backup.id, created_at: input.backup.created_at, schema_version: input.backup.schema_version, workspace_epoch: input.backup.workspace_epoch, size_bytes: input.backup.size_bytes, integrity: input.backup.integrity } : null;
  return {
    id: input.id, operation: input.operation, digest: input.digest, workspace_epoch: input.epoch, event_cursor: input.cursor, expires_at: input.expiresAt,
    backup,
    party: input.party ? { id: String(input.party.id), name: String(input.party.name) } : null,
    impact: input.impact, preserved_shared: input.preservedShared,
  };
}
