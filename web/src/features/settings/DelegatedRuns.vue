<script setup lang="ts">
/* global Event, CustomEvent, TextEncoder */
/**
 * U02 owner consent surface. The shell owns the session transport: this view
 * only reads authoritative DTOs through the shared clients and never keeps a
 * run in browser storage. Every start and cancellation carries its own
 * operation key, and a generation/epoch change stops polling immediately.
 */
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { newOperationKey, parley, type GraphEntity, type OpportunitySummary } from '../../api/client';
import {
  RUN_PERMISSIONS, RUN_STATES,
  CommandApiError,
  commands,
  type RunListItem, type RunPermission, type RunState, type RunStatus, type RunTargetKind, type TranscriptTurn,
} from '../../api/commands';

const MAX_TASK_BYTES = 8192;
const MAX_TURNS = 16;
const TRANSCRIPT_PAGE_SIZE = 2;
const POLL_MS = 1000;
const ACTIVE_STATES: RunState[] = ['queued', 'running', 'waiting_owner', 'waiting_job'];
const permissionHelp: Record<RunPermission, string> = {
  'party.read': 'read the granted party summary only',
  'opportunity.read': 'read the granted opportunity summary only',
  'opportunity.phase.edit': 'change the phase of a granted opportunity',
};

const task = ref('');
const targetQuery = ref('');
const selectedTargets = ref<string[]>([]);
const permissions = ref<RunPermission[]>(['party.read', 'opportunity.read']);
const acknowledged = ref(false);
const parties = ref<GraphEntity[]>([]);
const opportunities = ref<OpportunitySummary[]>([]);
const loadingTargets = ref(true);
const targetError = ref('');
const runsLoading = ref(true);
const runsError = ref('');
const runs = ref<RunListItem[]>([]);
const statusFilter = ref<RunState | ''>('');
const runStatuses = ref<Record<string, RunStatus>>({});
const selectedRunId = ref<string | null>(null);
const transcriptItems = ref<TranscriptTurn[]>([]);
const transcriptCursors = ref<Array<string | null>>([null]);
const pageIndex = ref(0);
const transcriptNext = ref<string | null>(null);
const transcriptLoading = ref(false);
const transcriptError = ref('');
const status = ref('');
const actionError = ref('');
const busy = ref(false);
const stale = ref(false);
const staleReason = ref('');
const observedEpoch = ref('');
let pollTimer: ReturnType<typeof globalThis.setInterval> | undefined;
let pollInFlight = false;

type TargetOption = { key: string; label: string; kind: RunTargetKind; id: string };

const allTargets = computed<TargetOption[]>(() => [
  ...parties.value.map((party): TargetOption => ({ key: `party:${party.id}`, label: `Party: ${party.name}`, kind: 'party', id: party.id })),
  ...opportunities.value.map((opportunity): TargetOption => ({ key: `opportunity:${opportunity.id}`, label: `Opportunity: ${opportunity.title}`, kind: 'opportunity', id: opportunity.id })),
]);
const targetOptions = computed(() => {
  const needle = targetQuery.value.trim().toLocaleLowerCase();
  return needle ? allTargets.value.filter((target) => target.label.toLocaleLowerCase().includes(needle)) : allTargets.value;
});
const taskBytes = computed(() => new TextEncoder().encode(task.value).length);
const canStart = computed(() => !stale.value && !busy.value && !!task.value.trim()
  && taskBytes.value <= MAX_TASK_BYTES && selectedTargets.value.length > 0
  && permissions.value.length > 0 && acknowledged.value);
const hasActiveRuns = computed(() => runs.value.some((row) => ACTIVE_STATES.includes(row.state)));
const visibleRuns = computed(() => statusFilter.value ? runs.value.filter((row) => row.state === statusFilter.value) : runs.value);
function message(reason: unknown, fallback: string): string {
  return reason instanceof Error ? reason.message : fallback;
}

function sessionInvalid(reason: unknown): boolean {
  return reason instanceof CommandApiError && (reason.status === 401 || reason.status === 403);
}

function stopPolling() {
  if (pollTimer) globalThis.clearInterval(pollTimer);
  pollTimer = undefined;
}

function markStale(reason: string) {
  if (stale.value) return;
  stale.value = true;
  staleReason.value = reason;
  status.value = '';
  stopPolling();
}

function checkEpoch(): boolean {
  const live = parley.workspaceEpoch ?? '';
  if (observedEpoch.value && live !== observedEpoch.value) {
    markStale('The workspace epoch changed. Delegated runs are stale until this view is reopened with the current session.');
    return false;
  }
  return !stale.value;
}

function applyRuns(items: RunListItem[]) {
  runs.value = items;
  const ids = new Set(items.map((item) => item.run_id));
  for (const id of Object.keys(runStatuses.value)) if (!ids.has(id)) delete runStatuses.value[id];
  if (selectedRunId.value && !ids.has(selectedRunId.value)) selectedRunId.value = null;
}

function handleRunError(reason: unknown) {
  if (sessionInvalid(reason)) markStale('The owner session expired. Reopen delegated runs with the current session.');
  else runsError.value = message(reason, 'Could not read delegated runs.');
}

async function load() {
  loadingTargets.value = true;
  runsLoading.value = true;
  targetError.value = '';
  runsError.value = '';
  try {
    await parley.connect();
    observedEpoch.value = parley.workspaceEpoch ?? '';
  } catch (reason) {
    loadingTargets.value = false;
    runsLoading.value = false;
    markStale(message(reason, 'The workspace session is unavailable.'));
    return;
  }
  const [partyResult, opportunityResult, runResult] = await Promise.allSettled([
    parley.listParties(), parley.listOpportunities(), commands.listRuns({ limit: 20 }),
  ]);
  if (partyResult.status === 'fulfilled') parties.value = partyResult.value;
  else targetError.value = message(partyResult.reason, 'Could not load parties.');
  if (opportunityResult.status === 'fulfilled') opportunities.value = opportunityResult.value;
  else targetError.value = targetError.value || message(opportunityResult.reason, 'Could not load opportunities.');
  if (runResult.status === 'fulfilled') applyRuns(runResult.value.items);
  else handleRunError(runResult.reason);
  loadingTargets.value = false;
  runsLoading.value = false;
  if (stale.value) stopPolling();
}

async function refreshStatus(runId: string) {
  try {
    runStatuses.value = { ...runStatuses.value, [runId]: await commands.getRun(runId) };
  } catch (reason) {
    handleRunError(reason);
  }
}

async function loadTranscriptPage(index: number) {
  const runId = selectedRunId.value;
  if (!runId || stale.value) return;
  transcriptLoading.value = true;
  transcriptError.value = '';
  try {
    const cursor = transcriptCursors.value[index] ?? undefined;
    const page = await commands.getRunTranscript(runId, { limit: TRANSCRIPT_PAGE_SIZE, ...(cursor ? { cursor } : {}) });
    transcriptItems.value = page.items;
    transcriptNext.value = page.next_cursor;
  } catch (reason) {
    if (sessionInvalid(reason)) markStale('The owner session expired. Reopen delegated runs with the current session.');
    else transcriptError.value = message(reason, 'Could not read this transcript.');
  } finally {
    transcriptLoading.value = false;
  }
}

async function poll() {
  if (stale.value || pollInFlight || !checkEpoch()) return;
  if (!hasActiveRuns.value && !selectedRunId.value) return;
  pollInFlight = true;
  try {
    const list = await commands.listRuns({ limit: 20 });
    applyRuns(list.items);
    for (const row of runs.value.filter((item) => ACTIVE_STATES.includes(item.state))) await refreshStatus(row.run_id);
    if (selectedRunId.value) await loadTranscriptPage(pageIndex.value);
    checkEpoch();
  } catch (reason) {
    handleRunError(reason);
  } finally {
    pollInFlight = false;
  }
}

function startPolling() {
  if (!pollTimer && !stale.value) pollTimer = globalThis.setInterval(() => { void poll(); }, POLL_MS);
}

async function openRun(runId: string) {
  if (!checkEpoch()) return;
  selectedRunId.value = runId;
  transcriptItems.value = [];
  transcriptCursors.value = [null];
  pageIndex.value = 0;
  transcriptNext.value = null;
  transcriptError.value = '';
  await Promise.all([refreshStatus(runId), loadTranscriptPage(0)]);
}

async function nextPage() {
  if (!transcriptNext.value || !checkEpoch()) return;
  pageIndex.value += 1;
  transcriptCursors.value[pageIndex.value] = transcriptNext.value;
  await loadTranscriptPage(pageIndex.value);
}

async function previousPage() {
  if (pageIndex.value < 1) return;
  pageIndex.value -= 1;
  await loadTranscriptPage(pageIndex.value);
}

async function start() {
  if (!canStart.value || !checkEpoch()) return;
  busy.value = true;
  actionError.value = '';
  status.value = '';
  const chosen = new Set(selectedTargets.value);
  const targets = allTargets.value.filter((option) => chosen.has(option.key)).map((option) => ({ kind: option.kind, id: option.id }));
  const key = newOperationKey();
  try {
    const started = await commands.startRun({
      task: task.value,
      targets,
      permissions: [...permissions.value],
      allow_created_opportunities: false,
    }, key);
    applyRuns([{ run_id: started.run_id, state: started.state, revision: started.revision, deadline_at: started.deadline_at }, ...runs.value.filter((row) => row.run_id !== started.run_id)]);
    status.value = `Run ${started.run_id} started with ${permissions.value.length} granted action(s).`;
    task.value = '';
    acknowledged.value = false;
    await openRun(started.run_id);
  } catch (reason) {
    if (sessionInvalid(reason)) markStale('The owner session expired. Reopen delegated runs with the current session.');
    else actionError.value = message(reason, 'The run could not be started.');
  } finally {
    busy.value = false;
    startPolling();
  }
}

async function cancel(row: RunListItem) {
  if (stale.value || !checkEpoch()) return;
  actionError.value = '';
  status.value = '';
  // A cancellation is a distinct intent and never reuses the start key.
  const key = newOperationKey();
  try {
    const result = await commands.cancelRun(row.run_id, key);
    if (result.state === 'cancelled') {
      applyRuns(runs.value.map((item) => item.run_id === row.run_id
        ? { ...item, state: 'cancelled', revision: item.revision + 1 } : item));
      status.value = `Run ${row.run_id} was cancelled.`;
    } else {
      status.value = `Run ${row.run_id} was already finished.`;
    }
  } catch (reason) {
    if (sessionInvalid(reason)) markStale('The owner session expired. Reopen delegated runs with the current session.');
    else actionError.value = message(reason, 'The run could not be cancelled.');
  }
}

async function refresh() {
  if (stale.value || !checkEpoch()) return;
  try {
    applyRuns((await commands.listRuns({ limit: 20 })).items);
  } catch (reason) {
    handleRunError(reason);
  }
}

async function reload() {
  stale.value = false;
  staleReason.value = '';
  observedEpoch.value = '';
  try {
    await parley.refreshSession();
    await load();
    startPolling();
  } catch (reason) {
    markStale(message(reason, 'The current session could not be reopened.'));
  }
}

function onWorkspaceChanged(event: Event) {
  const detail = (event as CustomEvent<{ epoch?: unknown }>).detail;
  if (detail && typeof detail.epoch === 'string' && observedEpoch.value && detail.epoch !== observedEpoch.value) {
    markStale('The workspace epoch changed. Delegated runs are stale until this view is reopened with the current session.');
  } else {
    markStale('The workspace session changed outside this view. Reopen delegated runs before continuing.');
  }
}

function progress(runId: string): number {
  const count = runStatuses.value[runId]?.turn_count ?? 0;
  return Math.max(0, Math.min(MAX_TURNS, count));
}

onMounted(() => {
  globalThis.addEventListener('parley:workspace-changed', onWorkspaceChanged);
  globalThis.addEventListener('parley:workspace-quarantined', onWorkspaceChanged);
  void load().then(startPolling);
});
onBeforeUnmount(() => {
  globalThis.removeEventListener('parley:workspace-changed', onWorkspaceChanged);
  globalThis.removeEventListener('parley:workspace-quarantined', onWorkspaceChanged);
  stopPolling();
});
</script>

<template>
  <section
    aria-labelledby="delegated-runs-heading"
    class="delegated-runs"
  >
    <h2 id="delegated-runs-heading">
      Delegated runs
    </h2>
    <p>Start one explicit, finite read grant. A run can only use the targets and permissions you select here; it never receives an independent credential.</p>
    <p
      v-if="stale"
      role="alert"
      data-stale
    >
      {{ staleReason }}
      <button
        type="button"
        aria-label="Reopen delegated runs with current session"
        @click="reload"
      >
        Reopen with current session
      </button>
    </p>
    <p
      v-if="status"
      role="status"
    >
      {{ status }}
    </p>
    <p
      v-if="actionError"
      role="alert"
    >
      {{ actionError }}
    </p>

    <form
      aria-label="Scoped delegated run"
      @submit.prevent="start"
    >
      <label>Question for the assistant<textarea
        v-model="task"
        aria-label="Task for delegated run"
        rows="4"
        :maxlength="MAX_TASK_BYTES"
      /></label>
      <p v-if="taskBytes > MAX_TASK_BYTES">
        The task must be at most 8 KiB. Shorten it before starting a run.
      </p>

      <label>Find a target<input
        v-model="targetQuery"
        type="search"
        aria-label="Filter available targets"
        placeholder="Filter parties and opportunities"
      ></label>
      <p
        v-if="loadingTargets"
        role="status"
      >
        Loading parties and opportunities…
      </p>
      <p
        v-else-if="targetError"
        role="alert"
      >
        {{ targetError }}
      </p>
      <p v-else-if="!allTargets.length">
        No parties or opportunities are available to delegate.
      </p>
      <p v-else-if="!targetOptions.length">
        No targets match this filter.
      </p>
      <ul
        v-else
        class="target-list"
      >
        <li
          v-for="option in targetOptions"
          :key="option.key"
        >
          <label><input
            v-model="selectedTargets"
            type="checkbox"
            :value="option.key"
            :aria-label="`Target ${option.label}`"
          > {{ option.label }}</label>
        </li>
      </ul>

      <fieldset>
        <legend>Granted permissions</legend>
        <label
          v-for="permission in RUN_PERMISSIONS"
          :key="permission"
        >
          <input
            v-model="permissions"
            type="checkbox"
            :value="permission"
            :aria-label="`Grant ${permission}`"
          > {{ permission }} — {{ permissionHelp[permission] }}
        </label>
      </fieldset>

      <label><input
        v-model="acknowledged"
        type="checkbox"
        aria-label="Acknowledge scoped read grant"
      > I understand this run may use only the selected targets and granted actions.</label>
      <button
        type="submit"
        aria-label="Start scoped run"
        :disabled="!canStart"
      >
        Start scoped run
      </button>
    </form>

    <section aria-label="Active runs">
      <h3>Active and recent runs</h3>
      <label>Filter by state<select
        v-model="statusFilter"
        aria-label="Filter runs by state"
      >
        <option value="">
          All states
        </option>
        <option
          v-for="state in RUN_STATES"
          :key="state"
          :value="state"
        >
          {{ state }}
        </option>
      </select></label>
      <button
        type="button"
        aria-label="Refresh runs"
        @click="refresh"
      >
        Refresh runs
      </button>
      <p
        v-if="runsLoading"
        role="status"
      >
        Loading delegated runs…
      </p>
      <template v-else>
        <p
          v-if="runsError"
          role="alert"
        >
          {{ runsError }}
        </p>
        <p v-else-if="!runs.length">
          No delegated runs yet.
        </p>
        <p v-else-if="!visibleRuns.length">
          No runs match this state filter.
        </p>
        <ul v-if="runs.length && visibleRuns.length">
          <li
            v-for="row in visibleRuns"
            :key="row.run_id"
            :data-run-id="row.run_id"
            :data-state="row.state"
            class="run-row"
          >
            <p>Run {{ row.run_id }} · {{ row.state }} · deadline {{ row.deadline_at }}</p>
            <progress
              v-if="ACTIVE_STATES.includes(row.state)"
              :value="progress(row.run_id)"
              :max="MAX_TURNS"
              :aria-label="`Progress for run ${row.run_id}`"
            />
            <p v-if="runStatuses[row.run_id]">
              Turn {{ progress(row.run_id) }} of at most {{ MAX_TURNS }} · {{ runStatuses[row.run_id]?.transcript_bytes }} transcript bytes
            </p>
            <button
              type="button"
              :aria-label="`View transcript ${row.run_id}`"
              @click="openRun(row.run_id)"
            >
              View transcript
            </button>
            <button
              v-if="ACTIVE_STATES.includes(row.state)"
              type="button"
              :aria-label="`Cancel run ${row.run_id}`"
              @click="cancel(row)"
            >
              Cancel run
            </button>
          </li>
        </ul>
      </template>
    </section>

    <section
      v-if="selectedRunId"
      aria-label="Transcript panel"
    >
      <h3>Transcript for {{ selectedRunId }}</h3>
      <p
        v-if="transcriptLoading"
        role="status"
      >
        Loading transcript…
      </p>
      <p
        v-else-if="transcriptError"
        role="alert"
      >
        {{ transcriptError }}
      </p>
      <p v-else-if="!transcriptItems.length">
        No transcript turns yet.
      </p>
      <div
        v-else
        role="region"
        :aria-label="`Transcript for ${selectedRunId}`"
        class="transcript"
      >
        <p
          v-for="(turn, index) in transcriptItems"
          :key="`${turn.turn_no}-${turn.ordinal}-${index}`"
        >
          <strong>{{ turn.speaker }}</strong> · turn {{ turn.turn_no }}: {{ turn.text }}
        </p>
      </div>
      <button
        v-if="pageIndex > 0"
        type="button"
        aria-label="Previous transcript page"
        @click="previousPage"
      >
        Previous page
      </button>
      <button
        v-if="transcriptNext"
        type="button"
        aria-label="Next transcript page"
        @click="nextPage"
      >
        Next page
      </button>
    </section>
  </section>
</template>

<style scoped>
.delegated-runs { display: grid; gap: .75rem; }
.target-list, .run-row { list-style: none; padding: 0; }
.run-row { border-top: 1px solid #d1dbd2; padding: .5rem 0; }
.transcript { white-space: pre-wrap; overflow-wrap: anywhere; background: #f4f6f2; border: 1px solid #ccd5cc; border-radius: .35rem; padding: .6rem; }
progress { width: min(100%, 18rem); }
</style>
