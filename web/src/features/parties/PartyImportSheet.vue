<script setup lang="ts">
import { onBeforeUnmount, watch } from 'vue';
import { Spreadsheet, jspreadsheet } from '@jspreadsheet-ce/vue';
import type Jspreadsheet from 'jspreadsheet-ce';
import 'jsuites/dist/jsuites.css';
import 'jspreadsheet-ce/dist/jspreadsheet.css';
import type { PartyImportPreview } from '../../api/client';

type ImportRow = PartyImportPreview['rows'][number];
const props = defineProps<{ rows: ImportRow[]; selectedRecord: number | null; partyType: 'person' | 'org' }>();
const emit = defineEmits<{ select: [record: number] }>();
const decisions: Record<ImportRow['decision'], string> = {
  create: 'Create', skip: 'Skip', review: 'Possible duplicate', error: 'Invalid fields',
};
let worksheet: Jspreadsheet.WorksheetInstance | undefined;

function worksheetData() {
  return props.rows.map(row => [
    row.source_record,
    row.name,
    props.partyType === 'org' ? 'Organization' : 'Person',
    row.focus,
    row.notes.length > 160 ? row.notes.slice(0, 160) + '…' : row.notes,
    decisions[row.decision],
  ]);
}

const sheets: Jspreadsheet.WorksheetOptions[] = [{
  data: worksheetData(),
  columns: [
    { title: 'Record', width: 74 },
    { title: 'Name', width: 186 },
    { title: 'Type', width: 106 },
    { title: 'Focus', width: 160 },
    { title: 'Markdown notes', width: 250 },
    { title: 'Decision', width: 145 },
  ].map(column => ({ ...column, type: 'text', align: 'left', readOnly: true })),
  minDimensions: [6, 0],
  editable: false,
  columnSorting: false,
  columnDrag: false,
  rowDrag: false,
  allowRenameColumn: false,
  allowInsertColumn: false,
  allowDeleteColumn: false,
  allowInsertRow: false,
  allowDeleteRow: false,
  allowManualInsertColumn: false,
  allowManualInsertRow: false,
  allowComments: false,
  tableOverflow: true,
  tableHeight: 'min(32dvh, 20rem)',
  tableWidth: '100%',
}];

function highlightRecord() {
  const index = props.rows.findIndex(row => row.source_record === props.selectedRecord);
  if (!worksheet || index < 0) return;
  worksheet.updateSelectionFromCoords(0, index, sheets[0]!.columns!.length - 1, index);
}

function loadWorksheet(instance: Jspreadsheet.SpreadsheetInstance) {
  worksheet = instance.worksheets[0];
  for (const header of worksheet?.headers ?? []) header.setAttribute('role', 'columnheader');
  highlightRecord();
}

function selectRecord(_worksheet: Jspreadsheet.WorksheetInstance, _column: number, row: number) {
  const record = props.rows[row]?.source_record;
  if (record !== undefined && record !== props.selectedRecord) emit('select', record);
}

watch(() => props.rows, () => {
  worksheet?.setData(worksheetData());
  highlightRecord();
});
watch(() => props.selectedRecord, highlightRecord);
onBeforeUnmount(() => {
  if (worksheet) jspreadsheet.destroy(worksheet.parent.el, true);
  worksheet = undefined;
});
</script>

<template>
  <div
    class="import-sheet"
    role="region"
    aria-label="CSV preview spreadsheet"
  >
    <Spreadsheet
      :worksheets="sheets"
      :onload="loadWorksheet"
      :onselection="selectRecord"
      :parse-formulas="false"
      :parse-h-t-m-l="false"
      :allow-export="false"
      :auto-increment="false"
      :context-menu="() => []"
    />
  </div>
</template>

<style scoped>
.import-sheet { min-width: 0; border: 1px solid #bac9bb; border-radius: .4rem; overflow: hidden; }
.import-sheet :deep(.jss_container) { display: block; max-width: 100%; }
.import-sheet :deep(.jss_worksheet) { font: inherit; color: inherit; }
.import-sheet :deep(.jss_worksheet thead td) { background: #edf2e8; color: #263f33; font-weight: 600; }
.import-sheet :deep(.jss_worksheet tbody td) { padding: .35rem .5rem; white-space: pre; }
</style>
