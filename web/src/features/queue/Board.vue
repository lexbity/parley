<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { newOperationKey } from '../../api/client';
import NotesEditor, { type NotesService } from './NotesEditor.vue';

defineOptions({ name: 'OpportunityBoard' });

export type OpportunityRow = {
  id: string;
  revision: number;
  list_id: string;
  position: number;
  title: string;
  status: string;
  participants: Array<{ id: string; kind: 'person' | 'org'; name: string }>;
  tags: string[];
  next_task: string | null;
  deadline: string | null;
  missing_material: boolean;
};
export type BoardList = { id: string; name: string; position: number; revision: number; archived: boolean };
export type BoardSnapshot = { items: OpportunityRow[]; lists: BoardList[]; board_revision: number };
export type ParticipantOption = { id: string; kind: 'person' | 'org'; name: string };
export interface BoardService extends NotesService {
  listOpportunities(): Promise<BoardSnapshot>;
  createOpportunity(input: { title: string; participant_ids: string[] }, key: string): Promise<unknown>;
  createList(input: { name: string; expected_revision: number }, key: string): Promise<BoardSnapshot>;
  renameList(id: string, input: { name: string; expected_revision: number }, key: string): Promise<BoardSnapshot>;
  reorderLists(input: { ordered_ids: string[]; expected_revision: number }, key: string): Promise<BoardSnapshot>;
  archiveList(id: string, input: { destination_list_id: string; expected_revision: number }, key: string): Promise<BoardSnapshot>;
  moveCard(input: {
    opportunity_id: string; destination_list_id: string; anchor_opportunity_id: string | null;
    expected_board_revision: number; expected_source_revision: number; expected_destination_revision: number;
  }, key: string): Promise<BoardSnapshot>;
  reorderCard(input: {
    list_id: string; opportunity_id: string; anchor_opportunity_id: string | null; expected_revision: number;
  }, key: string): Promise<BoardSnapshot>;
}

const props = withDefaults(defineProps<{ service: BoardService; workspaceEpoch: string; participantOptions?: ParticipantOption[]; refreshKey?:number }>(), { participantOptions:undefined, refreshKey:0 });
const snapshot = ref<BoardSnapshot>({ items: [], lists: [], board_revision: 1 });
const selectedId = ref<string | null>(null);
const query = ref('');
const statusFilter = ref('all');
const participantFilter = ref('all');
const tagFilter = ref('all');
const deadlineFilter = ref('all');
const materialFilter = ref('all');
const tableSort = ref<'title' | 'next_task' | 'deadline'>('title');
const tableSortDirection = ref<'asc' | 'desc'>('asc');
const view = ref<'board' | 'list'>('board');
const loading = ref(true);
const busy = ref(false);
const errorText = ref('');
const notice = ref('');
const newListName = ref('');
const newOpportunityTitle = ref('');
const newOpportunityParticipants = ref<string[]>([]);
const pendingOpportunityCreate = ref<{ key: string; epoch: string; input: { title: string; participant_ids: string[] } } | null>(null);
const pendingListCreate = ref<{ key: string; epoch: string; input: { name: string; expected_revision: number } } | null>(null);
const renameDraft = ref<Record<string, string>>({});
const archiveDestination = ref<Record<string, string>>({});
const draggedId = ref<string | null>(null);
const pendingMutation = ref<{ key: string; epoch: string; run: () => Promise<BoardSnapshot>; message: string } | null>(null);
let refreshGeneration = 0;

const activeLists = computed(() => snapshot.value.lists.filter((list) => !list.archived).sort((a, b) => a.position - b.position));
const selected = computed(() => snapshot.value.items.find((item) => item.id === selectedId.value) ?? null);
const participants = computed(() => [...new Map(snapshot.value.items.flatMap((item) => item.participants).map((participant) => [participant.id, participant])).values()].sort((a, b) => a.name.localeCompare(b.name)));
const participantOptions = computed(() => props.participantOptions ?? participants.value);
const tags = computed(() => [...new Set(snapshot.value.items.flatMap((item) => item.tags))].sort((a, b) => a.localeCompare(b)));
const filteredItems = computed(() => snapshot.value.items.filter((item) => {
  const term = query.value.trim().toLocaleLowerCase();
  const matchesText = !term || [item.title, ...item.participants.map((participant) => participant.name), ...item.tags].some((value) => value.toLocaleLowerCase().includes(term));
  return matchesText
    && (statusFilter.value === 'all' || item.status === statusFilter.value)
    && (participantFilter.value === 'all' || item.participants.some((participant) => participant.id === participantFilter.value))
    && (tagFilter.value === 'all' || item.tags.includes(tagFilter.value))
    && (deadlineFilter.value === 'all' || (deadlineFilter.value === 'has' ? !!item.deadline : !item.deadline))
    && (materialFilter.value === 'all' || (materialFilter.value === 'missing' ? item.missing_material : !item.missing_material));
}).sort((a, b) => {
  const listOrder = (activeLists.value.find((list) => list.id === a.list_id)?.position ?? 0) - (activeLists.value.find((list) => list.id === b.list_id)?.position ?? 0);
  return listOrder || a.position - b.position || a.id.localeCompare(b.id);
}));
const sortedItems = computed(() => [...filteredItems.value].sort((a, b) => {
  const first = tableSort.value === 'title' ? a.title.toLocaleLowerCase() : tableSort.value === 'next_task' ? a.next_task ?? '' : a.deadline ?? '';
  const second = tableSort.value === 'title' ? b.title.toLocaleLowerCase() : tableSort.value === 'next_task' ? b.next_task ?? '' : b.deadline ?? '';
  const compared = first.localeCompare(second) || a.id.localeCompare(b.id);
  return tableSortDirection.value === 'asc' ? compared : -compared;
}));

function key(): string {
  return newOperationKey();
}

async function refresh(): Promise<void> {
  const generation = ++refreshGeneration;
  const epoch = props.workspaceEpoch;
  loading.value = true;
  errorText.value = '';
  try {
    const result = await props.service.listOpportunities();
    if (generation !== refreshGeneration || epoch !== props.workspaceEpoch) return;
    snapshot.value = result;
    if (selectedId.value && !snapshot.value.items.some((item) => item.id === selectedId.value)) selectedId.value = null;
    notice.value = '';
  } catch (error) { if(generation === refreshGeneration && epoch === props.workspaceEpoch) errorText.value = error instanceof Error ? error.message : 'Opportunities could not be loaded.'; }
  finally { if(generation === refreshGeneration) loading.value = false; }
}

function install(result: BoardSnapshot, message: string): void {
  snapshot.value = result;
  notice.value = message;
  errorText.value = '';
}

async function executePendingMutation(): Promise<void> {
  const intent = pendingMutation.value;
  if (!intent || busy.value) return;
  if (intent.epoch !== props.workspaceEpoch) {
    pendingMutation.value = null;
    errorText.value = 'The workspace changed. This board command will not be replayed into another workspace.';
    return;
  }
  busy.value = true;
  errorText.value = '';
  try {
    install(await intent.run(), intent.message);
    if (pendingMutation.value?.key === intent.key) pendingMutation.value = null;
  }
  catch (error) {
    if ((error as Error & { status?: number }).status === 412) {
      if (pendingMutation.value?.key === intent.key) pendingMutation.value = null;
      await refresh();
      errorText.value = 'The board order changed in another session. Current order is shown; review before retrying.';
    } else if (error instanceof TypeError) {
      errorText.value = 'Board change status unknown. Retry the same operation to reconcile it.';
    } else {
      if (pendingMutation.value?.key === intent.key) pendingMutation.value = null;
      errorText.value = error instanceof Error ? error.message : 'Board change failed.';
    }
  } finally { busy.value = false; }
}

async function runMutation(action: (operationKey: string) => Promise<BoardSnapshot>, message: string): Promise<void> {
  if (busy.value || pendingMutation.value) return;
  const operationKey = key();
  pendingMutation.value = { key: operationKey, epoch: props.workspaceEpoch, run: () => action(operationKey), message };
  await executePendingMutation();
}

async function addList(): Promise<void> {
  const intent = pendingListCreate.value ?? {
    key: key(), epoch: props.workspaceEpoch, input: { name: newListName.value.trim(), expected_revision: snapshot.value.board_revision },
  };
  if (intent.epoch !== props.workspaceEpoch) { errorText.value = 'The workspace changed. The previous list creation will not be replayed into this workspace.'; return; }
  if (!intent.input.name) { errorText.value = 'Enter a name for the new list.'; return; }
  pendingListCreate.value = intent;
  if (busy.value) return;
  busy.value = true;
  errorText.value = '';
  try {
    install(await props.service.createList(intent.input, intent.key), 'List created.');
    pendingListCreate.value = null;
    newListName.value = '';
  } catch (error) {
    if (error instanceof TypeError) errorText.value = 'List creation status unknown. Retry the same operation to reconcile it.';
    else { pendingListCreate.value = null; errorText.value = error instanceof Error ? error.message : 'List could not be created.'; }
  } finally { busy.value = false; }
}

async function addOpportunity(): Promise<void> {
  const intent = pendingOpportunityCreate.value ?? {
    key: key(), epoch: props.workspaceEpoch, input: { title: newOpportunityTitle.value.trim(), participant_ids: [...newOpportunityParticipants.value] },
  };
  if (intent.epoch !== props.workspaceEpoch) { errorText.value = 'The workspace changed. The previous opportunity creation will not be replayed into this workspace.'; return; }
  if (!intent.input.title) { errorText.value = 'Enter a title for the opportunity.'; return; }
  if (busy.value) return;
  pendingOpportunityCreate.value = intent;
  busy.value = true;
  errorText.value = '';
  try {
    await props.service.createOpportunity(intent.input, intent.key);
    snapshot.value = await props.service.listOpportunities();
    pendingOpportunityCreate.value = null;
    newOpportunityTitle.value = '';
    newOpportunityParticipants.value = [];
    notice.value = 'Opportunity created. No outbound task was created.';
  } catch (error) {
    const status = (error as Error & { status?: number }).status;
    if (status !== undefined && status >= 400 && status < 500 && status !== 409) pendingOpportunityCreate.value = null;
    errorText.value = error instanceof TypeError
      ? 'Opportunity creation status unknown. Retry the same operation to reconcile it.'
      : error instanceof Error ? error.message : 'Opportunity could not be created; retry to reconcile the original command.';
  }
  finally { busy.value = false; }
}

async function rename(list: BoardList): Promise<void> {
  const name = (renameDraft.value[list.id] ?? list.name).trim();
  if (!name) { errorText.value = 'List name cannot be empty.'; return; }
  const input = { name, expected_revision: list.revision };
  await runMutation((operationKey) => props.service.renameList(list.id, input, operationKey), 'List renamed. Cards and their status are unchanged.');
}

async function shiftList(list: BoardList, delta: -1 | 1): Promise<void> {
  const lists = [...activeLists.value];
  const at = lists.findIndex((item) => item.id === list.id);
  const target = at + delta;
  if (target < 0 || target >= lists.length) return;
  [lists[at], lists[target]] = [lists[target]!, lists[at]!];
  const input = { ordered_ids: lists.map((item) => item.id), expected_revision: snapshot.value.board_revision };
  await runMutation((operationKey) => props.service.reorderLists(input, operationKey), 'List order updated.');
}

async function archive(list: BoardList): Promise<void> {
  const destination = archiveDestination.value[list.id];
  if (!destination) { errorText.value = 'Choose a destination list before archiving this list.'; return; }
  const input = { destination_list_id: destination, expected_revision: list.revision };
  await runMutation((operationKey) => props.service.archiveList(list.id, input, operationKey), 'List archived and its cards preserved in the selected destination.');
}

function listFor(item: OpportunityRow): BoardList | undefined {
  return activeLists.value.find((list) => list.id === item.list_id);
}

function sortBy(field: 'title' | 'next_task' | 'deadline'): void {
  if (tableSort.value === field) tableSortDirection.value = tableSortDirection.value === 'asc' ? 'desc' : 'asc';
  else { tableSort.value = field; tableSortDirection.value = 'asc'; }
}

async function move(item: OpportunityRow, destination: BoardList, anchor: OpportunityRow | null): Promise<void> {
  const source = listFor(item);
  if (!source) { errorText.value = 'Create an active list before moving an opportunity.'; return; }
  const input = {
    opportunity_id: item.id, destination_list_id: destination.id, anchor_opportunity_id: anchor?.id ?? null,
    expected_board_revision: snapshot.value.board_revision,
    expected_source_revision: source.revision,
    expected_destination_revision: destination.revision,
  };
  await runMutation((operationKey) => props.service.moveCard(input, operationKey), 'Opportunity moved. Status, dates, and contact history are unchanged.');
}

async function reorder(item: OpportunityRow, delta: -1 | 1): Promise<void> {
  const list = listFor(item);
  if (!list) return;
  const peers = filteredItems.value.filter((candidate) => listFor(candidate)?.id === list.id);
  const at = peers.findIndex((candidate) => candidate.id === item.id);
  const target = at + delta;
  if (target < 0 || target >= peers.length) return;
  const anchor = delta < 0 ? peers[target] : peers[target + 1] ?? null;
  const input = { list_id: list.id, opportunity_id: item.id, anchor_opportunity_id: anchor?.id ?? null, expected_revision: list.revision };
  await runMutation((operationKey) => props.service.reorderCard(input, operationKey), 'Card order updated. Status and dates are unchanged.');
}

function onDrop(destination: BoardList): void {
  if (!draggedId.value) return;
  const item = snapshot.value.items.find((candidate) => candidate.id === draggedId.value);
  if (item) void move(item, destination, null);
  draggedId.value = null;
}

onMounted(() => { void refresh(); });
watch(() => props.refreshKey, (keyValue) => { if(keyValue) void refresh(); });
watch(() => props.workspaceEpoch, async () => {
  const hadPending = !!pendingMutation.value || !!pendingOpportunityCreate.value || !!pendingListCreate.value;
  selectedId.value = null;
  snapshot.value = { items: [], lists: [], board_revision: 1 };
  pendingOpportunityCreate.value = null;
  pendingListCreate.value = null;
  pendingMutation.value = null;
  await refresh();
  if (hadPending) errorText.value = 'Workspace changed. Unacknowledged board commands were quarantined and will not be replayed.';
});
</script>

<template>
  <section
    class="queue"
    aria-label="Opportunity queue"
  >
    <div class="queue-toolbar">
      <label>Search opportunities, participants or tags <input
        v-model="query"
        type="search"
      ></label>
      <label>Status <select v-model="statusFilter"><option value="all">All</option><option value="open">Open</option><option value="resolved">Resolved</option><option value="timed_out">Timed out</option><option value="archived">Archived</option></select></label>
      <label>Participant <select v-model="participantFilter"><option value="all">All participants</option><option
        v-for="party in participants"
        :key="party.id"
        :value="party.id"
      >{{ party.name }}</option></select></label>
      <label>Tag <select v-model="tagFilter"><option value="all">All tags</option><option
        v-for="tag in tags"
        :key="tag"
        :value="tag"
      >{{ tag }}</option></select></label>
      <label>Deadline <select v-model="deadlineFilter"><option value="all">Any deadline</option><option value="has">Has deadline</option><option value="none">No deadline</option></select></label>
      <label>Material <select v-model="materialFilter"><option value="all">Any material state</option><option value="missing">Missing material</option><option value="ready">Material present</option></select></label>
      <div
        role="group"
        aria-label="Opportunity view"
      >
        <button
          type="button"
          :aria-pressed="view === 'board'"
          @click="view = 'board'"
        >
          Board
        </button>
        <button
          type="button"
          :aria-pressed="view === 'list'"
          @click="view = 'list'"
        >
          Opportunities
        </button>
      </div>
      <button
        type="button"
        :disabled="loading || busy"
        @click="refresh"
      >
        Refresh
      </button>
      <form
        class="create-opportunity"
        @submit.prevent="addOpportunity"
      >
        <label>New opportunity title <input
          v-model="newOpportunityTitle"
          maxlength="160"
          required
          :disabled="!!pendingOpportunityCreate"
        ></label>
        <label>Participants <select
          v-model="newOpportunityParticipants"
          multiple
          :disabled="!!pendingOpportunityCreate"
        ><option
          v-for="participant in participantOptions"
          :key="participant.id"
          :value="participant.id"
        >{{ participant.name }} · {{ participant.kind }}</option></select></label>
        <button
          type="submit"
          :disabled="busy"
        >
          {{ pendingOpportunityCreate ? 'Retry opportunity creation' : 'Create opportunity' }}
        </button>
      </form>
    </div>
    <p
      v-if="notice"
      role="status"
    >
      {{ notice }}
    </p>
    <p
      v-if="errorText"
      role="alert"
    >
      {{ errorText }}
    </p>
    <button
      v-if="pendingMutation"
      type="button"
      :disabled="busy"
      @click="executePendingMutation"
    >
      Retry same board change
    </button>
    <p v-if="loading">
      Loading opportunities…
    </p>
    <p v-else-if="snapshot.items.length === 0">
      No opportunities yet. Create an opportunity to organize a possible exchange.
    </p>
    <p v-else-if="filteredItems.length === 0">
      No opportunities match these filters. Change a filter to see other records.
    </p>

    <div
      v-if="!loading && activeLists.length === 0"
      class="empty-lists"
    >
      <p>No active lists. Create a list before arranging opportunities.</p>
      <form @submit.prevent="addList">
        <label>List name <input
          v-model="newListName"
          required
          maxlength="80"
          :disabled="!!pendingListCreate"
        ></label><button
          type="submit"
          :disabled="busy"
        >
          {{ pendingListCreate ? 'Retry list creation' : 'Create list' }}
        </button>
      </form>
    </div>

    <div
      v-if="view === 'board' && activeLists.length"
      class="board"
      aria-label="Opportunity board"
    >
      <section
        v-for="list in activeLists"
        :key="list.id"
        class="board-list"
        :aria-label="`${list.name} list`"
        @dragover.prevent
        @drop.prevent="onDrop(list)"
      >
        <header>
          <h2>{{ list.name }}</h2><span>{{ filteredItems.filter((item) => listFor(item)?.id === list.id).length }}</span>
          <button
            type="button"
            :disabled="busy || activeLists[0]?.id === list.id"
            :aria-label="`Move ${list.name} earlier`"
            @click="shiftList(list, -1)"
          >
            ↑
          </button>
          <button
            type="button"
            :disabled="busy || activeLists.at(-1)?.id === list.id"
            :aria-label="`Move ${list.name} later`"
            @click="shiftList(list, 1)"
          >
            ↓
          </button>
        </header>
        <form
          class="list-edit"
          @submit.prevent="rename(list)"
        >
          <label :for="`rename-${list.id}`">Rename list</label><input
            :id="`rename-${list.id}`"
            v-model="renameDraft[list.id]"
            :placeholder="list.name"
            maxlength="80"
          ><button
            type="submit"
            :disabled="busy"
          >
            Rename
          </button>
        </form>
        <label>Archive to <select
          v-model="archiveDestination[list.id]"
          :disabled="activeLists.length < 2"
        ><option value="">Choose destination</option><option
          v-for="target in activeLists.filter((item) => item.id !== list.id)"
          :key="target.id"
          :value="target.id"
        >{{ target.name }}</option></select></label>
        <button
          type="button"
          :disabled="busy || activeLists.length < 2"
          @click="archive(list)"
        >
          Archive list
        </button>
        <ul>
          <li
            v-for="item in filteredItems.filter((candidate) => listFor(candidate)?.id === list.id)"
            :key="item.id"
            draggable="true"
            @dragstart="draggedId = item.id"
            @dragend="draggedId = null"
          >
            <button
              class="card-title"
              type="button"
              :aria-current="selectedId === item.id ? 'true' : undefined"
              @click="selectedId = item.id"
            >
              {{ item.title }}
            </button>
            <p>{{ item.participants.map((party) => party.name).join(', ') || 'No participants' }}</p>
            <p v-if="item.next_task">
              Next: {{ item.next_task }}
            </p><p v-if="item.deadline">
              Deadline: {{ item.deadline }}
            </p><p v-if="item.missing_material">
              Material to create
            </p>
            <div class="card-controls">
              <label :for="`move-${item.id}`">Move to list</label><select
                :id="`move-${item.id}`"
                :value="list.id"
                :disabled="busy"
                @change="move(item, activeLists.find((target) => target.id === ($event.target as HTMLSelectElement).value)!, null)"
              >
                <option
                  v-for="target in activeLists"
                  :key="target.id"
                  :value="target.id"
                >
                  {{ target.name }}
                </option>
              </select>
              <button
                type="button"
                :disabled="busy"
                :aria-label="`Move ${item.title} earlier in list`"
                @click="reorder(item, -1)"
              >
                ↑
              </button><button
                type="button"
                :disabled="busy"
                :aria-label="`Move ${item.title} later in list`"
                @click="reorder(item, 1)"
              >
                ↓
              </button>
            </div>
          </li>
        </ul>
      </section>
      <form
        class="new-list"
        @submit.prevent="addList"
      >
        <label>New list name <input
          v-model="newListName"
          required
          maxlength="80"
          :disabled="!!pendingListCreate"
        ></label><button
          type="submit"
          :disabled="busy"
        >
          {{ pendingListCreate ? 'Retry list creation' : 'Create list' }}
        </button>
      </form>
    </div>

    <div
      v-if="view === 'list' && !loading && filteredItems.length"
      class="opportunity-table-wrap"
    >
      <table>
        <caption>Opportunities in the current filter</caption><thead>
          <tr>
            <th scope="col">
              <button
                type="button"
                @click="sortBy('title')"
              >
                Opportunity · {{ tableSort === 'title' ? tableSortDirection : 'sort' }}
              </button>
            </th><th scope="col">
              Participants
            </th><th scope="col">
              Status
            </th><th scope="col">
              <button
                type="button"
                @click="sortBy('next_task')"
              >
                Next task · {{ tableSort === 'next_task' ? tableSortDirection : 'sort' }}
              </button>
            </th><th scope="col">
              <button
                type="button"
                @click="sortBy('deadline')"
              >
                Deadline · {{ tableSort === 'deadline' ? tableSortDirection : 'sort' }}
              </button>
            </th><th scope="col">
              Material
            </th><th scope="col">
              List
            </th>
          </tr>
        </thead><tbody>
          <tr
            v-for="item in sortedItems"
            :key="item.id"
          >
            <th scope="row">
              <button
                type="button"
                @click="selectedId = item.id"
              >
                {{ item.title }}
              </button>
            </th><td>{{ item.participants.map((party) => party.name).join(', ') }}</td><td>{{ item.status }}</td><td>{{ item.next_task ?? '—' }}</td><td>{{ item.deadline ?? '—' }}</td><td>{{ item.missing_material ? 'To create' : '—' }}</td><td>{{ listFor(item)?.name ?? '—' }}</td>
          </tr>
        </tbody>
      </table>
    </div>

    <section
      v-if="selected"
      class="opportunity-detail"
      :aria-label="`Opportunity: ${selected.title}`"
    >
      <header>
        <h2>{{ selected.title }}</h2><button
          type="button"
          @click="selectedId = null"
        >
          Close opportunity
        </button>
      </header>
      <p>Participants: {{ selected.participants.map((party) => `${party.name} (${party.kind})`).join(', ') || 'None' }}</p>
      <div class="split-card">
        <div>
          <NotesEditor
            :key="`${props.workspaceEpoch}:${selected.id}`"
            :opportunity-id="selected.id"
            :service="props.service"
            :workspace-epoch="props.workspaceEpoch"
          />
        </div><div class="materials">
          <h3>Materials to send</h3><slot
            name="materials"
            :opportunity="selected"
          >
            <p>Materials and releases for this opportunity appear here.</p>
          </slot>
        </div>
      </div>
    </section>
  </section>
</template>

<style scoped>
.queue-toolbar,.card-controls,.board-list header,.opportunity-detail>header { display:flex; align-items:end; gap:.75rem; flex-wrap:wrap; }
.queue-toolbar label { max-width:20rem; }
.create-opportunity { display:flex; align-items:end; gap:.5rem; flex-wrap:wrap; }
.create-opportunity select { min-height:2.5rem; }
.board { display:flex; align-items:flex-start; gap:1rem; overflow:auto; padding:1rem 0; }
.board-list,.new-list { flex:0 0 min(21rem,88vw); min-width:17rem; background:#edf1ed; border:1px solid #d7dfd9; border-radius:.5rem; padding:.8rem; }
.board-list>header { align-items:center; }
.board-list h2 { flex:1; margin:.2rem 0; }
.board-list ul { list-style:none; padding:0; display:grid; gap:.6rem; }
.board-list li { background:white; border:1px solid #d7dfd9; border-radius:.4rem; padding:.75rem; }
.board-list li p { margin:.35rem 0; }
.card-title { width:100%; text-align:left; font-weight:650; }
.list-edit { display:grid; grid-template-columns:1fr auto; align-items:end; gap:.4rem; }
.list-edit label { grid-column:1/-1; }
.card-controls { align-items:center; }
.card-controls label { width:100%; }
.card-controls select { flex:1; min-width:8rem; }
.opportunity-table-wrap { overflow-x:auto; }
table { border-collapse:collapse; width:100%; background:white; }
th,td { border:1px solid #d7dfd9; text-align:left; padding:.5rem; vertical-align:top; }
.opportunity-detail { margin-top:1rem; }
.split-card { display:grid; grid-template-columns:minmax(0,1fr) minmax(0,1fr); gap:1rem; }
.materials { min-width:0; }
@media(max-width:760px) { .split-card { grid-template-columns:1fr; } }
</style>
