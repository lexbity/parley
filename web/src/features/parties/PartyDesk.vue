<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import {
  newOperationKey, parley, ApiError,
  type Dossier, type EntityNotes, type ExtractorJob, type ExtractorProposal,
  type GraphEntity, type Opportunity, type SavedSource,
} from '../../api/client';
import PartySheet from './PartySheet.vue';
import PartyInspector from './PartyInspector.vue';
import RelationshipGraph from '../leads/RelationshipGraph.vue';
import BriefWorkspace from '../leads/BriefWorkspace.vue';
import MarkdownEditor from '../../shared/MarkdownEditor.vue';
import MarkdownPreview from '../../shared/MarkdownPreview.vue';

const props = defineProps<{
  parties: GraphEntity[];
  sources: SavedSource[];
  opportunities: Opportunity[];
  partyId?: string;
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
const focusEdit = ref<string | null>(null);
const focusDraft = ref('');
const busy = ref(false);
const status = ref('');
const error = ref('');

const cardOpen = ref(false);
const notes = ref<EntityNotes | null>(null);
const notesDraft = ref('');
const notesBusy = ref(false);
const notesError = ref('');
const notesConflict = ref('');
const composing = ref(false);
let notesTimer: ReturnType<typeof globalThis.setTimeout> | undefined;
let notesGeneration = 0;
let cardOpenFor = '';

const dossier = ref<Dossier | null>(null);
const dossierLoading = ref(false);
const dossierError = ref('');

const captureText = ref('');
const reviewJob = ref<ExtractorJob | null>(null);
const reviewTargetId = ref('');
const reviewOpportunityId = ref('');
const inboxOpen = ref(false);

const selectedParty = computed(() => props.parties.find((party) => party.id === selectedRecordId.value) ?? null);

async function loadDossier() {
  const id = selectedRecordId.value;
  if (!id) { dossier.value = null; return; }
  const own = cardOpenFor + ':' + id;
  dossierLoading.value = true; dossierError.value = '';
  try {
    const next = await parley.getDossier(id);
    if (own === cardOpenFor + ':' + selectedRecordId.value) dossier.value = next;
  } catch (reason) {
    if (own === cardOpenFor + ':' + selectedRecordId.value) {
      dossierError.value = reason instanceof Error ? reason.message : 'Could not load the inspector dossier.';
    }
  } finally {
    if (own === cardOpenFor + ':' + selectedRecordId.value) dossierLoading.value = false;
  }
}

function selectRow(id: string) {
  rootPartyId.value = id;
  selectedRecordId.value = id;
  focusEdit.value = null;
  closeCard();
}

function selectNode(id: string) {
  if (selectedRecordId.value !== id) closeCard();
  selectedRecordId.value = id;
}

function makeRoot(id: string) {
  rootPartyId.value = id;
  status.value = `Network root is now ${props.parties.find((party) => party.id === id)?.name ?? id}.`;
}

function startFocusEdit(id: string) {
  const party = props.parties.find((item) => item.id === id);
  if (!party) return;
  focusEdit.value = id;
  focusDraft.value = party.focus ?? '';
}

async function saveFocus(id: string) {
  const party = props.parties.find((item) => item.id === id);
  if (!party || !party.etag) { error.value = 'The party row is not editable in this session.'; return; }
  busy.value = true; error.value = ''; status.value = '';
  try {
    const updated = await parley.updateParty(id, { focus: focusDraft.value }, party.etag, newOperationKey());
    emit('changed');
    focusEdit.value = null;
    if (selectedRecordId.value === id) {
      const next = await parley.getDossier(id);
      dossier.value = next;
    }
    status.value = 'Focus saved. The sheet, graph label and inspector share this revision.';
    void updated;
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'Could not save the focus.';
  } finally { busy.value = false; }
}

async function saveNewParty() {
  const name = partyName.value.trim();
  if (!name) { error.value = 'A party needs a name.'; return; }
  busy.value = true; error.value = ''; status.value = '';
  try {
    const party = await parley.createEntity({ kind: partyKind.value, name, focus: partyFocus.value.trim() }, newOperationKey());
    emit('changed');
    partyName.value = ''; partyFocus.value = ''; addOpen.value = false;
    selectRow(party.id);
    status.value = 'Party created. No opportunity, edge or outreach task was created.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save the party.'; }
  finally { busy.value = false; }
}

async function openCard() {
  const party = selectedParty.value;
  if (!party) return;
  cardOpenFor = party.id;
  cardOpen.value = true;
  notesConflict.value = ''; notesError.value = '';
  notes.value = null; notesDraft.value = '';
  loadDossier();
  try {
    const loaded = await parley.getEntityNotes(party.id);
    if (cardOpenFor === party.id && cardOpen.value) {
      notes.value = loaded;
      notesDraft.value = loaded.markdown;
    }
  } catch (reason) {
    if (cardOpenFor === party.id && cardOpen.value) notesError.value = reason instanceof Error ? reason.message : 'Could not load the card notes.';
  }
}

function scheduleNotesAutosave() {
  if (notesTimer) globalThis.clearTimeout(notesTimer);
  if (composing.value || notesConflict.value || !notes.value || !cardOpen.value) return;
  const own = ++notesGeneration;
  notesTimer = globalThis.setTimeout(() => { void autosaveNotes(own); }, 750);
}

async function autosaveNotes(own: number) {
  if (own !== notesGeneration || !notes.value || composing.value || notesConflict.value) return;
  const target = notes.value;
  if (notesDraft.value === target.markdown) return;
  notesBusy.value = true;
  try {
    const next = await parley.updateEntityNotes(target.entity_id, notesDraft.value, target.revision, target.etag, newOperationKey());
    if (own !== notesGeneration) return;
    notes.value = next;
    status.value = 'Notes saved.';
  } catch (reason) {
    if (own !== notesGeneration) return;
    if (reason instanceof ApiError && reason.status === 412) {
      notesConflict.value = reason.message;
    } else {
      notesError.value = reason instanceof Error ? reason.message : 'Could not save notes.';
    }
  } finally {
    if (own === notesGeneration) notesBusy.value = false;
  }
}

async function saveNotesNow() {
  if (!notes.value || notesConflict.value) return;
  const target = notes.value;
  notesGeneration += 1;
  notesBusy.value = true; notesError.value = '';
  try {
    const next = await parley.updateEntityNotes(target.entity_id, notesDraft.value, target.revision, target.etag, newOperationKey());
    notes.value = next;
    status.value = 'Notes saved.';
  } catch (reason) {
    if (reason instanceof ApiError && reason.status === 412) {
      notesConflict.value = reason.message;
    } else {
      notesError.value = reason instanceof Error ? reason.message : 'Could not save notes.';
    }
  } finally { notesBusy.value = false; }
}

function closeCard() {
  if (notesTimer) globalThis.clearTimeout(notesTimer);
  notesGeneration += 1;
  cardOpen.value = false;
  cardOpenFor = '';
  notes.value = null; notesDraft.value = ''; notesConflict.value = ''; notesError.value = ''; composing.value = false;
}

function cancelFocusEdit() {
  focusEdit.value = null;
  focusDraft.value = '';
}

// --- Sources inbox / capture review (kept from the former Leads desk) ---

async function saveCapture() {
  busy.value = true; error.value = ''; status.value = '';
  try {
    const source = await parley.saveSource({ text: captureText.value, title: null, url: null });
    emit('changed');
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
  try { await parley.cancelExtractorJob(job.id); reviewJob.value = await parley.getExtractorJob(job.id); status.value = 'Extractor cancellation was recorded; any late result is discarded.'; }
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

watch(() => props.partyId, (id) => {
  if (id) { rootPartyId.value = id; selectedRecordId.value = id; }
  void loadDossier();
}, { immediate: true });

watch(selectedRecordId, () => { void loadDossier(); });

onMounted(() => { void loadDossier(); });
</script>

<template>
  <section
    class="party-desk"
    aria-labelledby="parties-heading"
  >
    <h2
      id="parties-heading"
      class="sr-only"
    >
      Party sheet, neighborhood and inspector
    </h2>
    <aside
      class="sheet-column"
      aria-label="Party sheet"
    >
      <PartySheet
        :parties="props.parties"
        :root-party-id="rootPartyId"
        :selected-record-id="selectedRecordId"
        :query="query"
        :kind="kind"
        :sort="sort"
        :focus-edit="focusEdit"
        :focus-draft="focusDraft"
        :busy="busy"
        @update:query="query = $event"
        @update:kind="kind = $event"
        @update:sort="sort = $event"
        @update:focus-draft="focusDraft = $event"
        @select-row="selectRow"
        @edit-focus="startFocusEdit"
        @save-focus="saveFocus"
        @cancel-focus="cancelFocusEdit"
        @add-party="addOpen = !addOpen"
      />
      <form
        v-show="addOpen"
        class="add-party"
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
        <button
          type="submit"
          :disabled="busy"
        >
          Add party
        </button>
        <p>No opportunity, edge or outreach task is created by saving a party.</p>
      </form>
    </aside>
    <div class="graph-column">
      <RelationshipGraph
        :focus-entity-id="selectedRecordId || rootPartyId || undefined"
        :view-root-id="rootPartyId || undefined"
        @select-node="selectNode"
        @related-saved="emit('changed')"
      />
      <section
        v-if="cardOpen && selectedParty"
        class="card-editor"
        aria-label="Card editor"
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
          <label>Focus<input
            :value="selectedParty.focus"
            readonly
          ></label>
        </div>
        <div class="notes-edit">
          <h4>Owner notes · Markdown</h4>
          <MarkdownEditor
            v-model="notesDraft"
            label="Owner notes"
            :readonly="!!notesConflict"
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
        <footer class="card-actions">
          <button
            type="button"
            class="primary"
            :disabled="notesBusy || !!notesConflict || notesDraft === (notes?.markdown ?? '')"
            @click="saveNotesNow"
          >
            Save notes
          </button>
          <button
            type="button"
            :disabled="notesBusy"
            @click="closeCard"
          >
            Cancel
          </button>
        </footer>
      </section>
    </div>
    <aside
      class="inspector-column"
      aria-label="Inspector and chat"
    >
      <PartyInspector
        :party="selectedParty"
        :dossier="dossier"
        :loading="dossierLoading"
        :root-party-id="rootPartyId"
        :notes-error="dossierError"
        @open-card="openCard"
        @make-root="makeRoot"
      />
      <BriefWorkspace
        :parties="props.parties"
        :party-id="selectedRecordId || undefined"
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
            @submit.prevent="saveCapture"
          >
            <h3>Sources inbox</h3>
            <p>Paste captured text. Saving a source does not decide who it is about or create outreach.</p>
            <label>Captured text<textarea
              v-model="captureText"
              required
              minlength="1"
              rows="8"
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
.party-desk { display: grid; grid-template-columns: minmax(16rem, 1fr) minmax(0, 2.2fr) minmax(16rem, 1fr); gap: .85rem; align-items: start; }
.sheet-column, .graph-column, .inspector-column { display: flex; flex-direction: column; gap: .85rem; min-width: 0; }
.add-party { background: white; border: 1px solid #ccd5cc; border-radius: .55rem; padding: .8rem; }
.add-party h3 { margin-top: 0; }
.card-editor { background: white; border: 1px solid #ccd5cc; border-radius: .55rem; padding: 0; }
.card-editor .pane-bar { padding: .6rem .9rem; }
.card-fields, .notes-edit { padding: 0 .9rem; }
.card-actions { display: flex; gap: .5rem; padding: .7rem .9rem; border-top: 1px solid #dce2db; }
.notes-edit details { margin: .6rem 0; }
.notes-preview { border: 1px solid #e0e5df; border-radius: .35rem; padding: 0 .6rem; max-height: 14rem; overflow: auto; }
.conflict { background: #fdf1ef; border: 1px solid #e2b7b0; border-radius: .35rem; margin: .6rem .9rem; padding: .5rem .6rem; }
.sources-inbox { background: white; border: 1px solid #ccd5cc; border-radius: .55rem; padding: .6rem .9rem; }
.sources-inbox summary { cursor: pointer; font-weight: 600; }
.desk-status { grid-column: 1 / -1; margin: 0; }
.sr-only { position: absolute; clip-path: inset(50%); height: 1px; width: 1px; overflow: hidden; white-space: nowrap; }
@media (max-width: 68rem) { .party-desk { grid-template-columns: minmax(14rem, 1fr) minmax(0, 1.8fr); } .inspector-column { grid-column: 1 / -1; } }
@media (max-width: 54rem) { .party-desk { grid-template-columns: 1fr; } .inspector-column { grid-column: auto; } }
</style>