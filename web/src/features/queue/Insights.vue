<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue';
import {
  ApiError, newOperationKey, parley, type CoachJob, type CoachQuestion, type CoachReason, type CoachSelector, type CoachSnapshot, type InsightsReport,
} from '../../api/client';
import { inferenceActivity, jobIsActive } from '../../app/inference/activity';
import CoachQuestions from '../coach/CoachQuestions.vue';

defineOptions({ name: 'QueueInsightsView' });

const emit = defineEmits<{ navigate: [path: string] }>();
const report = ref<InsightsReport | null>(null);
const snapshot = ref<CoachSnapshot | null>(null);
const job = ref<CoachJob | null>(null);
const loading = ref(false);
const starting = ref(false);
const cancelling = ref(false);
const error = ref('');
const readError = ref('');
const status = ref('');
const startButton = ref<InstanceType<typeof globalThis.HTMLButtonElement> | null>(null);
const cancelButton = ref<InstanceType<typeof globalThis.HTMLButtonElement> | null>(null);
const active = computed(() => jobIsActive(job.value));
const pendingAdmission = ref<string | null>(null);
let unsubscribe: (() => void) | undefined;
let reportRequest: InstanceType<typeof globalThis.AbortController> | undefined;
const weeklyChoices = computed(() => [{ selector: { kind: 'weekly', period: snapshot.value?.period ?? currentWeek() } as CoachSelector, label: 'Weekly questions' }]);
let mounted = false;
let epoch: string | undefined;
let startKey: string | undefined;
let cancelKey: { id: string; key: string } | undefined;

function currentWeek(): string {
  const date = new Date();
  const day = (date.getUTCDay() + 6) % 7;
  date.setUTCDate(date.getUTCDate() - day + 3);
  const year = date.getUTCFullYear();
  const firstThursday = new Date(Date.UTC(year, 0, 4));
  const firstDay = (firstThursday.getUTCDay() + 6) % 7;
  firstThursday.setUTCDate(firstThursday.getUTCDate() - firstDay + 3);
  const week = 1 + Math.round((date.getTime() - firstThursday.getTime()) / 604800000);
  return `${year}-W${String(week).padStart(2, '0')}`;
}

function currentSession(): boolean {
  return mounted && epoch === parley.workspaceEpoch;
}

const coachRemedies: Readonly<Record<string, string>> = {
  RUNNER_INSTALL_INVALID: 'The installed Coach runtime is unavailable. Check runtime readiness in Settings.',
  ISOLATION_UNAVAILABLE: 'The local runner could not start in isolation. Check runtime readiness in Settings.',
  PROTOCOL_MISMATCH: 'The installed runner and service versions differ. Check runtime readiness in Settings.',
  MODEL_UNAVAILABLE: 'The configured model is unavailable. Check the local model in Settings.',
  PROFILE_CHANGED: 'The local model identity changed. Requalify the profile in Settings.',
  CAPABILITY_UNQUALIFIED: 'Coach requires a qualified local model. Check the profile in Settings.',
  INFERENCE_BUSY: 'The inference queue is full. Try again after existing work finishes.',
  CONTEXT_BUDGET: 'The weekly scope exceeds the local model budget. Review the deterministic report.',
  QUEUE_EXPIRED: 'The Coach job expired while queued. Start a fresh job when ready.',
  STALE_INPUT: 'The evidence changed while Coach was running. Refresh the report and start again.',
  RESULT_INVALID: 'The model returned an invalid proposal. Review the deterministic report.',
  INTERRUPTED_UPGRADE: 'A workspace upgrade interrupted this job. Start a fresh job when ready.',
};

const reasons: Readonly<Record<CoachReason, string>> = {
  assessment_check: 'Check unassessed or outdated resource eligibility',
  missing_proof: 'Find verified proof for recipients without a match',
  missing_gift: 'Find a useful gift for recipients without a match',
  missing_artifact: 'Find a relevant artifact for recipients without a match',
  jargon_check: 'Review an unfamiliar term',
  template_review: 'Review flagged templates',
  capacity_update: 'Review outdated capacity',
  missing_identity: 'Add a professional identity tag',
  insight: 'Review the available first-touch observations',
};

function problem(reason: unknown, fallback: string): string {
  return reason instanceof ApiError ? (coachRemedies[reason.code] ?? fallback) : fallback;
}

async function loadReport() {
  reportRequest?.abort();
  const request = new globalThis.AbortController();
  reportRequest = request;
  loading.value = true;
  readError.value = '';
  try {
    const next = await parley.getInsights(request.signal);
    if (!request.signal.aborted && currentSession()) report.value = next;
  } catch {
    if (!request.signal.aborted && currentSession()) readError.value = 'Could not load server-computed insights. Refresh the report to retry.';
  } finally {
    if (reportRequest === request) loading.value = false;
  }
}

function refresh() {
  inferenceActivity.refresh();
  void loadReport();
}

function observe() {
  unsubscribe = inferenceActivity.watchWeekly({
    update(value) {
      if (!currentSession()) return;
      const previous = job.value;
      snapshot.value = value.snapshot;
      job.value = value.job;
      pendingAdmission.value = null;
      if (!value.job || (value.job.id === previous?.id && value.job.state_revision === previous.state_revision)) return;
      const restoreFocus = globalThis.document.activeElement === cancelButton.value;
      const period = value.job.scope.period;
      switch (value.job.status) {
        case 'queued':
          status.value = `Coach queued for ${period}.`;
          break;
        case 'running':
          status.value = `Coach is wording questions for ${period}.`;
          break;
        case 'succeeded':
          status.value = `Coach questions for ${period} are ready for review.`;
          void loadReport();
          break;
        case 'cancelled':
          status.value = `Coach job for ${period} cancelled.`;
          break;
        case 'failed':
        case 'invalid':
          status.value = '';
          error.value = coachRemedies[value.job.error_code ?? ''] ?? 'Coach could not finish. Review the deterministic report or try again.';
          break;
      }
      if (!active.value && restoreFocus) void nextTick(() => startButton.value?.focus());
    },
    error(reason) {
      if (currentSession()) error.value = problem(reason, 'Coach status is temporarily unavailable. Refresh the report to retry.');
    },
  });
}

async function runCoach() {
  if (!currentSession()) return;
  starting.value = true;
  error.value = '';
  status.value = '';
  startKey ??= newOperationKey();
  const actionEpoch = epoch;
  try {
    const result = await parley.startWeeklyCoach({}, startKey);
    if (!currentSession() || actionEpoch !== epoch) return;
    startKey = undefined;
    if (result.kind === 'queued') {
      pendingAdmission.value = result.jobId;
      status.value = 'Coach queued for this week.';
      inferenceActivity.observeAdmission(result.jobId);
    } else {
      snapshot.value = result.snapshot;
      status.value = 'There are no new Coach questions to generate for this week.';
    }
  } catch (reason) {
    if (!currentSession() || actionEpoch !== epoch) return;
    if (reason instanceof ApiError) startKey = undefined;
    error.value = problem(reason, 'Coach is unavailable. You can continue with the deterministic report.');
  } finally {
    if (actionEpoch === epoch) starting.value = false;
  }
}

async function cancel() {
  const id = job.value?.id;
  if (!id || !active.value || !currentSession()) return;
  cancelling.value = true;
  error.value = '';
  const actionEpoch = epoch;
  if (cancelKey?.id !== id) cancelKey = { id, key: newOperationKey() };
  try {
    await parley.cancelJob(id, cancelKey.key);
    if (!currentSession() || actionEpoch !== epoch) return;
    cancelKey = undefined;
    inferenceActivity.refresh();
  } catch (reason) {
    if (!currentSession() || actionEpoch !== epoch) return;
    if (reason instanceof ApiError) cancelKey = undefined;
    error.value = reason instanceof ApiError && reason.status === 409
      ? 'This Coach job already finished. The latest state will refresh.'
      : 'Could not confirm cancellation. Check the job state and retry if it is still active.';
    inferenceActivity.refresh();
  } finally {
    if (actionEpoch === epoch) cancelling.value = false;
  }
}

function targetPath(question: CoachQuestion): string | null {
  const target = question.target;
  if (typeof target.opportunity_id === 'string') return `/queue/${encodeURIComponent(target.opportunity_id)}`;
  if (typeof target.party_id === 'string') return `/parties/${encodeURIComponent(target.party_id)}`;
  if (typeof target.segment_id === 'string' || question.kind === 'jargon_check') return '/resources';
  if (question.kind === 'template_review') return '/output/templates';
  if (question.kind === 'capacity_update' || question.kind === 'missing_identity') return '/settings';
  return null;
}

async function openTarget(question: CoachQuestion) {
  const path = targetPath(question);
  if (!path || !currentSession()) return;
  const actionEpoch = epoch;
  try {
    if (typeof question.target.opportunity_id === 'string') await parley.getOpportunity(question.target.opportunity_id);
    if (typeof question.target.party_id === 'string') await parley.getDossier(question.target.party_id);
    if (typeof question.target.segment_id === 'string') {
      const segments = await parley.listSegments();
      if (!segments.some((segment) => segment.id === question.target.segment_id)) throw new Error('Segment unavailable');
    }
    if (currentSession() && actionEpoch === epoch) emit('navigate', path);
  } catch {
    if (currentSession() && actionEpoch === epoch) error.value = 'The related record is unavailable. Refresh the report.';
  }
}

function metricRate(metric: InsightsReport['rates'][number]): string {
  if (!metric.n || !metric.interval_95) return 'Insufficient observations';
  const rate = `${((metric.k / metric.n) * 100).toFixed(1)}%`;
  const percent = (value: number) => `${(value * 100).toFixed(1)}%`;
  return `${rate} · 95% Wilson interval ${percent(metric.interval_95.lower_95)}–${percent(metric.interval_95.upper_95)}`;
}

async function initialize() {
  try {
    await parley.connect();
    if (!mounted) return;
    epoch = parley.workspaceEpoch;
    observe();
    void loadReport();
  } catch {
    if (mounted) readError.value = 'Could not connect to the workspace. Reload to try again.';
  }
}

function workspaceChanged() {
  unsubscribe?.();
  reportRequest?.abort();
  report.value = null;
  snapshot.value = null;
  job.value = null;
  pendingAdmission.value = null;
  startKey = undefined;
  cancelKey = undefined;
  starting.value = false;
  cancelling.value = false;
  status.value = '';
  error.value = '';
  void initialize();
}

onMounted(() => {
  mounted = true;
  globalThis.addEventListener('parley:workspace-changed', workspaceChanged);
  void initialize();
});
onBeforeUnmount(() => {
  mounted = false;
  unsubscribe?.();
  reportRequest?.abort();
  globalThis.removeEventListener('parley:workspace-changed', workspaceChanged);
});
</script>

<template>
  <section aria-labelledby="insights-heading">
    <h3 id="insights-heading">
      Weekly Coach and insights
    </h3>
    <p v-if="report">
      Historical first touches · data as of {{ report.as_of }}
    </p>
    <p v-if="snapshot?.period">
      Coach uses the weekly assessment for {{ snapshot.period }}.
    </p>
    <div class="actions">
      <button
        type="button"
        :disabled="loading"
        @click="refresh"
      >
        Refresh report
      </button>
      <button
        ref="startButton"
        type="button"
        :disabled="starting || active || pendingAdmission !== null"
        @click="runCoach"
      >
        Run Coach for this week
      </button>
      <button
        v-if="active"
        ref="cancelButton"
        type="button"
        :aria-disabled="cancelling"
        @click="!cancelling && cancel()"
      >
        Cancel Coach job
      </button>
    </div>
    <p
      v-if="loading"
      role="status"
    >
      Loading read-only report…
    </p>
    <p
      v-if="status"
      role="status"
      aria-live="polite"
    >
      {{ status }}
    </p>
    <p
      v-if="error || readError"
      role="alert"
    >
      {{ error || readError }}
    </p>
    <template v-if="report">
      <h4>Rates</h4>
      <p v-if="!report.rates.length">
        No rate metrics are available for this period.
      </p>
      <table v-else>
        <thead>
          <tr>
            <th scope="col">
              Measure
            </th><th scope="col">
              Count
            </th><th scope="col">
              Rate and uncertainty
            </th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="metric in report.rates"
            :key="metric.metric"
          >
            <th scope="row">
              {{ metric.metric === 'reply_rate' ? 'Reply rate' : 'Advance rate' }}
            </th><td>{{ metric.k }}/{{ metric.n }}</td><td>{{ metricRate(metric) }}</td>
          </tr>
        </tbody>
      </table>
    </template>
    <template v-if="snapshot">
      <h4>Assessment coverage</h4>
      <dl>
        <dt>Current assessments</dt><dd>{{ snapshot.coverage.current }}</dd>
        <dt>Missing assessments</dt><dd>{{ snapshot.coverage.missing }}</dd>
        <dt>Stale assessments</dt><dd>{{ snapshot.coverage.stale }}</dd>
        <dt>Calibrated no-match</dt><dd>{{ snapshot.coverage.no_match }}</dd>
      </dl>
      <ul v-if="snapshot.observations.length">
        <li
          v-for="observation in snapshot.observations"
          :key="JSON.stringify([observation.reason, observation.target])"
        >
          {{ reasons[observation.reason] }} · impact {{ observation.impact }}
        </li>
      </ul>
    </template>
    <CoachQuestions
      :choices="weeklyChoices"
      :active="true"
      :refresh-key="job?.state_revision ?? snapshot?.input_digest ?? ''"
      @open="openTarget"
    />
  </section>
</template>

<style scoped>
.actions { display: flex; flex-wrap: wrap; gap: .5rem; margin-block: .75rem; }
.question { margin-block: .75rem; padding: .75rem; background: #fff; border: 1px solid #cbd5ce; border-radius: .35rem; }
table { border-collapse: collapse; width: 100%; }
th, td { padding: .5rem; border-bottom: 1px solid #cbd5ce; text-align: left; }
</style>
