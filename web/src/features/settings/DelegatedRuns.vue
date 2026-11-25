<script setup lang="ts">
/* global Event, CustomEvent, TextEncoder */
/**
 * U02 owner consent surface. The shell owns the session transport: this view
 * only reads authoritative DTOs through the shared clients and never keeps a
 * run in browser storage. Every start and cancellation carries its own
 * operation key, and a generation/epoch change stops observation immediately.
 * The run list is the shared AR-001 AR09 activity reconciler (kind=run);
 * this view has no timer of its own.
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';

const props = defineProps<{
  navigate?: (path: string) => void;
}>();
import { newOperationKey, parley } from '../../api/client';
import { observeActivity } from '../../app/inference/activity';
import {
  RUN_PERMISSIONS, RUN_STATES,
  CommandApiError,
  commands,
  type RunListItem, type RunPermission, type RunState, type RunStatus, type RunTargetKind, type TranscriptTurn,
} from '../../api/commands';

const MAX_TASK_BYTES = 8192;
const MAX_TURNS = 16;
const TRANSCRIPT_PAGE_SIZE = 2;
const ACTIVE_STATES: RunState[] = ['queued', 'running', 'waiting_owner', 'waiting_job'];
const permissionHelp: Record<RunPermission, string> = {
  'party.read': 'read the granted party summary only',
  'opportunity.read': 'read the granted opportunity summary only',
  'opportunity.create': 'create an opportunity for a granted party',
  'opportunity.phase.edit': 'change the phase of a granted opportunity',
  'party.edit': 'rename a granted party or change its focus',
  'opportunity.task.edit': 'change the checked state of a task in a granted opportunity notes',
  'brief.start': 'queue a reviewable brief about a granted party',
  'job.read': 'read the status of jobs this run started',
  'resource.read': 'read the granted resource summary only',
  'resource.write': 'create drafts and ready granted resources',
  'resource.retire': 'retire a granted resource after your approval',
  'match.start': 'queue a scoped match using granted resources',
  'material.compose': 'queue a reviewable material draft from granted inputs',
  'material.gate': 'assess the deterministic release gates of a granted material',
};

const stateLabel: Record<RunState, string> = {
  queued: 'queued',
  running: 'working',
  waiting_owner: 'waiting for your review',
  waiting_job: 'waiting for a background job',
  succeeded: 'succeeded',
  failed: 'failed',
  cancelled: 'cancelled',
  expired: 'expired',
};

const task = ref('');
const targetQuery = ref('');
const selectedTargets = ref<string[]>([]);
const permissions = ref<RunPermission[]>(['party.read', 'opportunity.read']);
const allowCreated = ref(false);
const acknowledged = ref(false);
const targets = ref<TargetOption[]>([]);
const domainEmpty = ref(false);
const selectedMeta = ref<Record<string, TargetOption>>({});
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
let stopObserving: (() => void) | undefined;

type TargetOption = { key: string; label: string; kind: RunTargetKind; id: string };

// Targets are read from the bounded server projections. Typeahead widens to
// the whole domain instead of searching a stale client-side copy.
async function searchTargets(q: string) {
  const needle = q.trim();
  const [partyPage, opportunityPage] = await Promise.all([
    parley.queryDirectory({ q: needle || undefined, limit: 50 }),
    parley.listOpportunitiesPage({ q: needle, limit: 50 }),
  ]);
  const found: TargetOption[] = [
    ...partyPage.rows.map(party => ({ key: `party:${party.id}`, label: `Party: ${party.name}`, kind: 'party' as const, id: party.id })),
    ...opportunityPage.items.map(item => ({ key: `opportunity:${item.id}`, label: `Opportunity: ${item.title}`, kind: 'opportunity' as const, id: item.id })),
  ];
  if (!needle) domainEmpty.value = found.length === 0;
  for (const option of found) if (selectedTargets.value.includes(option.key)) selectedMeta.value[option.key] = option;
  const pinned = Object.values(selectedMeta.value).filter(option => selectedTargets.value.includes(option.key));
  targets.value = [...new Map([...pinned, ...found].map(option => [option.key, option])).values()].slice(0, 50);
}
let targetTimer: ReturnType<typeof globalThis.setTimeout> | undefined;
watch(targetQuery, value => {
  if (targetTimer) globalThis.clearTimeout(targetTimer);
  targetTimer = globalThis.setTimeout(() => { void searchTargets(value); }, 120);
});
watch(selectedTargets, chosen => {
  const next = { ...selectedMeta.value };
  for (const option of targets.value) if (chosen.includes(option.key)) next[option.key] = option;
  selectedMeta.value = next;
}, { immediate: true });
const taskBytes = computed(() => new TextEncoder().encode(task.value).length);
const canStart = computed(() => !stale.value && !busy.value && !!task.value.trim()
  && taskBytes.value <= MAX_TASK_BYTES && selectedTargets.value.length > 0
  && permissions.value.length > 0 && acknowledged.value);
const visibleRuns = computed(() => statusFilter.value ? runs.value.filter((row) => row.state === statusFilter.value) : runs.value);
function message(reason: unknown, fallback: string): string {
  return reason instanceof Error ? reason.message : fallback;
}

function sessionInvalid(reason: unknown): boolean {
  return reason instanceof CommandApiError && (reason.status === 401 || reason.status === 403);
}

function markStale(reason: string) {
  if (stale.value) return;
  stale.value = true;
  staleReason.value = reason;
  status.value = '';
  stopObserving?.();
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

/** Shared AR09 activity reconciler: kind=run, newest 20, no view timer. */
function restartObservation() {
  stopObserving?.();
  const epoch = parley.workspaceEpoch;
  stopObserving = observeActivity({
    read: (signal) => parley.getActivity({ kind: 'run', limit: 20, signal }),
    active: (item) => ACTIVE_STATES.includes(item.status as RunState),
    update(page) {
      runsLoading.value = false;
      const items: RunListItem[] = page.items.map(item => ({
        run_id: item.id,
        state: item.status as RunState,
        revision: item.state_revision,
        deadline_at: '',
      }));
      applyRuns(items);
      // Data-driven detail refresh: only runs whose status is not yet known or
      // that are still active are re-read; terminal runs were read once.
      const fresh = items
        .filter(row => !runStatuses.value[row.run_id] || ACTIVE_STATES.includes(row.state))
        .map(row => row.run_id);
      if (fresh.length) void refreshStatuses(fresh);
      if (selectedRunId.value && fresh.includes(selectedRunId.value)) {
        void loadTranscriptPage(pageIndex.value);
      }
    },
    failed(reason) {
      runsLoading.value = false;
      handleRunError(reason);
    },
    onEpochChange() {
      markStale('The workspace epoch changed. Delegated runs are stale until this view is reopened with the current session.');
    },
  });
  void epoch;
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
  const targetResult = await Promise.allSettled([searchTargets(targetQuery.value)]);
  if (targetResult[0]?.status === 'rejected') targetError.value = message(targetResult[0].reason, 'Could not load targets.');
  loadingTargets.value = false;
  restartObservation();
}

async function refreshStatus(runId: string) {
  try {
    runStatuses.value = { ...runStatuses.value, [runId]: await commands.getRun(runId) };
  } catch (reason) {
    handleRunError(reason);
  }
}

/** Read many run statuses concurrently; one slow run cannot stall the rest. */
async function refreshStatuses(ids: string[]) {
  if (!ids.length) return;
  const results = await Promise.allSettled(ids.map(id => commands.getRun(id)));
  const next = { ...runStatuses.value };
  for (let index = 0; index < ids.length; index += 1) {
    const result = results[index];
    if (result?.status === 'fulfilled') next[ids[index]!] = result.value;
    else if (result?.status === 'rejected') handleRunError(result.reason);
  }
  runStatuses.value = next;
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
  const targets = Object.values(selectedMeta.value).filter((option) => chosen.has(option.key)).map((option) => ({ kind: option.kind, id: option.id }));
  const key = newOperationKey();
  try {
    const started = await commands.startRun({
      task: task.value,
      targets,
      permissions: [...permissions.value],
      allow_created_opportunities: permissions.value.includes('opportunity.create') && allowCreated.value,
    }, key);
    runStatuses.value = { ...runStatuses.value, [started.run_id]: { run_id: started.run_id, state: started.state, revision: started.revision, deadline_at: started.deadline_at, turn_count: 1, transcript_bytes: 0, wake_condition: 'queued', profile_revision: null, transcript_major: 1, wait_intent: null, review: null } };
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
    hintReconciler();
  }
}

function hintReconciler() {
  globalThis.dispatchEvent(new globalThis.CustomEvent('parley:command-committed', { detail: {} }));
}

function openReview(row: RunListItem) {
  const review = runStatuses.value[row.run_id]?.review;
  if (!review || !props.navigate) return;
  // A writing review (match/compose) is scoped to its opportunity: open the
  // Output workspace on that opportunity's record so the owner continues at
  // the review surface (UCR-001 U07).
  if (review.scope_kind === 'opportunity') {
    props.navigate(`/output/${encodeURIComponent(review.scope_id)}`);
    return;
  }
  // Reopen the party desk on the reviewed party with the Brief (chat) pane
  // active. The hand-off rides the typed route query, not browser storage, so
  // no private state survives the click (AR-001 AR04).
  props.navigate(`/parties/${encodeURIComponent(review.scope_id)}?brief=1`);
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
    hintReconciler();
  } catch (reason) {
    if (sessionInvalid(reason)) markStale('The owner session expired. Reopen delegated runs with the current session.');
    else actionError.value = message(reason, 'The run could not be cancelled.');
  }
}

async function refresh() {
  if (stale.value || !checkEpoch()) return;
  hintReconciler();
}

async function reload() {
  stale.value = false;
  staleReason.value = '';
  observedEpoch.value = '';
  try {
    await parley.refreshSession();
    await load();
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

function runDeadline(row: RunListItem): string {
  return runStatuses.value[row.run_id]?.deadline_at ?? row.deadline_at;
}

onMounted(() => {
  globalThis.addEventListener('parley:workspace-changed', onWorkspaceChanged);
  globalThis.addEventListener('parley:workspace-quarantined', onWorkspaceChanged);
  void load();
});

onBeforeUnmount(() => {
  if (targetTimer) globalThis.clearTimeout(targetTimer);
  globalThis.removeEventListener('parley:workspace-changed', onWorkspaceChanged);
  globalThis.removeEventListener('parley:workspace-quarantined', onWorkspaceChanged);
  stopObserving?.();
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
      <p v-else-if="domainEmpty">
        No parties or opportunities are available to delegate.
      </p>
      <p v-else-if="!targets.length">
        No targets match this filter.
      </p>
      <ul
        v-else
        class="target-list"
      >
        <li
          v-for="option in targets"
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
        <label
          v-if="permissions.includes('opportunity.create')"
          class="created-target-grant"
        ><input
          v-model="allowCreated"
          type="checkbox"
          aria-label="Grant created by this run opportunities"
        > Bind opportunities this run creates to its own scope (created by this run).</label>
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
            <p>Run {{ row.run_id }} · <strong>{{ stateLabel[row.state] }}</strong> · deadline {{ runDeadline(row) || '—' }}</p>
            <progress
              v-if="ACTIVE_STATES.includes(row.state)"
              :value="progress(row.run_id)"
              :max="MAX_TURNS"
              :aria-label="`Progress for run ${row.run_id}`"
            />
            <p v-if="runStatuses[row.run_id]">
              Turn {{ progress(row.run_id) }} of at most {{ MAX_TURNS }} · {{ runStatuses[row.run_id]?.transcript_bytes }} transcript bytes
              · pinned profile {{ runStatuses[row.run_id]?.profile_revision ?? 'unqualified' }}
            </p>
            <p
              v-if="runStatuses[row.run_id]?.wait_intent"
              role="status"
            >
              Waiting on tool ordinal {{ runStatuses[row.run_id]?.wait_intent }}. Cancelling here revokes the grant and stops the run.
            </p>
            <p
              v-if="row.state === 'waiting_job'"
              role="status"
            >
              Waiting for its background job to finish. The queue, brief and REST surfaces stay usable.
            </p>
            <p
              v-if="row.state === 'waiting_owner'"
              role="status"
            >
              Waiting for your review before it can continue.
            </p>
            <button
              v-if="row.state === 'succeeded' && runStatuses[row.run_id]?.review"
              type="button"
              :aria-label="`Open reviewed candidate for run ${row.run_id}`"
              class="review-link"
              @click="openReview(row)"
            >
              Open reviewed brief
            </button>
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
