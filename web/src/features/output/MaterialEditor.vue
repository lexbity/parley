<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { newOperationKey as createOperationKey } from '../../api/client';

defineOptions({ name: 'MaterialEditor' });

export type MaterialRecipient = { id: string; kind: 'person' | 'org'; name: string };
export type MaterialRecord = {
  id: string;
  kind: 'outreach' | 'follow_up' | 'handoff' | 'attachment';
  versionId: string;
  revision: number;
  body: string;
  status: 'draft' | 'approved' | 'retired';
  updatedAt: string;
  sentences?: MaterialSentence[];
  slotFills?: Record<string, string>;
  protectedTokens?: string[];
};
export type MaterialClaim = {
  type: 'self_fact' | 'other_fact' | 'offer' | 'question' | 'ask' | 'generic';
  ref_kind?: string;
  ref_id?: string;
};
export type MaterialSentence = { text: string; claims: MaterialClaim[]; rules: string[] };
export type ClaimReference = { kind: string; id: string; label: string; tier: number };
export type GateFailure = { id: string; detail: string };
export type MaterialGate = {
  evaluatedAt: string;
  hardFailures: GateFailure[];
  softFailures: GateFailure[];
  copyEnabled: boolean;
};
export type MaterialApproval = { versionId: string; recipientId: string; approvedAt: string };
export type CurrentRelease = {
  grantId: string;
  versionId: string;
  recipientId: string;
  digest: string;
  body: string;
  acknowledgementState?: 'ready' | 'unknown' | 'recorded' | 'failed';
};

const props = withDefaults(defineProps<{
  opportunityId: string;
  opportunityTitle: string;
  recipient: MaterialRecipient;
  material: MaterialRecord;
  gate: MaterialGate;
  claimReferences?: ClaimReference[];
  approval: MaterialApproval | null;
  currentRelease: CurrentRelease | null;
  busy?: 'save' | 'approve' | 'copy' | null;
  operationError?: string | null;
}>(), { claimReferences: () => [], busy: null, operationError: null });

const emit = defineEmits<{
  saveVersion: [input: { materialId: string; expectedRevision: number; body: string; sentences: MaterialSentence[]; slotFills: Record<string, string>; protectedTokens: string[] }];
  approve: [input: { materialId: string; versionId: string; expectedRevision: number; overrideReasons: Record<string, string> }];
  requestCopy: [input: { materialId: string; versionId: string; recipientId: string }];
  acknowledgeCopy: [input: { grantId: string; success: boolean; operationKey: string }];
}>();

type Conflict = { baseBody: string; body: string; revision: number; versionId: string };
type CopyIntent = { grantId: string; success: boolean; operationKey: string };

function cloneSentence(sentence: MaterialSentence): MaterialSentence {
  return { text: sentence.text, claims: sentence.claims.map((claim) => ({ ...claim })), rules: [...sentence.rules] };
}

const baseBody = ref(props.material.body);
const body = ref(props.material.body);
const baseRevision = ref(props.material.revision);
const baseVersionId = ref(props.material.versionId);
const conflict = ref<Conflict | null>(null);
const overrideReasons = ref<Record<string, string>>({});
const sentenceAnnotations = ref<MaterialSentence[]>(props.material.sentences?.map(cloneSentence) ?? []);
const slotEntries = ref(Object.entries(props.material.slotFills ?? {}).map(([name, value]) => ({ name, value })));
const copyIntent = ref<CopyIntent | null>(null);
const localCopyState = ref<'idle' | 'writing' | 'ack-pending' | 'copied' | 'failed'>('idle');

const dirty = computed(() => body.value !== baseBody.value);
const approvalMatches = computed(() => props.approval?.versionId === props.material.versionId
  && props.approval.recipientId === props.recipient.id);
const releaseMatches = computed(() => props.currentRelease?.versionId === props.material.versionId
  && props.currentRelease.recipientId === props.recipient.id
  && props.approval?.versionId === props.material.versionId
  && props.approval.recipientId === props.recipient.id);
const hasOverrides = computed(() => props.gate.softFailures.every((failure) =>
  (overrideReasons.value[failure.id] ?? '').trim().length >= 10));
const canApprove = computed(() => props.gate.hardFailures.length === 0 && hasOverrides.value
  && !dirty.value && !conflict.value && !props.busy);
const canRequestCopy = computed(() => props.gate.copyEnabled && approvalMatches.value
  && !dirty.value && !conflict.value && !props.busy);
const gateReasonIds = computed(() => new Set([
  ...props.gate.hardFailures.map((failure) => failure.id),
  ...props.gate.softFailures.map((failure) => failure.id),
]));

function splitSentences(value: string): string[] {
  return (value.match(/[^.!?]+[.!?]+["')\]]*|[^.!?]+$/g) ?? [])
    .map((sentence) => sentence.trim())
    .filter(Boolean);
}

watch(body, (next) => {
  const old = sentenceAnnotations.value;
  const stored = props.material.sentences ?? [];
  sentenceAnnotations.value = splitSentences(next).map((text, index) => {
    const prior = old[index]?.text === text ? old[index] : stored.find((sentence) => sentence.text === text);
    return prior ? { text, claims: prior.claims.map((claim) => ({ ...claim })), rules: [...prior.rules] } : { text, claims: [], rules: [] };
  });
}, { immediate: true });

watch(() => props.material, (next) => {
  if (!dirty.value) {
    sentenceAnnotations.value = next.sentences?.map(cloneSentence) ?? [];
    slotEntries.value = Object.entries(next.slotFills ?? {}).map(([name, value]) => ({ name, value }));
  }
}, { deep: false });

watch(() => props.material, (next) => {
  if (next.versionId === baseVersionId.value && next.revision === baseRevision.value) {
    if (!dirty.value) {
      baseBody.value = next.body;
      body.value = next.body;
    }
    return;
  }
  if (dirty.value && next.body === body.value) {
    baseBody.value = next.body;
    body.value = next.body;
    baseRevision.value = next.revision;
    baseVersionId.value = next.versionId;
    conflict.value = null;
    return;
  }
  if (dirty.value) {
    conflict.value = { baseBody: baseBody.value, body: next.body, revision: next.revision, versionId: next.versionId };
    return;
  }
  baseBody.value = next.body;
  body.value = next.body;
  baseRevision.value = next.revision;
  baseVersionId.value = next.versionId;
  conflict.value = null;
}, { deep: false });

watch(gateReasonIds, (ids) => {
  overrideReasons.value = Object.fromEntries(Object.entries(overrideReasons.value).filter(([id]) => ids.has(id)));
});

watch(() => props.currentRelease, (release) => {
  if (!release || copyIntent.value?.grantId !== release.grantId) return;
  if (release.acknowledgementState === 'recorded') {
    localCopyState.value = copyIntent.value.success ? 'copied' : 'failed';
  } else if (release.acknowledgementState === 'failed') {
    localCopyState.value = 'failed';
  } else if (release.acknowledgementState === 'unknown' && copyIntent.value.success) {
    localCopyState.value = 'ack-pending';
  }
});

watch(() => [props.material.versionId, props.recipient.id], () => {
  copyIntent.value = null;
  localCopyState.value = 'idle';
});

function saveVersion() {
  emit('saveVersion', {
    materialId: props.material.id,
    expectedRevision: baseRevision.value,
    body: body.value,
    sentences: sentenceAnnotations.value.map((sentence) => ({
      text: sentence.text,
      claims: sentence.claims.map((claim) => ({ ...claim })),
      rules: [...sentence.rules],
    })),
    slotFills: Object.fromEntries(slotEntries.value.filter((slot) => slot.name.trim()).map((slot) => [slot.name.trim(), slot.value])),
    protectedTokens: [...(props.material.protectedTokens ?? [])],
  });
}

function referenceKey(claim: MaterialClaim): string {
  return claim.ref_kind && claim.ref_id ? claim.ref_kind + ':' + claim.ref_id : '';
}

function setClaimReference(claim: MaterialClaim, key: string) {
  const separator = key.indexOf(':');
  if (separator < 0) {
    delete claim.ref_kind;
    delete claim.ref_id;
    return;
  }
  claim.ref_kind = key.slice(0, separator);
  claim.ref_id = key.slice(separator + 1);
}

function addClaim(sentence: MaterialSentence) {
  sentence.claims.push({ type: 'question' });
}

function addSlot() {
  slotEntries.value.push({ name: '', value: '' });
}

function acceptCurrentVersion() {
  if (!conflict.value) return;
  baseBody.value = conflict.value.body;
  body.value = conflict.value.body;
  baseRevision.value = conflict.value.revision;
  baseVersionId.value = conflict.value.versionId;
  conflict.value = null;
}

function reapplyMyTextAsNewVersion() {
  if (!conflict.value) return;
  baseBody.value = conflict.value.body;
  baseRevision.value = conflict.value.revision;
  baseVersionId.value = conflict.value.versionId;
  conflict.value = null;
  saveVersion();
}

function approve() {
  const reasons = Object.fromEntries(props.gate.softFailures.map((failure) => [failure.id, overrideReasons.value[failure.id] ?? '']));
  emit('approve', {
    materialId: props.material.id,
    versionId: props.material.versionId,
    expectedRevision: props.material.revision,
    overrideReasons: reasons,
  });
}

function requestCopy() {
  emit('requestCopy', {
    materialId: props.material.id,
    versionId: props.material.versionId,
    recipientId: props.recipient.id,
  });
}

function newOperationKey(): string {
  return createOperationKey();
}

async function copyApprovedText() {
  const release = props.currentRelease;
  if (!release || !releaseMatches.value) return;

  if (localCopyState.value === 'ack-pending' && copyIntent.value?.grantId === release.grantId) {
    emit('acknowledgeCopy', copyIntent.value);
    return;
  }

  const intent = { grantId: release.grantId, success: false, operationKey: newOperationKey() };
  copyIntent.value = intent;
  localCopyState.value = 'writing';
  try {
    await globalThis.navigator.clipboard.writeText(release.body);
    intent.success = true;
    localCopyState.value = 'ack-pending';
    emit('acknowledgeCopy', intent);
  } catch {
    intent.success = false;
    localCopyState.value = 'failed';
    emit('acknowledgeCopy', intent);
  }
}
</script>

<template>
  <section
    class="material-editor"
    aria-label="Material editor"
    :data-opportunity-id="opportunityId"
  >
    <header class="material-context">
      <div>
        <p class="eyebrow">
          Opportunity
        </p>
        <h2>{{ opportunityTitle }}</h2>
      </div>
      <div
        class="recipient"
        aria-label="Named recipient"
      >
        <p class="eyebrow">
          Recipient
        </p>
        <strong>{{ recipient.name }}</strong>
        <small>{{ recipient.kind }}</small>
      </div>
    </header>

    <div class="material-meta">
      <span>{{ material.kind.replace('_', ' ') }}</span>
      <span>Version {{ material.versionId }}</span>
      <span>Revision {{ material.revision }}</span>
      <span>{{ material.status }}</span>
      <time :datetime="material.updatedAt">Updated {{ material.updatedAt }}</time>
    </div>

    <section
      v-if="conflict"
      class="conflict"
      role="alert"
      aria-labelledby="conflict-title"
    >
      <h3 id="conflict-title">
        This material changed while you were editing
      </h3>
      <p>Compare the saved version with your unsaved text before continuing.</p>
      <div class="comparison">
        <section>
          <h4>Your base version · {{ baseVersionId }}</h4>
          <pre>{{ conflict.baseBody }}</pre>
        </section>
        <section>
          <h4>Current saved version · {{ conflict.versionId }}</h4>
          <pre>{{ conflict.body }}</pre>
        </section>
        <section>
          <h4>Your unsaved text</h4>
          <pre>{{ body }}</pre>
        </section>
      </div>
      <button
        type="button"
        @click="acceptCurrentVersion"
      >
        Use current version
      </button>
      <button
        type="button"
        @click="reapplyMyTextAsNewVersion"
      >
        Create a new version from my text
      </button>
    </section>

    <div class="material-columns">
      <section
        class="writing-pane"
        aria-labelledby="writing-title"
      >
        <h3 id="writing-title">
          Working material
        </h3>
        <label for="material-body">Message text</label>
        <textarea
          id="material-body"
          v-model="body"
          rows="12"
          :disabled="Boolean(conflict)"
        />
        <p
          class="edit-state"
          role="status"
        >
          {{ dirty ? 'Unsaved changes' : 'Showing the saved version' }}
        </p>
        <section
          class="grounding-editor"
          aria-labelledby="grounding-title"
        >
          <h4 id="grounding-title">
            Sentence evidence and rules
          </h4>
          <p>Sentence boundaries are a writing aid. Choose the claim type and evidence yourself; the server checks grounding.</p>
          <article
            v-for="(sentence, sentenceIndex) in sentenceAnnotations"
            :key="sentenceIndex"
            class="sentence-row"
          >
            <h5>Sentence {{ sentenceIndex + 1 }}</h5>
            <blockquote>{{ sentence.text }}</blockquote>
            <div
              v-for="(claim, claimIndex) in sentence.claims"
              :key="claimIndex"
              class="claim-row"
            >
              <label :for="'claim-type-' + sentenceIndex + '-' + claimIndex">Claim type</label>
              <select
                :id="'claim-type-' + sentenceIndex + '-' + claimIndex"
                v-model="claim.type"
              >
                <option value="self_fact">
                  Owner fact
                </option>
                <option value="other_fact">
                  Recipient or organization fact
                </option>
                <option value="offer">
                  Offer
                </option>
                <option value="question">
                  Question
                </option>
                <option value="ask">
                  Ask
                </option>
                <option value="generic">
                  Generic wording
                </option>
              </select>
              <label :for="'claim-ref-' + sentenceIndex + '-' + claimIndex">Evidence reference</label>
              <select
                :id="'claim-ref-' + sentenceIndex + '-' + claimIndex"
                :value="referenceKey(claim)"
                @change="setClaimReference(claim, ($event.target as HTMLSelectElement).value)"
              >
                <option value="">
                  No linked evidence
                </option>
                <option
                  v-for="reference in claimReferences"
                  :key="reference.kind + ':' + reference.id"
                  :value="reference.kind + ':' + reference.id"
                >
                  {{ reference.label }} · {{ reference.kind }} · T{{ reference.tier }}
                </option>
              </select>
              <button
                type="button"
                @click="sentence.claims.splice(claimIndex, 1)"
              >
                Remove claim
              </button>
            </div>
            <button
              type="button"
              @click="addClaim(sentence)"
            >
              Add claim
            </button>
            <fieldset>
              <legend>Rules used in this sentence</legend>
              <label
                v-for="rule in ['R1','R2','R3','R4','R5','R6','R7']"
                :key="rule"
                class="rule-option"
              >
                <input
                  v-model="sentence.rules"
                  type="checkbox"
                  :value="rule"
                >
                {{ rule }}
              </label>
            </fieldset>
          </article>
          <p
            v-if="sentenceAnnotations.length === 0"
            class="empty-sentences"
          >
            Add message text to inspect sentence evidence.
          </p>
        </section>
        <section
          class="slot-editor"
          aria-labelledby="slots-title"
        >
          <h4 id="slots-title">
            Selected slot fills
          </h4>
          <div
            v-for="(slot, index) in slotEntries"
            :key="index"
            class="slot-row"
          >
            <label :for="'slot-name-' + index">Slot name</label>
            <input
              :id="'slot-name-' + index"
              v-model="slot.name"
              autocomplete="off"
            >
            <label :for="'slot-value-' + index">Approved value</label>
            <input
              :id="'slot-value-' + index"
              v-model="slot.value"
            >
            <button
              type="button"
              @click="slotEntries.splice(index, 1)"
            >
              Remove slot fill
            </button>
          </div>
          <button
            type="button"
            @click="addSlot"
          >
            Add slot fill
          </button>
          <h4>Protected tokens</h4>
          <ul v-if="material.protectedTokens?.length">
            <li
              v-for="(token, index) in material.protectedTokens"
              :key="index"
            >
              <code>{{ token }}</code>
            </li>
          </ul>
          <p v-else>
            No protected tokens are bound to this version.
          </p>
        </section>
        <button
          type="button"
          :disabled="!dirty || Boolean(conflict) || Boolean(busy)"
          @click="saveVersion"
        >
          Save as a new version
        </button>
      </section>

      <section
        class="preview-pane"
        aria-labelledby="preview-title"
      >
        <h3 id="preview-title">
          Preview for {{ recipient.name }}
        </h3>
        <pre class="preview">{{ body }}</pre>
        <p class="preview-note">
          Preview remains available for owner review. It is not release content.
        </p>

        <section
          class="gate-panel"
          aria-labelledby="gate-title"
        >
          <h4 id="gate-title">
            Server review · {{ gate.evaluatedAt }}
          </h4>
          <h5>Hard gates</h5>
          <p
            v-if="gate.hardFailures.length === 0"
            class="pass"
          >
            All reported hard gates pass.
          </p>
          <ul v-else>
            <li
              v-for="failure in gate.hardFailures"
              :key="failure.id"
              class="failure"
            >
              <strong>{{ failure.id }}</strong> — {{ failure.detail }}
            </li>
          </ul>
          <h5>Soft gates</h5>
          <p
            v-if="gate.softFailures.length === 0"
            class="pass"
          >
            No soft gate override is needed.
          </p>
          <div
            v-for="failure in gate.softFailures"
            :key="failure.id"
            class="soft-failure"
          >
            <p class="failure">
              <strong>{{ failure.id }}</strong> — {{ failure.detail }}
            </p>
            <label :for="'override-' + failure.id">Override reason (at least 10 characters)</label>
            <textarea
              :id="'override-' + failure.id"
              v-model="overrideReasons[failure.id]"
              rows="2"
              minlength="10"
            />
          </div>
          <button
            type="button"
            :disabled="!canApprove"
            @click="approve"
          >
            Approve this version
          </button>
          <p
            v-if="approvalMatches"
            role="status"
          >
            This exact version is approved for {{ recipient.name }}.
          </p>
          <p
            v-else-if="approval"
            role="status"
          >
            Approval belongs to an earlier version or recipient and cannot release this material.
          </p>
          <p
            v-if="operationError"
            role="alert"
          >
            {{ operationError }}
          </p>
        </section>

        <section
          class="copy-panel"
          aria-labelledby="copy-title"
        >
          <h4 id="copy-title">
            Copy approved text
          </h4>
          <p
            v-if="!gate.copyEnabled"
            class="failure"
          >
            The server currently blocks copy. Review the gate results above.
          </p>
          <button
            v-if="!releaseMatches"
            type="button"
            :disabled="!canRequestCopy"
            @click="requestCopy"
          >
            Prepare approved copy
          </button>
          <button
            v-else
            type="button"
            :disabled="localCopyState === 'writing' || Boolean(busy)"
            @click="copyApprovedText"
          >
            {{ localCopyState === 'ack-pending' ? 'Retry copy acknowledgement' : localCopyState === 'copied' ? 'Copy again' : 'Copy approved text' }}
          </button>
          <p
            v-if="currentRelease && releaseMatches"
            class="release-details"
          >
            Approved bytes · {{ currentRelease.digest }} · grant {{ currentRelease.grantId }}
          </p>
          <p
            v-if="localCopyState === 'ack-pending'"
            role="status"
          >
            Clipboard result recorded locally; waiting for the server receipt. Retry uses the same operation key.
          </p>
          <p
            v-else-if="localCopyState === 'copied'"
            role="status"
          >
            Copy confirmed by the clipboard API and acknowledged.
          </p>
          <p
            v-else-if="localCopyState === 'failed'"
            role="alert"
          >
            Clipboard copy failed. No delivery or reply timer was recorded.
          </p>
          <p class="boundary-note">
            Copying text does not mark it sent or start a reply timer.
          </p>
        </section>
      </section>
    </div>
  </section>
</template>

<style scoped>
.material-editor{display:grid;gap:1rem}.material-context{display:flex;justify-content:space-between;gap:1rem;align-items:start}.material-context h2,.material-context p{margin:.1rem 0}.eyebrow{font-size:.75rem;text-transform:uppercase;letter-spacing:.08em;color:#58665f}.recipient{min-width:12rem;padding:.75rem;border:1px solid #82918a;border-radius:.35rem}.recipient small{display:block;text-transform:capitalize}.material-meta{display:flex;flex-wrap:wrap;gap:.5rem 1rem;padding:.5rem 0;border-block:1px solid #d7dfd9;font-size:.875rem}.material-columns{display:grid;grid-template-columns:minmax(0,1fr) minmax(0,1fr);gap:1rem;align-items:start}.writing-pane,.preview-pane,.gate-panel,.copy-panel,.conflict{padding:1rem;border:1px solid #d7dfd9;border-radius:.5rem;background:#fff}.writing-pane textarea,.soft-failure textarea{display:block;width:100%;box-sizing:border-box;font:inherit;padding:.6rem;border:1px solid #82918a;border-radius:.25rem}.preview{min-height:12rem;white-space:pre-wrap;overflow-wrap:anywhere;font:inherit;padding:.75rem;background:#f4f6f3;border:1px solid #d7dfd9}.preview-note,.boundary-note{font-size:.875rem;color:#58665f}.gate-panel ul{padding-left:1.25rem}.failure{color:#8a241f}.pass{color:#175c3e}.soft-failure{padding:.4rem 0 .8rem;border-top:1px solid #e4e9e5}.soft-failure label{display:block;margin:.35rem 0}.edit-state{min-height:1.5em}.comparison{display:grid;grid-template-columns:1fr 1fr;gap:1rem}.comparison pre{white-space:pre-wrap;overflow-wrap:anywhere;padding:.75rem;background:#f4f6f3}.grounding-editor,.slot-editor{padding:.75rem 0;border-top:1px solid #d7dfd9}.sentence-row,.claim-row,.slot-row{display:grid;grid-template-columns:1fr 2fr;gap:.5rem;align-items:center;padding:.5rem 0}.sentence-row blockquote,.sentence-row h5{grid-column:1/-1}.sentence-row blockquote{margin:0;padding:.5rem;background:#f4f6f3;white-space:pre-wrap}.claim-row{grid-column:1/-1;padding:.5rem;border:1px solid #e4e9e5}.rule-option{display:inline-flex;align-items:center;gap:.35rem;margin-right:.75rem}.rule-option input{width:auto}fieldset{grid-column:1/-1}.slot-row{grid-template-columns:1fr 2fr 1fr 2fr auto}button{min-height:2.75rem;margin:.25rem .25rem .25rem 0}@media(max-width:767px){.material-columns,.comparison{grid-template-columns:1fr}.material-context{flex-direction:column}.recipient{width:100%;box-sizing:border-box}.sentence-row,.claim-row,.slot-row{grid-template-columns:1fr}}
</style>
