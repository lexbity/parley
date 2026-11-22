<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { newOperationKey, parley, type ContactOutcome, type ContactThread, type Material, type ReleasedMaterial, type SendRecord } from '../../api/client';

const props = defineProps<{ opportunityId: string; material: Material; delivered: ReleasedMaterial | null; threads: ContactThread[] }>();
const emit = defineEmits<{ changed: [] }>();
const sendRecord = ref<SendRecord | null>(null);
const outcome = ref<ContactOutcome | null>(null);
const replyByDate = ref('');
const receivedAt = ref('');
const outcomeClass = ref<'reply_positive' | 'reply_neutral' | 'reply_negative' | 'declined' | 'rung_accepted' | 'call_scheduled'>('reply_positive');
const rung = ref<number | null>(null);
const note = ref('');
const resolution = ref<'mutual_win' | 'one_sided' | 'no_deal'>('no_deal');
const theyBenefited = ref<'yes' | 'partial' | 'no' | 'unknown'>('unknown');
const iBenefited = ref<'yes' | 'partial' | 'no' | 'unknown'>('unknown');
const busy = ref(false);
const error = ref('');
const status = ref('');
let pendingSend: { threadId: string; key: string; release: ReleasedMaterial; date: string; zone: string } | null = null;

const thread = computed(() => props.threads.find((item) => item.opportunity_id === props.opportunityId && item.recipient_id === props.material.recipient_id) ?? null);
const usableRelease = computed(() => {
  const receipt = props.delivered;
  return receipt !== null && receipt.material_id === props.material.id && receipt.version === props.material.version
    && receipt.recipient_id === props.material.recipient_id && receipt.content_sha256 === props.material.content_sha256;
});
const canSend = computed(() => usableRelease.value && props.material.status === 'approved' && !['closed_declined', 'resolved', 'dormant'].includes(thread.value?.state ?? ''));

function localDateAfter(days: number): string {
  const date = new Date();
  date.setDate(date.getDate() + days);
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`;
}

function localDateTimeNow(): string {
  const date = new Date();
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}T${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}`;
}

async function run(action: () => Promise<void>, success: string) {
  busy.value = true; error.value = ''; status.value = '';
  try { await action(); status.value = success; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'The service did not confirm this contact action.'; }
  finally { busy.value = false; }
}

async function markSent() {
  if (!usableRelease.value) { error.value = 'Release this exact approved version to this recipient first. The service will validate the release receipt again.'; return; }
  const zone = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
  pendingSend ??= {
    threadId: thread.value?.id ?? newOperationKey(), key: newOperationKey(), release: props.delivered!, date: replyByDate.value, zone,
  };
  if (pendingSend.release.release_id !== props.delivered?.release_id || pendingSend.date !== replyByDate.value) {
    error.value = 'A previous send has an unknown result. Retry its frozen request first; do not change the recipient, release, or deadline.';
    return;
  }
  await run(async () => {
    const receipt = await parley.sendOnThread(pendingSend!.threadId, {
      opportunity_id: props.opportunityId,
      recipient_id: props.material.recipient_id,
      primary: { material_id: pendingSend!.release.material_id, version: pendingSend!.release.version, release_id: pendingSend!.release.release_id },
      companions: [], deadline: { date: pendingSend!.date, zone: pendingSend!.zone },
    }, pendingSend!.key);
    sendRecord.value = receipt;
    outcome.value = null;
    pendingSend = null;
    emit('changed');
  }, 'Sent touch recorded by the workspace. The reply deadline is service-owned.');
}

async function recordReply() {
  if (!sendRecord.value) { error.value = 'This session does not have the send ID required to attach an outcome. Refresh the server history when available.'; return; }
  await run(async () => {
    outcome.value = await parley.recordOutcome(sendRecord.value!.id, {
      class: outcomeClass.value,
      ...(outcomeClass.value === 'rung_accepted' && rung.value !== null ? { rung_accepted: rung.value } : {}),
      received_at: receivedAt.value ? new Date(receivedAt.value).toISOString() : null, note: note.value.trim() || null,
    });
    emit('changed');
  }, 'Outcome recorded against this touch.');
}

async function reopen() {
  if (!thread.value) return;
  await run(async () => { await parley.reopenThread(thread.value!.id); emit('changed'); }, 'Thread reopened by explicit owner action. Historical sends remain recorded.');
}

async function resolveOpportunity() {
  await run(async () => {
    await parley.recordResolution(props.opportunityId, { result: resolution.value, they_benefited: theyBenefited.value, i_benefited: iBenefited.value });
    emit('changed');
  }, 'Resolution recorded from the owner’s assessment of both sides’ benefit.');
}

watch(() => [props.opportunityId, props.material.recipient_id] as const, () => { sendRecord.value = null; outcome.value = null; pendingSend = null; });
replyByDate.value = localDateAfter(7);
receivedAt.value = localDateTimeNow();
</script>

<template>
  <section
    class="thread-panel"
    :aria-label="`Contact thread for ${material.recipient_name}`"
  >
    <h5>Contact thread · {{ material.recipient_name }}</h5>
    <p v-if="thread">
      {{ thread.state.replaceAll('_', ' ') }} · touch {{ thread.current_touch }}
    </p>
    <p v-if="thread?.sent_at">
      Latest recorded send: {{ thread.sent_at }}<span v-if="thread.expires_at"> · reply deadline {{ thread.expires_at }}</span>
    </p>
    <p v-else-if="!thread">
      No send has been recorded for this opportunity and recipient.
    </p>

    <template v-if="!['closed_declined', 'resolved'].includes(thread?.state ?? '')">
      <label>Reply deadline (inclusive local date)
        <input
          v-model="replyByDate"
          type="date"
          :disabled="busy || !!pendingSend"
        >
      </label>
      <button
        type="button"
        :disabled="busy || !canSend"
        @click="markSent"
      >
        {{ pendingSend ? 'Retry original Mark sent' : 'Mark this delivered version sent' }}
      </button>
      <p v-if="!usableRelease">
        Mark sent is available after copy confirmation or download issuance for this exact version and recipient.
      </p>
    </template>

    <p v-if="sendRecord">
      Touch {{ sendRecord.touch_no }} recorded {{ sendRecord.sent_at }} · response due {{ sendRecord.response_deadline }} ({{ sendRecord.deadline_zone }})<span v-if="sendRecord.capacity_charged"> · capacity charged</span>
    </p>
    <template v-if="sendRecord && !outcome && !['closed_declined', 'resolved'].includes(thread?.state ?? '')">
      <label>Record response
        <select v-model="outcomeClass">
          <option value="reply_positive">Positive reply</option><option value="reply_neutral">Neutral reply</option>
          <option value="reply_negative">Negative reply</option><option value="declined">Explicit decline</option>
          <option value="rung_accepted">Ask rung accepted</option><option value="call_scheduled">Call scheduled</option>
        </select>
      </label>
      <label>Response received at (local time)<input
        v-model="receivedAt"
        type="datetime-local"
        required
      ></label>
      <label v-if="outcomeClass === 'rung_accepted'">Accepted rung<input
        v-model.number="rung"
        type="number"
        min="1"
        max="5"
      ></label>
      <label>Owner note<input
        v-model="note"
        maxlength="1000"
      ></label>
      <button
        type="button"
        :disabled="busy || (outcomeClass === 'rung_accepted' && !rung)"
        @click="recordReply"
      >
        Record response
      </button>
    </template>
    <p v-if="outcome">
      Recorded {{ outcome.class.replaceAll('_', ' ') }} for this send at {{ outcome.recorded_at }}.
    </p>

    <button
      v-if="thread && ['timed_out', 'resolved'].includes(thread.state)"
      type="button"
      :disabled="busy"
      @click="reopen"
    >
      Reopen this thread
    </button>
    <p
      v-if="thread?.state === 'closed_declined'"
      role="note"
    >
      This recipient declined and remains suppressed across opportunities. Reopening does not clear do-not-contact status; a separate clearance reason is required.
    </p>
    <fieldset v-if="thread && ['waiting', 'conversing', 'timed_out'].includes(thread.state)">
      <legend>Record opportunity resolution</legend>
      <label>Result<select v-model="resolution"><option value="mutual_win">Mutual win</option><option value="one_sided">One-sided</option><option value="no_deal">No deal</option></select></label>
      <label>They benefited<select v-model="theyBenefited"><option value="yes">Yes</option><option value="partial">Partial</option><option value="no">No</option><option value="unknown">Unknown</option></select></label>
      <label>I benefited<select v-model="iBenefited"><option value="yes">Yes</option><option value="partial">Partial</option><option value="no">No</option><option value="unknown">Unknown</option></select></label>
      <button
        type="button"
        :disabled="busy"
        @click="resolveOpportunity"
      >
        Record resolution and close live timers
      </button>
    </fieldset>
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
  </section>
</template>

<style scoped>
.thread-panel { margin-block: .75rem; padding: .75rem; border: 1px solid #9baea2; border-radius: .35rem; }
label { display: block; margin-block: .5rem; }
fieldset { margin-block: .75rem; }
button { margin: .25rem .4rem .25rem 0; }
</style>
