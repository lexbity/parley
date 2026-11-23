<script setup lang="ts">
/* global HTMLDialogElement, File, Blob */
import { computed, reactive, ref } from 'vue';
import { ApiError, newOperationKey, parley, type PartyImportMapping, type PartyImportPreview, type PartyImportReceipt, type PartyImportStage } from '../../api/client';

const emit = defineEmits<{ imported: [PartyImportReceipt]; viewImported: [PartyImportReceipt] }>();
const dialog = ref<HTMLDialogElement>();
const csv = ref('');
const file = ref<File>();
const stage = ref<PartyImportStage>();
const mapping = reactive<PartyImportMapping>({ party_type: 'person', name_column: 0, focus_column: null, notes_column: null, excluded_records: [], distinct_records: [] });
const preview = ref<PartyImportPreview>();
const receipt = ref<PartyImportReceipt>();
const busy = ref(false);
const message = ref('');
const skippedHeaders = computed(() => stage.value?.headers.flatMap((header, i) =>
  [mapping.name_column, mapping.focus_column, mapping.notes_column].includes(i) ? [] : [columnLabel(header, i)]) ?? []);
type Commit = { stageId: string; preview_id: string; digest: string; key: string };
const pending = ref<Commit>();
const storageKey = () => 'parley.party-import.' + parley.workspaceId + '.' + parley.workspaceEpoch;
function remember(intent?: Commit) {
  pending.value = intent;
  if (intent) globalThis.sessionStorage.setItem(storageKey(), JSON.stringify(intent));
  else globalThis.sessionStorage.removeItem(storageKey());
}
function open() {
  csv.value = ''; file.value = undefined; stage.value = undefined; preview.value = undefined;
  receipt.value = undefined; message.value = ''; pending.value = undefined;
  const saved = globalThis.sessionStorage.getItem(storageKey());
  if (saved) pending.value = JSON.parse(saved) as Commit;
  dialog.value?.showModal();
}
function columnLabel(header: string, i: number) { return (i + 1) + ' · ' + (header || '(blank header)'); }
function reasonText(reason: unknown) { return reason instanceof Error ? reason.message : 'The import could not be completed.'; }
function clearReview() {
  preview.value = undefined;
  mapping.excluded_records = []; mapping.distinct_records = [];
  message.value = '';
}
async function readCsv() {
  if (busy.value) return;
  busy.value = true; message.value = '';
  try {
    stage.value = await parley.stagePartyImport(file.value ?? new Blob([csv.value]), newOperationKey());
    mapping.party_type = 'person'; mapping.name_column = 0; mapping.focus_column = null; mapping.notes_column = null;
    clearReview();
  } catch (reason) { message.value = reasonText(reason); }
  finally { busy.value = false; }
}
async function review() {
  if (!stage.value || busy.value) return;
  busy.value = true; message.value = '';
  try { preview.value = await parley.previewPartyImport(stage.value.stage_id, mapping, newOperationKey()); }
  catch (reason) { message.value = reasonText(reason); preview.value = undefined; }
  finally { busy.value = false; }
}
function decide(record: number, decision: string) {
  mapping.excluded_records = mapping.excluded_records.filter(n => n !== record);
  mapping.distinct_records = mapping.distinct_records.filter(n => n !== record);
  if (decision === 'skip') mapping.excluded_records.push(record);
  if (decision === 'distinct') mapping.distinct_records.push(record);
  void review();
}
async function commit() {
  if (busy.value) return;
  if (!pending.value && preview.value && stage.value) {
    remember({ stageId: stage.value.stage_id, preview_id: preview.value.preview_id, digest: preview.value.digest, key: newOperationKey() });
  }
  const intent = pending.value;
  if (!intent) return;
  busy.value = true; message.value = '';
  try {
    receipt.value = await parley.commitPartyImport(intent.stageId, intent, intent.key);
    remember(); emit('imported', receipt.value);
  } catch (reason) {
    message.value = reasonText(reason);
    if (reason instanceof ApiError && [400, 404, 409, 410, 412, 422].includes(reason.status)) {
      remember(); preview.value = undefined;
      if (reason.status === 404 || reason.status === 410) stage.value = undefined;
    }
  } finally { busy.value = false; }
}
function viewImported() {
  if (receipt.value) emit('viewImported', receipt.value);
  dialog.value?.close();
}
</script>

<template>
  <button
    v-if="parley.supports('party.imports')"
    type="button"
    @click="open"
  >
    Import CSV
  </button>
  <dialog
    ref="dialog"
    aria-label="Import parties from CSV"
    @cancel="busy && $event.preventDefault()"
  >
    <header>
      <h2>Import parties from CSV</h2><button
        type="button"
        :disabled="busy"
        @click="dialog?.close()"
      >
        {{ receipt || pending ? 'Close' : 'Cancel' }}
      </button>
    </header>
    <p
      v-if="message"
      role="alert"
    >
      {{ message }}
    </p>
    <p
      v-if="busy"
      role="status"
    >
      Working…
    </p>
    <template v-if="receipt">
      <p role="status">
        {{ receipt.created.length }} created · {{ receipt.skipped_count }} skipped
      </p>
      <button
        type="button"
        @click="viewImported"
      >
        View imported parties
      </button>
    </template>
    <template v-else-if="pending">
      <p>Check the receipt for your submitted import.</p>
      <button
        type="button"
        :disabled="busy"
        @click="commit"
      >
        Check import result
      </button>
    </template>
    <form
      v-else-if="!stage"
      @submit.prevent="readCsv"
    >
      <label>CSV file<input
        type="file"
        accept=".csv,text/csv"
        @change="file = ($event.target as HTMLInputElement).files?.[0]"
      ></label>
      <label>Or paste CSV<textarea
        v-model="csv"
        :disabled="!!file"
        rows="6"
      /></label>
      <button
        :disabled="busy || (!file && !csv)"
        type="submit"
      >
        Read columns
      </button>
    </form>
    <template v-else>
      <fieldset
        :disabled="busy"
        @change="clearReview"
      >
        <legend>Map {{ stage.row_count }} data records</legend>
        <label>Batch type<select v-model="mapping.party_type"><option value="person">Person</option><option value="org">Organization</option></select></label>
        <label>Name column<select v-model="mapping.name_column"><option
          v-for="(header, i) in stage.headers"
          :key="i"
          :value="i"
        >{{ columnLabel(header, i) }}</option></select></label>
        <label
          v-for="column in (['focus_column', 'notes_column'] as const)"
          :key="column"
        >{{ column === 'focus_column' ? 'Focus column' : 'Markdown notes column' }}
          <select v-model="mapping[column]"><option :value="null">Skip</option><option
            v-for="(header, i) in stage.headers"
            :key="i"
            :value="i"
          >{{ columnLabel(header, i) }}</option></select>
        </label>
      </fieldset>
      <p v-if="skippedHeaders.length">
        Skipped columns · {{ skippedHeaders.join(', ') }}
      </p>
      <button
        type="button"
        :disabled="busy"
        @click="review"
      >
        Preview mapped rows
      </button>
      <template v-if="preview">
        <p aria-live="polite">
          {{ preview.create_count }} create · {{ preview.skip_count }} skip · {{ preview.error_count }} need review
        </p>
        <div class="table-scroll">
          <table>
            <thead><tr><th>Record</th><th>Name</th><th>Type</th><th>Focus</th><th>Notes</th><th>Row decision</th></tr></thead>
            <tbody>
              <tr
                v-for="row in preview.rows"
                :key="row.source_record"
              >
                <td>{{ row.source_record }}</td><td>{{ row.name }}</td><td>{{ mapping.party_type === 'org' ? 'Organization' : 'Person' }}</td><td>{{ row.focus }}</td><td>{{ row.notes.slice(0, 160) }}</td>
                <td>
                  <small v-if="row.duplicates.length">Possible matches · {{ row.duplicates.join(', ') }}</small><small v-if="row.decision === 'error'">Check name or field limits, or skip this row.</small>
                  <select
                    :aria-label="'Decision for record ' + row.source_record"
                    :disabled="busy"
                    :value="mapping.excluded_records.includes(row.source_record) ? 'skip' : mapping.distinct_records.includes(row.source_record) ? 'distinct' : 'include'"
                    @change="decide(row.source_record, ($event.target as HTMLSelectElement).value)"
                  >
                    <option value="include">
                      {{ row.duplicates.length ? 'Choose a decision' : 'Include' }}
                    </option>
                    <option
                      v-if="row.duplicates.length"
                      value="distinct"
                    >
                      Create distinct
                    </option><option value="skip">
                      Skip
                    </option>
                  </select>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <button
          type="button"
          :disabled="busy || !preview.create_count || !!preview.error_count || preview.rows.some(row => row.decision === 'review')"
          @click="commit"
        >
          Import {{ preview.create_count }} parties
        </button>
      </template>
    </template>
  </dialog>
</template>
<style scoped>
dialog { box-sizing: border-box; width: min(54rem, 96vw); max-height: 88dvh; color: #253e32; background: #fffef7; border: 1px solid #abbcac; border-radius: .55rem; padding: 1.2rem; }
dialog::backdrop { background: #172b2260; }
header { display: flex; align-items: center; justify-content: space-between; gap: 1rem; }
h2 { margin: 0; font-family: Georgia, serif; font-weight: 500; }
form, fieldset { display: grid; gap: .6rem; margin: .8rem 0; }
fieldset { grid-template-columns: repeat(auto-fit,minmax(9rem,1fr)); }
label { display: grid; gap: .2rem; }
.table-scroll { overflow: auto; max-height: 40vh; margin-bottom: .8rem; }
table { width: 100%; border-collapse: collapse; } td,th { text-align: left; padding: .35rem; border-bottom: 1px solid #d7ded3; } td { max-width: 16rem; overflow-wrap: anywhere; }
td small { display: block; }
</style>
