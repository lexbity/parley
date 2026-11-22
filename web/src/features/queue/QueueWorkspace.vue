<script setup lang="ts">
/* global BeforeUnloadEvent */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { newOperationKey, parley, type BoardList, type BoardPlacement, type BoardSnapshot, type ContactThread, type GraphEntity, type Material, type OpportunityNotes, type ReleasedMaterial } from '../../api/client';
import MarkdownEditor from '../../shared/MarkdownEditor.vue';
import MarkdownPreview from '../../shared/MarkdownPreview.vue';
import MaterialActions from '../output/MaterialActions.vue';
import AttachmentPanel from '../output/AttachmentPanel.vue';
import ContactThreadPanel from './ContactThreadPanel.vue';
import Daily from './Daily.vue';
import Insights from './Insights.vue';

const props = withDefaults(defineProps<{ parties: GraphEntity[]; outputs: Material[]; routeId?: string | null; active?: boolean }>(), { routeId: null, active: true });
const emit = defineEmits<{ 'material-updated': [material: Material]; navigate: [path: string] }>();
const board = ref<BoardSnapshot>({ lists: [], cards: [] });
const selectedId = ref('');
const activeQueueView = ref<'Board' | 'Opportunities' | 'Daily' | 'Insights'>('Board');
const detail = ref<OpportunityNotes | null>(null);
const markdown = ref('');
const dirty = ref(false);
const workspaceQuarantined = ref(false);
const quarantinedMarkdown = ref('');
const quarantineStatus = ref('');
const title = ref('');
const partyId = ref('');
const newListTitle = ref('');
const renameValues = ref<Record<string, string>>({});
const moveTargets = ref<Record<string, string>>({});
const archiveTargets = ref<Record<string, string>>({});
const busy = ref(false);
const error = ref('');
const status = ref('');
const composing = ref(false);
const loadingNotes = ref(false);
const noteSequence = ref(0);
let openGeneration = 0;
const tagDrafts = ref<Record<string, string>>({});
const queuePanel = ref<'new' | 'lists' | ''>('');
const query = ref('');
const filterTag = ref('');
const appliedTag = ref('');
const sortKey = ref<'title' | 'state' | 'next_task'>('title');
let notesTimer: ReturnType<typeof globalThis.setTimeout> | undefined;
let notesWrite: Promise<void> | null = null;
let pendingNotesIntent: { key: string; opportunityId: string; body: string; revision: number; sequence: number } | null = null;

const activeLists = computed(() => board.value.lists.filter((list) => !list.archived).slice().sort((a, b) => a.position - b.position || a.id.localeCompare(b.id)));
const selectedCard = computed(() => board.value.cards.find((card) => card.opportunity.id === selectedId.value) ?? null);
const selectedMaterials = computed(() => props.outputs.filter((material) => material.opportunity_id === selectedId.value));
const selectedParticipants = computed(() => selectedCard.value
  ? props.parties.filter((party) => selectedCard.value!.opportunity.participant_ids.includes(party.id)).map((party) => party.name)
  : []);
const deliveredReleases = ref<Record<string, ReleasedMaterial>>({});
const threads = ref<ContactThread[]>([]);
let threadPollTimer: ReturnType<typeof globalThis.setInterval> | undefined;
let threadPollInFlight = false;

const sortedCards = computed(() => {
  const needle = query.value.trim().toLocaleLowerCase();
  let cards = board.value.cards.filter(({ opportunity }) => [opportunity.title, ...props.parties.filter(p => opportunity.participant_ids.includes(p.id)).map(p => p.name)].some(value => value.toLocaleLowerCase().includes(needle)));
  if (appliedTag.value) {
    cards = cards.filter((card) => card.opportunity.tags?.includes(appliedTag.value));
  }
  const key = sortKey.value;
  if (key === 'title') {
    cards.sort((a, b) => a.opportunity.title.localeCompare(b.opportunity.title) || a.opportunity.id.localeCompare(b.opportunity.id));
  } else if (key === 'state') {
    cards.sort((a, b) => a.opportunity.state.localeCompare(b.opportunity.state) || a.opportunity.title.localeCompare(b.opportunity.title));
  } else {
    cards.sort((a, b) => (b.next_task ? 1 : 0) - (a.next_task ? 1 : 0) || a.opportunity.title.localeCompare(b.opportunity.title));
  }
  return cards;
});

async function refreshBoard() {
  board.value = await parley.getBoard();
  if (!board.value.cards.some((card) => card.opportunity.id === selectedId.value)) {
    selectedId.value = '';
    detail.value = null;
    dirty.value = false;
  }
  renameValues.value = Object.fromEntries(board.value.lists.map((list) => [list.id, list.title]));
  moveTargets.value = Object.fromEntries(board.value.cards.map((card) => [card.opportunity.id, card.list_id]));
  archiveTargets.value = Object.fromEntries(board.value.lists.map((list) => [list.id, activeLists.value.find((candidate) => candidate.id !== list.id)?.id ?? '']));
  for (const card of board.value.cards) {
    if (tagDrafts.value[card.opportunity.id] === undefined) {
      tagDrafts.value[card.opportunity.id] = (card.opportunity.tags ?? []).join(', ');
    }
  }
}

function applyFilters() {
  appliedTag.value = filterTag.value.trim().toLowerCase();
}

async function saveTags(card: BoardPlacement) {
  const tags = [...new Set(
    (tagDrafts.value[card.opportunity.id] ?? '')
      .split(',')
      .map((tag) => tag.trim().toLowerCase())
      .filter(Boolean),
  )];
  await run(`Tags saved for ${card.opportunity.title}.`, async () => {
    const updated = await parley.setOpportunityTags(card.opportunity.id, {
      expected_revision: card.opportunity.revision,
      tags,
    });
    board.value.cards = board.value.cards.map((candidate) => candidate.opportunity.id === updated.id
      ? { ...candidate, opportunity: { ...candidate.opportunity, revision: updated.revision, tags: updated.tags } }
      : candidate);
    tagDrafts.value[card.opportunity.id] = tags.join(', ');
  });
}

async function refreshThreads() {
  if (!selectedId.value) { threads.value = []; return; }
  threads.value = await parley.listThreads({ opportunity_id: selectedId.value });
}

async function refreshCurrentCard() {
  await refreshBoard();
  await refreshThreads();
}

async function pollThreads() {
  if (!props.active || threadPollInFlight || !selectedId.value || globalThis.document.visibilityState !== 'visible') return;
  threadPollInFlight = true;
  try { await refreshThreads(); }
  catch { /* Preserve the last acknowledged thread state through transient read failures. */ }
  finally { threadPollInFlight = false; }
}

async function run(label: string, action: () => Promise<void>) {
  busy.value = true;
  error.value = '';
  status.value = '';
  try { await action(); status.value = label; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'The workspace command failed.'; }
  finally { busy.value = false; }
}

async function createOpportunity() {
  await run('Opportunity created. Its initial board placement does not change its stage or outreach state.', async () => {
    await parley.createOpportunity(title.value.trim(), partyId.value);
    title.value = '';
    partyId.value = ''; queuePanel.value = '';
    await refreshBoard();
  });
}

async function createList() {
  await run('Custom list created.', async () => {
    await parley.createBoardList({ title: newListTitle.value.trim(), before_list_id: null });
    newListTitle.value = ''; queuePanel.value = '';
    await refreshBoard();
  });
}

async function renameList(list: BoardList) {
  await run('List renamed.', async () => {
    await parley.renameBoardList(list.id, { title: (renameValues.value[list.id] ?? '').trim(), expected_revision: list.revision });
    await refreshBoard();
  });
}

async function reorderList(list: BoardList, direction: -1 | 1) {
  const index = activeLists.value.findIndex((candidate) => candidate.id === list.id);
  const target = index + direction;
  if (target < 0 || target >= activeLists.value.length) return;
  const ids = activeLists.value.map((candidate) => candidate.id);
  [ids[index], ids[target]] = [ids[target]!, ids[index]!];
  await run('Board list order saved.', async () => {
    await parley.reorderBoardLists({ ordered_ids: ids, expected_revisions: Object.fromEntries(activeLists.value.map((candidate) => [candidate.id, candidate.revision])) });
    await refreshBoard();
  });
}

async function archiveList(list: BoardList) {
  const destination = archiveTargets.value[list.id] ?? '';
  const target = activeLists.value.find((candidate) => candidate.id === destination);
  if (!target) { error.value = 'Choose an active destination list before archiving this list.'; return; }
  await run('List archived and its cards moved in their existing order.', async () => {
    await parley.archiveBoardList(list.id, {
      destination_list_id: target.id,
      expected_list_revision: list.revision,
      expected_destination_revision: target.revision,
    });
    await refreshBoard();
  });
}

async function moveCard(card: BoardPlacement, beforeOpportunityId: string | null = null) {
  const listId = moveTargets.value[card.opportunity.id] ?? card.list_id;
  const source = board.value.lists.find((list) => list.id === card.list_id);
  const destination = board.value.lists.find((list) => list.id === listId);
  if (!source || !destination) { error.value = 'Choose an active destination list.'; return; }
  await run('Card moved. Stage, notes, contact state, and deadlines were preserved.', async () => {
    await parley.moveOpportunityPlacement(card.opportunity.id, {
      list_id: listId,
      before_opportunity_id: beforeOpportunityId,
      expected_source_list_revision: source.revision,
      expected_list_revision: destination.revision,
      expected_placement_revision: card.placement_revision,
    });
    await refreshBoard();
  });
}

async function moveCardBy(card: BoardPlacement, direction: -1 | 1) {
  const cards = board.value.cards.filter((candidate) => candidate.list_id === card.list_id).slice().sort((a, b) => a.position - b.position || a.opportunity.id.localeCompare(b.opportunity.id));
  const index = cards.findIndex((candidate) => candidate.opportunity.id === card.opportunity.id);
  if (direction < 0) {
    if (index <= 0) return;
    await moveCard(card, cards[index - 1]!.opportunity.id);
    return;
  }
  if (index >= cards.length - 1) return;
  await moveCard(card, cards[index + 2]?.opportunity.id ?? null);
}

async function openCard(card: BoardPlacement) {
  const generation = ++openGeneration;
  await run('Card opened.', async () => {
    loadingNotes.value = true;
    const loaded = await parley.getOpportunityNotes(card.opportunity.id);
    if (generation !== openGeneration) {
      // A newer card was opened while this read was in flight; the stale
      // response must not re-select the earlier card (F-FR-1).
      loadingNotes.value = false;
      return;
    }
    detail.value = loaded;
    markdown.value = loaded.markdown;
    dirty.value = false;
    selectedId.value = card.opportunity.id;
    await refreshThreads();
    emit('navigate', `/queue/${encodeURIComponent(card.opportunity.id)}`);
    loadingNotes.value = false;
  });
}

async function openRoutedCard() {
  if (!props.routeId) return;
  const card = board.value.cards.find((item) => item.opportunity.id === props.routeId);
  if (!card || selectedId.value === props.routeId) return;
  await openCard(card);
}

async function saveNotes() {
  if (!detail.value) return;
  if (composing.value || notesWrite) return;
  if (notesTimer) globalThis.clearTimeout(notesTimer);
  pendingNotesIntent ??= {
    key: newOperationKey(), opportunityId: detail.value.opportunity_id, body: markdown.value,
    revision: detail.value.revision, sequence: noteSequence.value,
  };
  const intent = pendingNotesIntent;
  busy.value = true; error.value = ''; status.value = 'Saving notes…';
  notesWrite = (async () => {
    try {
      const saved = await parley.updateOpportunityNotes(intent.opportunityId, intent.body, intent.revision, intent.key);
      pendingNotesIntent = null;
      if (detail.value?.opportunity_id !== intent.opportunityId) return;
      detail.value = saved;
      if (noteSequence.value === intent.sequence) {
        loadingNotes.value = true;
        markdown.value = saved.markdown;
        dirty.value = false;
        loadingNotes.value = false;
        status.value = 'Markdown notes saved.';
      } else {
        status.value = 'Earlier notes saved; newer edits are still being saved.';
      }
      await refreshBoard();
    } catch (reason) {
      error.value = reason instanceof Error ? reason.message : 'Save status unknown. Your local notes remain available; retry the same edit after checking the current server version.';
      status.value = 'Notes were not acknowledged as saved.';
    } finally {
      notesWrite = null;
      busy.value = false;
      if (noteSequence.value !== intent.sequence && dirty.value && !composing.value) scheduleNotesSave();
    }
  })();
  await notesWrite;
}

function scheduleNotesSave() {
  if (workspaceQuarantined.value || !detail.value || loadingNotes.value || composing.value || !dirty.value) return;
  if (notesTimer) globalThis.clearTimeout(notesTimer);
  notesTimer = globalThis.setTimeout(() => { void saveNotes(); }, 750);
}

async function reloadNotes() {
  if (!selectedId.value) return;
  await run('Latest notes loaded.', async () => {
    loadingNotes.value = true;
    detail.value = await parley.getOpportunityNotes(selectedId.value);
    markdown.value = detail.value.markdown;
    dirty.value = false;
    pendingNotesIntent = null;
    loadingNotes.value = false;
  });
}

async function copyQuarantinedNotes() {
  try {
    await globalThis.navigator.clipboard.writeText(quarantinedMarkdown.value);
    quarantineStatus.value = 'Unsaved notes copied. They remain quarantined until you discard them or close this workspace.';
  } catch { quarantineStatus.value = 'Clipboard access was denied. Select and copy the read-only text manually; it remains quarantined.'; }
}

async function discardQuarantinedNotes() {
  workspaceQuarantined.value = false;
  quarantineStatus.value = '';
  if (notesTimer) globalThis.clearTimeout(notesTimer);
  notesTimer = undefined;
  dirty.value = false;
  pendingNotesIntent = null;
  detail.value = null;
  markdown.value = '';
  selectedId.value = '';
  await parley.refreshSession();
  await refreshBoard();
  globalThis.dispatchEvent(new globalThis.CustomEvent('parley:workspace-reloaded'));
}

async function onWorkspaceChanged(event: unknown) {
  const detailEvent = event as { detail?: { operation?: string } };
  if (['restore', 'erase_workspace'].includes(detailEvent.detail?.operation ?? '')) {
    if (dirty.value) {
      if (notesTimer) globalThis.clearTimeout(notesTimer);
      notesTimer = undefined;
      quarantinedMarkdown.value = markdown.value;
      workspaceQuarantined.value = true;
      quarantineStatus.value = 'This workspace was replaced. The notes buffer is isolated and cannot be saved into the restored workspace.';
      globalThis.dispatchEvent(new globalThis.CustomEvent('parley:workspace-quarantined'));
      return;
    }
    selectedId.value = '';
    detail.value = null;
    markdown.value = '';
  }
  try {
    await refreshBoard();
    globalThis.dispatchEvent(new globalThis.CustomEvent('parley:workspace-reloaded'));
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not refresh Queue after a workspace change.'; }
}

async function toggleTask(task: NonNullable<OpportunityNotes['tasks']>[number]) {
  if (!detail.value || dirty.value) { error.value = 'Save or discard Markdown edits before changing a task.'; return; }
  await run('Task updated in the canonical Markdown.', async () => {
    detail.value = await parley.toggleOpportunityTask(detail.value!.opportunity_id, {
      notes_revision: detail.value!.revision,
      byte_start: task.byte_start,
      byte_end: task.byte_end,
      line_sha256: task.line_sha256,
      checked: !task.checked,
    });
    loadingNotes.value = true;
    markdown.value = detail.value.markdown;
    loadingNotes.value = false;
    await refreshBoard();
  });
}

function noteDelivered(materialId: string, release: ReleasedMaterial) {
  deliveredReleases.value[materialId] = release;
}

function materialUpdated(material: Material) {
  if (deliveredReleases.value[material.id]?.version !== material.version) delete deliveredReleases.value[material.id];
  emit('material-updated', material);
}

watch(() => props.parties, (parties) => {
  if (!partyId.value && parties.length) partyId.value = parties[0]!.id;
}, { immediate: true });
watch(markdown, () => {
  if (loadingNotes.value || !detail.value) return;
  noteSequence.value += 1;
  dirty.value = true;
  scheduleNotesSave();
}, { flush: 'sync' });
watch(composing, (active) => { if (!active) scheduleNotesSave(); });
onMounted(async () => {
  try {
    await parley.connect(); await refreshBoard(); await openRoutedCard();
    threadPollTimer = globalThis.setInterval(() => { void pollThreads(); }, 2_000);
    globalThis.document.addEventListener('visibilitychange', onThreadVisibilityChange);
    globalThis.addEventListener('parley:workspace-changed', onWorkspaceChanged);
  }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not load the board.'; }
});
watch(() => props.routeId, (id) => {
  if (!id) return;
  void openRoutedCard();
});
watch(() => props.active, (active) => { if (active) void pollThreads(); });

function warnDirtyNavigation(event: BeforeUnloadEvent) {
  if (!dirty.value) return;
  event.preventDefault();
  event.returnValue = '';
}
onMounted(() => globalThis.addEventListener('beforeunload', warnDirtyNavigation));
onBeforeUnmount(() => {
  if (notesTimer) globalThis.clearTimeout(notesTimer);
  if (threadPollTimer) globalThis.clearInterval(threadPollTimer);
  globalThis.document.removeEventListener('visibilitychange', onThreadVisibilityChange);
  globalThis.removeEventListener('parley:workspace-changed', onWorkspaceChanged);
  globalThis.removeEventListener('beforeunload', warnDirtyNavigation);
});

function onThreadVisibilityChange() {
  if (globalThis.document.visibilityState === 'visible') void pollThreads();
}
</script>

<template>
  <section
    v-if="workspaceQuarantined"
    role="alertdialog"
    aria-modal="true"
    aria-labelledby="quarantine-heading"
    class="quarantine-dialog"
  >
    <h2 id="quarantine-heading">
      Unsaved notes quarantined
    </h2>
    <p>The workspace changed while these notes were dirty. They are not being replayed into the restored data.</p>
    <label>Copy out the isolated notes<textarea
      :value="quarantinedMarkdown"
      readonly
      rows="8"
      aria-label="Quarantined notes text"
    /></label>
    <button
      type="button"
      @click="copyQuarantinedNotes"
    >
      Copy quarantined notes
    </button>
    <button
      type="button"
      @click="discardQuarantinedNotes"
    >
      Discard local notes and load current workspace
    </button>
    <p
      v-if="quarantineStatus"
      role="status"
    >
      {{ quarantineStatus }}
    </p>
  </section>
  <section aria-labelledby="queue-heading">
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
        v-for="view in ['Board', 'Opportunities', 'Daily', 'Insights'] as const"
        :key="view"
        type="button"
        :aria-pressed="activeQueueView === view"
        @click="activeQueueView = view"
      >
        {{ view }}
      </button>
      <button
        :aria-expanded="queuePanel === 'new'"
        @click="queuePanel = queuePanel === 'new' ? '' : 'new'"
      >
        New opportunity
      </button>
      <button
        :aria-expanded="queuePanel === 'lists'"
        @click="queuePanel = queuePanel === 'lists' ? '' : 'lists'"
      >
        Manage lists
      </button>
    </nav>
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
    <div
      v-if="activeQueueView === 'Board' || activeQueueView === 'Opportunities'"
      class="queue-filter-sort"
    >
      <input
        v-model="query"
        type="search"
        aria-label="Find opportunities"
        placeholder="Find opportunity or participant"
      >
      <label>Filter by tag<input
        v-model="filterTag"
        aria-label="Filter by tag"
        maxlength="120"
      ></label>
      <label>Sort opportunities<select
        v-model="sortKey"
        aria-label="Sort opportunities"
      ><option value="title">Title</option><option value="state">State</option><option value="next_task">Next task</option></select></label>
      <button
        type="button"
        :disabled="busy"
        @click="applyFilters"
      >
        Apply to board and table
      </button>
    </div>
    <div class="workspace-grid">
      <form
        v-show="queuePanel === 'new'"
        @submit.prevent="createOpportunity"
      >
        <h3>Create an opportunity</h3>
        <label>Opportunity title<input
          v-model="title"
          required
          maxlength="500"
        ></label>
        <label>Participant<select
          v-model="partyId"
          required
        ><option
          disabled
          value=""
        >Choose a lead</option><option
          v-for="party in parties"
          :key="party.id"
          :value="party.id"
        >{{ party.name }} · {{ party.kind }}</option></select></label>
        <button
          :disabled="busy || !parties.length"
          type="submit"
        >
          Create opportunity
        </button>
        <p>A lead alone does not create an opportunity or outreach.</p>
      </form>
      <form
        v-show="queuePanel === 'lists'"
        @submit.prevent="createList"
      >
        <h3>Custom board lists</h3>
        <label>New list<input
          v-model="newListTitle"
          required
          maxlength="120"
        ></label>
        <button
          :disabled="busy"
          type="submit"
        >
          Add list
        </button>
      </form>
    </div>
    <p v-if="activeQueueView !== 'Daily' && activeQueueView !== 'Insights' && !board.cards.length">
      No opportunities yet. Select a lead to bring records together.
    </p>
    <div
      v-if="activeQueueView === 'Board' && board.lists.length"
      class="board-lists"
      aria-label="Opportunity board"
    >
      <section
        v-for="list in activeLists"
        :key="list.id"
        class="board-list"
        :aria-label="`${list.title} list`"
      >
        <header>
          <h3>{{ list.title }} <small>{{ board.cards.filter(card => card.list_id === list.id).length }}</small></h3>
          <details>
            <summary>List options</summary>
            <label>Rename {{ list.title }}<input
              v-model="renameValues[list.id]"
              :aria-label="`Rename ${list.title}`"
            ></label>
            <button
              :disabled="busy || renameValues[list.id] === list.title"
              @click="renameList(list)"
            >
              Save list name
            </button>
            <button
              :disabled="busy || activeLists.indexOf(list) === 0"
              :aria-label="`Move ${list.title} up`"
              @click="reorderList(list, -1)"
            >
              Move {{ list.title }} up
            </button>
            <button
              :disabled="busy || activeLists.indexOf(list) === activeLists.length - 1"
              :aria-label="`Move ${list.title} down`"
              @click="reorderList(list, 1)"
            >
              Move {{ list.title }} down
            </button>
            <label v-if="activeLists.length > 1">Archive destination for {{ list.title }}<select
              v-model="archiveTargets[list.id]"
              :aria-label="`Archive destination for ${list.title}`"
            ><option
              v-for="candidate in activeLists.filter((item) => item.id !== list.id)"
              :key="candidate.id"
              :value="candidate.id"
            >{{ candidate.title }}</option></select></label>
            <button
              :disabled="busy || activeLists.length < 2"
              @click="archiveList(list)"
            >
              Archive list
            </button>
          </details>
        </header>
        <article
          v-for="(card, index) in sortedCards.filter((item) => item.list_id === list.id)"
          :key="card.opportunity.id"
          class="record-card"
        >
          <h4>{{ card.opportunity.title }}</h4>
          <p>{{ card.opportunity.state }} · {{ card.opportunity.material_ids.length }} material(s)</p>
          <p v-if="card.next_task">
            Next task: {{ card.next_task }}
          </p>
          <details>
            <summary>Organize card</summary>
            <label>Move to list<select
              v-model="moveTargets[card.opportunity.id]"
              :aria-label="`Move ${card.opportunity.title} to list`"
            ><option
              v-for="candidate in activeLists"
              :key="candidate.id"
              :value="candidate.id"
            >{{ candidate.title }}</option></select></label>
            <button
              :disabled="busy || moveTargets[card.opportunity.id] === list.id"
              @click="moveCard(card)"
            >
              Move card
            </button>
            <button
              :disabled="busy || index === 0"
              :aria-label="`Move ${card.opportunity.title} up`"
              @click="moveCardBy(card, -1)"
            >
              Move {{ card.opportunity.title }} up
            </button>
            <button
              :disabled="busy || index === board.cards.filter((item) => item.list_id === list.id).length - 1"
              :aria-label="`Move ${card.opportunity.title} down`"
              @click="moveCardBy(card, 1)"
            >
              Move {{ card.opportunity.title }} down
            </button>
            <label>Edit tags for {{ card.opportunity.title }}
              <input
                :value="tagDrafts[card.opportunity.id] ?? ''"
                :aria-label="`Edit tags for ${card.opportunity.title}`"
                maxlength="500"
                @input="tagDrafts[card.opportunity.id] = ($event.target as HTMLInputElement).value"
              >
            </label>
            <button
              :disabled="busy"
              @click="saveTags(card)"
            >
              Save tags
            </button>
          </details>
          <button
            :disabled="busy"
            @click="openCard(card)"
          >
            Open card
          </button>
        </article>
      </section>
    </div>
    <table
      v-show="activeQueueView === 'Opportunities'"
      class="opportunity-table"
    >
      <thead>
        <tr>
          <th scope="col">
            Opportunity
          </th><th scope="col">
            Participants
          </th><th scope="col">
            State
          </th><th scope="col">
            Next task
          </th><th scope="col">
            Action
          </th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="card in sortedCards"
          :key="card.opportunity.id"
          :data-opportunity-id="card.opportunity.id"
        >
          <th scope="row">
            {{ card.opportunity.title }}
          </th><td>{{ parties.filter((party) => card.opportunity.participant_ids.includes(party.id)).map((party) => party.name).join(', ') }}</td><td>{{ card.opportunity.state }}</td><td>{{ card.next_task || '—' }}</td><td>
            <button
              type="button"
              @click="openCard(card)"
            >
              Open
            </button>
          </td>
        </tr>
      </tbody>
      <caption v-if="!board.cards.length">
        No opportunities yet.
      </caption>
    </table>
    <div v-show="activeQueueView === 'Daily'">
      <Daily @navigate="emit('navigate', $event)" />
    </div>
    <div v-show="activeQueueView === 'Insights'">
      <Insights @navigate="emit('navigate', $event)" />
    </div>
    <section
      v-if="selectedCard && detail"
      aria-label="Opportunity card detail"
    >
      <h3>{{ selectedCard.opportunity.title }}</h3>
      <div class="card-layout">
        <section aria-label="Notes and tasks">
          <h4>Canonical Markdown</h4>
          <label>Markdown notes<MarkdownEditor
            v-model="markdown"
            label="Markdown notes"
            @composition="composing = $event"
          /></label>
          <div class="card-actions">
            <button
              :disabled="busy || !dirty || composing"
              @click="saveNotes"
            >
              Save notes
            </button>
            <button
              :disabled="busy || !dirty"
              @click="reloadNotes"
            >
              Discard edits and reload
            </button>
          </div>
          <h4>Rendered preview</h4><MarkdownPreview :source="markdown" />
          <p
            role="status"
            aria-live="polite"
          >
            {{ dirty ? 'Unsaved notes' : status }}
          </p>
          <h4>Tasks</h4>
          <ul>
            <li
              v-for="(task, index) in detail.tasks"
              :key="`${task.byte_start}-${index}`"
            >
              <label><input
                type="checkbox"
                :checked="task.checked"
                :disabled="busy || dirty"
                @change="toggleTask(task)"
              >{{ task.text }}</label>
            </li>
          </ul>
          <p v-if="!detail.tasks.length">
            No checklist tasks. Add a Markdown list item with an unchecked task marker.
          </p>
        </section>
        <section aria-label="Materials and history">
          <h4>Material to send</h4>
          <p>Recipient, opportunity, version, approval and release remain bound to the material record.</p>
          <article
            v-for="material in selectedMaterials"
            :key="material.id"
            class="record-card"
          >
            <strong>{{ material.recipient_name }}</strong> · {{ material.kind }} · v{{ material.version }} · {{ material.status }}
            <p>{{ material.body }}</p>
            <MaterialActions
              :material="material"
              @updated="materialUpdated"
              @delivered="noteDelivered(material.id, $event)"
            />
            <AttachmentPanel
              v-if="material.attachment"
              :material="material"
              :release="deliveredReleases[material.id] ?? null"
            />
            <ContactThreadPanel
              :opportunity-id="selectedId"
              :material="material"
              :delivered="deliveredReleases[material.id] ?? null"
              :threads="threads"
              @changed="refreshCurrentCard"
            />
          </article>
          <p v-if="!selectedMaterials.length">
            No material is linked to this opportunity yet.
          </p>
          <h4>Participants</h4><p>{{ selectedParticipants.join(', ') }}</p>
        </section>
      </div>
    </section>
  </section>
</template>

<style scoped>
.board-lists { display: grid; grid-template-rows:minmax(18rem,auto); grid-auto-columns: max(17rem,calc((100% - 3rem)/4)); grid-auto-flow: column; gap: 1rem; overflow-x: auto; align-items: stretch; margin: .65rem 0; }
.board-list { background: #eef2ef; border: 1px solid #cbd5ce; border-radius: .5rem; min-width: 0; padding: .75rem; }
.board-list header { display: block; background: transparent; border: 0; padding: 0; }
.board-list details { margin-block: .4rem; }
.board-list summary { color: #365741; }
.board-list h3 small { float:right; font-weight:400; }
.board-list header h3 { margin-top: 0; }
.card-layout { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 1rem; }
.card-layout > section { min-width: 0; }
.card-actions { display: flex; gap: .5rem; }
.queue-views { display: flex; gap: .5rem; margin-block: .75rem; }
.queue-filter-sort { display: flex; flex-wrap: wrap; gap: .5rem; align-items: end; margin-block: .75rem; }
.opportunity-table { width: 100%; border-collapse: collapse; }
.opportunity-table th, .opportunity-table td { padding: .5rem; border-bottom: 1px solid #cbd5ce; text-align: left; }
.quarantine-dialog { position: fixed; z-index: 1000; inset: 10vh max(1rem, calc((100vw - 42rem) / 2)); max-height: 80vh; overflow: auto; background: white; border: 2px solid #80552a; border-radius: .5rem; padding: 1.25rem; box-shadow: 0 1rem 3rem #0005; }
pre { white-space: pre-wrap; overflow-wrap: anywhere; }
input[type="checkbox"] { width: auto; }
@media (max-width: 700px) { .card-layout { grid-template-columns: 1fr; } }
</style>
