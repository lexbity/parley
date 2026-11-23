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
const undoIntent = ref<{ opportunityId: string; toStage: string; eventId: string; phaseRevision: number; fromLabel: string } | null>(null);
const undoBusy = ref(false);
const narrowInspector = ref(false);
let searchHandle: ReturnType<typeof globalThis.setTimeout> | undefined;
const createOpen = ref(false);
const createTitle = ref('');
const createPartyId = ref('');
const createBusy = ref(false);
const rowRefs = ref<Record<string, HTMLElement | null>>({});
const phaseSelectRefs = ref<Record<string, HTMLSelectElement | null>>({});

const phases = computed(() => sheet.value.phases);
const filteredPhaseOptions = computed(() => phases.value);
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

function actionDateLabel(row: QueueSheetRow): string {
  const date = row.next_action.date;
  if (!date) return '—';
  return date.slice(0, 10);
}

function isToday(date: string | null): boolean {
  if (!date) return false;
  const now = new Date();
  const local = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
  return date.slice(0, 10) === local;
}

async function loadSheet() {
  const current = ++generation.value;
  loading.value = true;
  loadError.value = '';
  try {
    const result = await parley.getQueueSheet({
      ...(query.value.trim() ? { search: query.value.trim() } : {}),
      ...(phaseFilter.value ? { phase: phaseFilter.value } : {}),
      sort: sortKey.value,
    });
    if (current !== generation.value) return;
    sheet.value = result;
    if (result.rows.length && !result.rows.some((row) => row.opportunity_id === selectedId.value)) {
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
    const result = await parley.setOpportunityPhase(row.opportunity_id, {
      target_stage_id: targetStage,
      pack_version: row.phase?.pack_version ?? '1.0.0',
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
    undoIntent.value = { opportunityId: result.row.opportunity_id, toStage: previous.phase?.stage_id ?? '', eventId: result.event_id, phaseRevision: result.phase_revision, fromLabel: nextLabel };
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
      undoIntent.value = { opportunityId: result.row.opportunity_id, toStage: previous.phase.stage_id, eventId: result.event_id, phaseRevision: result.phase_revision, fromLabel: nextLabel };
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
      pack_version: '1.0.0',
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

watch(query, scheduleSearch);
watch(phaseFilter, () => { void loadSheet(); });
watch(sortKey, () => { void loadSheet(); });
watch(() => props.routeId, (id) => { if (id) { void loadSheet().then(openRouted); } });

onMounted(async () => {
  try {
    await parley.connect();
    await loadSheet();
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
              @click="loadSheet"
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
                        v-for="phase in filteredPhaseOptions.filter((p) => p.kind === 'planning')"
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
                    v-for="phase in filteredPhaseOptions.filter((p) => p.kind === 'planning')"
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
