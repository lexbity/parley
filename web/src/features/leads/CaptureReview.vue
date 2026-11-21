<script setup lang="ts">
import { computed, ref } from 'vue';

export type CaptureJobStatus = 'queued' | 'running' | 'ok' | 'invalid' | 'failed' | 'cancelled' | 'source_only';
export type CaptureProposalAction = 'confirm' | 'correct' | 'dismiss' | 'unresolved';
export type CaptureReviewState = 'proposed' | 'confirmed' | 'corrected' | 'dismissed' | 'unresolved' | 'source_only';

export interface CaptureReviewSource {
  occurrence_id: string;
  content_id: string;
  title: string | null;
  url: string | null;
  text: string;
  raw_sha256: string;
  captured_at: string;
}

export interface CaptureTargetOption {
  id: string;
  kind: 'person' | 'org' | 'project' | 'artifact' | 'capture' | 'resource';
  label: string;
}

export interface CaptureProposal {
  id: string;
  action: 'create' | 'link' | 'update';
  target_kind: string;
  target_id: string | null;
  target_label: string;
  candidate_targets: CaptureTargetOption[];
  statement: string;
  quote: string;
  quote_start: number;
  quote_end: number;
  tier: 1 | 2 | 3;
  ambiguity: string | null;
  unresolved: boolean;
  candidate: Record<string, unknown> | null;
  target_revision: number | null;
  review_state: CaptureReviewState;
}

export interface CaptureReviewBatch {
  batch_id: string;
  revision: number;
  status: CaptureJobStatus;
  job_id?: string;
  source: CaptureReviewSource;
  proposals: CaptureProposal[];
}

export interface CaptureDecision {
  proposal_id: string;
  action: CaptureProposalAction;
  target_id: string | null;
  corrected_statement: string | null;
}

export interface CaptureMutationReceipt {
  batch_id: string;
  revision: number;
  status: CaptureJobStatus;
  committed: Array<{ proposal_id: string; review_state: Exclude<CaptureReviewState, 'proposed'> }>;
  source: CaptureReviewSource;
}

export interface CaptureReviewService {
  stageCapture(input: { text: string; title: string | null; url: string | null; opportunity_ids: string[] }): Promise<CaptureReviewBatch>;
  getCaptureReview(batchId: string): Promise<CaptureReviewBatch>;
  cancelCaptureJob(batchId: string, jobId: string): Promise<CaptureReviewBatch>;
  applyCaptureReview(batchId: string, expectedRevision: number, decisions: CaptureDecision[], idempotencyKey: string): Promise<CaptureMutationReceipt>;
  saveCaptureSourceOnly(batchId: string, expectedRevision: number, idempotencyKey: string): Promise<CaptureMutationReceipt>;
}

const props = defineProps<{
  service: CaptureReviewService;
  opportunities: Array<{ id: string; title: string }>;
}>();

const emit = defineEmits<{
  chat: [context: { batchId: string; occurrenceId: string; proposalId: string; quote: string; targetLabel: string }];
  linked: [batchId: string, occurrenceId: string];
}>();

type EditableDecision = { action: CaptureProposalAction | ''; target_id: string; corrected_statement: string };

const text = ref('');
const title = ref('');
const url = ref('');
const opportunityIds = ref<string[]>([]);
const batch = ref<CaptureReviewBatch | null>(null);
const decisions = ref<Record<string, EditableDecision>>({});
const busy = ref(false);
const error = ref('');
const statusMessage = ref('');
let requestGeneration = 0;

const selectedDecisions = computed(() => Object.entries(decisions.value)
  .filter(([id, decision]) => decision.action !== '' && batch.value?.proposals.some((proposal) => proposal.id === id && proposal.review_state === 'proposed'))
  .map(([proposal_id, decision]): CaptureDecision => ({
    proposal_id,
    action: decision.action as CaptureProposalAction,
    target_id: decision.target_id || null,
    corrected_statement: decision.action === 'correct' ? decision.corrected_statement.trim() : null,
  })));

function decisionFor(proposal: CaptureProposal): EditableDecision {
  return decisions.value[proposal.id] ?? {
    action: '',
    target_id: proposal.target_id ?? '',
    corrected_statement: proposal.statement,
  };
}

function chooseAction(proposal: CaptureProposal, action: CaptureProposalAction | '') {
  decisions.value[proposal.id] = { ...decisionFor(proposal), action };
}

function chooseTarget(proposal: CaptureProposal, targetId: string) {
  decisions.value[proposal.id] = { ...decisionFor(proposal), target_id: targetId };
}

function correctStatement(proposal: CaptureProposal, corrected_statement: string) {
  decisions.value[proposal.id] = { ...decisionFor(proposal), corrected_statement, action: 'correct' };
}

function canApply(decision: CaptureDecision): boolean {
  if (decision.action === 'correct') return Boolean(decision.corrected_statement);
  if (decision.action === 'confirm') return Boolean(decision.target_id) || !batch.value?.proposals.find((item) => item.id === decision.proposal_id)?.candidate_targets.length;
  return true;
}

function newKey(): string {
  return globalThis.crypto.randomUUID();
}

async function stage() {
  error.value = '';
  statusMessage.value = '';
  busy.value = true;
  const generation = ++requestGeneration;
  try {
    const result = await props.service.stageCapture({
      text: text.value,
      title: title.value.trim() || null,
      url: url.value.trim() || null,
      opportunity_ids: [...opportunityIds.value],
    });
    if (generation !== requestGeneration) return;
    batch.value = result;
    decisions.value = {};
    statusMessage.value = result.status === 'failed' || result.status === 'invalid'
      ? 'The source is staged, but extraction needs attention. You can save source only or try again.'
      : 'Source staged. Review each proposal before applying any change.';
    if (result.status === 'ok') text.value = '';
  } catch (cause) {
    if (generation === requestGeneration) error.value = cause instanceof Error ? cause.message : 'Source could not be staged.';
  } finally {
    if (generation === requestGeneration) busy.value = false;
  }
}

async function refreshReview() {
  if (!batch.value) return;
  const batchId = batch.value.batch_id;
  const generation = ++requestGeneration;
  error.value = '';
  busy.value = true;
  try {
    const result = await props.service.getCaptureReview(batchId);
    if (generation === requestGeneration && batch.value?.batch_id === batchId) batch.value = result;
  } catch (cause) {
    if (generation === requestGeneration) error.value = cause instanceof Error ? cause.message : 'Review status could not be refreshed.';
  } finally {
    if (generation === requestGeneration) busy.value = false;
  }
}

async function cancelJob() {
  if (!batch.value || !['queued', 'running'].includes(batch.value.status)) return;
  if (!batch.value.job_id) {
    error.value = 'The extraction job ID is unavailable; refresh its status before cancelling.';
    return;
  }
  const batchId = batch.value.batch_id;
  const generation = ++requestGeneration;
  error.value = '';
  busy.value = true;
  try {
    const result = await props.service.cancelCaptureJob(batchId, batch.value.job_id);
    if (generation === requestGeneration && batch.value?.batch_id === batchId) {
      batch.value = result;
      decisions.value = {};
      statusMessage.value = 'Extraction cancelled. No proposal was applied; the staged source remains available.';
    }
  } catch (cause) {
    if (generation === requestGeneration) error.value = cause instanceof Error ? cause.message : 'Job cancellation could not be confirmed.';
  } finally {
    if (generation === requestGeneration) busy.value = false;
  }
}

async function applySelected() {
  if (!batch.value || !selectedDecisions.value.length) return;
  const invalid = selectedDecisions.value.find((decision) => !canApply(decision));
  if (invalid) {
    error.value = invalid.action === 'correct'
      ? 'Add corrected wording before applying this proposal.'
      : 'Choose a saved target before confirming this proposal.';
    return;
  }
  const current = batch.value;
  error.value = '';
  statusMessage.value = '';
  busy.value = true;
  try {
    const submitted = selectedDecisions.value;
    const result = await props.service.applyCaptureReview(current.batch_id, current.revision, submitted, newKey());
    if (batch.value?.batch_id !== current.batch_id) return;
    const committed = new Map(result.committed.map((item) => [item.proposal_id, item.review_state]));
    batch.value = {
      ...current,
      revision: result.revision,
      status: result.status,
      source: result.source,
      proposals: current.proposals.map((proposal) => ({
        ...proposal,
        review_state: committed.get(proposal.id) ?? proposal.review_state,
      })),
    };
    decisions.value = {};
    statusMessage.value = 'Selected review decisions were saved. Unselected proposals remain pending.';
    emit('linked', current.batch_id, result.source.occurrence_id);
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Review decisions were not saved. Compare the current source and targets.';
  } finally {
    busy.value = false;
  }
}

async function saveSourceOnly() {
  if (!batch.value) return;
  const current = batch.value;
  error.value = '';
  statusMessage.value = '';
  busy.value = true;
  try {
    const receipt = await props.service.saveCaptureSourceOnly(current.batch_id, current.revision, newKey());
    if (batch.value?.batch_id !== current.batch_id) return;
    const committed = new Map(receipt.committed.map((item) => [item.proposal_id, item.review_state]));
    batch.value = {
      ...current,
      revision: receipt.revision,
      status: receipt.status,
      source: receipt.source,
      proposals: current.proposals.map((proposal) => ({
        ...proposal,
        review_state: committed.get(proposal.id) ?? proposal.review_state,
      })),
    };
    statusMessage.value = 'Source saved without applying any proposals.';
    decisions.value = {};
    emit('linked', current.batch_id, receipt.source.occurrence_id);
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Source was not saved. The staged source remains available.';
  } finally {
    busy.value = false;
  }
}

function askInChat(proposal: CaptureProposal) {
  if (!batch.value) return;
  emit('chat', {
    batchId: batch.value.batch_id,
    occurrenceId: batch.value.source.occurrence_id,
    proposalId: proposal.id,
    quote: proposal.quote,
    targetLabel: proposal.target_label,
  });
}
</script>

<template>
  <section
    class="capture-review"
    aria-labelledby="capture-review-title"
  >
    <header>
      <h2 id="capture-review-title">
        Sources inbox
      </h2>
      <p>Paste a source to stage it. Proposals stay separate from saved records until you choose decisions.</p>
    </header>

    <form
      class="capture-form"
      @submit.prevent="stage"
    >
      <label>Source text<textarea
        v-model="text"
        required
        rows="7"
        autocomplete="off"
      /></label>
      <div class="capture-meta">
        <label>Title (optional)<input
          v-model="title"
          maxlength="500"
        ></label>
        <label>Source URL (optional)<input
          v-model="url"
          type="url"
          maxlength="2000"
        ></label>
        <fieldset class="opportunity-options">
          <legend>Link to opportunities (optional)</legend>
          <label
            v-for="opportunity in opportunities"
            :key="opportunity.id"
            class="opportunity-option"
          >
            <input
              v-model="opportunityIds"
              type="checkbox"
              :value="opportunity.id"
            >{{ opportunity.title }}
          </label>
          <small v-if="!opportunityIds.length">No opportunity selected; the source stays independent.</small>
        </fieldset>
      </div>
      <button
        type="submit"
        :disabled="busy || !text.trim()"
      >
        Stage source for review
      </button>
    </form>

    <p
      v-if="error"
      role="alert"
    >
      {{ error }}
    </p>
    <p
      v-if="statusMessage"
      role="status"
    >
      {{ statusMessage }}
    </p>

    <section
      v-if="batch"
      class="staged-source"
      aria-label="Staged source review"
    >
      <header>
        <div>
          <h3>{{ batch.source.title || 'Untitled source' }}</h3>
          <p>Stage {{ batch.batch_id }} · revision {{ batch.revision }} · {{ batch.source.captured_at }}</p>
        </div>
        <span
          class="job-status"
          :data-status="batch.status"
        >Extraction {{ batch.status }}</span>
      </header>
      <p v-if="batch.source.url">
        {{ batch.source.url }}
      </p>
      <details>
        <summary>Inspect original source ({{ batch.source.raw_sha256.slice(0, 12) }}…)</summary>
        <pre>{{ batch.source.text }}</pre>
      </details>

      <div
        v-if="['queued', 'running'].includes(batch.status)"
        class="job-actions"
      >
        <button
          type="button"
          :disabled="busy"
          @click="refreshReview"
        >
          Refresh extraction status
        </button>
        <button
          type="button"
          :disabled="busy"
          @click="cancelJob"
        >
          Cancel extraction
        </button>
      </div>

      <p
        v-if="['invalid', 'failed', 'cancelled'].includes(batch.status)"
        class="manual-path"
      >
        Extraction did not produce a reviewable result. The original source is preserved; save it alone or paste it into chat for manual review.
      </p>

      <section
        v-if="batch.proposals.length"
        class="proposal-list"
        aria-label="Proposed changes"
      >
        <h3>Proposed changes</h3>
        <article
          v-for="proposal in batch.proposals"
          :key="proposal.id"
          class="proposal-card"
          :data-proposal-id="proposal.id"
        >
          <header>
            <h4>{{ proposal.action }} · {{ proposal.target_kind }}</h4>
            <span class="tier">T{{ proposal.tier }}</span>
          </header>
          <p><strong>Target:</strong> {{ proposal.target_label }}<span v-if="proposal.unresolved"> · identity unresolved</span></p>
          <blockquote>{{ proposal.quote }}</blockquote>
          <small>Source bytes {{ proposal.quote_start }}–{{ proposal.quote_end }} · target revision {{ proposal.target_revision ?? 'new/unresolved' }}</small>
          <p class="proposal-text">
            {{ proposal.statement }}
          </p>
          <p
            v-if="proposal.review_state !== 'proposed'"
            class="decision-state"
            role="status"
          >
            Owner decision saved: {{ proposal.review_state }}
          </p>
          <p
            v-if="proposal.ambiguity"
            class="ambiguity"
          >
            <strong>Ambiguity:</strong> {{ proposal.ambiguity }}
          </p>
          <template v-if="proposal.review_state === 'proposed'">
            <label v-if="proposal.candidate_targets.length">Link to existing record<select
              :value="decisionFor(proposal).target_id"
              @change="chooseTarget(proposal, ($event.target as HTMLSelectElement).value)"
            >
              <option value="">Leave target unresolved</option>
              <option
                v-for="target in proposal.candidate_targets"
                :key="target.id"
                :value="target.id"
              >{{ target.label }} · {{ target.kind }}</option>
            </select></label>
            <label>Owner decision<select
              :value="decisionFor(proposal).action"
              @change="chooseAction(proposal, ($event.target as HTMLSelectElement).value as CaptureProposalAction | '')"
            >
              <option value="">Leave pending</option>
              <option value="confirm">Confirm selected proposal</option>
              <option value="correct">Correct before saving</option>
              <option value="dismiss">Dismiss proposal</option>
              <option value="unresolved">Keep identity unresolved</option>
            </select></label>
            <label v-if="decisionFor(proposal).action === 'correct'">Corrected wording<input
              :value="decisionFor(proposal).corrected_statement"
              @input="correctStatement(proposal, ($event.target as HTMLInputElement).value)"
            ></label>
            <button
              type="button"
              @click="askInChat(proposal)"
            >
              Ask about this proposal in chat
            </button>
          </template>
        </article>
      </section>
      <p v-else-if="batch.status === 'ok'">
        No proposals were extracted. The source can still be saved independently.
      </p>

      <footer class="review-actions">
        <button
          type="button"
          :disabled="busy || !selectedDecisions.length || batch.status !== 'ok'"
          @click="applySelected"
        >
          Apply selected decisions
        </button>
        <button
          type="button"
          :disabled="busy || batch.status === 'source_only'"
          @click="saveSourceOnly"
        >
          Save source only
        </button>
      </footer>
    </section>
  </section>
</template>

<style scoped>
.capture-review{display:grid;gap:1rem}.capture-form,.staged-source,.proposal-card{padding:1rem;border:1px solid #cbd5cf;border-radius:.5rem;background:#fff}.capture-form label,.proposal-card label{display:grid;gap:.35rem;margin:.75rem 0}.capture-meta{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:.75rem}.staged-source>header,.proposal-card>header{display:flex;align-items:flex-start;justify-content:space-between;gap:1rem}.job-status,.tier{font-weight:700;white-space:nowrap}.job-status[data-status=failed],.job-status[data-status=invalid]{color:#8b2c22}.proposal-list{display:grid;gap:.75rem}.proposal-card blockquote{margin:.75rem 0;padding-left:.75rem;border-left:3px solid #62796d}.ambiguity{padding:.5rem;background:#fff4d6}.staged-source pre{white-space:pre-wrap;overflow-wrap:anywhere;max-height:24rem;overflow:auto}.review-actions,.job-actions{display:flex;flex-wrap:wrap;gap:.5rem;margin-top:1rem}button{min-height:2.75rem}@media(max-width:500px){.staged-source>header,.proposal-card>header{display:block}}
</style>
