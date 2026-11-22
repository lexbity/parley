<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { parley, type Opportunity } from '../../api/client';
import CaptureReview, {
  type CaptureMutationReceipt,
  type CaptureReviewBatch,
  type CaptureReviewService,
} from './CaptureReview.vue';

const opportunities = ref<Opportunity[]>([]);
const error = ref('');
const context = ref<{ quote: string; targetLabel: string } | null>(null);

function jobStatus(status: string): CaptureReviewBatch['status'] {
  if (status === 'queued' || status === 'running' || status === 'invalid' || status === 'failed' || status === 'cancelled' || status === 'source_only') return status;
  return 'ok';
}

function review(value: Awaited<ReturnType<typeof parley.getCaptureReview>>): CaptureReviewBatch {
  return {
    batch_id: value.batch_id,
    job_id: value.job_id,
    revision: value.revision,
    status: jobStatus(value.status),
    injection_suspected: value.injection_suspected,
    source: value.source,
    proposals: value.proposals.map((proposal) => ({
      ...proposal,
      target_label: proposal.target_label,
      candidate_targets: proposal.candidate_targets,
      quote_start: proposal.quote_start ?? 0,
      quote_end: proposal.quote_end ?? 0,
    })),
  };
}

function receipt(value: Awaited<ReturnType<typeof parley.saveCaptureSourceOnly>>): CaptureMutationReceipt {
  return {
    ...value,
    status: jobStatus(value.status),
    committed: value.committed.map((item) => ({
      proposal_id: item.proposal_id,
      review_state: item.review_state,
    })),
  };
}

const service: CaptureReviewService = {
  async stageCapture(input) {
    return review(await parley.stageCapture({
      ...input,
      actor_kind: 'unknown',
      actor_entity_id: null,
    }));
  },
  async getCaptureReview(batchId) {
    return review(await parley.getCaptureReview(batchId));
  },
  async cancelCaptureJob(batchId, jobId) {
    return review(await parley.cancelCaptureJob(batchId, jobId));
  },
  async applyCaptureReview(batchId, expectedRevision, decisions, idempotencyKey) {
    return receipt(await parley.applyCaptureReview(batchId, expectedRevision, decisions, idempotencyKey));
  },
  async saveCaptureSourceOnly(batchId, expectedRevision, idempotencyKey) {
    return receipt(await parley.saveCaptureSourceOnly(batchId, expectedRevision, idempotencyKey));
  },
};

onMounted(() => {
  void parley.connect().then(async () => {
    opportunities.value = (await parley.listOpportunities()).filter((item) => item.status === 'open');
  }).catch((cause: unknown) => {
    error.value = cause instanceof Error ? cause.message : 'Workspace opportunities could not be loaded.';
  });
});
</script>

<template>
  <section aria-label="Capture inbox workspace">
    <p
      v-if="error"
      role="alert"
    >
      {{ error }}
    </p>
    <p v-if="!opportunities.length && !error">
      Sources can be saved independently. Create an opportunity in Queue to make a source association available.
    </p>
    <CaptureReview
      :service="service"
      :opportunities="opportunities.map(({ id, title }) => ({ id, title }))"
      @chat="context = { quote: $event.quote, targetLabel: $event.targetLabel }"
    />
    <aside
      v-if="context"
      aria-label="Chat context"
    >
      <h2>Review context for chat</h2>
      <p>Unresolved proposal: {{ context.targetLabel }}</p>
      <blockquote>{{ context.quote }}</blockquote>
      <p>This question stays a proposal. No identity or graph link has been saved.</p>
      <button
        type="button"
        @click="context = null"
      >
        Close context
      </button>
    </aside>
  </section>
</template>
