<script setup lang="ts">
import { ref } from 'vue';
import { newOperationKey, parley, type GraphEntity, type GraphSnapshot } from '../../api/client';

const props = defineProps<{ edge: GraphSnapshot['edges'][number]; parties: GraphEntity[] }>();
const emit = defineEmits<{ saved: []; cancel: [] }>();
const wording = ref(props.edge.quote);
const speaker = ref(props.edge.speaker_kind);
const speakerId = ref(props.edge.speaker_entity_id ?? '');
const busy = ref(false);
const error = ref('');
let lastIntent = '';
let operationKey = '';

async function save() {
  const { claim_id, claim_revision, occurrence_id } = props.edge;
  if (!claim_id || !claim_revision || busy.value) return;
  const input = { expected_revision: claim_revision, occurrence_id, wording: wording.value,
    speaker_kind: speaker.value, speaker_entity_id: speaker.value === 'party' ? speakerId.value : null };
  const intent = JSON.stringify(input);
  if (intent !== lastIntent) { lastIntent = intent; operationKey = newOperationKey(); }
  busy.value = true;
  error.value = '';
  try {
    await parley.reviseClaim(claim_id, input, operationKey);
    emit('saved');
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not correct the attribution.'; }
  finally { busy.value = false; }
}
</script>

<template>
  <form
    aria-label="Correct attribution"
    @submit.prevent="save"
  >
    <label>Exact source wording<textarea
      v-model="wording"
      rows="3"
      required
    /></label>
    <label>Speaker<select v-model="speaker"><option value="owner">Owner</option><option value="party">Party</option><option value="unknown">Unknown</option></select></label>
    <label v-if="speaker === 'party'">
      Speaking party
      <select
        v-model="speakerId"
        required
      >
        <option value="">Choose a party</option>
        <option
          v-for="party in parties"
          :key="party.id"
          :value="party.id"
        >{{ party.name }} · {{ party.focus || party.kind }}</option>
      </select>
    </label>
    <p
      v-if="error"
      role="alert"
    >
      {{ error }}
    </p>
    <button
      type="submit"
      :disabled="busy"
    >
      Save attribution
    </button>
    <button
      type="button"
      :disabled="busy"
      @click="emit('cancel')"
    >
      Cancel
    </button>
  </form>
</template>
