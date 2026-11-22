<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { parley, type BoardList, type BoardPlacement, type BoardSnapshot, type GraphEntity, type Material, type OpportunityNotes } from '../../api/client';

const props = defineProps<{ parties: GraphEntity[]; outputs: Material[] }>();
const board = ref<BoardSnapshot>({ lists: [], cards: [] });
const selectedId = ref('');
const detail = ref<OpportunityNotes | null>(null);
const markdown = ref('');
const dirty = ref(false);
const title = ref('');
const partyId = ref('');
const newListTitle = ref('');
const renameValues = ref<Record<string, string>>({});
const moveTargets = ref<Record<string, string>>({});
const archiveTargets = ref<Record<string, string>>({});
const busy = ref(false);
const error = ref('');
const status = ref('');

const activeLists = computed(() => board.value.lists.filter((list) => !list.archived).slice().sort((a, b) => a.position - b.position || a.id.localeCompare(b.id)));
const selectedCard = computed(() => board.value.cards.find((card) => card.opportunity.id === selectedId.value) ?? null);
const selectedMaterials = computed(() => props.outputs.filter((material) => material.opportunity_id === selectedId.value));
const selectedParticipants = computed(() => selectedCard.value
  ? props.parties.filter((party) => selectedCard.value!.opportunity.participant_ids.includes(party.id)).map((party) => party.name)
  : []);

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
    partyId.value = '';
    await refreshBoard();
  });
}

async function createList() {
  await run('Custom list created.', async () => {
    await parley.createBoardList({ title: newListTitle.value.trim(), before_list_id: null });
    newListTitle.value = '';
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
  await run('Card opened.', async () => {
    detail.value = await parley.getOpportunityNotes(card.opportunity.id);
    markdown.value = detail.value.markdown;
    dirty.value = false;
    selectedId.value = card.opportunity.id;
  });
}

async function saveNotes() {
  if (!detail.value) return;
  await run('Markdown notes saved.', async () => {
    detail.value = await parley.updateOpportunityNotes(detail.value!.opportunity_id, markdown.value, detail.value!.revision);
    markdown.value = detail.value.markdown;
    dirty.value = false;
    await refreshBoard();
  });
}

async function reloadNotes() {
  if (!selectedId.value) return;
  await run('Latest notes loaded.', async () => {
    detail.value = await parley.getOpportunityNotes(selectedId.value);
    markdown.value = detail.value.markdown;
    dirty.value = false;
  });
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
    markdown.value = detail.value.markdown;
    await refreshBoard();
  });
}

async function copyMaterial(material: Material) {
  await run('Approved version copied. Marking it sent remains a separate owner action.', async () => {
    const release = await parley.releaseMaterial(material.id);
    try {
      await globalThis.navigator.clipboard.writeText(release.body);
      await parley.acknowledgeRelease(release.release_id, true);
    } catch (reason) {
      await parley.acknowledgeRelease(release.release_id, false).catch(() => undefined);
      throw reason;
    }
  });
}

watch(() => props.parties, (parties) => {
  if (!partyId.value && parties.length) partyId.value = parties[0]!.id;
}, { immediate: true });
onMounted(async () => {
  try { await parley.connect(); await refreshBoard(); }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not load the board.'; }
});
</script>

<template>
  <section aria-labelledby="queue-heading">
    <h2 id="queue-heading">
      Opportunities
    </h2>
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
    <div class="workspace-grid">
      <form @submit.prevent="createOpportunity">
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
      <form @submit.prevent="createList">
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
    <p v-if="!board.cards.length">
      No opportunities yet. Select a lead to bring records together.
    </p>
    <div
      v-if="board.lists.length"
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
          <h3>{{ list.title }}</h3>
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
            Move list up
          </button>
          <button
            :disabled="busy || activeLists.indexOf(list) === activeLists.length - 1"
            :aria-label="`Move ${list.title} down`"
            @click="reorderList(list, 1)"
          >
            Move list down
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
        </header>
        <article
          v-for="(card, index) in board.cards.filter((item) => item.list_id === list.id).slice().sort((a, b) => a.position - b.position || a.opportunity.id.localeCompare(b.opportunity.id))"
          :key="card.opportunity.id"
          class="record-card"
        >
          <h4>{{ card.opportunity.title }}</h4>
          <p>{{ card.opportunity.state }} · {{ card.opportunity.material_ids.length }} material(s)</p>
          <p v-if="card.next_task">
            Next task: {{ card.next_task }}
          </p>
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
            Move card up
          </button>
          <button
            :disabled="busy || index === board.cards.filter((item) => item.list_id === list.id).length - 1"
            :aria-label="`Move ${card.opportunity.title} down`"
            @click="moveCardBy(card, 1)"
          >
            Move card down
          </button>
          <button
            :disabled="busy"
            @click="openCard(card)"
          >
            Open card
          </button>
        </article>
      </section>
    </div>
    <section
      v-if="selectedCard && detail"
      aria-label="Opportunity card detail"
    >
      <h3>{{ selectedCard.opportunity.title }}</h3>
      <div class="card-layout">
        <section aria-label="Notes and tasks">
          <h4>Canonical Markdown</h4>
          <label>Markdown notes<textarea
            v-model="markdown"
            rows="18"
            @input="dirty = true"
          /></label>
          <div class="card-actions">
            <button
              :disabled="busy || !dirty"
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
          <h4>Rendered preview</h4><pre>{{ markdown }}</pre>
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
            <button
              :disabled="busy || material.status !== 'approved'"
              @click="copyMaterial(material)"
            >
              Copy approved text
            </button>
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
.board-lists { display: grid; grid-auto-columns: minmax(18rem, 1fr); grid-auto-flow: column; gap: 1rem; overflow-x: auto; align-items: start; margin: 1rem 0; }
.board-list { background: #eef2ef; border: 1px solid #cbd5ce; border-radius: .5rem; min-width: 18rem; padding: .75rem; }
.board-list header { display: block; background: transparent; border: 0; padding: 0; }
.board-list header h3 { margin-top: 0; }
.card-layout { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 1rem; }
.card-layout > section { min-width: 0; }
.card-actions { display: flex; gap: .5rem; }
pre { white-space: pre-wrap; overflow-wrap: anywhere; }
input[type="checkbox"] { width: auto; }
@media (max-width: 700px) { .card-layout { grid-template-columns: 1fr; } }
</style>
