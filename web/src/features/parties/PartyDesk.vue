<script setup lang="ts">
/* global HTMLElement, HTMLDialogElement, HTMLInputElement */
import { computed, nextTick, onMounted, onBeforeUnmount, ref, watch } from 'vue';
import {
  newOperationKey, parley, ApiError,
  type PartyImportReceipt, type Dossier, type EntityNotes, type ExtractorJob, type ExtractorProposal,
  type GraphSnapshot, type GraphEntity, type OpportunitySummary, type SavedSource,
  type CoachSelector, type CoachQuestion,
} from '../../api/client';
import { commands } from '../../api/commands';
import { partyActions, registerScoped, useCommandRegistry } from '../../app/commands/registry';
import ClaimCorrection from './ClaimCorrection.vue';
import PartyFiles from './PartyFiles.vue';
import PartyImport from './PartyImport.vue';
import PartySheet from './PartySheet.vue';
import PartyInspector from './PartyInspector.vue';
import RelationshipGraph from '../leads/RelationshipGraph.vue';
import BriefWorkspace from '../leads/BriefWorkspace.vue';
import CoachQuestions from '../coach/CoachQuestions.vue';
import MarkdownEditor from '../../shared/MarkdownEditor.vue';
import MarkdownPreview from '../../shared/MarkdownPreview.vue';

const props = defineProps<{
  parties: GraphEntity[];
  sources: SavedSource[];
  opportunities: OpportunitySummary[];
  partyId?: string;
  active?: boolean;
  inbox?: boolean;
}>();

const emit = defineEmits<{ navigate: [path: string]; changed: [] }>();

// Two identities drive navigation (amendment §1): the sheet root and the
// inspected record stay separate. Row click sets both; node click sets only
// the selection; "open network" promotes the selection to root.
const rootPartyId = ref<string | null>(props.partyId ?? null);
const selectedRecordId = ref<string | null>(props.partyId ?? null);

const query = ref('');
const kind = ref<'all' | 'person' | 'org'>('all');
const sort = ref<'name' | 'type' | 'focus'>('name');
const addOpen = ref(false);
const partyKind = ref<'person' | 'org'>('person');
const partyName = ref('');
const partyFocus = ref('');
const partyNotes = ref('');
const newPartyFiles = ref<string[]>([]);
const newPartyFilesBusy = ref(false);
const duplicateParties = computed(() => {
  const name = partyName.value.trim().toLocaleLowerCase();
  return name ? props.parties.filter(p => p.kind === partyKind.value && p.name.toLocaleLowerCase() === name) : [];
});
let createKey = '';
let createIntent = '';
const focusEdit = ref<string | null>(null);
const focusDraft = ref('');
const focusBase = ref('');
const busy = ref(false);
const status = ref('');
const error = ref('');

const cardOpen = ref(false);
const graph = ref<InstanceType<typeof RelationshipGraph> | null>(null);
const graphRevision = ref(0);
const graphNodes = ref<GraphSnapshot['nodes']>([]);
const inspectedEdge = ref<GraphSnapshot['edges'][number] | null>(null);
const removingEdge = ref(false);
const correctingClaim = ref(false);
let removalKey = '';
function correctEdge() {
  if (!inspectedEdge.value) return;
  pane.value = 'graph';
  graph.value?.openRelated(inspectedEdge.value);
}
async function removeEdge() {
  if (!inspectedEdge.value || busy.value) return;
  const edge = inspectedEdge.value;
  busy.value = true; error.value = '';
  try {
    await parley.removeEdge(edge.id, edge.revision, removalKey ||= newOperationKey());
    inspectedEdge.value = null; removingEdge.value = false; removalKey = '';
    graphRevision.value++; await loadDossier();
    status.value = 'Relationship removed. Both cards remain in the workspace.';
  } catch(reason) { error.value = reason instanceof Error ? reason.message : 'Could not remove relationship.'; }
  finally { busy.value = false; }
}
async function claimSaved() {
  correctingClaim.value = false;
  const edgeId = inspectedEdge.value?.id;
  await graph.value?.reload();
  if (edgeId) graph.value?.inspectEdge(edgeId);
  await loadDossier();
  changed();
  status.value = 'Attribution corrected.';
}
const retirement = ref<Awaited<ReturnType<typeof parley.getEntityUsage>> | null>(null);
let retireKey = '';
async function reviewRetirement() {
  const id = selectedRecordId.value;
  if (!id) return;
  error.value = '';
  try {
    const usage = await parley.getEntityUsage(id);
    if (selectedRecordId.value === id) { retirement.value = usage; retireKey = newOperationKey(); }
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not load usage.'; }
}
async function retireSelection() {
  const saved = dossier.value?.entity;
  if (!saved || busy.value || quarantined.value) return;
  busy.value = true; error.value = '';
  try {
    await parley.retireEntity(saved, retireKey);
    retirement.value = null;
    changed();
    graphRevision.value++;
    await loadDossier();
    status.value = 'Card retired.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not retire the card.'; }
  finally { busy.value = false; }
}
const pane = ref<'sheet' | 'graph' | 'details'>('sheet');
const auxiliary = ref<'inspector' | 'chat'>('inspector');
const desk = ref<HTMLElement | null>(null);
const cardEditor = ref<HTMLElement | null>(null);
const draftDialog = ref<HTMLDialogElement | null>(null);
const partyForm = ref<HTMLElement | null>(null);
const addPartyButton = ref<HTMLElement | null>(null);
const openingOpportunity = ref(false);
const opportunityTitle = ref('');
async function startOpportunity() {
  const party = selectedParty.value;
  if (!party || !opportunityTitle.value.trim() || busy.value || quarantined.value) return;
  busy.value = true; error.value = '';
  try {
    const created = await parley.createOpportunity(opportunityTitle.value.trim(), party.id);
    openingOpportunity.value = false;
    opportunityTitle.value = '';
    changed();
    emit('navigate', '/queue/' + created.id);
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not create opportunity.'; }
  finally { busy.value = false; }
}
const nameDraft = ref('');
const nameBase = ref('');
const conflictCurrent = ref<EntityNotes | null>(null);
const importBatch = ref<string>();
const directoryRows = ref<GraphEntity[]>([]);
const directoryCursor = ref<string | null>(null);
const directoryLoading = ref(false);
const directoryError = ref('');
const directoryRevision = ref(0);
const remoteDirectory = parley.supports('party.relations');
let directoryRequest = 0;
async function loadDirectory(cursor?: string) {
  const own = ++directoryRequest;
  directoryLoading.value = true; directoryError.value = '';
  try {
    const page = await parley.queryDirectory({
      q: query.value.trim() || undefined, kind: kind.value === 'all' ? undefined : kind.value,
      sort: sort.value, batch_id: importBatch.value, cursor, limit: 50,
    });
    if (own !== directoryRequest) return;
    directoryRows.value = cursor ? [...new Map([...directoryRows.value, ...page.rows].map(row => [row.id, row])).values()] : page.rows;
    directoryCursor.value = page.nextCursor;
  } catch (reason) {
    if (own === directoryRequest) directoryError.value = reason instanceof Error ? reason.message : 'Could not load the party sheet.';
  } finally { if (own === directoryRequest) directoryLoading.value = false; }
}
function changed() { directoryRevision.value++; emit('changed'); }

const previousFilters = ref<{query:string; kind:typeof kind.value} | null>(null);
function viewImported(receipt: PartyImportReceipt) {
  previousFilters.value = { query: query.value, kind: kind.value };
  query.value = ''; kind.value = 'all';
  const ids = receipt.created.map(row => row.party_id);
  importBatch.value = receipt.batch_id;
  if (ids[0]) selectRow(ids[0]);
  pane.value = 'sheet';
}
function returnToResults() {
  importBatch.value = undefined;
  if (previousFilters.value) { query.value = previousFilters.value.query; kind.value = previousFilters.value.kind; }
  previousFilters.value = null;
}
const sheetRows = computed(() => remoteDirectory ? directoryRows.value : props.parties);
const pendingSwitch = ref<(() => void) | null>(null);
const quarantined = ref(false);
const dirty = computed(() => cardOpen.value && (notes.value && notesDraft.value !== notes.value.markdown || nameDraft.value !== nameBase.value));
const hasDraft = computed(() => Boolean(dirty.value || notesBusy.value || (cardOpen.value && (busy.value || frozenNotes)) || (focusEdit.value && focusDraft.value !== focusBase.value)));
let invocation: HTMLElement | null = null;
let switchInvocation: HTMLElement | null = null;
let requestVersion = 0;
let frozenNotes: { markdown: string; key: string; etag: string } | null = null;
let pendingSave: Promise<boolean> | null = null;
const notes = ref<EntityNotes | null>(null);
const notesDraft = ref('');
const notesBusy = ref(false);
const notesError = ref('');
const notesConflict = ref('');
const composing = ref(false);
let notesTimer: ReturnType<typeof globalThis.setTimeout> | undefined;
const dossier = ref<Dossier | null>(null);
const dossierLoading = ref(false);
const dossierError = ref('');
const captureText = ref('');
let captureIntent: { text: string; key: string } | null = null;
const reviewJob = ref<ExtractorJob | null>(null);
const reviewTargetId = ref('');
const reviewOpportunityId = ref('');
const inboxOpen = ref(props.inbox ?? false);
watch(() => props.inbox, open => {
  if (open) { inboxOpen.value = true; pane.value = 'details'; }
}, { immediate: true });

const selectedParty = computed(() => directoryRows.value.find(p => p.id === selectedRecordId.value) ?? props.parties.find(p => p.id === selectedRecordId.value)
  ?? graphNodes.value.find(p => p.id === selectedRecordId.value)
  ?? (dossier.value?.entity.id === selectedRecordId.value ? dossier.value.entity : null));

const coachChoices = computed(() => {
  const party = selectedParty.value;
  if (!party || !['person','org'].includes(party.kind) || !Number.isSafeInteger(party.revision)) return [];
  const choices: Array<{ selector: CoachSelector; label: string }> = [{
    selector: { kind: 'party', id: party.id, revision: party.revision }, label: `${party.name} · party research`,
  }];
  for (const opportunity of props.opportunities) {
    if (opportunity.state !== 'open' || !opportunity.participants.some(item => item.id === party.id)) continue;
    choices.push({ selector: { kind: 'opportunity', id: opportunity.id, revision: opportunity.revision, recipient_id: party.id, recipient_revision: party.revision }, label: `${opportunity.title} · ${party.name}` });
  }
  return choices;
});
const coachEvidenceChoices = computed(() => {
  const dossierRecord = dossier.value;
  if (!dossierRecord || dossierRecord.entity.id !== selectedParty.value?.id) return [];
  return dossierRecord.direct_links.flatMap(link => {
    if (link.to_id !== dossierRecord.entity.id || link.tier !== 3 || !link.claim_id || !link.claim_revision || !link.wording) return [];
    return [{ claim_id: link.claim_id, revision: link.claim_revision, label: link.wording.slice(0, 180) }];
  }).slice(0, 8);
});

async function openCoachQuestion(question: CoachQuestion) {
  const scope = question.selector;
  try {
    const path = scope.kind === 'opportunity'
      ? (await parley.getOpportunity(scope.id), `/queue/${encodeURIComponent(scope.id)}`)
      : scope.kind === 'party'
        ? (await parley.getDossier(scope.id), `/parties/${encodeURIComponent(scope.id)}`)
        : '/queue';
    emit('navigate', path);
  } catch {
    error.value = 'The question context is no longer available. Refresh the selection.';
  }
}
const rootName = computed(() => (directoryRows.value.find(p => p.id === rootPartyId.value)
  ?? graphNodes.value.find(p => p.id === rootPartyId.value)
  ?? props.parties.find(p => p.id === rootPartyId.value))?.name ?? '');

// Scoped party commands (UCR-001 U04). The palette freezes the selected
// party, its card revision and the prompted value; the server re-checks the
// revision under reservation. Unit-mount scenarios without an installed shell
// registry skip registration gracefully.
let commandRegistry: ReturnType<typeof useCommandRegistry> | null = null;
try {
  commandRegistry = useCommandRegistry();
} catch {
  commandRegistry = null;
}
let disposePartyActions: (() => void) | undefined;
watch([selectedRecordId, dossier, sheetRows], () => {
  disposePartyActions?.();
  disposePartyActions = undefined;
  if (!commandRegistry) return;
  const id = selectedRecordId.value;
  const party = selectedParty.value;
  if (!id || !party || !['person', 'org'].includes(party.kind)) return;
  const revision = Number(party.revision ?? 1);
  if (!Number.isSafeInteger(revision) || revision < 1) return;
  const fingerprint = JSON.stringify([parley.workspaceEpoch, id, revision]);
  disposePartyActions = registerScoped(commandRegistry, [
    ...partyActions({
      partyId: id,
      cardRevision: revision,
      intentFingerprint: fingerprint,
      invoke: (commandId: string, args: Record<string, unknown>, key: string) =>
        commands.invoke(commandId, { version: 1, args, dry_run: false }, key),
      onCommitted: (_receipt, commandId) => {
        status.value = commandId === 'party.rename' ? 'Party renamed.' : 'Focus updated.';
        error.value = '';
        changed();
        if (selectedRecordId.value === id) void loadDossier().then(() => { directoryRevision.value++; });
      },
    }),
    {
      id: 'ui.party.open_brief',
      title: 'Open brief',
      summary: 'Open the selected party brief research and proposals',
      keywords: ['brief', 'leads', 'research', 'proposal'],
      effect: 'read',
      when: (ctx) => ctx.context.route === 'parties' && ctx.context['selection.party'] === true,
      run: () => {
        auxiliary.value = 'chat';
      },
    },
  ]);
}, { flush: 'sync' });
async function loadDossier() {
  const id = selectedRecordId.value;
  const own = ++requestVersion;
  if (dossier.value?.entity.id !== id) dossier.value = null;
  dossierError.value = '';
  dossierLoading.value = !!id;
  if (!id) return;
  try {
    const next = await parley.getDossier(id);
    if (own === requestVersion) dossier.value = next;
  } catch (reason) {
    if (own === requestVersion) dossierError.value = reason instanceof Error ? reason.message : 'Card unavailable.';
  } finally { if (own === requestVersion) dossierLoading.value = false; }
}

function requestSwitch(action: () => void) {
  if (pendingSwitch.value) return;
  if (hasDraft.value) {
    if (notesTimer) globalThis.clearTimeout(notesTimer);
    switchInvocation = focusedElement();
    pendingSwitch.value = action;
    void nextTick(() => {
      if (pendingSwitch.value && draftDialog.value && !draftDialog.value.open) draftDialog.value.showModal();
    });
    return;
  }
  action();
}
function focusedElement() {
  const element = globalThis.document.activeElement;
  return element instanceof HTMLElement ? element : null;
}
function focusPanel(selector: string) {
  void nextTick(() => {
    const element = desk.value?.querySelector<HTMLElement>(selector);
    if (element?.getClientRects().length) element.focus();
  });
}
function selectRow(id: string, moveFocus = false) {
  requestSwitch(() => {
    closeCard();
    focusEdit.value = null;
    inspectedEdge.value = null; auxiliary.value = 'inspector';
    rootPartyId.value = id;
    selectedRecordId.value = id;
    pane.value = 'graph';
    if (moveFocus && globalThis.matchMedia?.('(max-width: 767px)').matches) focusPanel('#relationship-graph-heading');
  });
}
function selectNode(id: string) {
  if (cardOpen.value && id === selectedRecordId.value) return;
  requestSwitch(() => {
    closeCard();
    selectedRecordId.value = id;
    inspectedEdge.value = null;
    pane.value = 'graph';
    void openCard();
  });
}
function returnToRootList() {
  rootPartyId.value = null;
  selectedRecordId.value = null;
  pane.value = 'sheet';
  directoryRevision.value++;
}
function makeRoot(id: string) {
  selectRow(id);
  status.value = `Network root is now ${props.parties.find(p => p.id === id)?.name ?? id}.`;
}
function inspectEdge(edge: GraphSnapshot['edges'][number]) {
  requestSwitch(() => {
    closeCard();
    inspectedEdge.value = edge;
    correctingClaim.value = false;
    auxiliary.value = 'inspector';
    removingEdge.value = false;
    removalKey = '';
    pane.value = 'details';
    focusPanel('#party-relationship-heading');
  });
}

function startFocusEdit(id: string) {
  const party = sheetRows.value.find((item) => item.id === id);
  if (!party) return;
  requestSwitch(() => {
    focusEdit.value = id;
    focusBase.value = focusDraft.value = party.focus ?? '';
    focusPanel('.focus-input');
  });
}

async function saveFocus(id: string) {
  if (busy.value || focusEdit.value !== id) return;
  const party = sheetRows.value.find((item) => item.id === id);
  if (!party || !party.etag) { error.value = 'The party row is not editable in this session.'; return; }
  busy.value = true; error.value = ''; status.value = '';
  try {
    await parley.updateParty(id, { focus: focusDraft.value }, party.etag, newOperationKey());
    changed();
    cancelFocusEdit();
    if (selectedRecordId.value === id) {
      await loadDossier();
    }
    status.value = 'Focus saved. The sheet, graph label and inspector share this revision.';

  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'Could not save the focus.';
  } finally { busy.value = false; }
}

function resetPartyForm() {
  partyName.value = ''; partyFocus.value = ''; partyNotes.value = '';
  newPartyFiles.value = []; newPartyFilesBusy.value = false; addOpen.value = false;
  createIntent = ''; createKey = '';
}
function togglePartyForm() {
  if (addOpen.value) {
    resetPartyForm();
    addPartyButton.value?.focus();
  } else {
    addPartyButton.value = focusedElement();
    addOpen.value = true;
    void nextTick(() => partyForm.value?.querySelector('input')?.focus());
  }
}
async function saveNewParty() {
  const name = partyName.value.trim();
  if (!name) { error.value = 'A party needs a name.'; return; }
  busy.value = true; error.value = ''; status.value = '';
  try {
    const input = { kind: partyKind.value, name, focus: partyFocus.value.trim(),
      ...(partyNotes.value ? { notes: partyNotes.value } : {}),
      ...(newPartyFiles.value.length ? { staged_upload_ids: [...newPartyFiles.value] } : {}) };
    const serialized = JSON.stringify(input);
    if (serialized !== createIntent) { createIntent = serialized; createKey = newOperationKey(); }
    const party = await parley.createEntity(input, createKey);
    changed();
    resetPartyForm();
    selectRow(party.id);
    status.value = 'Party created. No opportunity, edge or outreach task was created.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save the party.'; }
  finally { busy.value = false; }
}

let selectionVersion = 0;
async function loadNotes() {
  const id = selectedRecordId.value;
  const own = ++selectionVersion;
  notes.value = null;
  notesDraft.value = ''; notesError.value = ''; notesConflict.value = ''; conflictCurrent.value = null;
  if (!id) return;
  try {
    const loaded = await parley.getEntityNotes(id);
    if (own === selectionVersion) { notes.value = loaded; notesDraft.value = loaded.markdown; }
  } catch (reason) {
    if (own === selectionVersion) notesError.value = reason instanceof Error ? reason.message : 'Could not load notes.';
  }
}
async function openCard() {
  if (cardOpen.value || !selectedRecordId.value || dossier.value?.retired_at) return;
  invocation = focusedElement();
  graph.value?.anchorTo(selectedRecordId.value);
  nameDraft.value = nameBase.value = selectedParty.value?.name ?? '';
  cardOpen.value = true;
  pane.value = 'graph';
  inspectedEdge.value = null;
  await nextTick();
  cardEditor.value?.querySelector<HTMLInputElement>('input')?.focus();
}
function scheduleNotesAutosave() {
  if (notesTimer) globalThis.clearTimeout(notesTimer);
  if (composing.value || notesConflict.value || !notes.value || !cardOpen.value || pendingSwitch.value || quarantined.value) return;
  notesTimer = globalThis.setTimeout(() => { void saveNotesNow(); }, 750);
}
async function saveNotesNow(): Promise<boolean> {
  if (pendingSave) return pendingSave;
  if (notesTimer) globalThis.clearTimeout(notesTimer);
  const target = notes.value;
  if (!target || notesConflict.value || composing.value || quarantined.value) return false;
  const markdown = notesDraft.value;
  if (markdown === target.markdown && !frozenNotes) return true;
  const intent = frozenNotes ?? { markdown, key: newOperationKey(), etag: target.etag };
  frozenNotes = intent;
  notesBusy.value = true; notesError.value = '';
  pendingSave = (async () => {
    try {
      const next = await parley.updateEntityNotes(target.entity_id, intent.markdown, target.revision, intent.etag, intent.key);
      if (selectedRecordId.value !== target.entity_id || quarantined.value) return false;
      notes.value = next;
      frozenNotes = null;
      if (notesDraft.value === intent.markdown) notesDraft.value = next.markdown;
      else scheduleNotesAutosave();
      status.value = notesDraft.value === next.markdown ? 'Notes saved.' : 'Saving newer edits…';
      return true;
    } catch (reason) {
      if (reason instanceof ApiError && reason.status === 412) {
        frozenNotes = null;
        notesConflict.value = reason.message;
        try { conflictCurrent.value = await parley.getEntityNotes(target.entity_id); } catch { notesError.value = 'Current revision unavailable. Retry comparison.'; }
      } else {
        if (reason instanceof ApiError && reason.status < 500) frozenNotes = null;
        notesError.value = reason instanceof Error ? reason.message : 'Could not save notes.';
      }
      return false;
    } finally { pendingSave = null; notesBusy.value = false; }
  })();
  return pendingSave;
}
async function saveCard() {
  if (quarantined.value) return false;
  const party = selectedParty.value;
  if (nameDraft.value !== nameBase.value && party) {
    const etag = dossier.value?.entity.etag ?? party.etag;
    if (!etag) { notesError.value = 'Reload the card before editing its name.'; return false; }
    busy.value = true;
    try {
      const updated = await parley.updateParty(party.id, { name: nameDraft.value.trim() }, etag, newOperationKey());
      nameBase.value = nameDraft.value = updated.name;
      graphRevision.value++;
      changed(); await loadDossier();
    } catch (reason) { notesError.value = reason instanceof Error ? reason.message : 'Could not save the name.'; return false; }
    finally { busy.value = false; }
  }
  if (notes.value && !await saveNotesNow()) return false;
  return !dirty.value;
}
function closeCard() {
  if (notesTimer) globalThis.clearTimeout(notesTimer);
  cardOpen.value = false; notesDraft.value = notes.value?.markdown ?? '';
  nameDraft.value = nameBase.value; frozenNotes = null; composing.value = false;
  if (notesConflict.value) void loadNotes();
}
function stayOnCard() {
  pendingSwitch.value = null;
  if (draftDialog.value?.open) draftDialog.value.close();
  if (switchInvocation && desk.value?.contains(switchInvocation)) switchInvocation.focus();
  else if (cardOpen.value) focusPanel('.card-editor input');
  else if (focusEdit.value) focusPanel('.focus-input');
  scheduleNotesAutosave();
}
function dismissCard() {
  requestSwitch(() => {
    closeCard();
    void nextTick(() => {
      if (invocation?.getClientRects().length) invocation.focus();
      else focusPanel('#relationship-graph-heading');
    });
  });
}
async function resolveSwitch(save: boolean) {
  if (pendingSave) await pendingSave;
  if (save && cardOpen.value && !await saveCard()) return;
  if (save && focusEdit.value) { await saveFocus(focusEdit.value); if (focusEdit.value) return; }
  const action = pendingSwitch.value;
  pendingSwitch.value = null;
  if (draftDialog.value?.open) draftDialog.value.close();
  closeCard(); cancelFocusEdit(); action?.();
}
function quarantine() {
  if (notesTimer) globalThis.clearTimeout(notesTimer);
  quarantined.value = true;
}
defineExpose({ requestSwitch, hasDraft });
function cancelFocusEdit() {
  const id = focusEdit.value;
  focusEdit.value = null;
  focusBase.value = focusDraft.value = '';
  if (id) void nextTick(() => {
    const row = [...(desk.value?.querySelectorAll<HTMLElement>('[data-party-id]') ?? [])].find(item => item.dataset.partyId === id);
    if (row?.getClientRects().length) row.querySelector<HTMLElement>('.focus-cell')?.focus();
  });
}

// --- Sources inbox / capture review (kept from the former Leads desk) ---

async function saveCapture() {
  if (busy.value || quarantined.value || !captureText.value.trim()) return;
  if (captureIntent?.text !== captureText.value) captureIntent = { text: captureText.value, key: newOperationKey() };
  const { text, key } = captureIntent;
  busy.value = true; error.value = ''; status.value = '';
  try {
    const source = await parley.saveSource({ text, title: null, url: null }, key);
    captureIntent = null;
    changed();
    captureText.value = '';
    inboxOpen.value = true;
    status.value = source.job_status === 'not_queued'
      ? 'Source saved independently. Extractor is unavailable or at capacity; review the source manually or retry later.'
      : 'Source saved independently. No person was inferred or linked.';
    reviewJob.value = source.job_id ? await parley.getExtractorJob(source.job_id) : null;
    if (source.job_id) void pollExtractor(source.job_id);
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save the source.'; }
  finally { busy.value = false; }
}

async function pollExtractor(id: string) {
  for (let attempt = 0; attempt < 180; attempt += 1) {
    await new Promise((resolve) => globalThis.setTimeout(resolve, 1000));
    try {
      const job = await parley.getExtractorJob(id);
      if (reviewJob.value?.id === id) reviewJob.value = job;
      if (!['queued', 'running'].includes(job.status)) return;
    } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not refresh Extractor review.'; return; }
  }
}

async function openCaptureReview(source: SavedSource) {
  try { reviewJob.value = await parley.getCaptureProposals(source.occurrence_id); }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'There is no Extractor review for this source yet.'; }
}

async function cancelExtractor(job: ExtractorJob) {
  busy.value = true; error.value = '';
  try { await parley.cancelJob(job.id); reviewJob.value = await parley.getExtractorJob(job.id); status.value = 'Extractor cancellation was recorded; any late result is discarded.'; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not cancel this Extractor job.'; }
  finally { busy.value = false; }
}

async function decideProposal(proposal: ExtractorProposal, action: 'confirm' | 'dismiss' | 'source_only' | 'unresolved') {
  if (!reviewJob.value?.proposal_batch_id) return;
  if (action === 'confirm' && !reviewTargetId.value) { error.value = 'Choose the person or organization this claim is about. Names in a source are never linked automatically.'; return; }
  busy.value = true; error.value = '';
  try {
    await parley.resolveProposals(reviewJob.value.proposal_batch_id, [{ item_id: proposal.id, action, ...(action === 'confirm' ? { target_entity_id: reviewTargetId.value } : {}) }], reviewOpportunityId.value || undefined);
    reviewJob.value = await parley.getExtractorJob(reviewJob.value.id);
    status.value = action === 'confirm' ? 'Claim accepted with its source evidence.' : action === 'unresolved' ? 'Mention saved without linking it to a person or organization.' : `Proposal ${action.replace('_', ' ')}.`;
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save the review decision.'; }
  finally { busy.value = false; }
}

const prefsKey = parley.workspaceEpoch ? 'parley.parties.' + parley.workspaceEpoch : '';
if (prefsKey) {
  try {
    const saved = JSON.parse(globalThis.sessionStorage.getItem(prefsKey) ?? '{}');
    if (typeof saved.query === 'string') query.value = saved.query;
    if (['all','person','org'].includes(saved.kind)) kind.value = saved.kind;
    if (['name','type','focus'].includes(saved.sort)) sort.value = saved.sort;
    if (!props.partyId && typeof saved.root === 'string') {
      rootPartyId.value = saved.root;
      selectedRecordId.value = typeof saved.selected === 'string' ? saved.selected : saved.root;
    }
    if (['sheet','graph','details'].includes(saved.pane)) pane.value = saved.pane;
    if (saved.auxiliary === 'chat') auxiliary.value = 'chat';
  } catch { /* Storage can be unavailable in a private browsing session. */ }
}
watch([query,kind,sort,rootPartyId,selectedRecordId,pane,auxiliary], () => {
  if (prefsKey) try { globalThis.sessionStorage.setItem(prefsKey, JSON.stringify({ query:query.value,kind:kind.value,sort:sort.value,root:rootPartyId.value,selected:selectedRecordId.value,pane:pane.value,auxiliary:auxiliary.value })); } catch { /* Keep in-memory navigation. */ }
});
watch([query, kind, sort, importBatch, directoryRevision, () => props.active], (_, previous, cleanup) => {
  if (!remoteDirectory || props.active === false) return;
  directoryRequest++;
  directoryCursor.value = null;
  const timer = globalThis.setTimeout(() => { void loadDirectory(); }, previous.length ? 120 : 0);
  cleanup(() => { globalThis.clearTimeout(timer); directoryRequest++; });
}, { immediate: true });
watch(() => props.partyId, id => {
  if (id && globalThis.sessionStorage.getItem('parley.open-brief') === id) {
    auxiliary.value = 'chat';
    globalThis.sessionStorage.removeItem('parley.open-brief');
  }
  if (id && id !== rootPartyId.value) selectRow(id);
});
watch(selectedRecordId, () => { openingOpportunity.value = false; retirement.value = null; void loadDossier(); void loadNotes(); }, { immediate: true });
let eventCursor = parley.eventCursor;
let eventTimer: ReturnType<typeof globalThis.setInterval> | undefined;
let polling = false;
async function pollChanges() {
  if (!remoteDirectory || props.active === false || globalThis.document.visibilityState === 'hidden' || polling || quarantined.value) return;
  polling = true;
  try {
    const result = await parley.getEvents(eventCursor);
    eventCursor = result.next_cursor;
    const types = new Set(result.items.map(item => item.entity_type));
    if (['entity', 'edge', 'claim', 'opportunity', 'resource', 'attachment'].some(type => types.has(type))) {
      directoryRevision.value++;
      graphRevision.value++;
      void loadDossier();
    }
    if (!cardOpen.value && result.items.some(item => item.entity_id === selectedRecordId.value && item.event_type === 'entity.notes_updated')) void loadNotes();
  } catch (reason) {
    if (reason instanceof ApiError && reason.status === 410) {
      eventCursor = '0';
      directoryRevision.value++;
      graphRevision.value++;
      void loadDossier();
    }
  } finally { polling = false; }
}
onMounted(() => { if (remoteDirectory) eventTimer = globalThis.setInterval(pollChanges, 2000); });
globalThis.addEventListener('parley:workspace-quarantined', quarantine);
globalThis.addEventListener('parley:workspace-changed', quarantine);
onBeforeUnmount(() => {
  if (notesTimer) globalThis.clearTimeout(notesTimer);
  if (eventTimer) globalThis.clearInterval(eventTimer);
  selectionVersion++; requestVersion++;
  globalThis.removeEventListener('parley:workspace-quarantined', quarantine);
  globalThis.removeEventListener('parley:workspace-changed', quarantine);
});
</script>

<template>
  <section
    ref="desk"
    class="party-desk"
    :data-pane="pane"
    aria-labelledby="parties-heading"
  >
    <h2
      id="parties-heading"
      class="sr-only"
    >
      Party sheet, neighborhood and inspector
    </h2>
    <nav
      class="pane-switch"
      aria-label="Party workspace panels"
    >
      <button
        type="button"
        :aria-pressed="pane === 'sheet'"
        @click="pane = 'sheet'"
      >
        Sheet
      </button>
      <button
        type="button"
        :aria-pressed="pane === 'graph'"
        @click="pane = 'graph'"
      >
        Graph / relationships
      </button>
      <button
        type="button"
        :aria-pressed="pane === 'details'"
        @click="pane = 'details'"
      >
        Details
      </button>
    </nav>
    <dialog
      ref="draftDialog"
      class="draft-prompt"
      role="alertdialog"
      aria-label="Unsaved changes"
      @cancel.prevent="stayOnCard"
    >
      <strong>Unsaved changes</strong>
      <button
        :disabled="busy || notesBusy || quarantined"
        @click="resolveSwitch(true)"
      >
        Save
      </button>
      <button
        :disabled="busy || notesBusy"
        @click="resolveSwitch(false)"
      >
        Discard unsaved changes
      </button>
      <button
        autofocus
        @click="stayOnCard"
      >
        Stay
      </button>
    </dialog>
    <aside
      class="sheet-column"
      aria-label="Party sheet"
    >
      <PartySheet
        :parties="sheetRows"
        :server-paged="remoteDirectory"
        :loading="directoryLoading"
        :root-known="!!rootName"
        :more="!!directoryCursor"
        :root-party-id="rootPartyId"
        :selected-record-id="selectedRecordId"
        :query="query"
        :kind="kind"
        :sort="sort"
        :focus-edit="focusEdit"
        :focus-draft="focusDraft"
        :busy="busy"
        :add-open="addOpen"
        @more="loadDirectory(directoryCursor ?? undefined)"
        @update:query="query = $event"
        @update:kind="kind = $event"
        @update:sort="sort = $event"
        @update:focus-draft="focusDraft = $event"
        @select-row="selectRow($event, true)"
        @edit-focus="startFocusEdit"
        @save-focus="saveFocus"
        @cancel-focus="cancelFocusEdit"
        @first-visible="id => { if (!rootPartyId) { rootPartyId = id; selectedRecordId = id; } }"
        @add-party="togglePartyForm"
      >
        <template #import>
          <PartyImport
            @imported="changed"
            @view-imported="viewImported"
          />
        </template>
      </PartySheet>
      <button
        v-if="importBatch"
        @click="returnToResults"
      >
        Back to all parties
      </button>
      <p
        v-if="directoryError"
        role="alert"
      >
        {{ directoryError }} <button @click="loadDirectory()">
          Retry
        </button>
      </p>
      <form
        v-if="addOpen"
        ref="partyForm"
        class="add-party"
        @keydown.esc.stop="togglePartyForm"
        @submit.prevent="saveNewParty"
      >
        <h3>Add a party</h3>
        <label>Type<select v-model="partyKind"><option value="person">Person</option><option value="org">Organization</option></select></label>
        <label>Name<input
          v-model="partyName"
          required
          autocomplete="off"
        ></label>
        <label>Focus (optional)<input
          v-model="partyFocus"
          maxlength="250"
          autocomplete="off"
        ></label>
        <div v-if="duplicateParties.length">
          <p>Same-name parties</p>
          <ul>
            <li
              v-for="match in duplicateParties"
              :key="match.id"
            >
              <button
                type="button"
                @click="resetPartyForm(); selectRow(match.id)"
              >
                {{ match.name }} · {{ match.focus || match.kind }} · {{ match.id.slice(-8) }}
              </button>
            </li>
          </ul>
        </div>
        <div>
          <h4>Owner notes · Markdown</h4>
          <MarkdownEditor
            v-model="partyNotes"
            label="Owner notes"
            :readonly="busy || quarantined"
            compact
          />
          <details v-if="partyNotes">
            <summary>Preview Markdown</summary>
            <MarkdownPreview :source="partyNotes" />
          </details>
        </div>
        <PartyFiles
          @staged="newPartyFiles = $event"
          @busy="newPartyFilesBusy = $event"
        />
        <button
          type="submit"
          :disabled="busy || newPartyFilesBusy || quarantined"
        >
          {{ duplicateParties.length ? 'Create distinct party' : 'Add party' }}
        </button>

        <button
          type="button"
          :disabled="busy"
          @click="togglePartyForm"
        >
          Cancel
        </button>
      </form>
    </aside>
    <div class="graph-column">
      <RelationshipGraph
        ref="graph"
        :sources="props.sources"
        :active="props.active"
        :root-name="rootName"
        :refresh="graphRevision"
        :editor-open="cardOpen"
        :can-open="!dossier?.retired_at"
        :focus-entity-id="selectedRecordId || rootPartyId || undefined"
        :view-root-id="rootPartyId || undefined"
        @nodes-loaded="graphNodes = $event"
        @inspect-edge="inspectEdge"
        @open-card="openCard"
        @select-node="selectNode"
        @related-saved="changed"
      >
        <template #editor>
          <section
            v-if="cardOpen && selectedParty"
            ref="cardEditor"
            class="card-editor"
            aria-label="Card editor"
            @keydown.esc.stop="dismissCard"
          >
            <header class="pane-bar">
              <h3>Edit card · {{ selectedParty.name }}</h3>
              <span
                v-if="notesBusy"
                role="status"
              >Saving…</span>
            </header>
            <p
              v-if="notesConflict"
              role="alert"
              class="conflict"
            >
              {{ notesConflict }} Keep your input; reload the card to compare, then review before reapplying.
            </p>
            <p
              v-if="notesError"
              role="alert"
            >
              {{ notesError }}
            </p>
            <div class="card-fields">
              <label>Name<input
                v-model="nameDraft"
                required
                maxlength="240"
                :readonly="quarantined || busy"
              ></label>
              <p
                v-if="quarantined"
                role="alert"
              >
                Workspace changed. Copy your draft or discard it before reloading.
              </p>
              <details v-if="notesConflict">
                <summary>Compare revisions</summary>
                <p>Original</p><pre>{{ notes?.markdown }}</pre>
                <p>Current saved notes</p><pre>{{ conflictCurrent?.markdown }}</pre>
                <button
                  v-if="conflictCurrent"
                  @click="notes = conflictCurrent; notesConflict = ''; conflictCurrent = null"
                >
                  Use current revision as base for my draft
                </button>
                <button
                  v-else
                  @click="parley.getEntityNotes(selectedParty.id).then(result => conflictCurrent = result).catch(reason => notesError = reason.message)"
                >
                  Retry comparison
                </button>
              </details>
            </div>
            <div class="notes-edit">
              <h4>Owner notes · Markdown</h4>
              <MarkdownEditor
                v-if="notes"
                v-model="notesDraft"
                label="Owner notes"
                :readonly="quarantined"
                compact
                @composition="composing = $event; scheduleNotesAutosave()"
                @update:model-value="scheduleNotesAutosave"
              />
              <details>
                <summary>Preview Markdown</summary>
                <div class="notes-preview">
                  <MarkdownPreview :source="notesDraft" />
                </div>
              </details>
            </div>
            <PartyFiles
              :key="selectedParty.id"
              :entity-id="selectedParty.id"
              :files="dossier?.files ?? []"
              @changed="loadDossier"
            />
            <footer class="card-actions">
              <button
                type="button"
                class="primary"
                :disabled="!dirty || busy || notesBusy || !!notesConflict || quarantined"
                @click="saveCard"
              >
                {{ nameBase === nameDraft ? 'Save notes' : 'Save card' }}
              </button>
              <button
                type="button"
                :disabled="notesBusy"
                @click="dismissCard"
              >
                Cancel
              </button>
              <button
                :disabled="busy || notesBusy || quarantined"
                @click="async () => { if (await saveCard()) { closeCard(); graph?.openRelated(); } }"
              >
                Save &amp; add related
              </button>
            </footer>
          </section>
        </template>
      </RelationshipGraph>
      <aside
        class="inspector-column"
        aria-label="Inspector and chat"
      >
        <div
          class="auxiliary-tabs"
          aria-label="Party details and research"
        >
          <button
            :aria-pressed="auxiliary === 'inspector'"
            @click="auxiliary = 'inspector'"
          >
            Details
          </button>
          <button
            :aria-pressed="auxiliary === 'chat'"
            @click="auxiliary = 'chat'"
          >
            Research chat
          </button>
        </div>
        <div v-show="auxiliary === 'inspector'">
          <section
            v-if="inspectedEdge"
            class="edge-detail"
          >
            <h3
              id="party-relationship-heading"
              tabindex="-1"
            >
              {{ inspectedEdge.verb.replaceAll('_', ' ') }}
            </h3>
            <p>{{ graphNodes.find(n => n.id === inspectedEdge?.from_id)?.name }} → {{ graphNodes.find(n => n.id === inspectedEdge?.to_id)?.name }}</p>
            <blockquote>{{ inspectedEdge.quote }}</blockquote>
            <small>{{ inspectedEdge.source_title }} · {{ inspectedEdge.speaker_kind }} · T{{ inspectedEdge.tier }}</small>
            <ClaimCorrection
              v-if="correctingClaim"
              :edge="inspectedEdge"
              :parties="props.parties"
              @cancel="correctingClaim = false"
              @saved="claimSaved"
            />
            <div v-else-if="parley.supports('party.relations')">
              <button
                v-if="inspectedEdge.claim_id"
                @click="correctingClaim = true"
              >
                Correct attribution
              </button>
              <button @click="correctEdge">
                Correct relationship
              </button>
              <button
                v-if="!removingEdge"
                @click="removingEdge = true"
              >
                Remove link
              </button>
              <template v-else>
                <p>Remove this relationship from both cards?</p><button
                  :disabled="busy"
                  @click="removeEdge"
                >
                  Confirm removal
                </button><button @click="removingEdge = false">
                  Cancel
                </button>
              </template>
            </div>
          </section>
          <PartyInspector
            v-else
            :markdown="notes?.markdown"
            :nodes="graphNodes"
            :party="selectedParty"
            :dossier="dossier"
            :loading="dossierLoading && !dossier"
            :root-party-id="rootPartyId"
            :notes-error="dossierError"
            @select-node="selectNode"
            @inspect-link="graph?.inspectEdge($event)"
            @navigate="emit('navigate', $event)"
            @open-card="openCard"
            @start-opportunity="requestSwitch(() => { openingOpportunity = true; })"
            @review-retirement="requestSwitch(() => { void reviewRetirement(); })"
            @return-to-results="returnToRootList"
            @make-root="makeRoot"
          />
          <CoachQuestions
            v-if="coachChoices.length"
            :choices="coachChoices"
            :evidence-choices="coachEvidenceChoices"
            :active="props.active !== false"
            @open="openCoachQuestion"
          />
          <section
            v-if="retirement"
            aria-label="Retirement impact"
          >
            <h3>Used by</h3>
            <p>{{ retirement.counts.links }} relationships · {{ retirement.counts.opportunities }} opportunities · {{ retirement.counts.materials }} materials · {{ retirement.counts.recommendations }} recommendations</p>
            <ul>
              <li
                v-for="link in retirement.links"
                :key="link.id"
              >
                {{ link.verb.replaceAll('_', ' ') }}
              </li>
              <li
                v-for="opportunity in retirement.opportunities"
                :key="opportunity.id"
              >
                <button @click="emit('navigate', '/queue/' + opportunity.id)">
                  {{ opportunity.title }}
                </button>
              </li>
              <li
                v-for="material in retirement.materials"
                :key="material.id"
              >
                <button @click="emit('navigate', '/output/' + material.id)">
                  {{ material.kind }}
                </button>
              </li>
            </ul>
            <button
              :disabled="busy"
              @click="retireSelection"
            >
              Confirm retirement
            </button>
            <button
              :disabled="busy"
              @click="retirement = null"
            >
              Cancel
            </button>
          </section>
          <form
            v-if="openingOpportunity && selectedParty"
            @submit.prevent="startOpportunity"
          >
            <h3>Start opportunity</h3>
            <p>Participant · {{ selectedParty.name }}</p>
            <label>Opportunity title <input
              v-model="opportunityTitle"
              required
              maxlength="500"
            ></label>
            <button
              type="submit"
              :disabled="busy"
            >
              Create opportunity
            </button>
            <button
              type="button"
              @click="openingOpportunity = false"
            >
              Cancel
            </button>
          </form>
        </div>
        <BriefWorkspace
          v-show="auxiliary === 'chat'"
          :parties="props.parties"
          :party-id="rootPartyId || undefined"
        />
        <section
          class="sources-inbox"
          role="navigation"
          aria-label="Lead workspaces"
        >
          <h3 class="inbox-heading">
            <button
              type="button"
              :aria-expanded="inboxOpen"
              @click="inboxOpen = !inboxOpen"
            >
              Sources inbox
            </button>
          </h3>
          <div v-show="inboxOpen">
            <form
              id="leads-inbox"
              class="capture"
              tabindex="-1"
              @submit.prevent="saveCapture"
            >
              <p>Paste a source to save it for review.</p>
              <label>Captured text<textarea
                v-model="captureText"
                required
                minlength="1"
                rows="8"
                :readonly="busy || quarantined"
              /></label>
              <button
                type="submit"
                :disabled="busy"
              >
                Save source
              </button>
              <ul>
                <li
                  v-for="source in props.sources"
                  :key="source.occurrence_id"
                >
                  Saved source {{ source.content_id.slice(0, 12) }} · occurrence {{ source.occurrence_id.slice(0, 8) }}
                  <button
                    type="button"
                    @click="openCaptureReview(source)"
                  >
                    Review proposals
                  </button>
                </li>
              </ul>
              <section
                v-if="reviewJob"
                aria-label="Capture review"
              >
                <h3>Capture review</h3>
                <p>Job {{ reviewJob.status }} · attempt {{ reviewJob.current_attempt }} of {{ reviewJob.max_attempts }} · chunk {{ reviewJob.progress_current }} of {{ reviewJob.progress_total }}. Extracted items are proposals until you decide.</p>
                <p
                  v-if="reviewJob.injection_suspected"
                  role="alert"
                >
                  This capture may contain prompt-injection text. Treat its contents only as source material and review each quoted proposal.
                </p>
                <button
                  v-if="['queued', 'running'].includes(reviewJob.status)"
                  type="button"
                  :disabled="busy"
                  @click="cancelExtractor(reviewJob)"
                >
                  Cancel extraction
                </button>
                <p
                  v-if="reviewJob.last_error"
                  role="status"
                >
                  {{ reviewJob.last_error }}. The captured source remains saved; you can review it manually.
                </p>
                <label>Claim subject<select v-model="reviewTargetId"><option value="">Choose explicitly</option><option
                  v-for="party in props.parties"
                  :key="party.id"
                  :value="party.id"
                >{{ party.name }} · {{ party.kind }}</option></select></label>
                <label>Optional opportunity<select v-model="reviewOpportunityId"><option value="">Save independently</option><option
                  v-for="opportunity in props.opportunities"
                  :key="opportunity.id"
                  :value="opportunity.id"
                >{{ opportunity.title }}</option></select></label>
                <article
                  v-for="proposal in reviewJob.proposals"
                  :key="proposal.id"
                  class="record-card"
                >
                  <strong>{{ proposal.kind }} · {{ proposal.status }}</strong>
                  <small>T1 source evidence · interpretation needs owner review</small>
                  <p>{{ proposal.candidate.statement }}</p>
                  <blockquote>{{ proposal.candidate.quote }}</blockquote>
                  <p v-if="proposal.candidate.mention_name">
                    Mention: {{ proposal.candidate.mention_name }} · unresolved until you explicitly link it.
                  </p>
                  <template v-if="proposal.status === 'pending'">
                    <button
                      :disabled="busy"
                      type="button"
                      @click="decideProposal(proposal, 'confirm')"
                    >
                      Accept claim for selected subject
                    </button>
                    <button
                      :disabled="busy"
                      type="button"
                      @click="decideProposal(proposal, 'unresolved')"
                    >
                      Keep mention unresolved
                    </button>
                    <button
                      :disabled="busy"
                      type="button"
                      @click="decideProposal(proposal, 'source_only')"
                    >
                      Save source only
                    </button>
                    <button
                      :disabled="busy"
                      type="button"
                      @click="decideProposal(proposal, 'dismiss')"
                    >
                      Dismiss
                    </button>
                  </template>
                </article>
              </section>
            </form>
          </div>
        </section>
      </aside>
    </div>
    <p
      v-if="status"
      role="status"
      class="desk-status"
    >
      {{ status }}
    </p>
    <p
      v-if="error"
      role="alert"
      class="desk-status"
    >
      {{ error }}
    </p>
  </section>
</template>

<style scoped>
.party-desk { display: grid; grid-template-columns: minmax(18rem, 1fr) minmax(0, 1.8fr); gap: 1rem; align-items: start; }
.sheet-column, .graph-column, .inspector-column { min-width: 0; }
.sheet-column { display: grid; gap: .75rem; }
.graph-column, .sheet-column :deep(.party-sheet), .add-party {
  background: #fffef9;
  border: 1px solid #bcc8b9;
  border-radius: .55rem;
}
.sheet-column :deep(.party-sheet), .graph-column :deep(.network-pane), .inspector-column :deep(.party-inspector) { padding: 0; }
.graph-column :deep(.network-pane), .inspector-column :deep(.party-inspector), .edge-detail, .sources-inbox { border: 0; border-radius: 0; background: transparent; }
.graph-column :deep(.network-pane) { border-radius: .55rem .55rem 0 0; }
.inspector-column { border-top: 1px solid #d4dcd4; }
.add-party h3 { margin-top: 0; }
.card-editor { background: #fffef9; border: 0; padding: 0; }
.card-editor .pane-bar { padding: .65rem .9rem; }
.card-fields, .notes-edit { padding: 0 .9rem; }
.card-actions { display: flex; flex-wrap: wrap; gap: .5rem; padding: .7rem .9rem; border-top: 1px solid #dce2db; }
.notes-edit details { margin: .6rem 0; }
.notes-preview { border: 1px solid #e0e5df; border-radius: .35rem; padding: 0 .6rem; max-height: 14rem; overflow: auto; }
.conflict { background: #fdf1ef; border: 1px solid #e2b7b0; border-radius: .35rem; margin: .6rem .9rem; padding: .5rem .6rem; }
.edge-detail, .sources-inbox { padding: .8rem 1rem; overflow-wrap: anywhere; }
.sources-inbox { border-top: 1px solid #d4dcd4; }
.sources-inbox .capture { margin-top: .65rem; }
.inbox-heading { margin: 0; }
.desk-status { grid-column: 1 / -1; margin: 0; }
.pane-switch { display: none; }
.auxiliary-tabs { display: flex; flex-wrap: wrap; gap: .4rem; padding: .55rem 1rem; border-bottom: 1px solid #dce2db; }
.draft-prompt {
  width: min(29rem, calc(100% - 2rem));
  padding: 1rem;
  color: inherit;
  background: #fffdf7;
  border: 1px solid #bcc8b9;
  border-radius: .55rem;
}
.draft-prompt[open] { display: flex; flex-wrap: wrap; gap: .55rem; }
.draft-prompt strong { flex-basis: 100%; }
.draft-prompt::backdrop { background: #152a2175; }
@media (max-width: 767px) {
  .party-desk { grid-template-columns: minmax(0, 1fr); }
  .pane-switch { display: flex; flex-wrap: wrap; gap: .4rem; }
  .pane-switch button { flex: 1; min-height: 2.75rem; }
  .party-desk:not([data-pane="sheet"]) .sheet-column,
  .party-desk[data-pane="sheet"] .graph-column,
  .party-desk[data-pane="graph"] .inspector-column,
  .party-desk[data-pane="details"] :deep(.network-pane) { display: none; }
  .inspector-column { border: 0; }
}
</style>
