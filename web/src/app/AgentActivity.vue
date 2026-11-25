<script setup lang="ts">
/* global AbortSignal */
/**
 * AR-001 AR09 · Owner Activity. One shared, keyboard-accessible work list for
 * durable role jobs and delegated runs. The list is a bounded server page; the
 * session-owned reconciler refreshes visible active content every 2 s and
 * terminal content every 8 s, and stops while hidden. Cancellation uses the
 * real job/run cancel operations; review opens the source-of-truth record via
 * typed navigation. No browser storage, no per-view timers.
 */
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { ApiError, newOperationKey, parley, type ActivityItem, type ActivityPage } from '../api/client';
import { CommandApiError, commands, type RunStatus, type TranscriptTurn } from '../api/commands';
import { activityItemIsActive, observeActivity } from './inference/activity';

const props = defineProps<{
  navigate: (path: string) => void;
}>();

const PAGE_SIZE = 20;
const TRANSCRIPT_PAGE_SIZE = 4;
const ACTIVE_STATES = new Set(['queued', 'running', 'waiting_owner', 'waiting_job']);

const roleLabels: Record<string, string> = {
  coach: 'Coach',
  template_builder: 'Template Builder',
  interviewer: 'Interviewer',
  briefer: 'Briefer',
  extractor: 'Extractor',
  matcher: 'Matcher',
  composer: 'Composer',
  linter: 'Linter',
  delegated: 'Delegated run',
};

const jobStatusLabels: Record<string, string> = {
  queued: 'queued',
  running: 'working',
  succeeded: 'succeeded',
  invalid: 'invalid',
  failed: 'failed',
  cancelled: 'cancelled',
};

const runStatusLabels: Record<string, string> = {
  queued: 'queued',
  running: 'working',
  waiting_owner: 'waiting for your review',
  waiting_job: 'waiting for a background job',
  succeeded: 'succeeded',
  failed: 'failed',
  cancelled: 'cancelled',
  expired: 'expired',
};

type KindFilter = '' | 'job' | 'run';
const kindFilter = ref<KindFilter>('');
const statusFilter = ref('');
const scopeFilter = ref('');
const items = ref<ActivityItem[]>([]);
const pageEpoch = ref('');
const pages = ref<Array<{ cursor: string | null; page: ActivityPage }>>([]);
const pageIndex = ref(0);
const loaded = ref(false);
const loading = ref(true);
const listError = ref('');
const stale = ref(false);

const expanded = ref<string | null>(null);
const detail = ref<{ kind: 'job' | 'run'; id: string; job?: Record<string, unknown>; run?: RunStatus; transcript?: TranscriptTurn[]; transcriptNext?: string | null; transcriptPage?: number; error?: string } | null>(null);
const busy = ref(false);
const actionStatus = ref('');
const actionError = ref('');

const statusOptions = computed<(string)[]>(() => kindFilter.value === 'run'
  ? ['queued', 'running', 'waiting_owner', 'waiting_job', 'succeeded', 'failed', 'cancelled', 'expired']
  : ['queued', 'running', 'succeeded', 'invalid', 'failed', 'cancelled']);
const scopeOptions = ['owner', 'party', 'segment', 'opportunity', 'material', 'delegated'];

function statusLabel(item: ActivityItem): string {
  return (item.kind === 'run' ? runStatusLabels : jobStatusLabels)[item.status] ?? item.status;
}

function isActive(item: ActivityItem): boolean {
  return ACTIVE_STATES.has(item.status);
}

function roleLabel(item: ActivityItem): string {
  return roleLabels[item.role] ?? item.role;
}

function reviewDestination(item: ActivityItem): string | null {
  const review = item.review_ref;
  if (!review) return null;
  switch (review.kind) {
    case 'brief_scope':
      return `/parties/${encodeURIComponent(String(review.id))}?brief=1`;
    case 'delegated_review':
      if (review.scope_kind === 'opportunity' && typeof review.scope_id === 'string') {
        return `/output/${encodeURIComponent(review.scope_id)}`;
      }
      if (review.scope_kind === 'party' && typeof review.scope_id === 'string') {
        return `/parties/${encodeURIComponent(review.scope_id)}?brief=1`;
      }
      return null;
    case 'coach_report':
      return '/queue';
    case 'template_build_candidate':
      return '/output/templates';
    case 'interviewer_candidate': {
      const scopeId = item.scope?.id;
      return typeof scopeId === 'string' ? `/parties/${encodeURIComponent(scopeId)}?interviewer=1` : null;
    }
    default:
      return null;
  }
}

async function readPage(signal: AbortSignal): Promise<ActivityPage> {
  const cursor = pageIndex.value > 0 ? pages.value[pageIndex.value - 1]?.cursor ?? undefined : undefined;
  return parley.getActivity({
    ...(kindFilter.value ? { kind: kindFilter.value } : {}),
    ...(statusFilter.value ? { status: statusFilter.value } : {}),
    ...(scopeFilter.value ? { scope: scopeFilter.value } : {}),
    limit: PAGE_SIZE,
    ...(cursor ? { cursor } : {}),
    signal,
  });
}

function applyPage(page: ActivityPage) {
  items.value = page.items;
  pageEpoch.value = page.epoch;
  loaded.value = true;
  loading.value = false;
  listError.value = '';
}

function markStale(reason: string) {
  if (stale.value) return;
  stale.value = true;
  listError.value = reason;
}

let stopObserving: (() => void) | undefined;

function restartObserver() {
  stopObserving?.();
  const epoch = parley.workspaceEpoch;
  stopObserving = observeActivity({
    read: readPage,
    active: activityItemIsActive,
    update: applyPage,
    failed(reason: unknown) {
      loading.value = false;
      if (reason instanceof ApiError && (reason.status === 401 || reason.status === 403 || reason.status === 409)) {
        markStale('The workspace session changed. Reopen Activity with the current session.');
        return;
      }
      listError.value = reason instanceof Error ? reason.message : 'Could not read activity.';
    },
    onEpochChange() {
      markStale('The workspace epoch changed. Activity is stale until reopened with the current session.');
    },
  });
  void epoch;
}

watch([kindFilter, statusFilter, scopeFilter], () => {
  pages.value = [];
  pageIndex.value = 0;
  expanded.value = null;
  detail.value = null;
  loading.value = true;
  listError.value = '';
  stale.value = false;
  restartObserver();
});

watch(pageIndex, () => {
  if (pageIndex.value === 0) {
    restartObserver();
    return;
  }
  // Deeper pages are a stable snapshot: stop live observation so the visible
  // page is not replaced by the newest first page.
  stopObserving?.();
  const page = pages.value[pageIndex.value - 1]?.page;
  if (page) applyPage(page);
});

restartObserver();

function goNext() {
  const page = pages.value[pageIndex.value - 1]?.page;
  if (!page?.next_cursor) return;
  pages.value = [...pages.value.slice(0, pageIndex.value), { cursor: page.next_cursor, page }];
  pageIndex.value += 1;
}

function goPrevious() {
  if (pageIndex.value === 0) return;
  pageIndex.value -= 1;
}

async function toggleDetail(item: ActivityItem) {
  if (expanded.value === item.id) {
    expanded.value = null;
    detail.value = null;
    return;
  }
  expanded.value = item.id;
  detail.value = { kind: item.kind, id: item.id };
  if (item.kind === 'job') {
    try {
      const job = await parley.getRoleJob(item.id);
      if (expanded.value !== item.id) return;
      detail.value = { kind: 'job', id: item.id, job: job as unknown as Record<string, unknown> };
    } catch (reason) {
      detail.value = { kind: 'job', id: item.id, error: reason instanceof Error ? reason.message : 'Could not read job detail.' };
    }
    return;
  }
  await loadRunDetail(item.id, 0);
}

async function loadRunDetail(runId: string, page: number) {
  try {
    const run = await commands.getRun(runId);
    const cursor = page > 0 ? detail.value?.transcriptNext ?? undefined : undefined;
    const transcript = await commands.getRunTranscript(runId, { limit: TRANSCRIPT_PAGE_SIZE, ...(cursor ? { cursor } : {}) });
    if (expanded.value !== runId) return;
    detail.value = { kind: 'run', id: runId, run, transcript: transcript.items, transcriptNext: transcript.next_cursor, transcriptPage: page };
  } catch (reason) {
    if (expanded.value !== runId) return;
    detail.value = { kind: 'run', id: runId, error: reason instanceof CommandApiError ? reason.message : 'Could not read run detail.' };
  }
}

async function transcriptPage(next: boolean) {
  const current = detail.value;
  if (!current || current.kind !== 'run' || !current.run) return;
  const page = (current.transcriptPage ?? 0) + (next ? 1 : -1);
  if (page < 0) return;
  await loadRunDetail(current.id, page);
}

async function cancel(item: ActivityItem) {
  if (!isActive(item) || busy.value) return;
  busy.value = true;
  actionError.value = '';
  actionStatus.value = '';
  try {
    if (item.kind === 'job') {
      await parley.cancelJob(item.id);
      actionStatus.value = `Job ${item.id} was cancelled.`;
    } else {
      const result = await commands.cancelRun(item.id, newOperationKey());
      actionStatus.value = result.state === 'cancelled' ? `Run ${item.id} was cancelled.` : `Run ${item.id} was already finished.`;
    }
    // Let the reconciler surface the terminal state on its next read.
    activityReconcilerRefresh();
  } catch (reason) {
    if (reason instanceof ApiError && reason.status === 409) {
      actionError.value = 'This item already finished and can no longer be cancelled.';
    } else {
      actionError.value = reason instanceof Error ? reason.message : 'The item could not be cancelled.';
    }
  } finally {
    busy.value = false;
  }
}

function activityReconcilerRefresh() {
  globalThis.dispatchEvent(new globalThis.CustomEvent('parley:command-committed', { detail: {} }));
}

function openReview(item: ActivityItem) {
  const target = reviewDestination(item);
  if (target) props.navigate(target);
}

onBeforeUnmount(() => {
  stopObserving?.();
});

defineExpose({ refresh: activityReconcilerRefresh });
</script>

<template>
  <section
    class="agent-activity"
    aria-labelledby="activity-heading"
  >
    <h1 id="activity-heading">
      Activity
    </h1>
    <p>One place to find and continue durable work: role jobs and delegated runs, with real progress and cancellation.</p>
    <p
      v-if="stale"
      role="alert"
      data-stale
    >
      {{ listError }}
    </p>
    <p
      v-if="actionStatus"
      role="status"
    >
      {{ actionStatus }}
    </p>
    <p
      v-if="actionError"
      role="alert"
    >
      {{ actionError }}
    </p>

    <form
      class="activity-filters"
      aria-label="Activity filters"
      @submit.prevent
    >
      <label>Kind<select
        v-model="kindFilter"
        aria-label="Filter by kind"
      >
        <option value="">
          All kinds
        </option>
        <option value="job">
          Jobs
        </option>
        <option value="run">
          Delegated runs
        </option>
      </select></label>
      <label>Status<select
        v-model="statusFilter"
        aria-label="Filter by status"
      >
        <option value="">
          All states
        </option>
        <option
          v-for="state in statusOptions"
          :key="state"
          :value="state"
        >
          {{ state }}
        </option>
      </select></label>
      <label>Scope<select
        v-model="scopeFilter"
        aria-label="Filter by scope"
      >
        <option value="">
          All scopes
        </option>
        <option
          v-for="scope in scopeOptions"
          :key="scope"
          :value="scope"
        >
          {{ scope }}
        </option>
      </select></label>
    </form>

    <p
      v-if="loading"
      role="status"
    >
      Loading activity…
    </p>
    <p
      v-else-if="listError && !stale"
      role="alert"
    >
      {{ listError }}
    </p>
    <p v-else-if="!items.length">
      No work matches these filters.
    </p>
    <ul
      v-else
      class="activity-list"
    >
      <li
        v-for="item in items"
        :key="`${item.kind}:${item.id}`"
        :data-kind="item.kind"
        :data-id="item.id"
        :data-state="item.status"
        class="activity-row"
      >
        <div class="activity-main">
          <p>
            <strong>{{ roleLabel(item) }}</strong>
            · {{ statusLabel(item) }}
            · {{ item.id }}
            <span v-if="item.scope?.kind"> · {{ item.scope.kind }}: {{ item.scope.id ?? '—' }}</span>
          </p>
          <progress
            v-if="item.progress"
            :value="item.progress.current"
            :max="item.progress.total"
            :aria-label="`Progress for ${item.id}`"
          />
          <p
            v-if="item.progress"
            class="progress-note"
          >
            {{ item.progress.current }} of {{ item.progress.total }} · attempt {{ item.attempt_count ?? '—' }}
          </p>
          <p
            v-if="item.error_code"
            role="alert"
          >
            {{ item.error_code }}
          </p>
          <p
            v-if="item.status === 'waiting_owner'"
            role="status"
          >
            Waiting for your review before it can continue.
          </p>
          <p
            v-if="item.status === 'waiting_job'"
            role="status"
          >
            Waiting for its background job to finish.
          </p>
          <div class="activity-actions">
            <button
              v-if="reviewDestination(item)"
              type="button"
              :aria-label="`Open review for ${item.id}`"
              @click="openReview(item)"
            >
              Open review
            </button>
            <button
              type="button"
              :aria-label="`View detail for ${item.id}`"
              @click="toggleDetail(item)"
            >
              {{ expanded === item.id ? 'Close detail' : 'View detail' }}
            </button>
            <button
              v-if="isActive(item)"
              type="button"
              :aria-label="`Cancel ${item.id}`"
              :disabled="busy"
              @click="cancel(item)"
            >
              Cancel
            </button>
          </div>
        </div>
        <div
          v-if="expanded === item.id && detail?.id === item.id"
          class="activity-detail"
          role="region"
          :aria-label="`Detail for ${item.id}`"
        >
          <p
            v-if="detail.error"
            role="alert"
          >
            {{ detail.error }}
          </p>
          <template v-else-if="detail.kind === 'job' && detail.job">
            <p>Status: {{ detail.job.status }} · state revision {{ detail.job.state_revision }} · attempts {{ detail.job.attempt_count }}</p>
            <p v-if="detail.job.last_error_code">
              Last error: {{ detail.job.last_error_code }}
            </p>
            <p v-if="detail.job.last_error">
              {{ detail.job.last_error }}
            </p>
          </template>
          <template v-else-if="detail.kind === 'run' && detail.run">
            <p>Turn {{ detail.run.turn_count }} of at most 16 · transcript {{ detail.run.transcript_bytes }} bytes · profile {{ detail.run.profile_revision ?? 'unqualified' }}</p>
            <p
              v-if="detail.run.wait_intent"
              role="status"
            >
              Waiting on tool ordinal {{ detail.run.wait_intent }}.
            </p>
            <div
              v-if="detail.transcript?.length"
              class="transcript"
            >
              <p
                v-for="(turn, index) in detail.transcript"
                :key="`${turn.turn_no}-${turn.ordinal}-${index}`"
              >
                <strong>{{ turn.speaker }}</strong> · turn {{ turn.turn_no }}: {{ turn.text }}
              </p>
            </div>
            <p v-else>
              No transcript turns yet.
            </p>
            <div v-if="detail.transcriptPage">
              <button
                type="button"
                aria-label="Previous transcript page"
                @click="transcriptPage(false)"
              >
                Previous page
              </button>
            </div>
            <button
              v-if="detail.transcriptNext"
              type="button"
              aria-label="Next transcript page"
              @click="transcriptPage(true)"
            >
              Next page
            </button>
          </template>
          <p
            v-else
            role="status"
          >
            Loading detail…
          </p>
        </div>
      </li>
    </ul>

    <div class="activity-pager">
      <button
        type="button"
        :disabled="pageIndex === 0"
        aria-label="Previous activity page"
        @click="goPrevious"
      >
        Previous page
      </button>
      <span role="status">
        Page {{ pageIndex + 1 }}
      </span>
      <button
        type="button"
        :disabled="!pages[pageIndex - 1]?.page?.next_cursor"
        aria-label="Next activity page"
        @click="goNext"
      >
        Next page
      </button>
    </div>
  </section>
</template>

<style scoped>
.agent-activity { display: grid; gap: .75rem; max-width: 64rem; }
.activity-filters { display: flex; flex-wrap: wrap; gap: .6rem; background: var(--panel, white); border: 1px solid var(--line, #d1dbd2); border-radius: .45rem; padding: .7rem; }
.activity-filters label { display: grid; gap: .2rem; margin: 0; }
.activity-list { list-style: none; padding: 0; margin: 0; display: grid; gap: .55rem; }
.activity-row { border: 1px solid var(--line, #d1dbd2); border-radius: .45rem; background: var(--panel, white); padding: .7rem .8rem; }
.activity-main p { margin: .3rem 0; }
.activity-actions { display: flex; flex-wrap: wrap; gap: .45rem; margin-top: .45rem; }
.progress-note { color: var(--muted, #58665f); font-size: .88rem; }
.activity-detail { border-top: 1px solid var(--line, #dce2db); margin-top: .6rem; padding-top: .6rem; }
.transcript { white-space: pre-wrap; overflow-wrap: anywhere; background: #f4f6f2; border: 1px solid #ccd5cc; border-radius: .35rem; padding: .6rem; }
.activity-pager { display: flex; align-items: center; gap: .75rem; }
progress { width: min(100%, 22rem); }
</style>
