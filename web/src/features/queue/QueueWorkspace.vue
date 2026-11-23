<script setup lang="ts">
/* global HTMLElement, HTMLSelectElement */
/**
 * Queue workspace (Parley-Queue implementation spec).
 *
 * The Queue is a filterable sheet of opportunities. Each row shows one
 * scenario with its pack phase, next action and action date; the main edit
 * is the phase selector. Selecting a row opens the read-only compact
 * inspector. There is no board, custom list, drag ordering or tag-in-row
 * editing here.
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import {
  parley,
  type GraphEntity,
  type Material,
  type QueueSheetResponse,
  type QueueSheetRow,
  type SheetPhase,
  type SetPhaseResult,
} from '../../api/client';
import Daily from './Daily.vue';
import Insights from './Insights.vue';

defineOptions({ name: 'QueueWorkspace' });

const props = withDefaults(defineProps<{ parties: GraphEntity[]; outputs: Material[]; routeId?: string | null; active?: boolean }>(), { routeId: null, active: true });
const emit = defineEmits<{ navigate: [path: string] }>();

type View = 'sheet' | 'daily' | 'insights';
const view = ref<View>('sheet');
const query = ref('');
const phaseFilter = ref('');
const sortKey = ref<'due' | 'name'>('due');
const sheet = ref<QueueSheetResponse>({ generation: '', total: 0, rows: [], phases: [], cursor: null, next_cursor: null });
const generation = ref(0);
const loadError = ref('');
const loading = ref(false);
const selectedId = ref('');
const saving = ref<Record<string, boolean>>({});
const status = ref('');
const error = ref('');
const undoIntent = ref<{ opportunityId: string; toStage: string; packVersion: string; eventId: string; phaseRevision: number; fromLabel: string } | null>(null);
const undoBusy = ref(false);
const narrowInspector = ref(false);
let searchHandle: ReturnType<typeof globalThis.setTimeout> | undefined;
const createOpen = ref(false);
const createTitle = ref('');
const createPartyId = ref('');
const createBusy = ref(false);
const rowRefs = ref<Record<string, HTMLElement | null>>({});
const phaseSelectRefs = ref<Record<string, HTMLSelectElement | null>>({});

// Entry restores the last valid search, filters, sort and selection for this
// workspace (spec §2), scoped to the workspace epoch like the party desk.
// The key needs the session epoch, so restore runs once connect() resolves.
const prefsKey = ref('');
const hydrated = ref(false);
function restorePrefs() {
  const key = parley.workspaceEpoch ? 'parley.queue.' + parley.workspaceEpoch : '';
  if (!key) return;
  prefsKey.value = key;
  hydrated.value = true;
  try {
    const saved = JSON.parse(globalThis.sessionStorage.getItem(key) ?? '{}');
    if (typeof saved.query === 'string') query.value = saved.query;
    if (typeof saved.phaseFilter === 'string') phaseFilter.value = saved.phaseFilter;
    if (saved.sortKey === 'due' || saved.sortKey === 'name') sortKey.value = saved.sortKey;
    if (typeof saved.selectedId === 'string') selectedId.value = saved.selectedId;
  } catch { /* Storage can be unavailable in a private browsing session. */ }
}
watch([query, phaseFilter, sortKey, selectedId], () => {
  if (!prefsKey.value) return;
  try {
    globalThis.sessionStorage.setItem(prefsKey.value, JSON.stringify({
      query: query.value, phaseFilter: phaseFilter.value, sortKey: sortKey.value, selectedId: selectedId.value,
    }));
  } catch { /* Keep in-memory navigation. */ }
});

const phases = computed(() => sheet.value.phases);
const filteredPhaseOptions = computed(() => phases.value);
/** Phase choices for the row and inspector selects: permitted planning
 * stages only, never the Unmapped pseudo-stage or terminal lifecycle. */
const editablePhases = computed(() => filteredPhaseOptions.value.filter((phase) => phase.kind === 'planning' && phase.stage_id !== '__unmapped__'));
const countLabel = computed(() => `${sheet.value.rows.length} of ${sheet.value.total} scenarios`);
const hasFilters = computed(() => query.value.trim().length > 0 || phaseFilter.value.length > 0 || sortKey.value !== 'due');
const selectedRow = computed(() => sheet.value.rows.find((row) => row.opportunity_id === selectedId.value) ?? null);
const selectedMaterials = computed(() => props.outputs.filter((material) => material.opportunity_id === selectedId.value));
const emptyWorkspace = computed(() => !loading.value && !loadError.value && sheet.value.total === 0 && !hasFilters.value);
const noMatches = computed(() => !loading.value && !loadError.value && sheet.value.total === 0 && hasFilters.value);

function stageLabel(phase: SheetPhase | null): string {
  if (!phase) return 'Unmapped';
  return phase.label;
}

/** Owner-timezone date label: em dash when missing, "Today", otherwise a
 * short month/day (with the year when it differs from the current one). */
function formatActionDate(date: string | null): string {
  if (!date) return '—';
  const parsed = new Date(date.length <= 10 ? date + 'T00:00:00' : date);
  if (Number.isNaN(parsed.getTime())) return date;
  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const target = new Date(parsed.getFullYear(), parsed.getMonth(), parsed.getDate());
  if (target.getTime() === today.getTime()) return 'Today';
  const month = parsed.toLocaleString('en', { month: 'short' });
  return parsed.getFullYear() === now.getFullYear()
    ? `${month} ${parsed.getDate()}`
    : `${month} ${parsed.getDate()}, ${parsed.getFullYear()}`;
}

function actionDateLabel(row: QueueSheetRow): string {
  return formatActionDate(row.next_action.date);
}

function isToday(date: string | null): boolean {
  if (!date) return false;
  const now = new Date();
  const local = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
  return date.slice(0, 10) === local;
}

async function loadSheet(cursor?: string) {
  const current = ++generation.value;
  loading.value = true;
  loadError.value = '';
  try {
    const result = await parley.getQueueSheet({
      ...(query.value.trim() ? { search: query.value.trim() } : {}),
      ...(phaseFilter.value ? { phase: phaseFilter.value } : {}),
      sort: sortKey.value,
      ...(cursor ? { cursor } : {}),
    });
    if (current !== generation.value) return;
    if (cursor) {
      const merged = [...sheet.value.rows];
      for (const row of result.rows) {
        if (!merged.some((item) => item.opportunity_id === row.opportunity_id)) merged.push(row);
      }
      sheet.value = { ...result, rows: merged };
    } else {
      sheet.value = result;
    }
    if (result.rows.length && !result.rows.some((row) => row.opportunity_id === selectedId.value) && !cursor) {
      selectedId.value = result.rows[0]!.opportunity_id;
    }
  } catch (reason) {
    if (current !== generation.value) return;
    loadError.value = reason instanceof Error ? reason.message : 'Could not load the Queue sheet.';
  } finally {
    if (current === generation.value) loading.value = false;
  }
}

function scheduleSearch() {
  if (searchHandle) globalThis.clearTimeout(searchHandle);
  searchHandle = globalThis.setTimeout(() => { void loadSheet(); }, 250);
}

function selectRow(id: string) {
  selectedId.value = id;
  narrowInspector.value = true;
}

function openRouted() {
  if (!props.routeId) return;
  const row = sheet.value.rows.find((item) => item.opportunity_id === props.routeId);
  if (!row) return;
  selectedId.value = row.opportunity_id;
}

async function changePhase(row: QueueSheetRow, targetStage: string) {
  if (saving.value[row.opportunity_id] || targetStage === row.phase?.stage_id) return;
  saving.value = { ...saving.value, [row.opportunity_id]: true };
  error.value = '';
  status.value = '';
  try {
    // An Unmapped row has no stored pack version; the sheet's planning
    // stages carry the owner pack's current version for the first mapping.
    const packVersion = row.phase?.pack_version
      ?? editablePhases.value[0]?.pack_version
      ?? '1.0.0';
    const result = await parley.setOpportunityPhase(row.opportunity_id, {
      target_stage_id: targetStage,
      pack_version: packVersion,
      expected_phase_revision: row.phase?.revision ?? 1,
    });
    applyPhaseResult(result, row);
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'The phase change was not acknowledged.';
    await loadSheet();
  } finally {
    saving.value = { ...saving.value, [row.opportunity_id]: false };
  }
}

function applyPhaseResult(result: SetPhaseResult, previous: QueueSheetRow) {
  const nextLabel = stageLabel(result.row.phase);
  const leavesFilter = phaseFilter.value.length > 0 && phaseFilter.value !== '__unmapped__' && result.row.phase?.stage_id !== phaseFilter.value;
  // Keep the filter and search; drop the row locally when it no longer matches.
  if (leavesFilter && phaseFilter.value !== '__unmapped__') {
    const index = sheet.value.rows.findIndex((item) => item.opportunity_id === result.row.opportunity_id);
    const nextRows = sheet.value.rows.filter((item) => item.opportunity_id !== result.row.opportunity_id);
    sheet.value = { ...sheet.value, rows: nextRows, total: Math.max(0, sheet.value.total - 1) };
    if (selectedId.value === result.row.opportunity_id) {
      const replacement = nextRows[index] ?? nextRows[index - 1] ?? null;
      selectedId.value = replacement?.opportunity_id ?? '';
    }
    status.value = `${previous.title} moved to ${nextLabel} and is hidden by the ${phaseLabelFor(phaseFilter.value)} filter.`;
    undoIntent.value = { opportunityId: result.row.opportunity_id, toStage: previous.phase?.stage_id ?? '', packVersion: result.row.phase?.pack_version ?? previous.phase?.pack_version ?? '1.0.0', eventId: result.event_id, phaseRevision: result.phase_revision, fromLabel: nextLabel };
    if (selectedId.value) {
      void nextTick(() => {
        const row = sheet.value.rows.find((item) => item.opportunity_id === selectedId.value);
        if (row) rowRefs.value[row.opportunity_id]?.focus();
        else phaseSelectRefs.value[selectedId.value]?.focus();
      });
    }
  } else {
    sheet.value = {
      ...sheet.value,
      rows: sheet.value.rows.map((item) => item.opportunity_id === result.row.opportunity_id ? result.row : item),
    };
    if (selectedId.value === result.row.opportunity_id) {
      selectedId.value = result.row.opportunity_id;
      void nextTick(() => {
        phaseSelectRefs.value[selectedId.value]?.focus();
      });
    }
    status.value = `${result.row.title} moved to ${nextLabel}.`;
    if (previous.phase?.stage_id !== result.row.phase?.stage_id && previous.phase) {
      undoIntent.value = { opportunityId: result.row.opportunity_id, toStage: previous.phase.stage_id, packVersion: previous.phase.pack_version, eventId: result.event_id, phaseRevision: result.phase_revision, fromLabel: nextLabel };
    }
  }
  // Resync quietly so counts/generation reflect the committed event.
  void loadSheet().catch(() => undefined);
}

function phaseLabelFor(stageId: string): string {
  const phase = phases.value.find((item) => item.stage_id === stageId);
  return phase?.label ?? stageId.replaceAll('_', ' ');
}

async function undoPhaseChange() {
  const intent = undoIntent.value;
  if (!intent || undoBusy.value) return;
  undoBusy.value = true;
  error.value = '';
  try {
    const result = await parley.setOpportunityPhase(intent.opportunityId, {
      target_stage_id: intent.toStage,
      pack_version: intent.packVersion,
      expected_phase_revision: intent.phaseRevision,
      original_event_id: intent.eventId,
    });
    undoIntent.value = null;
    status.value = `${result.row.title} returned to ${stageLabel(result.row.phase)}.`;
    const reAdded = phaseFilter.value.length === 0 || phaseFilter.value === result.row.phase?.stage_id || (phaseFilter.value === '__unmapped__' && !result.row.phase);
    if (!sheet.value.rows.some((item) => item.opportunity_id === result.row.opportunity_id) && reAdded) {
      sheet.value = { ...sheet.value, rows: [...sheet.value.rows, result.row], total: sheet.value.total + 1 };
    } else {
      sheet.value = { ...sheet.value, rows: sheet.value.rows.map((item) => item.opportunity_id === result.row.opportunity_id ? result.row : item) };
    }
    if (reAdded) selectedId.value = result.row.opportunity_id;
    void loadSheet().catch(() => undefined);
  } catch (reason) {
    status.value = '';
    error.value = reason instanceof Error ? reason.message : 'Undo was not acknowledged. The current phase is shown.';
    await loadSheet();
  } finally {
    undoBusy.value = false;
  }
}

function clearFilters() {
  query.value = '';
  phaseFilter.value = '';
  sortKey.value = 'due';
  void loadSheet();
}

function loadMore() {
  void loadSheet(sheet.value.next_cursor ?? undefined);
}

// --- Read-only scenario context (spec §2 inspector): proposed exchange,
// source passages with speaker and tier, participant follow-ups, history. ---

type DetailExchange = { participantId: string; participantName: string; theyGet: string; iGet: string };
type DetailSource = { id: string; participantName: string; statement: string; knowledgeKind: string; claim: { wording: string; tier: number; speakerKind: string } | null };
type DetailHistory = { cursor: string; when: string; label: string };
type DetailThread = { recipientId: string; state: string; nextActionAt: string | null };
const detail = ref<{ exchanges: DetailExchange[]; sources: DetailSource[]; history: DetailHistory[]; threads: DetailThread[] } | null>(null);
const detailLoading = ref(false);
let detailVersion = 0;

function eventSummary(payload: Record<string, unknown>, eventType: string): string {
  const text = (key: string) => (typeof payload[key] === 'string' ? (payload[key] as string) : null);
  switch (eventType) {
    case 'opportunity.created': return 'Opportunity created';
    case 'opportunity.participant_added': return `Participant added${text('role') ? ` · ${text('role')}` : ''}`;
    case 'opportunity.phase_changed': return `Phase ${text('from_stage_id') ? phaseLabelFor(text('from_stage_id')!) : '—'} → ${text('to_stage_id') ? phaseLabelFor(text('to_stage_id')!) : ''}${payload.operation === 'undo' ? ' (undo)' : ''}`;
    case 'opportunity.exchange_version_created': return 'Proposed exchange recorded';
    case 'opportunity.exchange_attested': return 'Exchange attested';
    case 'opportunity.interest_added': return 'Interest recorded';
    case 'opportunity.source_linked': return 'Source linked';
    case 'opportunity.source_unlinked': return 'Source unlinked';
    case 'opportunity.source_claim_corrected': return 'Source claim corrected';
    case 'opportunity.notes_updated': return 'Notes updated';
    case 'opportunity.task_toggled': return 'Task updated';
    case 'opportunity.tags_changed': return 'Tags updated';
    case 'opportunity.resolved': return `Resolved${text('result') ? ` · ${text('result')}` : ''}`;
    case 'opportunity.closed': return `Closed${text('reason') ? ` · ${text('reason')}` : ''}`;
    case 'opportunity.reopened': return 'Reopened';
    default: return eventType.replaceAll('_', ' ');
  }
}

async function loadDetail(opportunityId: string) {
  const own = ++detailVersion;
  detail.value = null;
  const row = sheet.value.rows.find((item) => item.opportunity_id === opportunityId);
  if (!row) return;
  detailLoading.value = true;
  try {
    const participantResults = await Promise.allSettled(row.participants.map(async (participant) => {
      // A recipient without a recorded exchange simply contributes no block.
      const [exchange, interests] = await Promise.all([
        parley.getWritingExchange(opportunityId, participant.id).catch(() => null),
        parley.getWritingInterests(opportunityId, participant.id).catch(() => []),
      ]);
      return { participant, exchange, interests };
    }));
    // History pages the focused event feed; 10 pages is a generous bound.
    const historyItems: Array<{ cursor: string; event_type: string; occurred_at: string; payload: Record<string, unknown> }> = [];
    let cursor = '0';
    for (let page = 0; page < 10; page += 1) {
      const result = await parley.getEvents(cursor, { entity_type: 'opportunity', entity_id: opportunityId });
      historyItems.push(...result.items);
      if (!result.items.length || result.next_cursor === cursor) break;
      cursor = result.next_cursor;
    }
    const threads = await parley.listThreads({ opportunity_id: opportunityId }).catch(() => []);
    if (own !== detailVersion) return;
    const exchanges: DetailExchange[] = [];
    const sources: DetailSource[] = [];
    for (const result of participantResults) {
      if (result.status !== 'fulfilled') continue;
      const { participant, exchange, interests } = result.value;
      const current = (exchange?.current ?? null) as Record<string, unknown> | null;
      if (current && typeof current.they_get === 'string' && typeof current.i_get === 'string') {
        exchanges.push({ participantId: participant.id, participantName: participant.name, theyGet: current.they_get, iGet: current.i_get });
      }
      for (const interest of interests) {
        sources.push({
          id: interest.id, participantName: participant.name, statement: interest.statement,
          knowledgeKind: interest.knowledge_kind,
          claim: interest.claim ? { wording: interest.claim.wording, tier: interest.claim.tier, speakerKind: interest.claim.speaker_kind } : null,
        });
      }
    }
    detail.value = {
      exchanges,
      sources,
      history: historyItems.map((event) => ({ cursor: event.cursor, when: formatActionDate(event.occurred_at), label: eventSummary(event.payload ?? {}, event.event_type) })),
      threads: threads.map((thread) => ({ recipientId: thread.recipient_id, state: thread.state, nextActionAt: thread.next_action_at ?? null })),
    };
  } catch {
    if (own === detailVersion) detail.value = null;
  } finally {
    if (own === detailVersion) detailLoading.value = false;
  }
}

function threadFor(participantId: string): DetailThread | null {
  return detail.value?.threads.find((thread) => thread.recipientId === participantId) ?? null;
}

watch(selectedId, (id) => {
  if (id) void loadDetail(id);
  else { detailVersion++; detail.value = null; }
}, { immediate: true });

function openInParties(partyId: string) {
  emit('navigate', `/parties/${encodeURIComponent(partyId)}`);
}

function openInOutput(materialId: string) {
  emit('navigate', `/output/${encodeURIComponent(materialId)}`);
}

async function createOpportunity() {
  if (createBusy.value || !createTitle.value.trim() || !createPartyId.value) return;
  createBusy.value = true;
  error.value = '';
  try {
    await parley.createOpportunity(createTitle.value.trim(), createPartyId.value);
    createTitle.value = '';
    createOpen.value = false;
    status.value = 'Opportunity created. It is Unmapped until you choose a phase.';
    await loadSheet();
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'Could not create the opportunity.';
  } finally {
    createBusy.value = false;
  }
}

watch(query, () => { if (hydrated.value) return; scheduleSearch(); });
watch(phaseFilter, () => { if (hydrated.value) return; void loadSheet(); });
watch(sortKey, () => { if (hydrated.value) return; void loadSheet(); });
watch(() => props.routeId, (id) => { if (id) { void loadSheet().then(openRouted); } });

onMounted(async () => {
  try {
    await parley.connect();
    restorePrefs();
    await loadSheet();
    hydrated.value = false;
    openRouted();
  } catch (reason) {
    loadError.value = reason instanceof Error ? reason.message : 'Could not load the Queue sheet.';
  }
});

onBeforeUnmount(() => {
  if (searchHandle) globalThis.clearTimeout(searchHandle);
});
</script>

<template>
  <section aria-labelledby="queue-heading">
    <h2
      id="queue-heading"
      class="sr-only"
    >
      Opportunities
    </h2>
    <nav
      aria-label="Queue views"
      class="queue-views"
    >
      <button
        type="button"
        :aria-pressed="view === 'sheet'"
        @click="view = 'sheet'"
      >
        All scenarios
      </button>
      <button
        type="button"
        :aria-pressed="view === 'daily'"
        @click="view = 'daily'"
      >
        Daily work and insights
      </button>
      <button
        type="button"
        :aria-pressed="view === 'insights'"
        @click="view = 'insights'"
      >
        Insights
      </button>
      <button
        type="button"
        :aria-expanded="createOpen"
        @click="createOpen = !createOpen"
      >
        New opportunity
      </button>
    </nav>

    <form
      v-show="createOpen"
      class="create-form"
      @submit.prevent="createOpportunity"
    >
      <label>Opportunity title<input
        v-model="createTitle"
        type="text"
        required
        maxlength="500"
      ></label>
      <label>Participant
        <select
          v-model="createPartyId"
          required
        >
          <option
            value=""
            disabled
          >Choose a lead</option>
          <option
            v-for="party in parties"
            :key="party.id"
            :value="party.id"
          >{{ party.name }} · {{ party.kind }}</option>
        </select>
      </label>
      <button
        type="submit"
        :disabled="createBusy || !parties.length"
      >
        Create opportunity
      </button>
      <p class="hint">
        A lead alone does not create an opportunity or outreach.
      </p>
    </form>

    <p
      v-if="status"
      role="status"
      class="announce"
    >
      {{ status }}
    </p>
    <p
      v-if="error"
      role="alert"
      class="announce"
    >
      {{ error }}
    </p>
    <div
      v-if="undoIntent"
      class="undo-toast"
      role="status"
    >
      <span>Phase change recorded.</span>
      <button
        type="button"
        class="undo"
        :disabled="undoBusy"
        @click="undoPhaseChange"
      >
        Undo phase change
      </button>
    </div>

    <div
      v-if="view === 'sheet'"
      class="queue-filter-sort"
    >
      <label>Find scenario
        <input
          v-model="query"
          type="search"
          aria-label="Find scenario"
          placeholder="Scenario, party or contact"
        >
      </label>
      <label>Phase
        <select
          v-model="phaseFilter"
          aria-label="Phase filter"
        >
          <option value="">All phases</option>
          <option
            v-for="phase in filteredPhaseOptions"
            :key="phase.stage_id"
            :value="phase.stage_id"
          >
            {{ phase.label }} ({{ phase.count }})
          </option>
        </select>
      </label>
      <label>Order
        <select
          v-model="sortKey"
          aria-label="Order"
        >
          <option value="due">Next action date</option>
          <option value="name">Scenario name</option>
        </select>
      </label>
      <span
        class="count"
        aria-live="polite"
      >{{ countLabel }}</span>
    </div>

    <div
      v-show="view === 'sheet'"
      class="sheet-layout"
    >
      <section
        class="panel sheet-panel"
        aria-label="Scenario sheet"
      >
        <template v-if="loading && !sheet.rows.length">
          <p
            class="meta"
            role="status"
          >
            Loading scenarios…
          </p>
        </template>

        <template v-else-if="loadError">
          <div
            id="empty"
            class="empty-state"
          >
            <h3>Could not load scenarios</h3>
            <p>The Queue read failed. Your filters are preserved.</p>
            <button
              type="button"
              @click="() => loadSheet()"
            >
              Retry
            </button>
          </div>
        </template>

        <template v-else-if="emptyWorkspace">
          <div
            id="empty"
            class="empty-state"
          >
            <h3>No scenarios yet</h3>
            <p>Create an opportunity, or return to Parties to start one.</p>
            <button
              type="button"
              @click="emit('navigate', '/parties')"
            >
              Return to Parties
            </button>
          </div>
        </template>

        <template v-else-if="noMatches">
          <div
            id="empty"
            class="empty-state"
          >
            <h3>No matching scenarios</h3>
            <p>Change the phase filter or search to see more.</p>
            <button
              type="button"
              @click="clearFilters"
            >
              Clear filters
            </button>
          </div>
        </template>

        <template v-else>
          <div class="scroll">
            <table>
              <thead>
                <tr>
                  <th scope="col">
                    Scenario / party
                  </th>
                  <th scope="col">
                    Phase
                  </th>
                  <th scope="col">
                    Next action
                  </th>
                  <th scope="col">
                    Action date
                  </th>
                </tr>
              </thead>
              <tbody>
                <tr
                  v-for="row in sheet.rows"
                  :key="row.opportunity_id"
                  :class="{ active: row.opportunity_id === selectedId }"
                  :data-opportunity-id="row.opportunity_id"
                >
                  <td>
                    <button
                      :ref="(el) => { rowRefs[row.opportunity_id] = el as HTMLElement | null; }"
                      type="button"
                      class="open"
                      :data-open="row.opportunity_id"
                      :aria-pressed="row.opportunity_id === selectedId"
                      @click="selectRow(row.opportunity_id)"
                    >
                      {{ row.title }}
                    </button>
                    <div class="small">
                      {{ row.participants.map((p) => p.name).join(', ') }}
                    </div>
                  </td>
                  <td>
                    <select
                      :ref="(el) => { phaseSelectRefs[row.opportunity_id] = el as HTMLSelectElement | null; }"
                      class="phase"
                      :aria-label="`Phase for ${row.title}`"
                      :value="row.phase?.stage_id ?? ''"
                      :disabled="saving[row.opportunity_id]"
                      @change="changePhase(row, ($event.target as HTMLSelectElement).value)"
                    >
                      <option
                        value=""
                        disabled
                      >
                        Unmapped
                      </option>
                      <option
                        v-for="phase in editablePhases"
                        :key="phase.stage_id"
                        :value="phase.stage_id"
                      >
                        {{ phase.label }}
                      </option>
                    </select>
                    <span
                      v-if="saving[row.opportunity_id]"
                      class="saving"
                      role="status"
                    >Saving…</span>
                  </td>
                  <td class="action">
                    <strong>{{ row.next_action.label }}</strong>
                    <div class="hint">
                      {{ row.next_action.reason }}
                    </div>
                  </td>
                  <td
                    class="date"
                    :class="{ due: isToday(row.next_action.date) }"
                  >
                    {{ actionDateLabel(row) }}
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          <div class="meta">
            {{ countLabel }}
            <button
              v-if="sheet.next_cursor && !loading"
              type="button"
              @click="loadMore"
            >
              Load more
            </button>
          </div>
        </template>
      </section>

      <aside
        class="panel inspector"
        aria-label="Scenario details"
        :class="{ 'inspector-open': narrowInspector }"
      >
        <template v-if="selectedRow">
          <div class="top">
            <small>{{ selectedRow.participants.map((p) => p.name).join(', ').toUpperCase() }} / SCENARIO</small>
            <h3>{{ selectedRow.title }}</h3>
            <button
              type="button"
              class="back"
              @click="narrowInspector = false"
            >
              Back to list
            </button>
          </div>
          <div class="body">
            <div class="block">
              <label>Scenario phase
                <select
                  :value="selectedRow.phase?.stage_id ?? ''"
                  :disabled="saving[selectedRow.opportunity_id]"
                  :aria-label="`Inspector phase for ${selectedRow.title}`"
                  @change="changePhase(selectedRow, ($event.target as HTMLSelectElement).value)"
                >
                  <option
                    value=""
                    disabled
                  >Unmapped</option>
                  <option
                    v-for="phase in editablePhases"
                    :key="phase.stage_id"
                    :value="phase.stage_id"
                  >
                    {{ phase.label }}
                  </option>
                </select>
              </label>
              <small>NEXT ACTION</small>
              <h4>{{ selectedRow.next_action.label }}</h4>
              <p class="hint">
                {{ selectedRow.next_action.reason }}
              </p>
              <p
                v-if="selectedRow.next_action.date"
                class="hint"
              >
                Due {{ formatActionDate(selectedRow.next_action.date) }}
              </p>
              <p
                v-if="selectedRow.other_action_count"
                class="hint"
              >
                {{ selectedRow.other_action_count }} other action(s) available.
              </p>
              <p
                v-if="selectedRow.lifecycle !== 'open'"
                class="hint"
              >
                Lifecycle: {{ selectedRow.lifecycle }}<template v-if="selectedRow.close_reason">
                  — {{ selectedRow.close_reason }}
                </template>.
              </p>
            </div>
            <div
              v-if="detailLoading || detail?.exchanges.length"
              class="block"
            >
              <small>PROPOSED EXCHANGE</small>
              <p
                v-if="detailLoading"
                class="hint"
              >
                Loading scenario context…
              </p>
              <template v-else>
                <div
                  v-for="entry in detail!.exchanges"
                  :key="entry.participantId"
                  class="exchange"
                >
                  <p>
                    <strong>They get</strong> · {{ entry.theyGet }}
                  </p>
                  <p>
                    <strong>I hope to get</strong> · {{ entry.iGet }}
                  </p>
                </div>
              </template>
            </div>
            <div
              v-if="detail?.sources.length"
              class="block"
            >
              <small>SOURCE PASSAGES</small>
              <details
                v-for="passage in detail!.sources"
                :key="passage.id"
                class="source-passage"
              >
                <summary>{{ passage.statement }}</summary>
                <blockquote
                  v-if="passage.claim"
                  class="passage-quote"
                >
                  {{ passage.claim.wording }}
                </blockquote>
                <small
                  v-if="passage.claim"
                  class="hint"
                >
                  {{ passage.claim.speakerKind }} · T{{ passage.claim.tier }} · {{ passage.participantName }}
                </small>
                <small
                  v-else
                  class="hint"
                >
                  Owner-entered · {{ passage.participantName }}
                </small>
              </details>
            </div>
            <div class="block">
              <small>PARTICIPANTS</small>
              <ul class="participants">
                <li
                  v-for="party in selectedRow.participants"
                  :key="party.id"
                >
                  <button
                    type="button"
                    class="link"
                    @click="openInParties(party.id)"
                  >
                    Open {{ party.name }} in Parties
                  </button>
                  <span class="hint"> · {{ party.role }}</span>
                  <span
                    v-if="threadFor(party.id)"
                    class="hint"
                  > · thread {{ threadFor(party.id)!.state }}<template v-if="threadFor(party.id)!.nextActionAt">
                    · next {{ formatActionDate(threadFor(party.id)!.nextActionAt) }}
                  </template></span>
                </li>
              </ul>
            </div>
            <div
              v-if="selectedMaterials.length"
              class="block"
            >
              <small>LINKED MATERIAL</small>
              <ul class="participants">
                <li
                  v-for="material in selectedMaterials"
                  :key="material.id"
                >
                  <button
                    type="button"
                    class="link"
                    @click="openInOutput(material.id)"
                  >
                    Review {{ material.kind }} v{{ material.version }} in Output
                  </button>
                  <span class="hint"> · {{ material.status }}</span>
                </li>
              </ul>
            </div>
            <details
              v-if="detail?.history.length"
              class="block history"
            >
              <summary class="hint">
                SCENARIO HISTORY ({{ detail!.history.length }})
              </summary>
              <ol class="history-list">
                <li
                  v-for="entry in detail!.history"
                  :key="entry.cursor"
                >
                  <span class="hint">{{ entry.when }}</span> · {{ entry.label }}
                </li>
              </ol>
            </details>
            <div class="block">
              <small>QUEUE RULES</small>
              <p class="hint">
                A phase edit never creates a send, reply, benefit or ladder advance. Lifecycle, threads and next actions stay separate.
              </p>
            </div>
          </div>
        </template>
        <template v-else>
          <div class="body">
            <h3>Scenario details</h3>
            <p>Select a matching scenario to inspect its next action.</p>
          </div>
        </template>
      </aside>
    </div>

    <div v-show="view === 'daily'">
      <Daily @navigate="emit('navigate', $event)" />
    </div>
    <div v-show="view === 'insights'">
      <Insights @navigate="emit('navigate', $event)" />
    </div>
  </section>
</template>

<style scoped>
.queue-views { display: flex; flex-wrap: wrap; gap: .5rem; margin-block: .75rem; }
.create-form { display: flex; flex-wrap: wrap; gap: .75rem; align-items: end; border: 1px solid #cbd1c6; border-radius: 8px; background: #fcfbf6; padding: .8rem 1rem; margin-block: .5rem 1rem; }
.create-form label { display: flex; flex-direction: column; gap: .25rem; font-size: 13px; }
.queue-filter-sort { display: flex; flex-wrap: wrap; gap: .75rem; align-items: end; margin-block: .75rem; }
.queue-filter-sort input, .queue-filter-sort select { min-width: 11rem; }
.count { color: #56675d; font: 11px/1.5 monospace; letter-spacing: .045em; }
.announce { min-height: 1.25rem; }
.undo-toast { display: flex; align-items: center; gap: .75rem; border: 1px solid #cbd1c6; border-left: 3px solid #225744; background: #eff3e9; border-radius: 6px; padding: .5rem .9rem; margin-block: .5rem; font-size: 13px; color: #21372e; }
.undo-toast .undo { margin-left: auto; }
.sheet-layout { display: grid; grid-template-columns: minmax(0, 2.2fr) minmax(280px, 1fr); gap: 1rem; align-items: start; }
.panel { border: 1px solid #cbd1c6; border-radius: 8px; background: #fcfbf6; overflow: hidden; }
.scroll { overflow-x: auto; }
table { width: 100%; border-collapse: collapse; text-align: left; }
td, th { border-top: 1px solid #d7dcd1; padding: .7rem .6rem; vertical-align: top; }
td:first-child, th:first-child { padding-left: 1rem; }
th { background: #f0f2e9; color: #445b4d; font: 11px/1.5 monospace; letter-spacing: .045em; }
tr.active { background: #e8efe6; }
tr.active td:first-child { box-shadow: inset 3px 0 #225744; }
.open { background: transparent; color: inherit; border: 0; padding: 0; text-align: left; font: 600 15px/1.35 system-ui; cursor: pointer; }
.open:hover { text-decoration: underline; }
.small { font-size: 12px; color: #56675d; margin-top: .2rem; }
.phase { font-size: 13px; min-width: 104px; max-width: 130px; }
.date { white-space: nowrap; font-size: 13px; }
.due { color: #804824; font-weight: 600; }
td.action { min-width: 150px; font-size: 14px; }
.hint { color: #56675d; font-size: 13px; margin: .2rem 0 0; }
.saving { margin-left: .4rem; font-size: 12px; color: #225744; }
.meta { border-top: 1px solid #cbd1c6; padding: .55rem 1rem; color: #56675d; font-size: 12px; display: flex; gap: 1rem; align-items: center; }
.undo { margin-left: auto; }
.top { border-bottom: 1px solid #cbd1c6; padding: 1rem; background: #eff3e9; }
.top small { display: block; margin-bottom: .4rem; font: 11px/1.5 monospace; letter-spacing: .045em; color: #56675d; }
.top h3 { margin: .2rem 0; }
.top .back { float: right; }
.body { padding: 1rem; }
.block { border-top: 1px solid #cbd1c6; padding: .85rem 0; }
.body > .block:first-child { border-top: 0; padding-top: 0; }
.block small { font: 11px/1.5 monospace; letter-spacing: .045em; color: #56675d; }
.block h4 { margin: .3rem 0; }
.block label { display: block; }
.block label select { display: block; margin: .3rem 0 .8rem; }
.participants { list-style: none; padding: 0; margin: .4rem 0 0; }
.participants li { margin: .3rem 0; }
.link { background: transparent; border: 0; padding: 0; color: #225744; font: 600 13px system-ui; cursor: pointer; text-align: left; }
.link:hover { text-decoration: underline; }
.empty-state { padding: 2rem; }
.empty-state h3 { margin-top: 0; }
.exchange p { margin: .15rem 0; }
.source-passage { padding: .3rem 0; }
.source-passage summary { cursor: pointer; font-size: 13px; }
.passage-quote { margin: .3rem 0; padding: .35rem .6rem; border-left: 3px solid #225744; background: #eff3e9; font-size: 13px; }
.history summary { cursor: pointer; font: 11px/1.5 monospace; letter-spacing: .045em; color: #56675d; }
.history-list { margin: .4rem 0 0; padding-left: 1.1rem; font-size: 13px; }
.history-list li { margin: .2rem 0; }
@media (max-width: 1020px) {
  .sheet-layout { grid-template-columns: 1fr; }
  .inspector { display: none; }
  .inspector.inspector-open { display: block; }
}
@media (max-width: 640px) {
  .scroll { max-width: 100%; }
  table { min-width: 590px; }
  .queue-filter-sort label { flex: 1 1 10rem; }
}
</style>
