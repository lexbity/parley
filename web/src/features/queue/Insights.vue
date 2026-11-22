<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { parley, type InsightsReport } from '../../api/client';

defineOptions({ name: 'QueueInsightsView' });

const emit = defineEmits<{ navigate: [path: string] }>();
const report = ref<InsightsReport | null>(null);
const loading = ref(false);
const running = ref(false);
const error = ref('');
const status = ref('');
const activeJobId = ref('');
let pollTimer: ReturnType<typeof globalThis.setTimeout> | undefined;
let generation = 0;
const openQuestions = computed(() => (report.value?.questions ?? []).filter((item) => item.state === 'open'));

async function load() {
  const current = ++generation;
  loading.value = true; error.value = '';
  try { await parley.connect(); const next = await parley.getInsights(); if (current === generation) report.value = next; }
  catch (reason) { if (current === generation) error.value = reason instanceof Error ? reason.message : 'Could not load server-computed insights.'; }
  finally { if (current === generation) loading.value = false; }
}

async function runCoach() {
  running.value = true; error.value = ''; status.value = '';
  try {
    const job = await parley.runCoach();
    activeJobId.value = job.job_id;
    status.value = 'Coach run queued. Its proposed wording is not saved as a fact until owner review.';
    await pollCoach(job.job_id, ++generation);
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'Coach is unavailable. Existing reports and manual work remain available.';
    running.value = false;
  }
}

async function pollCoach(jobId: string, runGeneration: number) {
  try {
    const job = await parley.getRoleJob(jobId);
    if (runGeneration !== generation || activeJobId.value !== jobId) return;
    if (job.status === 'succeeded' || job.status === 'ok') {
      await load();
      status.value = 'Coach report refreshed from the committed run.';
      activeJobId.value = ''; running.value = false;
      return;
    }
    if (!['queued', 'running'].includes(job.status)) {
      error.value = job.last_error ?? `Coach run ended as ${job.status}. The existing report remains available.`;
      activeJobId.value = ''; running.value = false;
      return;
    }
    pollTimer = globalThis.setTimeout(() => { void pollCoach(jobId, runGeneration); }, 1000);
  } catch (reason) {
    if (runGeneration !== generation) return;
    error.value = reason instanceof Error ? reason.message : 'Coach status is temporarily unavailable. Reopen this view to reconcile the durable job.';
    activeJobId.value = ''; running.value = false;
  }
}

async function dismiss(questionId: string) {
  error.value = ''; status.value = '';
  try {
    await parley.decideCoachQuestion(questionId);
    await load();
    status.value = 'Question dismissed. It will not be proposed again for this scope.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not record this question disposition.'; }
}

function openTarget(kind: string, id: string) {
  const path = kind === 'opportunity' ? `/queue/${encodeURIComponent(id)}` : kind === 'party' ? `/leads/${encodeURIComponent(id)}` : '/resources';
  emit('navigate', path);
}

function metricRate(metric: InsightsReport['metrics'][number]): string {
  if (metric.denominator === 0 || metric.state === 'insufficient_observations') return 'Insufficient observations';
  const rate = `${(metric.rate! * 100).toFixed(1)}%`;
  if (metric.lower === null || metric.upper === null) return rate;
  return `${rate} · 95% Wilson interval ${(metric.lower * 100).toFixed(1)}–${(metric.upper * 100).toFixed(1)}%`;
}

function cancelWait() {
  generation += 1;
  if (pollTimer) globalThis.clearTimeout(pollTimer);
  pollTimer = undefined; running.value = false;
  status.value = 'Stopped waiting for the Coach result. The server job continues; refresh to reconcile it.';
}

onMounted(() => { void load(); });
onBeforeUnmount(() => { generation += 1; if (pollTimer) globalThis.clearTimeout(pollTimer); });
</script>

<template>
  <section aria-labelledby="insights-heading">
    <h3 id="insights-heading">
      Weekly Coach and insights
    </h3>
    <p v-if="report">
      Week {{ report.period_start }}–{{ report.period_end }} · pack {{ report.pack_id }} v{{ report.pack_version }} · {{ report.sample_size }} sends in this report · data as of {{ report.as_of }}
    </p>
    <div class="actions">
      <button
        type="button"
        :disabled="loading || running"
        @click="load"
      >
        Refresh report
      </button>
      <button
        type="button"
        :disabled="loading || running"
        @click="runCoach"
      >
        Run Coach for this week
      </button>
      <button
        v-if="running"
        type="button"
        @click="cancelWait"
      >
        Stop waiting
      </button>
    </div>
    <p
      v-if="loading"
      role="status"
    >
      Loading read-only report…
    </p>
    <p
      v-if="running"
      role="status"
    >
      Coach is {{ activeJobId ? 'running as a durable job' : 'being queued' }}. Queue and report reads do not start additional model work.
    </p>
    <p
      v-if="status"
      role="status"
      aria-live="polite"
    >
      {{ status }}
    </p>
    <p
      v-if="error"
      role="alert"
    >
      {{ error }}
    </p>
    <template v-if="report">
      <h4>Rates</h4>
      <p v-if="!report.metrics.length">
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
            v-for="metric in report.metrics"
            :key="metric.id"
          >
            <th scope="row">
              {{ metric.label }}
            </th><td>{{ metric.numerator }}/{{ metric.denominator }}</td><td>{{ metricRate(metric) }}</td>
          </tr>
        </tbody>
      </table>
      <h4>Comparisons</h4>
      <ul v-if="report.comparisons.length">
        <li
          v-for="comparison in report.comparisons"
          :key="comparison.label"
        >
          {{ comparison.label }} · {{ comparison.state }} · {{ comparison.left.numerator }}/{{ comparison.left.denominator }} vs {{ comparison.right.numerator }}/{{ comparison.right.denominator }}
        </li>
      </ul>
      <p v-else>
        No eligible comparisons yet; small samples remain inconclusive.
      </p>
      <h4>Coverage gaps</h4>
      <ul v-if="report.gaps.length">
        <li
          v-for="gap in report.gaps"
          :key="gap.id"
        >
          {{ gap.label }} · {{ gap.covered }}/{{ gap.total }} covered · {{ gap.blocked_count }} blocked · consider {{ gap.suggested_kind }}
        </li>
      </ul>
      <p v-else>
        No coverage gaps are reported for this snapshot.
      </p>
      <h4>Open coaching questions</h4>
      <p v-if="!openQuestions.length">
        No open coaching questions.
      </p>
      <article
        v-for="question in openQuestions"
        :key="question.id"
        class="question"
      >
        <p>{{ question.text }}</p>
        <small>{{ question.kind }} · impact {{ question.impact }} · owner review required</small>
        <div class="actions">
          <button
            type="button"
            @click="openTarget(question.target_kind, question.target_id)"
          >
            Open related work
          </button><button
            type="button"
            @click="dismiss(question.id)"
          >
            Dismiss question
          </button>
        </div>
      </article>
    </template>
  </section>
</template>

<style scoped>
.actions { display: flex; flex-wrap: wrap; gap: .5rem; margin-block: .75rem; }
.question { margin-block: .75rem; padding: .75rem; background: #fff; border: 1px solid #cbd5ce; border-radius: .35rem; }
table { border-collapse: collapse; width: 100%; }
th, td { padding: .5rem; border-bottom: 1px solid #cbd5ce; text-align: left; }
</style>
