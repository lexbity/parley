<script setup lang="ts">
/* global HTMLDialogElement, HTMLSelectElement, File, Blob */
import { computed, defineAsyncComponent, nextTick, reactive, ref } from 'vue';
import { ApiError, newOperationKey, parley, type PartyImportMapping, type PartyImportPreview, type PartyImportReceipt, type PartyImportStage } from '../../api/client';

const PartyImportSheet = defineAsyncComponent(() => import('./PartyImportSheet.vue'));
const emit = defineEmits<{ imported: [PartyImportReceipt]; viewImported: [PartyImportReceipt] }>();
const dialog = ref<HTMLDialogElement>();
const csv = ref('');
const file = ref<File>();
const stage = ref<PartyImportStage>();
const mapping = reactive<PartyImportMapping>({ party_type: 'person', name_column: 0, focus_column: null, notes_column: null, excluded_records: [], distinct_records: [] });
const preview = ref<PartyImportPreview>();
const selectedRecord = ref<number | null>(null);
const selectedRow = computed(() => preview.value?.rows.find(row => row.source_record === selectedRecord.value));
const recordPicker = ref<HTMLSelectElement>();
const receipt = ref<PartyImportReceipt>();
const busy = ref(false);
const message = ref('');
const canImport = computed(() => Boolean(preview.value?.create_count
  && !preview.value.error_count && !preview.value.rows.some(row => row.decision === 'review')));
const skippedHeaders = computed(() => stage.value?.headers.flatMap((header, i) =>
  [mapping.name_column, mapping.focus_column, mapping.notes_column].includes(i) ? [] : [columnLabel(header, i)]) ?? []);
type Commit = { stageId: string; preview_id: string; digest: string; key: string };
const pending = ref<Commit>();
const storageKey = () => 'parley.party-import.' + parley.workspaceId + '.' + parley.workspaceEpoch;
function remember(intent?: Commit) {
  pending.value = intent;
  try {
    if (intent) globalThis.sessionStorage.setItem(storageKey(), JSON.stringify(intent));
    else globalThis.sessionStorage.removeItem(storageKey());
  } catch { /* The open importer still holds the submitted intent. */ }
}
function resetDialog() {
  csv.value = ''; file.value = undefined; stage.value = undefined; preview.value = undefined;
  receipt.value = undefined; message.value = ''; pending.value = undefined;
  selectedRecord.value = null;
}
function open() {
  resetDialog();
  try {
    const saved = globalThis.sessionStorage.getItem(storageKey());
    if (saved) {
      const intent: Partial<Commit> | null = JSON.parse(saved);
      if (intent && typeof intent.stageId === 'string' && typeof intent.preview_id === 'string'
        && typeof intent.digest === 'string' && typeof intent.key === 'string') pending.value = intent as Commit;
    }
  } catch { /* Start with an empty importer when its saved receipt cannot be read. */ }
  dialog.value?.showModal();
}
function closed() {
  if (!dialog.value?.open) resetDialog();
}
function columnLabel(header: string, i: number) { return (i + 1) + ' · ' + (header || '(blank header)'); }
function reasonText(reason: unknown) { return reason instanceof Error ? reason.message : 'The import could not be completed.'; }
function clearReview() {
  preview.value = undefined;
  selectedRecord.value = null;
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
async function review(focusPicker = false) {
  if (!stage.value || busy.value) return;
  busy.value = true; message.value = '';
  try {
    const result = await parley.previewPartyImport(stage.value.stage_id, mapping, newOperationKey());
    preview.value = result;
    if (!result.rows.some(row => row.source_record === selectedRecord.value)) selectedRecord.value = result.rows[0]?.source_record ?? null;
  }
  catch (reason) { message.value = reasonText(reason); preview.value = undefined; }
  finally { busy.value = false; }
  if (focusPicker) void nextTick(() => recordPicker.value?.focus());
}
function decide(decision: string) {
  const record = selectedRecord.value;
  if (busy.value || record === null) return;
  mapping.excluded_records = mapping.excluded_records.filter(n => n !== record);
  mapping.distinct_records = mapping.distinct_records.filter(n => n !== record);
  if (decision === 'skip') mapping.excluded_records.push(record);
  if (decision === 'distinct') mapping.distinct_records.push(record);
  void review();
}
async function commit() {
  if (busy.value) return;
  if (!pending.value && canImport.value && preview.value && stage.value) {
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
function nextIssue() {
  const issues = preview.value?.rows.filter(row => row.decision === 'review' || row.decision === 'error') ?? [];
  selectedRecord.value = (issues.find(row => row.source_record > (selectedRecord.value ?? 0)) ?? issues[0])?.source_record ?? selectedRecord.value;
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
    @close="closed"
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
        :disabled="busy"
        @change="file = ($event.target as HTMLInputElement).files?.[0]"
      ></label>
      <label>Or paste CSV<textarea
        v-model="csv"
        :disabled="!!file || busy"
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
        @click="review(true)"
      >
        Preview mapped rows
      </button>
      <template v-if="preview">
        <p
          class="review-count"
          aria-live="polite"
        >
          {{ preview.create_count }} create · {{ preview.skip_count }} skip · {{ preview.error_count }} need review
        </p>
        <PartyImportSheet
          v-if="preview.rows.length"
          :rows="preview.rows"
          :selected-record="selectedRecord"
          :party-type="mapping.party_type"
          @select="selectedRecord = $event"
        />
        <fieldset
          v-if="selectedRow"
          class="record-review"
          :disabled="busy"
        >
          <legend>Review a record</legend>
          <label>Record<select
            ref="recordPicker"
            v-model="selectedRecord"
          ><option
            v-for="row in preview.rows"
            :key="row.source_record"
            :value="row.source_record"
          >{{ row.source_record }} · {{ row.name || 'Missing name' }}</option></select></label>
          <button
            v-if="preview.rows.some(row => row.decision === 'review' || row.decision === 'error')"
            type="button"
            @click="nextIssue"
          >
            Next issue
          </button>
          <label>Decision<select
            :value="mapping.excluded_records.includes(selectedRow.source_record) ? 'skip' : mapping.distinct_records.includes(selectedRow.source_record) ? 'distinct' : 'include'"
            @change="decide(($event.target as HTMLSelectElement).value)"
          >
            <option value="include">{{ selectedRow.duplicates.length ? 'Choose a decision' : 'Include' }}</option>
            <option
              v-if="selectedRow.duplicates.length"
              value="distinct"
            >Create distinct</option>
            <option value="skip">Skip</option>
          </select></label>
          <p
            v-if="selectedRow.duplicates.length"
            class="record-detail"
          >
            Possible matches · {{ selectedRow.duplicates.join(', ') }}
          </p>
          <p
            v-if="selectedRow.decision === 'error'"
            class="record-detail"
          >
            Review the mapping and field lengths, or skip this record.
          </p>
          <details
            v-if="selectedRow.notes"
            class="record-detail"
          >
            <summary>Show full Markdown notes</summary>
            <pre>{{ selectedRow.notes }}</pre>
          </details>
        </fieldset>
        <button
          type="button"
          class="primary"
          :disabled="busy || !canImport"
          @click="commit"
        >
          Import {{ preview.create_count }} {{ preview.create_count === 1 ? 'party' : 'parties' }}
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
.review-count { margin-top: 1rem; }
.record-review { grid-template-columns: minmax(10rem, 1.5fr) auto minmax(9rem, 1fr); align-items: end; }
.record-review label { margin: 0; }
.record-review button { justify-self: start; }
.record-detail { grid-column: 1 / -1; margin: 0; overflow-wrap: anywhere; }
.record-detail pre { margin-bottom: 0; white-space: pre-wrap; overflow-wrap: anywhere; }
@media (max-width: 630px) { .record-review { grid-template-columns: minmax(0, 1fr); } }
</style>
