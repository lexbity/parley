<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { ApiError, newOperationKey, parley, type CoachContextRequest, type CoachQuestion, type CoachSelector } from '../../api/client';

type ScopeChoice = { selector: CoachSelector; label: string };
type EvidenceChoice = { claim_id: string; revision: number; label: string };
type ResourceChoice = { id: string; label: string };

const props = withDefaults(defineProps<{
  choices: ScopeChoice[];
  active?: boolean;
  refreshKey?: string | number;
  evidenceChoices?: EvidenceChoice[];
  resourceChoices?: ResourceChoice[];
}>(), { active: true, refreshKey: '', evidenceChoices: () => [], resourceChoices: () => [] });
const emit = defineEmits<{ open: [question: CoachQuestion] }>();

const selected = ref('');
const selectedEvidence = ref<string[]>([]);
const selectedResources = ref<string[]>([]);
const eligibleResources = ref<ResourceChoice[]>([]);
const questions = ref<CoachQuestion[]>([]);
const error = ref('');
const status = ref('');
const answerDrafts = ref<Record<string, string>>({});
const busy = ref(false);
const generation = ref(0);
let request: globalThis.AbortController | undefined;

const choice = computed(() => props.choices.find(item => JSON.stringify(item.selector) === selected.value) ?? props.choices[0]);
const contextual = computed(() => choice.value?.selector.kind !== 'weekly');
const title = computed(() => choice.value?.label ?? 'Selected context');
const resources = computed(() => props.resourceChoices.length ? props.resourceChoices : eligibleResources.value);
function filter() {
  const scope = choice.value?.selector;
  if (!scope) return {};
  return { scope_kind: scope.kind, ...(scope.kind === 'weekly' ? { scope_period: scope.period } : {}), ...(scope.kind === 'party' || scope.kind === 'opportunity' ? { scope_id: scope.id } : {}), ...(scope.kind === 'opportunity' ? { recipient_id: scope.recipient_id } : {}) };
}

async function refresh() {
  request?.abort();
  const active = new globalThis.AbortController();
  request = active;
  const ownGeneration = generation.value;
  try {
    const rows = await parley.listCoachQuestions(filter(), active.signal);
    if (!active.signal.aborted && ownGeneration === generation.value && props.active) questions.value = rows;
  } catch {
    if (!active.signal.aborted && ownGeneration === generation.value && props.active) error.value = 'Could not load questions for this selection.';
  }
}

async function loadEligibleResources() {
  eligibleResources.value = [];
  const scope = choice.value?.selector;
  if (scope?.kind !== 'opportunity') return;
  try {
    const match = await parley.getWritingMatch(scope.id, scope.recipient_id);
    const shortlist: unknown = JSON.parse(match.shortlist_json);
    const root = shortlist && typeof shortlist === 'object' && !Array.isArray(shortlist) ? shortlist as Record<string, unknown> : null;
    const results = Array.isArray(root?.results) ? root.results : [];
    const items = results.flatMap((result) => {
      if (!result || typeof result !== 'object' || Array.isArray(result)) return [];
      const candidates = (result as Record<string, unknown>).candidates;
      return Array.isArray(candidates) ? candidates : [];
    });
    const seen = new Set<string>();
    eligibleResources.value = items.flatMap((raw) => {
      if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return [];
      const item = raw as Record<string, unknown>;
      if (typeof item.id !== 'string' || seen.has(item.id)) return [];
      seen.add(item.id);
      return [{ id: item.id, label: typeof item.title === 'string' ? item.title : item.id }];
    }).slice(0, 8);
  } catch {
    // An absent, stale or uncalibrated shortlist offers no resource suggestions.
  }
}

async function generate() {
  const scope = choice.value?.selector;
  if (!scope || scope.kind === 'weekly' || busy.value) return;
  const frozen: CoachContextRequest = {
    scope,
    evidence: [...selectedEvidence.value].map(key => {
      const item = props.evidenceChoices.find(candidate => `${candidate.claim_id}:${candidate.revision}` === key);
      return item ? { claim_id: item.claim_id, revision: item.revision } : null;
    }).filter((item): item is NonNullable<typeof item> => item !== null),
    resource_ids: [...selectedResources.value],
  };
  error.value = ''; status.value = ''; busy.value = true;
  try {
    const result = await parley.startContextCoach(frozen, newOperationKey());
    status.value = result.kind === 'queued' ? `Questions queued for ${title.value}.` : `Current assessment refreshed for ${title.value}.`;
    generation.value++;
    await refresh();
  } catch (cause) {
    error.value = cause instanceof ApiError && cause.code === 'STALE_INPUT'
      ? 'This selection changed. Refresh the party or opportunity and review its current revisions.'
      : cause instanceof Error ? cause.message : 'Coach could not start for this selection.';
  } finally { busy.value = false; }
}

async function dispose(question: CoachQuestion, action: 'dismiss' | 'defer') {
  if (busy.value) return;
  busy.value = true; error.value = ''; status.value = '';
  try {
    if (action === 'dismiss') await parley.dismissCoachQuestion(question);
    else await parley.deferCoachQuestion(question);
    status.value = action === 'dismiss' ? 'Question dismissed.' : 'Question deferred for seven days.';
    await refresh();
  } catch (cause) {
    error.value = cause instanceof ApiError && cause.code === 'QUESTION_STALE'
      ? 'This question changed in another session. Refresh it before choosing an action.'
      : 'Could not record the question disposition.';
  } finally { busy.value = false; }
}

async function answer(question: CoachQuestion) {
  const text = answerDrafts.value[question.id]?.trim() ?? '';
  if (!text || busy.value) return;
  busy.value = true; error.value = ''; status.value = '';
  try {
    const answer = question.action === 'interviewer' ? { text } : { note: text };
    const result = await parley.answerCoachQuestion(question.id, {
      answer,
      expected_revision: question.revision,
      question_key: question.question_key,
    });
    status.value = result.status === 'answer_pending'
      ? 'Answer added to Interviewer. Review its candidate before this question is resolved.'
      : 'Answer recorded for this question.';
    answerDrafts.value[question.id] = '';
    await refresh();
  } catch (cause) {
    error.value = cause instanceof ApiError && cause.code === 'QUESTION_STALE'
      ? 'This question changed in another session. Refresh it before answering.'
      : cause instanceof ApiError && cause.code === 'IDENTITY_INTERVIEW_UNAVAILABLE'
        ? 'Add or update this identity detail in the typed identity editor.'
        : 'Could not record the answer for this question.';
  } finally { busy.value = false; }
}

watch(() => props.choices.map(item => JSON.stringify(item.selector)).join('|'), () => {
  selected.value = props.choices[0] ? JSON.stringify(props.choices[0].selector) : '';
  selectedEvidence.value = []; selectedResources.value = []; generation.value++;
  if (props.active) void refresh();
});
watch(() => props.active, active => { if (active) void refresh(); else { generation.value++; request?.abort(); } });
watch(() => props.refreshKey, () => { if (props.active) void refresh(); });
watch(choice, () => { selectedEvidence.value = []; selectedResources.value = []; eligibleResources.value = []; if (props.active) { void refresh(); void loadEligibleResources(); } });
onMounted(() => { if (!selected.value && props.choices[0]) selected.value = JSON.stringify(props.choices[0].selector); if (props.active) { void refresh(); void loadEligibleResources(); } });
onBeforeUnmount(() => { generation.value++; request?.abort(); });
</script>

<template>
  <section
    class="coach-questions"
    aria-labelledby="coach-questions-title"
  >
    <div class="heading">
      <div>
        <h3 id="coach-questions-title">
          Coach questions
        </h3><p>For {{ title }}. Questions and actions stay tied to the revisions shown here.</p>
      </div>
      <button
        type="button"
        :disabled="busy"
        @click="refresh"
      >
        Refresh
      </button>
    </div>
    <label
      v-if="choices.length > 1"
      class="scope-picker"
    >Question scope
      <select v-model="selected">
        <option
          v-for="item in choices"
          :key="JSON.stringify(item.selector)"
          :value="JSON.stringify(item.selector)"
        >{{ item.label }}</option>
      </select>
    </label>
    <fieldset
      v-if="contextual && evidenceChoices.length"
      :disabled="busy"
    >
      <legend>Use selected evidence (up to eight)</legend>
      <label
        v-for="item in evidenceChoices.slice(0, 8)"
        :key="`${item.claim_id}:${item.revision}`"
      >
        <input
          v-model="selectedEvidence"
          type="checkbox"
          :value="`${item.claim_id}:${item.revision}`"
        > {{ item.label }}
      </label>
    </fieldset>
    <fieldset
      v-if="contextual && resources.length"
      :disabled="busy"
    >
      <legend>Suggest from selected eligible resources (up to eight)</legend>
      <label
        v-for="item in resources.slice(0, 8)"
        :key="item.id"
      >
        <input
          v-model="selectedResources"
          type="checkbox"
          :value="item.id"
        > {{ item.label }}
      </label>
    </fieldset>
    <p v-if="contextual">
      <button
        type="button"
        :disabled="busy || !choice"
        @click="generate"
      >
        {{ busy ? 'Working…' : 'Find questions for this selection' }}
      </button>
    </p>
    <p
      v-if="status"
      role="status"
    >
      {{ status }}
    </p>
    <p
      v-if="error"
      role="alert"
    >
      {{ error }}
    </p>
    <p v-if="!questions.length && !error">
      No current questions for this selection.
    </p>
    <article
      v-for="question in questions"
      :key="question.id"
      class="question"
    >
      <p>{{ question.text }}</p>
      <small>{{ question.kind.replaceAll('_', ' ') }} · impact {{ question.impact }} · {{ question.status.replaceAll('_', ' ') }}</small>
      <p v-if="question.suggested_resource_ref">
        Suggested selected resource: {{ question.suggested_resource_ref }}
      </p>
      <form
        v-if="question.status === 'open'"
        class="answer-form"
        @submit.prevent="answer(question)"
      >
        <label :for="`coach-answer-${question.id}`">Your answer</label>
        <textarea
          :id="`coach-answer-${question.id}`"
          v-model="answerDrafts[question.id]"
          :disabled="busy"
          maxlength="12000"
          rows="2"
        />
        <button
          type="submit"
          :disabled="busy || !answerDrafts[question.id]?.trim()"
        >
          {{ question.action === 'interviewer' ? 'Send to Interviewer' : 'Record answer' }}
        </button>
      </form>
      <div class="actions">
        <button
          type="button"
          @click="emit('open', question)"
        >
          Open related work
        </button>
        <button
          v-if="question.status === 'open'"
          type="button"
          :disabled="busy"
          @click="dispose(question, 'defer')"
        >
          Defer 7 days
        </button>
        <button
          v-if="question.status === 'open'"
          type="button"
          :disabled="busy"
          aria-label="Dismiss question"
          @click="dispose(question, 'dismiss')"
        >
          Dismiss question
        </button>
      </div>
    </article>
  </section>
</template>

<style scoped>
.heading,.actions { display:flex; flex-wrap:wrap; align-items:center; justify-content:space-between; gap:.5rem; }
.coach-questions { display:grid; gap:.75rem; }
.coach-questions h3,.coach-questions p { margin:.25rem 0; }
.coach-questions fieldset { display:flex; flex-wrap:wrap; gap:.5rem 1rem; border:1px solid #cbd5ce; }
.coach-questions fieldset legend { font-weight:600; }
.question { padding:.75rem; background:#fff; border:1px solid #cbd5ce; border-radius:.35rem; }
.question small { display:block; margin:.25rem 0 .65rem; }
.answer-form { display:grid; gap:.35rem; margin:.75rem 0; }
.answer-form textarea { width:100%; min-height:4rem; }
.scope-picker select { display:block; max-width:100%; }
</style>
