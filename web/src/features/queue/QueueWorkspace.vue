<script setup lang="ts">
/* global HTMLElement, HTMLSelectElement, AbortController, Event */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from 'vue';
import {
  parley,
  ApiError,
  newOperationKey,
  type Material,
  type QueueSheetResponse,
  type QueueSheetRow,
  type SheetPhase,
  type SetPhaseResult,
  type SetPhaseInput,
} from '../../api/client';
import { commands, type CommandArgs, type CommandSuccess } from '../../api/commands';
import { sessionEventReader } from '../../app/commands/events';
import { inferenceActivity } from '../../app/inference/activity';
import type { QueuePaletteBinding } from '../../app/commands/context';
import { opportunityCreateAction, queuePhaseActions, queueTaskAction, registerScoped, useCommandRegistry, type UiCommand } from '../../app/commands/registry';
import type { OpportunityNotes } from '../../api/client';
import Daily from './Daily.vue';
import Insights from './Insights.vue';
import SearchableSelect from '../../shared/SearchableSelect.vue';
import type { SelectOption } from '../../shared/select-options';

defineOptions({ name: 'QueueWorkspace' });

const props = withDefaults(defineProps<{ outputs: Material[]; routeId?: string | null; active?: boolean }>(), { routeId: null, active: true });
const emit = defineEmits<{ navigate: [path: string]; binding: [value: QueuePaletteBinding] }>();

type View = 'sheet' | 'daily' | 'insights';
const view = ref<View>('sheet');
const query = ref('');
const phaseFilter = ref('');
const sortKey = ref<'due' | 'name'>('due');
const sheet = ref<QueueSheetResponse>({ generation: '', total: 0, rows: [], phases: [], cursor: null, next_cursor: null });
let sheetRequest: AbortController | undefined;
let routeRequest: AbortController | undefined;
const loadError = ref('');
const loading = ref(false);
const selectedId = ref('');
const linkedRow = ref<QueueSheetRow | null>(null);
const routeLoading = ref(false);
const routeError = ref('');
const savingId = ref('');
const status = ref('');
const error = ref('');
type PhaseCommand = { opportunityId: string; input: SetPhaseInput; epoch: string | undefined; key: string; previous?: QueueSheetRow; source: HTMLElement | null };
const undoIntent = ref<{ opportunityId: string; toStage: string; packVersion: string; eventId: string; phaseRevision: number; epoch: string | undefined } | null>(null);
const uncertain = shallowRef<PhaseCommand | null>(null);
const narrowInspector = ref(false);
let searchHandle: ReturnType<typeof globalThis.setTimeout> | undefined;
const createOpen = ref(false);
const createTrigger = ref<HTMLElement | null>(null);
const createInput = ref<HTMLElement | null>(null);
const searchInput = ref<HTMLElement | null>(null);
const sheetScroll = ref<HTMLElement | null>(null);
const inspectorHeading = ref<HTMLElement | null>(null);
let savedScroll = 0;
const createTitle = ref('');
const createPartyId = ref('');
// Typeahead reads the server directory directly: searching never stops at a
// client-side page boundary.
const partyOptions = ref<SelectOption[]>([]);
async function searchParties(q: string): Promise<SelectOption[]> {
  const page = await parley.queryDirectory({ q: q.trim() || undefined, limit: 50 });
  partyOptions.value = page.rows.map(party => ({ value: party.id, label: `${party.name} · ${party.kind}` }));
  return partyOptions.value;
}
const createBusy = ref(false);
const rowRefs = ref<Record<string, HTMLElement | null>>({});
// Entry restores the last valid search, filters, sort and selection for this
// workspace (spec §2), scoped to the workspace epoch like the party desk.
// The key needs the session epoch, so restore runs once connect() resolves.
const prefsKey = ref('');
const ready = ref(false);
function restorePrefs() {
  const key = parley.workspaceEpoch ? 'parley.queue.' + parley.workspaceEpoch : '';
  if (!key) return;
  prefsKey.value = key;
  try {
    const saved = JSON.parse(globalThis.sessionStorage.getItem(key) ?? '{}');
    if (typeof saved.query === 'string') query.value = saved.query;
    if (typeof saved.phaseFilter === 'string') phaseFilter.value = saved.phaseFilter;
    if (saved.sortKey === 'due' || saved.sortKey === 'name') sortKey.value = saved.sortKey;
    if (typeof saved.selectedId === 'string') selectedId.value = saved.selectedId;
    if (Number.isFinite(saved.scroll)) savedScroll = saved.scroll;
  } catch { /* Storage can be unavailable in a private browsing session. */ }
}
function savePrefs() {
  if (!prefsKey.value) return;
  try {
    globalThis.sessionStorage.setItem(prefsKey.value, JSON.stringify({
      query: query.value, phaseFilter: phaseFilter.value, sortKey: sortKey.value, selectedId: selectedId.value, scroll: savedScroll,
    }));
  } catch { /* Keep in-memory navigation. */ }
}
watch([query, phaseFilter, sortKey, selectedId], savePrefs);
watch(sheetScroll, element => { if (element) element.scrollTop = savedScroll; });
function rememberScroll() {
  if (sheetScroll.value) {
    savedScroll = sheetScroll.value.scrollTop;
    savePrefs();
  }
}

const phases = computed(() => sheet.value.phases);
const editablePhases = computed(() => phases.value.filter(phase => phase.kind === 'planning' && phase.stage_id !== '__unmapped__'));
const countLabel = computed(() => `${sheet.value.rows.length} of ${sheet.value.total} scenarios`);
const hasFilters = computed(() => Boolean(query.value.trim() || phaseFilter.value));
const selectedRow = computed(() => {
  const id = props.routeId || selectedId.value;
  return sheet.value.rows.find(row => row.opportunity_id === id)
    ?? (linkedRow.value?.opportunity_id === id ? linkedRow.value : null);
});
const linkedOutsideSheet = computed(() => Boolean(props.routeId && selectedRow.value && !sheet.value.rows.some(row => row.opportunity_id === props.routeId)));
const selectedMaterials = computed(() => props.outputs.filter(material => material.opportunity_id === selectedRow.value?.opportunity_id));
const emptyWorkspace = computed(() => !loading.value && !loadError.value && sheet.value.total === 0 && !hasFilters.value);
const noMatches = computed(() => !loading.value && !loadError.value && sheet.value.total === 0 && hasFilters.value);

const latestPhaseEvent = ref<{ id: string; opportunityId: string; revision: number } | null>(null);
const originalEventId = computed(() => {
  const event = latestPhaseEvent.value;
  const row = selectedRow.value;
  return event && row && event.opportunityId === row.opportunity_id && event.revision === row.phase_revision ? event.id : null;
});
const commandRegistry = useCommandRegistry();
let disposeActions: (() => void) | undefined;
let scopedFingerprint: string | null | undefined;
const selectedNotes = ref<OpportunityNotes | null>(null);

// Authoritative task index for the selected record. The palette freezes the
// notes revision and the typed span from this read; it never accepts a byte
// span typed by the user.
watch(selectedRow, (row) => {
  if (!row) { selectedNotes.value = null; return; }
  parley.getOpportunityNotes(row.opportunity_id)
    .then((notes) => { selectedNotes.value = notes; })
    .catch(() => { selectedNotes.value = null; });
}, { immediate: true });

function selectedTasks() {
  const revision = selectedNotes.value?.revision ?? 1;
  return (selectedNotes.value?.tasks ?? []).map(task => ({
    // The line digest is equal for identical task lines; the composite token
    // carries the unique occurrence span and the notes revision so dispatch
    // needs no lookup and can never use a stale revision.
    value: `${task.byte_start}:${task.byte_end}:${task.line_sha256}:${revision}`,
    label: task.text,
    byteStart: task.byte_start,
    byteEnd: task.byte_end,
    lineSha256: task.line_sha256,
    checked: task.checked,
  }));
}

async function refreshScopedActions() {
  const row = selectedRow.value;
  const pack = row ? { id: row.pack_id, version: row.pack_version } : { id: '', version: '' };
  const revision = row?.phase_revision ?? 1;
  // Ensure the authoritative task index is loaded for the selected record
  // before the palette picker options are built.
  if (row && !selectedNotes.value) {
    try {
      selectedNotes.value = await parley.getOpportunityNotes(row.opportunity_id);
    } catch {
      selectedNotes.value = null;
    }
  }
  const notesRevision = selectedNotes.value?.revision ?? 1;
  const tasks = selectedTasks();
  const editable = row?.lifecycle === 'open' && editablePhases.value.length > 0 && !uncertain.value;
  const fingerprint = editable
    ? JSON.stringify([parley.workspaceEpoch, row.opportunity_id, revision, pack.id, pack.version, originalEventId.value, notesRevision])
    : null;
  if (fingerprint !== scopedFingerprint) {
    scopedFingerprint = fingerprint;
    disposeActions?.();
    disposeActions = undefined;
    const actions: UiCommand[] = [opportunityCreateAction({
      invoke: (id: string, args: CommandArgs, key: string) => commands.invoke(id, { version: 1, args, dry_run: false }, key),
      onCommitted: () => {
        status.value = 'Opportunity created. It is Unmapped until you choose a phase.';
        error.value = '';
        void loadSheet();
        sessionEventReader.hint();
      },
    })];
    if (row && fingerprint) {
      actions.push(...queuePhaseActions({
        opportunityId: row.opportunity_id,
        phaseRevision: revision,
        packId: pack.id,
        packVersion: pack.version,
        originalEventId: originalEventId.value,
        intentFingerprint: fingerprint,
        invoke: async (id: string, args: CommandArgs, key: string) => {
          if (savingId.value || uncertain.value) throw new Error('Finish the current phase edit first.');
          savingId.value = row.opportunity_id;
          try { return await commands.invoke(id, { version: 1, args, dry_run: false }, key); }
          finally { savingId.value = ''; }
        },
        onCommitted: applyCommandResult,
      }));
      actions.push(queueTaskAction({
        opportunityId: row.opportunity_id,
        notesRevision,
        intentFingerprint: fingerprint,
        invoke: (id: string, args: CommandArgs, key: string) => commands.invoke(id, { version: 1, args, dry_run: false }, key),
        onCommitted: () => {
          status.value = 'Task updated.';
          error.value = '';
          void parley.getOpportunityNotes(row.opportunity_id)
            .then((notes) => { selectedNotes.value = notes; })
            .catch(() => {});
          sessionEventReader.hint();
        },
      }));
    }
    disposeActions = registerScoped(commandRegistry, actions);
  }
  emit('binding', {
    opportunityId: editable ? row.opportunity_id : null,
    phaseRevision: revision,
    pack,
    stages: editablePhases.value.map(phase => ({ value: phase.stage_id, label: phase.label })),
    originalEventId: originalEventId.value,
    tasks,
    notesRevision,
  });
}

watch([selectedRow, editablePhases, originalEventId, uncertain, selectedNotes], refreshScopedActions);

function applyCommandResult(receipt: CommandSuccess) {
  const { opportunity_id: id, stage_id: stageId, phase_revision: revision, event_id: eventId, reverted } = receipt.result;
  const previous = sheet.value.rows.find(row => row.opportunity_id === id)
    ?? (linkedRow.value?.opportunity_id === id ? linkedRow.value : null);
  if (!previous) {
    error.value = 'The scenario is no longer visible in this sheet. Refresh before editing it again.';
    void loadSheet(undefined, true);
    return;
  }
  if (typeof revision !== 'number' || revision < previous.phase_revision) {
    void loadSheet(undefined, true);
    return;
  }
  const stage = phases.value.find(phase => phase.stage_id === stageId);
  if (!stage && stageId != null) {
    void loadSheet(undefined, true);
    return;
  }
  error.value = '';
  applyPhaseResult({
    row: { ...previous, phase_revision: revision, phase: stage ? { ...stage, revision } : null },
    phase_revision: revision,
    event_id: typeof eventId === 'string' ? eventId : '',
    reverted: reverted === true,
  }, previous, reverted === true);
}

function stageLabel(phase: SheetPhase | null): string {
  if (!phase) return 'Unmapped';
  return phase.label;
}

/** Owner-timezone date label: em dash when missing, "Today", otherwise a
 * short month/day (with the year when it differs from the current one). */
function formatActionDate(date: string | null): string {
  if (!date) return '—';
  const parsed = new Date(date.length <= 10 ? date + 'T00:00:00' : date);
  if (Number.isNaN(parsed.getTime())) return date;
  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const target = new Date(parsed.getFullYear(), parsed.getMonth(), parsed.getDate());
  if (target.getTime() === today.getTime()) return 'Today';
  const month = parsed.toLocaleString('en', { month: 'short' });
  return parsed.getFullYear() === now.getFullYear()
    ? `${month} ${parsed.getDate()}`
    : `${month} ${parsed.getDate()}, ${parsed.getFullYear()}`;
}

function actionDateLabel(row: QueueSheetRow): string {
  return formatActionDate(row.next_action.date);
}

async function loadSheet(cursor?: string, preserveLoadedRows = false) {
  if (searchHandle !== undefined) {
    globalThis.clearTimeout(searchHandle);
    searchHandle = undefined;
  }
  sheetRequest?.abort();
  const request = new AbortController();
  sheetRequest = request;
  loading.value = true;
  loadError.value = '';
  try {
    const filters = {
      ...(query.value.trim() ? { search: query.value.trim() } : {}),
      ...(phaseFilter.value ? { phase: phaseFilter.value } : {}),
      sort: sortKey.value,
    };
    const count = preserveLoadedRows ? sheet.value.rows.length : 0;
    let result = await parley.getQueueSheet({ ...filters, cursor }, request.signal);
    if (request.signal.aborted) return;
    if (count > result.rows.length && result.next_cursor) {
      const rows = new Map(result.rows.map(row => [row.opportunity_id, row]));
      while (result.next_cursor && rows.size < count) {
        result = await parley.getQueueSheet({ ...filters, cursor: result.next_cursor }, request.signal);
        if (request.signal.aborted) return;
        for (const row of result.rows) rows.set(row.opportunity_id, row);
      }
      result = { ...result, rows: [...rows.values()] };
    }
    if (cursor) {
      const merged = [...sheet.value.rows];
      for (const row of result.rows) {
        if (!merged.some((item) => item.opportunity_id === row.opportunity_id)) merged.push(row);
      }
      sheet.value = { ...result, rows: merged };
    } else {
      sheet.value = result;
      if (!result.rows.some(row => row.opportunity_id === selectedId.value)) selectedId.value = result.rows[0]?.opportunity_id ?? '';
    }
    if (props.routeId) {
      const match = result.rows.find(row => row.opportunity_id === props.routeId);
      if (match) linkedRow.value = match;
    }
  } catch (reason) {
    if (request.signal.aborted) return;
    loadError.value = reason instanceof Error ? reason.message : 'Could not load the Queue sheet.';
  } finally {
    if (sheetRequest === request) loading.value = false;
  }
}

function scheduleSearch() {
  sheetRequest?.abort();
  if (searchHandle !== undefined) globalThis.clearTimeout(searchHandle);
  loading.value = true;
  searchHandle = globalThis.setTimeout(() => { void loadSheet(); }, 250);
}

function isNarrow() { return globalThis.matchMedia?.('(max-width: 960px)').matches ?? false; }
function focusSheet(id = selectedId.value) {
  void nextTick(() => (rowRefs.value[id] ?? searchInput.value)?.focus());
}
function selectRow(id: string) {
  selectedId.value = id;
  if (props.routeId && props.routeId !== id) emit('navigate', '/queue');
  narrowInspector.value = true;
  if (isNarrow()) void nextTick(() => inspectorHeading.value?.focus());
}
function backToSheet() {
  const id = selectedRow.value?.opportunity_id;
  narrowInspector.value = false;
  if (props.routeId) emit('navigate', '/queue');
  focusSheet(id && rowRefs.value[id] ? id : selectedId.value);
}

async function openRouted(id: string | null) {
  routeRequest?.abort();
  linkedRow.value = null;
  routeError.value = '';
  routeLoading.value = false;
  if (!id) return;
  view.value = 'sheet';
  narrowInspector.value = true;
  const existing = sheet.value.rows.find(row => row.opportunity_id === id);
  if (existing) {
    linkedRow.value = existing;
    if (isNarrow()) void nextTick(() => inspectorHeading.value?.focus());
    return;
  }
  const request = new AbortController();
  routeRequest = request;
  routeLoading.value = true;
  try {
    const opportunity = await parley.getOpportunity(id, request.signal);
    const search = opportunity.title.trim();
    let cursor: string | undefined;
    do {
      const page = await parley.getQueueSheet({ search, cursor }, request.signal);
      if (request.signal.aborted) return;
      const match = page.rows.find(row => row.opportunity_id === id);
      if (match) {
        linkedRow.value = match;
        if (isNarrow()) void nextTick(() => inspectorHeading.value?.focus());
        return;
      }
      cursor = page.next_cursor ?? undefined;
    } while (cursor);
    if (!selectedRow.value) routeError.value = 'This scenario is unavailable in Queue.';
  } catch (reason) {
    if (!request.signal.aborted && !selectedRow.value) {
      routeError.value = reason instanceof ApiError && reason.status === 404
        ? 'This scenario is unavailable in Queue.'
        : 'Could not open the linked scenario.';
    }
  } finally {
    if (!request.signal.aborted) routeLoading.value = false;
  }
}

function changePhase(row: QueueSheetRow, event: Event) {
  const control = event.currentTarget as HTMLSelectElement;
  const targetStage = control.value;
  control.value = row.phase?.stage_id ?? '';
  const target = editablePhases.value.find(phase => phase.stage_id === targetStage);
  if (!target || targetStage === row.phase?.stage_id) return;
  void submitPhase({
    opportunityId: row.opportunity_id,
    input: { target_stage_id: target.stage_id, pack_version: row.pack_version, expected_phase_revision: row.phase_revision },
    epoch: parley.workspaceEpoch, key: newOperationKey(), previous: row, source: control,
  });
}

function expirePhaseCommands() {
  uncertain.value = null;
  undoIntent.value = null;
  status.value = '';
  error.value = 'The workspace changed. Review the current phase before editing.';
  void loadSheet();
}

async function submitPhase(command: PhaseCommand) {
  const { opportunityId, input, previous, source } = command;
  if (savingId.value || (uncertain.value && uncertain.value !== command)) return;
  if (command.epoch !== parley.workspaceEpoch) { expirePhaseCommands(); return; }
  savingId.value = opportunityId;
  error.value = '';
  status.value = '';
  const focusWasOnControl = globalThis.document.activeElement === source;
  try {
    const result = await parley.setOpportunityPhase(opportunityId, input, command.key);
    if (command.epoch !== parley.workspaceEpoch) { expirePhaseCommands(); return; }
    uncertain.value = null;
    error.value = '';
    applyPhaseResult(result, previous, Boolean(input.original_event_id));
  } catch (reason) {
    if (command.epoch !== parley.workspaceEpoch) { expirePhaseCommands(); return; }
    const unknown = !(reason instanceof ApiError) || reason.status >= 500 || reason.status < 400;
    uncertain.value = unknown ? command : null;
    if (!unknown && input.original_event_id) undoIntent.value = null;
    error.value = unknown
      ? 'The service did not confirm the phase change. Retry the original request to check its result.'
      : reason.message;
    await loadSheet(undefined, true);
  } finally {
    savingId.value = '';
    await nextTick();
    const focused = globalThis.document.activeElement;
    if (focusWasOnControl && (focused === source || focused === globalThis.document.body)) {
      if (source?.getClientRects().length && !uncertain.value) source.focus();
      else if (isNarrow() && narrowInspector.value) inspectorHeading.value?.focus();
      else focusSheet();
    }
  }
}

function applyPhaseResult(result: SetPhaseResult, previous: QueueSheetRow | undefined, undo: boolean) {
  const row = result.row;
  const nextLabel = stageLabel(result.row.phase);
  const index = sheet.value.rows.findIndex(item => item.opportunity_id === row.opportunity_id);
  const leavesFilter = Boolean(phaseFilter.value && (row.phase?.stage_id ?? '__unmapped__') !== phaseFilter.value);
  if (row.opportunity_id === props.routeId) linkedRow.value = row;
  if (leavesFilter && index >= 0) {
    const nextRows = sheet.value.rows.filter(item => item.opportunity_id !== row.opportunity_id);
    sheet.value = { ...sheet.value, rows: nextRows, total: Math.max(0, sheet.value.total - 1) };
    if (selectedId.value === row.opportunity_id) selectedId.value = (nextRows[index] ?? nextRows[index - 1])?.opportunity_id ?? '';
    if (!nextRows.length && !props.routeId) narrowInspector.value = false;
  } else if (index >= 0) {
    sheet.value = { ...sheet.value, rows: sheet.value.rows.map(item => item.opportunity_id === row.opportunity_id ? row : item) };
  }
  const samePhase = (previous?.phase?.stage_id ?? '__unmapped__') === (row.phase?.stage_id ?? '__unmapped__');
  if (result.event_id && selectedRow.value?.opportunity_id === row.opportunity_id) {
    latestPhaseEvent.value = undo ? null : { id: result.event_id, opportunityId: row.opportunity_id, revision: result.phase_revision };
  }
  if (result.event_id) sessionEventReader.hint();
  status.value = `${row.title} ${!undo && previous && samePhase ? 'is already at' : undo ? 'returned to' : 'moved to'} ${nextLabel}${leavesFilter ? ` and is hidden by the ${phaseLabelFor(phaseFilter.value)} filter` : ''}.`;
  if (undo) undoIntent.value = null;
  else if (previous?.phase && previous.phase.stage_id !== row.phase?.stage_id && result.event_id) {
    undoIntent.value = { opportunityId: row.opportunity_id, toStage: previous.phase.stage_id, packVersion: previous.phase.pack_version, eventId: result.event_id, phaseRevision: result.phase_revision, epoch: parley.workspaceEpoch };
  } else undoIntent.value = null;
  void loadSheet(undefined, true).then(() => {
    if (undo && sheet.value.rows.some(item => item.opportunity_id === row.opportunity_id)) {
      selectedId.value = row.opportunity_id;
      if (!isNarrow() || !narrowInspector.value) focusSheet(row.opportunity_id);
    }
  });
  if (selectedRow.value?.opportunity_id === row.opportunity_id) void loadDetail(row.opportunity_id);
}

function phaseLabelFor(stageId: string): string {
  const phase = phases.value.find((item) => item.stage_id === stageId);
  return phase?.label ?? stageId.replaceAll('_', ' ');
}

function undoPhaseChange(event: Event) {
  const intent = undoIntent.value;
  if (!intent) return;
  void submitPhase({
    opportunityId: intent.opportunityId,
    input: { target_stage_id: intent.toStage, pack_version: intent.packVersion, expected_phase_revision: intent.phaseRevision, original_event_id: intent.eventId },
    epoch: intent.epoch, key: newOperationKey(), source: event.currentTarget as HTMLElement,
  });
}

function clearFilters() {
  query.value = '';
  phaseFilter.value = '';
  sortKey.value = 'due';
  void nextTick(() => searchInput.value?.focus());
}

function loadMore() {
  if (sheet.value.next_cursor && !loading.value) void loadSheet(sheet.value.next_cursor);
}

function toggleCreate() {
  createOpen.value = !createOpen.value;
  void nextTick(() => (createOpen.value ? createInput.value : createTrigger.value)?.focus());
}
function rowPhaseChoices(row: QueueSheetRow) {
  const phase = row.phase;
  return phase && !editablePhases.value.some(option => option.stage_id === phase.stage_id)
    ? [phase, ...editablePhases.value]
    : editablePhases.value;
}

type DetailExchange = { participantId: string; participantName: string; theyGet: string; iGet: string };
type DetailSource = { id: string; participantName: string; statement: string; knowledgeKind: string; claim: { wording: string; tier: number; speakerKind: string } | null };
type DetailHistory = { cursor: string; when: string; label: string };
type DetailThread = { recipientId: string; state: string; nextActionAt: string | null };
const detail = ref<{ exchanges: DetailExchange[]; sources: DetailSource[]; history: DetailHistory[]; threads: DetailThread[] } | null>(null);
const detailLoading = ref(false);
const detailError = ref('');
let detailVersion = 0;

function eventSummary(payload: Record<string, unknown>, eventType: string): string {
  const text = (key: string) => (typeof payload[key] === 'string' ? (payload[key] as string) : null);
  switch (eventType) {
    case 'opportunity.created': return 'Opportunity created';
    case 'opportunity.participant_added': return `Participant added${text('role') ? ` · ${text('role')}` : ''}`;
    case 'opportunity.phase_changed': return `Phase ${text('from_stage_id') ? phaseLabelFor(text('from_stage_id')!) : '—'} → ${text('to_stage_id') ? phaseLabelFor(text('to_stage_id')!) : ''}${payload.operation === 'undo' ? ' (undo)' : ''}`;
    case 'opportunity.exchange_version_created': return 'Proposed exchange recorded';
    case 'opportunity.exchange_attested': return 'Exchange attested';
    case 'opportunity.interest_added': return 'Interest recorded';
    case 'opportunity.source_linked': return 'Source linked';
    case 'opportunity.source_unlinked': return 'Source unlinked';
    case 'opportunity.source_claim_corrected': return 'Source claim corrected';
    case 'opportunity.notes_updated': return 'Notes updated';
    case 'opportunity.task_toggled': return 'Task updated';
    case 'opportunity.tags_changed': return 'Tags updated';
    case 'opportunity.resolved': return `Resolved${text('result') ? ` · ${text('result')}` : ''}`;
    case 'opportunity.closed': return `Closed${text('reason') ? ` · ${text('reason')}` : ''}`;
    case 'opportunity.reopened': return 'Reopened';
    default: return eventType.replaceAll('_', ' ');
  }
}

async function loadHistory(opportunityId: string, version: number) {
  const items: DetailHistory[] = [];
  let phaseEvent: typeof latestPhaseEvent.value = null;
  let cursor = '0';
  while (version === detailVersion) {
    const result = await parley.getEvents(cursor, { entity_type: 'opportunity', entity_id: opportunityId });
    for (const event of result.items) {
      if (event.event_type === 'opportunity.phase_changed') {
        phaseEvent = event.payload.operation === 'manual'
          ? { id: event.event_id, opportunityId, revision: event.revision }
          : null;
      }
    }
    items.push(...result.items.map(event => ({
      cursor: event.cursor, when: formatActionDate(event.occurred_at), label: eventSummary(event.payload ?? {}, event.event_type),
    })));
    if (!result.items.length || result.next_cursor === cursor || result.next_cursor === result.observed_cursor) break;
    cursor = result.next_cursor;
  }
  return { items, phaseEvent };
}

async function loadDetail(opportunityId: string, reset = false) {
  const own = ++detailVersion;
  if (reset) { detail.value = null; latestPhaseEvent.value = null; }
  detailError.value = '';
  const row = selectedRow.value;
  if (row?.opportunity_id !== opportunityId) { detailLoading.value = false; return; }
  detailLoading.value = true;
  const [participantResults, [history, threads]] = await Promise.all([
    Promise.all(row.participants.map(async (participant) => {
      const [exchange, interests] = await Promise.allSettled([
        parley.getWritingExchange(opportunityId, participant.id),
        parley.getWritingInterests(opportunityId, participant.id),
      ]);
      return { participant, exchange, interests };
    })),
    Promise.allSettled([loadHistory(opportunityId, own), parley.listThreads({ opportunity_id: opportunityId })]),
  ]);
  if (own !== detailVersion) return;
  const exchanges: DetailExchange[] = [];
  const sources: DetailSource[] = [];
  let incomplete = history.status === 'rejected' || threads.status === 'rejected';
  for (const { participant, exchange, interests } of participantResults) {
    if (exchange.status === 'rejected' || interests.status === 'rejected') incomplete = true;
    if (exchange.status === 'fulfilled') {
      const current = exchange.value.current;
      if (current && typeof current.they_get === 'string' && typeof current.i_get === 'string') {
        exchanges.push({ participantId: participant.id, participantName: participant.name, theyGet: current.they_get, iGet: current.i_get });
      }
    }
    if (interests.status === 'fulfilled') {
      for (const interest of interests.value) {
        sources.push({
          id: interest.id, participantName: participant.name, statement: interest.statement,
          knowledgeKind: interest.knowledge_kind,
          claim: interest.claim ? { wording: interest.claim.wording, tier: interest.claim.tier, speakerKind: interest.claim.speaker_kind } : null,
        });
      }
    }
  }
  detail.value = {
    exchanges,
    sources,
    history: history.status === 'fulfilled' ? history.value.items : [],
    threads: threads.status === 'fulfilled' ? threads.value.map(thread => ({
      recipientId: thread.recipient_id, state: thread.state, nextActionAt: thread.next_action_at ?? null,
    })) : [],
  };
  if (history.status === 'fulfilled') latestPhaseEvent.value = history.value.phaseEvent;
  if (incomplete) detailError.value = 'Some scenario context could not be loaded.';
  detailLoading.value = false;
}

function threadFor(participantId: string): DetailThread | null {
  return detail.value?.threads.find((thread) => thread.recipientId === participantId) ?? null;
}

watch(() => selectedRow.value?.opportunity_id, id => { void loadDetail(id ?? '', true); });

function openInParties(partyId: string) {
  emit('navigate', `/parties/${encodeURIComponent(partyId)}`);
}

function openInOutput(materialId: string) {
  emit('navigate', `/output/${encodeURIComponent(materialId)}`);
}

async function createOpportunity() {
  if (createBusy.value || !createTitle.value.trim() || !createPartyId.value) return;
  createBusy.value = true;
  error.value = '';
  try {
    const opportunity = await parley.createOpportunity(createTitle.value.trim(), createPartyId.value);
    createTitle.value = '';
    createOpen.value = false;
    view.value = 'sheet';
    status.value = 'Opportunity created.';
    await loadSheet();
    if (sheet.value.rows.some(row => row.opportunity_id === opportunity.id)) {
      selectRow(opportunity.id);
      if (!isNarrow()) focusSheet(opportunity.id);
    } else {
      emit('navigate', `/queue/${encodeURIComponent(opportunity.id)}`);
    }
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'Could not create the opportunity.';
  } finally {
    createBusy.value = false;
  }
}

watch([query, phaseFilter, sortKey], ([nextQuery, nextPhase, nextSort], [oldQuery, oldPhase, oldSort]) => {
  if (!ready.value) return;
  savedScroll = 0;
  if (sheetScroll.value) sheetScroll.value.scrollTop = 0;
  if (nextPhase === oldPhase && nextSort === oldSort && nextQuery !== oldQuery && nextQuery.trim()) scheduleSearch();
  else void loadSheet();
});
watch(() => props.routeId, id => { if (ready.value) void openRouted(id); });
watch(() => props.active, async active => {
  if (!active || !ready.value) return;
  await loadSheet(undefined, true);
  if (selectedRow.value) void loadDetail(selectedRow.value.opportunity_id);
});

onMounted(async () => {
  try {
    await parley.connect();
    void searchParties('').catch(() => { /* the picker reports an empty list until retry */ });
    sessionEventReader.start({
      epoch: () => parley.workspaceEpoch ?? '',
      onEvents: page => {
        if (!props.active) return;
        if (view.value === 'insights' && page.items.some(event =>
          event.entity_type === 'opportunity' || event.event_type.startsWith('coach.') || event.event_type.startsWith('match.'),
        )) inferenceActivity.refresh();
        if (!page.items.some(event => event.entity_type === 'opportunity')) return;
        if (savingId.value) {
          error.value = 'A phase edit is in flight. The sheet will refresh after it finishes.';
          return;
        }
        void loadSheet(undefined, true).then(() => {
          if (selectedRow.value) void loadDetail(selectedRow.value.opportunity_id);
        });
      },
    });
    restorePrefs();
    await loadSheet();
    ready.value = true;
    if (props.routeId) void openRouted(props.routeId);
    refreshScopedActions();
  } catch (reason) {
    loadError.value = reason instanceof Error ? reason.message : 'Could not load the Queue sheet.';
  }
});

onBeforeUnmount(() => {
  if (searchHandle) globalThis.clearTimeout(searchHandle);
  sheetRequest?.abort();
  routeRequest?.abort();
  detailVersion++;
  disposeActions?.();
  sessionEventReader.stop();
});
</script>

<template>
  <section
    class="queue-workspace"
    aria-labelledby="queue-heading"
  >
    <h2
      id="queue-heading"
      class="sr-only"
    >
      Opportunities
    </h2>
    <nav
      aria-label="Queue views"
      class="queue-views"
    >
      <button
        type="button"
        :aria-pressed="view === 'sheet'"
        @click="view = 'sheet'"
      >
        All scenarios
      </button>
      <button
        type="button"
        :aria-pressed="view === 'daily'"
        @click="view = 'daily'"
      >
        Daily work
      </button>
      <button
        type="button"
        :aria-pressed="view === 'insights'"
        @click="view = 'insights'"
      >
        Insights
      </button>
      <button
        ref="createTrigger"
        type="button"
        class="primary new-opportunity"
        :aria-expanded="createOpen"
        aria-controls="queue-create"
        @click="toggleCreate"
      >
        New opportunity
      </button>
    </nav>

    <form
      v-show="createOpen"
      id="queue-create"
      class="create-form"
      @keydown.esc.stop.prevent="!createBusy && toggleCreate()"
      @submit.prevent="createOpportunity"
    >
      <label>Opportunity title<input
        ref="createInput"
        v-model="createTitle"
        type="text"
        required
        maxlength="500"
        :readonly="createBusy"
      ></label>
      <SearchableSelect
        id="create-party"
        label="Participant"
        placeholder="Filter parties"
        :model-value="createPartyId"
        :options="partyOptions"
        :search="searchParties"
        :disabled="createBusy"
        required
        @update:model-value="createPartyId = $event"
      />
      <button
        type="submit"
        class="primary"
        :disabled="createBusy || !createTitle.trim() || !createPartyId"
      >
        Create opportunity
      </button>
      <button
        type="button"
        :disabled="createBusy"
        @click="toggleCreate"
      >
        Cancel
      </button>
      <p
        v-if="!partyOptions.length"
        class="hint"
      >
        <button
          type="button"
          class="link"
          @click="emit('navigate', '/parties')"
        >
          Add a party first
        </button>
      </p>
    </form>

    <div
      v-if="status || error || undoIntent || uncertain"
      class="undo-toast"
    >
      <span
        v-if="error"
        role="alert"
      >{{ error }}</span>
      <span
        v-else
        role="status"
      >{{ status }}</span>
      <button
        v-if="uncertain"
        type="button"
        class="undo"
        :disabled="!!savingId"
        @click="submitPhase(uncertain)"
      >
        Retry phase change
      </button>
      <button
        v-else-if="undoIntent"
        type="button"
        class="undo"
        :disabled="!!savingId"
        @click="undoPhaseChange"
      >
        Undo phase change
      </button>
    </div>

    <div
      v-show="view === 'sheet'"
      class="sheet-layout"
      :class="{ 'show-inspector': narrowInspector }"
    >
      <section
        class="panel sheet-panel"
        aria-label="Scenario sheet"
      >
        <div class="queue-filter-sort">
          <label>Find scenario
            <input
              ref="searchInput"
              v-model="query"
              type="search"
              aria-label="Find scenario"
              placeholder="Scenario, party or contact"
            >
          </label>
          <label>Phase
            <select
              v-model="phaseFilter"
              aria-label="Phase filter"
            >
              <option value="">All phases</option>
              <option
                v-for="phase in phases"
                :key="phase.stage_id"
                :value="phase.stage_id"
              >
                {{ phase.label }} ({{ phase.count }})
              </option>
            </select>
          </label>
          <label>Order
            <select
              v-model="sortKey"
              aria-label="Order"
            >
              <option value="due">Next action date</option>
              <option value="name">Scenario name</option>
            </select>
          </label>
        </div>

        <template v-if="loading && !sheet.rows.length && !loadError">
          <p
            class="meta"
            role="status"
          >
            Loading scenarios…
          </p>
        </template>

        <template v-else-if="loadError">
          <div
            class="empty-state"
          >
            <h3>Could not load scenarios</h3>
            <p>{{ loadError }}</p>
            <button
              type="button"
              @click="() => loadSheet()"
            >
              Retry
            </button>
          </div>
        </template>

        <template v-else-if="emptyWorkspace">
          <div
            class="empty-state"
          >
            <h3>No scenarios yet</h3>
            <button
              type="button"
              @click="emit('navigate', '/parties')"
            >
              Open Parties
            </button>
          </div>
        </template>

        <template v-else-if="noMatches">
          <div
            class="empty-state"
          >
            <h3>No matching scenarios</h3>
            <p>Change the phase filter or search to see more.</p>
            <button
              type="button"
              @click="clearFilters"
            >
              Clear filters
            </button>
          </div>
        </template>

        <template v-else>
          <div
            ref="sheetScroll"
            class="scroll"
            @scroll.passive="rememberScroll"
          >
            <table role="table">
              <thead role="rowgroup">
                <tr role="row">
                  <th
                    scope="col"
                    role="columnheader"
                  >
                    Scenario / party
                  </th>
                  <th
                    scope="col"
                    role="columnheader"
                  >
                    Phase
                  </th>
                  <th
                    scope="col"
                    role="columnheader"
                  >
                    Next action
                  </th>
                  <th
                    scope="col"
                    role="columnheader"
                  >
                    Action date
                  </th>
                </tr>
              </thead>
              <tbody role="rowgroup">
                <tr
                  v-for="row in sheet.rows"
                  :key="row.opportunity_id"
                  role="row"
                  :class="{ active: row.opportunity_id === selectedRow?.opportunity_id }"
                  :data-opportunity-id="row.opportunity_id"
                >
                  <td
                    role="cell"
                    class="scenario"
                  >
                    <button
                      :ref="(el) => { rowRefs[row.opportunity_id] = el as HTMLElement | null; }"
                      type="button"
                      class="open"
                      :data-open="row.opportunity_id"
                      :aria-pressed="row.opportunity_id === selectedRow?.opportunity_id"
                      @click="selectRow(row.opportunity_id)"
                    >
                      {{ row.title }}
                    </button>
                    <div class="small">
                      {{ row.participants.map((p) => p.name).join(', ') }}
                    </div>
                  </td>
                  <td
                    role="cell"
                    class="phase-cell"
                  >
                    <span
                      class="mobile-label"
                      aria-hidden="true"
                    >Phase</span>
                    <select
                      class="phase"
                      :aria-label="`Phase for ${row.title}`"
                      :value="row.phase?.stage_id ?? ''"
                      :disabled="row.lifecycle !== 'open' || !editablePhases.length || !!savingId || !!uncertain"
                      @change="changePhase(row, $event)"
                    >
                      <option
                        value=""
                        disabled
                      >
                        Unmapped
                      </option>
                      <option
                        v-for="phase in rowPhaseChoices(row)"
                        :key="phase.stage_id"
                        :value="phase.stage_id"
                        :disabled="phase.kind !== 'planning'"
                      >
                        {{ phase.label }}
                      </option>
                    </select>
                    <span
                      v-if="savingId === row.opportunity_id"
                      class="saving"
                      role="status"
                    >Saving…</span>
                  </td>
                  <td
                    role="cell"
                    class="action"
                  >
                    <span
                      class="mobile-label"
                      aria-hidden="true"
                    >Next action</span>
                    {{ row.next_action.label }}
                    <div
                      v-if="row.next_action.recipient_name"
                      class="small"
                    >
                      {{ row.next_action.recipient_name }}
                    </div>
                  </td>
                  <td
                    class="date"
                    role="cell"
                    :class="{ due: actionDateLabel(row) === 'Today' }"
                  >
                    <span
                      class="mobile-label"
                      aria-hidden="true"
                    >Action date</span>
                    {{ actionDateLabel(row) }}
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          <div class="meta">
            <span aria-live="polite">{{ countLabel }}</span>
            <button
              v-if="sheet.next_cursor"
              type="button"
              :disabled="loading"
              @click="loadMore"
            >
              Load more
            </button>
          </div>
        </template>
      </section>

      <aside
        class="panel inspector"
        aria-label="Scenario details"
        @keydown.esc.stop="isNarrow() && backToSheet()"
      >
        <button
          v-if="narrowInspector || linkedOutsideSheet || routeError"
          type="button"
          class="back"
          :class="{ 'linked-back': linkedOutsideSheet || routeError }"
          @click="backToSheet"
        >
          Back to list
        </button>
        <template v-if="selectedRow">
          <div class="top">
            <small>{{ selectedRow.participants.map((p) => p.name).join(', ') || 'Scenario' }}</small>
            <h3
              ref="inspectorHeading"
              tabindex="-1"
            >
              {{ selectedRow.title }}
            </h3>
            <p
              v-if="linkedOutsideSheet"
              class="hint"
            >
              Opened from a link · outside the current sheet.
            </p>
          </div>
          <div class="body">
            <div class="block">
              <label>Scenario phase
                <select
                  :value="selectedRow.phase?.stage_id ?? ''"
                  :disabled="selectedRow.lifecycle !== 'open' || !editablePhases.length || !!savingId || !!uncertain"
                  :aria-label="`Inspector phase for ${selectedRow.title}`"
                  @change="changePhase(selectedRow, $event)"
                >
                  <option
                    value=""
                    disabled
                  >Unmapped</option>
                  <option
                    v-for="phase in rowPhaseChoices(selectedRow)"
                    :key="phase.stage_id"
                    :value="phase.stage_id"
                    :disabled="phase.kind !== 'planning'"
                  >
                    {{ phase.label }}
                  </option>
                </select>
              </label>
              <p
                v-if="savingId === selectedRow.opportunity_id"
                class="saving"
                role="status"
              >
                Saving phase…
              </p>
              <small>NEXT ACTION</small>
              <h4>{{ selectedRow.next_action.label }}</h4>
              <p class="hint">
                {{ selectedRow.next_action.reason }}
              </p>
              <p
                v-if="selectedRow.next_action.date"
                class="hint"
              >
                {{ selectedRow.next_action.kind === 'wait' ? 'Check on' : 'Action date' }} {{ formatActionDate(selectedRow.next_action.date) }}
              </p>
              <p
                v-if="selectedRow.other_action_count"
                class="hint"
              >
                {{ selectedRow.other_action_count }} more {{ selectedRow.other_action_count === 1 ? 'action' : 'actions' }}
              </p>
              <p
                v-if="selectedRow.lifecycle !== 'open'"
                class="hint"
              >
                Lifecycle: {{ selectedRow.lifecycle }}<template v-if="selectedRow.close_reason">
                  — {{ selectedRow.close_reason }}
                </template>.
              </p>
            </div>
            <p
              v-if="detailLoading"
              class="hint"
              role="status"
            >
              Loading scenario context…
            </p>
            <p
              v-if="detailError"
              class="hint"
              role="alert"
            >
              {{ detailError }}
              <button
                type="button"
                @click="loadDetail(selectedRow.opportunity_id)"
              >
                Retry
              </button>
            </p>
            <div
              v-if="detail?.exchanges.length"
              class="block"
            >
              <small>PROPOSED EXCHANGE</small>
              <div
                v-for="entry in detail!.exchanges"
                :key="entry.participantId"
                class="exchange"
              >
                <p
                  v-if="detail!.exchanges.length > 1"
                  class="hint"
                >
                  {{ entry.participantName }}
                </p>
                <p>
                  <strong>They get</strong> · {{ entry.theyGet }}
                </p>
                <p>
                  <strong>I hope to get</strong> · {{ entry.iGet }}
                </p>
              </div>
            </div>
            <details class="block disclosure">
              <summary>Party &amp; source</summary>
              <ul class="participants">
                <li
                  v-for="party in selectedRow.participants"
                  :key="party.id"
                >
                  <button
                    type="button"
                    class="link"
                    @click="openInParties(party.id)"
                  >
                    Open {{ party.name }} in Parties
                  </button>
                  <span class="hint"> · {{ party.role }}</span>
                  <span
                    v-if="threadFor(party.id)"
                    class="hint"
                  > · thread {{ threadFor(party.id)!.state }}<template v-if="threadFor(party.id)!.nextActionAt">
                    · next {{ formatActionDate(threadFor(party.id)!.nextActionAt) }}
                  </template></span>
                </li>
              </ul>
              <small v-if="detail?.sources.length">SOURCE PASSAGES</small>
              <details
                v-for="passage in detail?.sources ?? []"
                :key="passage.id"
                class="source-passage"
              >
                <summary>{{ passage.statement }}</summary>
                <blockquote
                  v-if="passage.claim"
                  class="passage-quote"
                >
                  {{ passage.claim.wording }}
                </blockquote>
                <small
                  v-if="passage.claim"
                  class="hint"
                >
                  {{ passage.claim.speakerKind }} · T{{ passage.claim.tier }} · {{ passage.participantName }}
                </small>
                <small
                  v-else
                  class="hint"
                >
                  Owner-entered · {{ passage.participantName }}
                </small>
              </details>
            </details>
            <details
              v-if="selectedMaterials.length"
              class="block disclosure"
            >
              <summary>Linked material ({{ selectedMaterials.length }})</summary>
              <ul class="participants">
                <li
                  v-for="material in selectedMaterials"
                  :key="material.id"
                >
                  <button
                    type="button"
                    class="link"
                    @click="openInOutput(material.id)"
                  >
                    Review {{ material.kind }} v{{ material.version }} in Output
                  </button>
                  <span class="hint"> · {{ material.status }}</span>
                </li>
              </ul>
            </details>
            <details
              v-if="detail?.history.length"
              class="block disclosure"
            >
              <summary>
                Scenario history ({{ detail!.history.length }})
              </summary>
              <ol class="history-list">
                <li
                  v-for="entry in detail!.history"
                  :key="entry.cursor"
                >
                  <span class="hint">{{ entry.when }}</span> · {{ entry.label }}
                </li>
              </ol>
            </details>
          </div>
        </template>
        <template v-else>
          <div class="body">
            <h3>Scenario details</h3>
            <p
              v-if="routeLoading"
              role="status"
            >
              Opening linked scenario…
            </p>
            <template v-else-if="routeError">
              <p role="alert">
                {{ routeError }}
              </p>
              <button
                type="button"
                @click="openRouted(props.routeId)"
              >
                Retry
              </button>
            </template>
            <p v-else>
              Select a scenario to see its details.
            </p>
          </div>
        </template>
      </aside>
    </div>

    <div v-show="view === 'daily'">
      <Daily @navigate="emit('navigate', $event)" />
    </div>
    <Insights
      v-if="props.active && view === 'insights'"
      @navigate="emit('navigate', $event)"
    />
  </section>
</template>

<style scoped>
.queue-workspace { --line: #c7d1c3; --muted: #53665b; --panel: #fffef9; }
.queue-views { display: flex; flex-wrap: wrap; gap: .45rem; margin: 0 0 .85rem; }
.queue-views button, .create-form button { min-height: 2.5rem; }
.queue-views .new-opportunity { margin-inline-start: auto; }
.create-form { display: flex; flex-wrap: wrap; gap: .55rem; align-items: end; border-color: var(--line); background: var(--panel); padding: .8rem 1rem; margin: 0 0 .85rem; }
.create-form label { flex: 1 1 12rem; margin: 0; }
.create-form input, .create-form select, .queue-filter-sort input, .queue-filter-sort select { min-height: 2.5rem; }
.undo-toast { display: flex; flex-wrap: wrap; align-items: center; gap: .55rem; border: 1px solid var(--line); border-inline-start: 3px solid #225744; background: #eff3e9; border-radius: .45rem; padding: .55rem .85rem; margin-bottom: .85rem; }
.undo-toast .undo { margin-inline-start: auto; }
.sheet-layout { display: grid; grid-template-columns: minmax(0, 2.18fr) minmax(17.5rem, 1fr); gap: 1rem; align-items: start; }
.panel { min-width: 0; padding: 0; border: 1px solid var(--line); border-radius: .55rem; background: var(--panel); overflow: hidden; }
.queue-filter-sort { display: grid; grid-template-columns: minmax(0, 1.6fr) repeat(2, minmax(0, 1fr)); gap: .65rem; margin: 0; padding: .8rem 1rem; }
.queue-filter-sort label { min-width: 0; margin: 0; color: #334a3e; }
.queue-filter-sort input, .queue-filter-sort select { width: 100%; }
.scroll { overflow: auto; max-height: min(63dvh, 46rem); }
table { width: 100%; table-layout: fixed; border-collapse: separate; border-spacing: 0; text-align: left; }
td, th { border-top: 1px solid #d7dcd1; padding: .8rem .6rem; vertical-align: top; overflow-wrap: anywhere; }
td:first-child, th:first-child { padding-inline-start: 1rem; }
th { position: sticky; top: 0; z-index: 1; background: #f0f2e9; color: #445b4d; font: .83rem/1.5 ui-monospace, monospace; }
th:first-child { width: 30%; }
th:nth-child(2) { width: 24%; }
th:last-child { width: 15%; }
tr.active { background: #e8efe6; }
tr.active td:first-child { box-shadow: inset 3px 0 #225744; }
.open, .open[aria-pressed="true"] { background: transparent; box-shadow: none; color: inherit; border: 0; padding: .2rem 0; text-align: left; font-weight: 650; overflow-wrap: anywhere; }
.open:hover { text-decoration: underline; }
.small, .hint { color: var(--muted); font-size: .9rem; margin: .25rem 0 0; }
.phase { min-height: 2.3rem; max-width: 12rem; }
.date { font-variant-numeric: tabular-nums; }
.due { color: #804824; font-weight: 650; }
.saving { display: block; margin-top: .25rem; font-size: .9rem; color: #225744; }
.meta { border-top: 1px solid var(--line); padding: .65rem 1rem; color: var(--muted); display: flex; justify-content: space-between; align-items: center; gap: .55rem; margin: 0; }
.top { border-bottom: 1px solid var(--line); padding: 1rem; background: #edf2e8; }
.top small { display: block; margin-bottom: .35rem; font: .85rem/1.45 ui-monospace, monospace; color: var(--muted); }
.top h3 { margin: 0; font: 1.85rem/1.17 Georgia, serif; }
.inspector { overflow-wrap: anywhere; }
.back { display: none; margin: .65rem 1rem; min-height: 2.5rem; }
.linked-back { display: inline-block; }
.body { padding: 1rem; }
.block { border-top: 1px solid var(--line); padding: .75rem 0; }
.body > .block:first-child { border-top: 0; padding-top: 0; }
.body > .block:last-child { padding-bottom: 0; }
.block small { font: .8rem/1.5 ui-monospace, monospace; color: var(--muted); }
.block h4 { margin: .2rem 0; font: 1.28rem/1.2 Georgia, serif; }
.block label { margin: 0 0 .75rem; max-width: 15rem; }
.block select { min-height: 2.5rem; }
.disclosure > summary { font-weight: 550; }
.participants { list-style: none; padding: 0; margin: .5rem 0 0; }
.participants li { margin: .35rem 0; }
.link { background: transparent; border: 0; padding: .15rem 0; color: #225744; font-weight: 600; text-align: left; overflow-wrap: anywhere; }
.link:hover { text-decoration: underline; }
.empty-state { padding: 1.25rem 1rem; border-top: 1px solid var(--line); }
.empty-state h3 { margin-top: 0; }
.empty-state button + button { margin-inline-start: .4rem; }
.exchange { margin-top: .45rem; }
.exchange p { margin: .25rem 0; }
.source-passage { padding: .35rem 0; }
.passage-quote { margin: .3rem 0; padding: .4rem .65rem; border-inline-start: 3px solid #225744; background: #eff3e9; }
.history-list { margin: .5rem 0 0; padding-inline-start: 1.25rem; }
.history-list li { margin: .3rem 0; }
.mobile-label { display: none; }
@media (max-width: 960px) {
  .sheet-layout { grid-template-columns: minmax(0, 1fr); }
  .sheet-layout:not(.show-inspector) .inspector, .show-inspector .sheet-panel { display: none; }
  .back { display: inline-block; }
}
@media (max-width: 600px) {
  .queue-views .new-opportunity { margin-inline-start: 0; }
  .queue-filter-sort { grid-template-columns: repeat(2, minmax(0, 1fr)); padding: .7rem .8rem; }
  .queue-filter-sort label:first-child { grid-column: 1 / -1; }
  .scroll { max-height: none; overflow: visible; }
  table, tbody { display: block; }
  thead { position: absolute; clip-path: inset(50%); width: 1px; height: 1px; overflow: hidden; white-space: nowrap; }
  tbody tr { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); padding: .7rem .8rem; gap: .55rem .8rem; border-top: 1px solid #d7dcd1; }
  td, td:first-child { border: 0; padding: 0; }
  .scenario, .action { grid-column: 1 / -1; }
  .phase-cell { grid-column: 1; grid-row: 2; }
  .date { grid-column: 2; grid-row: 2; }
  tr.active { box-shadow: inset 3px 0 #225744; }
  tr.active td:first-child { box-shadow: none; }
  .mobile-label { display: block; font-size: .82rem; color: var(--muted); margin-bottom: .15rem; }
  .open { min-height: 2.25rem; }
}
</style>
