<script setup lang="ts">
/* global DragEvent, Event, HTMLSelectElement, BeforeUnloadEvent, CustomEvent, BroadcastChannel, MessageEvent, HTMLElement, HTMLDialogElement */
import { computed, nextTick, onMounted, ref, watch } from 'vue';
import { Background, Controls, VueFlow, type Edge, type Node } from '@xyflow/vue';
import {
  newOperationKey, parley,
  type DeliveryRow, type EntityCard, type GraphSnapshot, type PlacementView, type ResourceBoardSnapshot,
} from '../../api/client';
import ResourceEditorForm from './ResourceEditorForm.vue';

/**
 * Resources workspace (amendment 2026-11-22): an owner-maintained kanban
 * beside a graph. Three view IDs drive navigation: the selected board
 * resource, the graph root, and the inspected item (node or connection).
 * Selecting a tile sets all three; opening a neighbor changes only the
 * inspected item; "Explore connections" promotes it to the root.
 */

const props = defineProps<{ routeId?: string | null }>();
const emit = defineEmits<{ navigate: [path: string]; changed: [] }>();

const snapshot = ref<ResourceBoardSnapshot | null>(null);
const loading = ref(false);
const error = ref('');
const status = ref('');

const selectedId = ref<string | null>(null);
const rootId = ref<string | null>(null);
const inspectedId = ref<string | null>(null);
const inspectedEdgeId = ref<string | null>(null);

const deliveries = ref<DeliveryRow[]>([]);
const deliveriesLoading = ref(false);

async function loadDeliveries(id: string | null) {
  if (!id) { deliveries.value = []; return; }
  deliveriesLoading.value = true;
  try {
    deliveries.value = await parley.getEntityCardDeliveries(id);
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'Could not load deliveries.';
    deliveries.value = [];
  } finally {
    deliveriesLoading.value = false;
  }
}

const graph = ref<GraphSnapshot>({ nodes: [], edges: [], omitted_nodes: 0, omitted_edges: 0 });
const graphLoading = ref(false);
const graphError = ref('');
const graphGeneration = ref(0);

type SummaryForEdit = { resource_id: string; title: string; subtype: string; readiness: string; placement: PlacementView | null; etag: string };

const editor = ref<{ open: boolean; mode: 'create' | 'edit'; card: EntityCard | null; summary: SummaryForEdit | null; columnId: string | null; graphMode: boolean }>({
  open: false, mode: 'create', card: null, summary: null, columnId: null, graphMode: false,
});

function summaryForEdit(card: CardSummary): SummaryForEdit {
  return {
    resource_id: card.resource_id, title: card.title, subtype: card.subtype,
    readiness: card.readiness, placement: card.placement, etag: card.etag,
  };
}
const formKey = ref(0);
const editorDirty = ref(false);
const formRef = ref<InstanceType<typeof ResourceEditorForm> | null>(null);
const boardDialog = ref<HTMLDialogElement | null>(null);
/** While a draft is dirty, switching cards offers Save / Discard / Stay
 * (spec §4) instead of silently discarding. */
const pendingSwitch = ref<(() => void) | null>(null);
let invocation: HTMLElement | null = null;
const layoutBusy = ref(false);
const canvasFrame = ref<HTMLElement | null>(null);
const anchor = ref({ left: '0.6rem', top: '0.6rem' });

type CardSummary = NonNullable<ResourceBoardSnapshot['cards'][number]>;

const cardsByColumn = computed(() => {
  const map = new Map<string, CardSummary[]>();
  for (const column of snapshot.value?.columns ?? []) map.set(column.id, []);
  for (const card of snapshot.value?.cards ?? []) {
    const columnId = card.placement?.column_id;
    if (columnId && map.has(columnId)) map.get(columnId)!.push(card);
  }
  for (const list of map.values()) list.sort((a, b) => (a.placement?.position ?? 0) - (b.placement?.position ?? 0) || a.resource_id.localeCompare(b.resource_id));
  return map;
});

const offBoardCards = computed(() => snapshot.value?.off_board ?? []);
const selectedCard = computed(() => snapshot.value?.cards.find((card) => card.resource_id === selectedId.value) ?? null);
const selectedNode = computed(() => graph.value.nodes.find((node) => node.id === inspectedId.value) ?? null);
const selectedEdge = computed(() => graph.value.edges.find((edge) => edge.id === inspectedEdgeId.value) ?? null);

const rootName = computed(() =>
  graph.value.nodes.find((node) => node.id === rootId.value)?.name
  ?? snapshot.value?.cards.find((card) => card.resource_id === rootId.value)?.title
  ?? snapshot.value?.off_board.find((card) => card.resource_id === rootId.value)?.title
  ?? '');

const graphNodes = computed<Node[]>(() => graph.value.nodes.map((node) => {
  const classes = [node.id === rootId.value ? 'root-node' : '', node.kind !== 'resource' ? 'party-node' : ''].filter(Boolean).join(' ');
  return {
    id: node.id, type: 'default', position: { x: node.x, y: node.y },
    data: { label: `${node.name}\n${node.kind}` },
    selected: node.id === inspectedId.value,
    draggable: !layoutBusy.value,
    class: classes || undefined,
  };
}));
const graphEdges = computed<Edge[]>(() => graph.value.edges.map((edge) => ({
  id: edge.id, source: edge.from_id, target: edge.to_id, label: edge.verb,
  selected: edge.id === inspectedEdgeId.value,
  data: { edgeId: edge.id },
  style: { stroke: edge.tier === 2 ? '#8b6725' : '#326c53', strokeWidth: 2 },
  labelStyle: { fill: '#18221f', fontWeight: 600 },
})));

async function refreshBoard() {
  loading.value = true;
  try {
    const next = await parley.getResourceBoard({ includeOffboard: true });
    snapshot.value = next;
    if (selectedId.value && !next.cards.some((card) => card.resource_id === selectedId.value) && !next.off_board.some((card) => card.resource_id === selectedId.value)) {
      selectedId.value = null;
    }
    const firstCard = next.cards[0];
    if (!selectedId.value && firstCard) selectCard(firstCard.resource_id);
    if (!rootId.value && firstCard) rootId.value = firstCard.resource_id;
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'Could not load the resource board.';
  } finally {
    loading.value = false;
  }
}

async function loadGraph() {
  const own = ++graphGeneration.value;
  const center = rootId.value ?? selectedId.value;
  graphLoading.value = true;
  graphError.value = '';
  try {
    const next = await parley.getGraph({ center_id: center ?? undefined, view_scope: 'resources', view_id: rootId.value ?? 'board' });
    if (own !== graphGeneration.value) return;
    graph.value = next;
    if (!next.nodes.some((node) => node.id === inspectedId.value)) inspectedId.value = null;
    if (!next.edges.some((edge) => edge.id === inspectedEdgeId.value)) inspectedEdgeId.value = null;
  } catch (reason) {
    if (own === graphGeneration.value) graphError.value = reason instanceof Error ? reason.message : 'Could not load the graph.';
  } finally {
    if (own === graphGeneration.value) graphLoading.value = false;
  }
}

function selectCard(id: string) {
  if (selectedId.value === id) return;
  selectedId.value = id;
  rootId.value = id;
  inspectedId.value = id;
  inspectedEdgeId.value = null;
  void loadGraph();
  void loadDeliveries(id);
}

function selectNode(id: string) {
  inspectedId.value = id;
  inspectedEdgeId.value = null;
  // Selecting a resource node expands its editor at that node (spec §1);
  // clicking the node already being edited keeps its live draft.
  if (editor.value.open && editor.value.graphMode && editor.value.card?.id === id) return;
  const node = graph.value.nodes.find((item) => item.id === id);
  if (node && node.kind === 'resource') openEdit(id, true);
}

function exploreConnections(id: string) {
  rootId.value = id;
  inspectedId.value = id;
  inspectedEdgeId.value = null;
  status.value = `Network root is now ${graph.value.nodes.find((node) => node.id === id)?.name ?? id}.`;
  void loadGraph();
}

function returnToSelected() {
  if (selectedId.value) {
    rootId.value = selectedId.value;
    inspectedId.value = selectedId.value;
    inspectedEdgeId.value = null;
    void loadGraph();
  }
}

function captureInvocation() {
  invocation = globalThis.document.activeElement instanceof HTMLElement ? globalThis.document.activeElement : null;
}

/** Route editor switches through the dirty-draft Save / Discard / Stay flow. */
function requestEditorSwitch(action: () => void) {
  if (!editorDirty.value) { action(); return; }
  pendingSwitch.value = action;
}

function openCreate(columnId: string) {
  requestEditorSwitch(() => {
    captureInvocation();
    editor.value = { open: true, mode: 'create', card: null, summary: null, columnId, graphMode: false };
    formKey.value += 1;
    void nextTick(showBoardDialog);
  });
}

function openEdit(id: string, graphMode = false) {
  requestEditorSwitch(() => {
    captureInvocation();
    void (async () => {
      try {
        const card = await parley.getEntityCard(id);
        const found = snapshot.value?.cards.find((item) => item.resource_id === id);
        editor.value = { open: true, mode: 'edit', card, summary: found ? summaryForEdit(found) : null, columnId: null, graphMode };
        formKey.value += 1;
        if (graphMode) {
          await nextTick();
          setAnchor(id);
        } else {
          await nextTick();
          showBoardDialog();
        }
      } catch (reason) {
        error.value = reason instanceof Error ? reason.message : 'Could not open the card editor.';
      }
    })();
  });
}

/** jsdom implements <dialog> without modal methods; content stays inline there. */
function showBoardDialog() {
  const dialog = boardDialog.value;
  if (dialog && !dialog.open && typeof dialog.showModal === 'function') dialog.showModal();
  else if (dialog && !dialog.open && typeof dialog.show === 'function') dialog.show();
}

function dismissBoardDialog() {
  const dialog = boardDialog.value;
  if (dialog?.open && typeof dialog.close === 'function') dialog.close();
}

function closeEditor() {
  editor.value.open = false;
  editorDirty.value = false;
  dismissBoardDialog();
  void nextTick(() => invocation?.focus());
}

async function resolveEditorSwitch(save: boolean) {
  if (save && !await (formRef.value?.save() ?? Promise.resolve(false))) return;
  if (!save) closeEditor();
  const action = pendingSwitch.value;
  pendingSwitch.value = null;
  action?.();
}

function stayOnEditor() {
  pendingSwitch.value = null;
}

function onDialogCancel(event: Event) {
  // Escape follows the draft rules: a dirty draft offers Save / Discard / Stay.
  if (editorDirty.value) {
    event.preventDefault();
    if (!pendingSwitch.value) pendingSwitch.value = () => {};
    return;
  }
  closeEditor();
}

function setAnchor(id: string) {
  const frame = canvasFrame.value;
  if (!frame) return;
  const node = Array.from(frame.querySelectorAll<HTMLElement>('[data-id]')).find((el) => el.getAttribute('data-id') === id);
  if (!node) return;
  const bounds = frame.getBoundingClientRect();
  const rect = node.getBoundingClientRect();
  anchor.value = {
    left: Math.max(6, Math.min(rect.left - bounds.left, bounds.width - 436)) + 'px',
    top: Math.max(6, Math.min(rect.top - bounds.top, Math.max(6, bounds.height - 330))) + 'px',
  };
}

function onSaved(result: { entity: EntityCard; placement: PlacementView | null; createdRelated: { edge_id: string; to_id: string } | null }) {
  const wasCreate = editor.value.mode === 'create';
  editorDirty.value = false;
  editor.value.open = false;
  dismissBoardDialog();
  selectedId.value = result.entity.id;
  inspectedId.value = result.entity.id;
  inspectedEdgeId.value = null;
  // Only a create promotes the graph root; an ordinary edit never recenters
  // the graph (spec §3).
  if (wasCreate) rootId.value = result.entity.id;
  postBoardChanged();
  void refreshBoard().then(() => void loadGraph());
  emit('changed');
  status.value = result.createdRelated ? 'Card and relationship saved; the related card is off board.' : 'Card saved.';
  const action = pendingSwitch.value;
  pendingSwitch.value = null;
  void nextTick(() => invocation?.focus());
  action?.();
}

async function moveCard(card: CardSummary, destinationColumnId: string | null) {
  try {
    await parley.moveResourceCard({
      resource_id: card.resource_id,
      destination_column_id: destinationColumnId,
      before_resource_id: null,
      expected_placement_revision: card.placement?.revision ?? null,
      expected_source_column_revision: card.placement ? snapshot.value?.columns.find((c) => c.id === card.placement?.column_id)?.revision ?? null : null,
      expected_destination_column_revision: destinationColumnId ? snapshot.value?.columns.find((c) => c.id === destinationColumnId)?.revision ?? null : null,
    });
    status.value = destinationColumnId
      ? `${card.title} moved to ${snapshot.value?.columns.find((c) => c.id === destinationColumnId)?.title ?? 'the board'}.`
      : `${card.title} removed from the board.`;
    postBoardChanged();
    void refreshBoard().then(() => void loadGraph());
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'Could not move the card.';
  }
}

function onDragOver() { /* prevent default to allow drops */ }

function onDragStart(event: DragEvent, cardId: string) {
  event.dataTransfer?.setData('text/plain', cardId);
}

function onDrop(event: DragEvent, destinationColumnId: string) {
  event.preventDefault();
  const cardId = event.dataTransfer?.getData('text/plain');
  if (!cardId) return;
  const card = snapshot.value?.cards.find((item) => item.resource_id === cardId);
  if (card && card.placement?.column_id !== destinationColumnId) void moveCard(card, destinationColumnId);
}

function onColumnChange(card: CardSummary, event: Event) {
  const value = (event.target as HTMLSelectElement).value;
  void moveCard(card, value === '__off__' ? null : value);
}

/** Persist a manual node drag per scope/root; a drag changes only view
 * geometry (spec §3). Positions load from the same scoped view. */
async function saveNodePosition(event: { node: { id: string; position: { x: number; y: number } } }) {
  const node = graph.value.nodes.find((item) => item.id === event.node.id);
  if (!node || layoutBusy.value) return;
  layoutBusy.value = true;
  error.value = '';
  try {
    await parley.saveGraphLayout({
      view_scope: 'resources',
      view_id: rootId.value ?? 'board',
      positions: [{ entity_id: node.id, x: event.node.position.x, y: event.node.position.y, expected_revision: node.position_revision }],
    }, newOperationKey());
  } catch (reason) {
    error.value = reason instanceof Error ? `${reason.message} The saved layout is unchanged; reload the graph before retrying.` : 'Layout could not be saved. The saved layout is unchanged.';
  } finally {
    layoutBusy.value = false;
  }
  void loadGraph();
}

function placeOffBoard(card: { resource_id: string; title: string; legacy?: boolean }, event: Event) {
  const value = (event.target as HTMLSelectElement).value;
  if (!value) return;
  const summary = snapshot.value?.cards.find((item) => item.resource_id === card.resource_id);
  void moveCard(summary ?? { resource_id: card.resource_id, title: card.title, subtype: '', readiness: 'draft', placement: null, counts: { files: 0, connections: 0, proposed: 0, deliveries: 0 }, gaps: [], etag: '' }, value);
}

async function detachEdge(edgeId: string) {
  if (!globalThis.confirm('Detach this relationship? Both cards remain.')) return;
  const edge = graph.value.edges.find((item) => item.id === edgeId);
  if (!edge) return;
  try {
    await parley.deleteEdge(edgeId, edge.revision);
    status.value = 'Relationship detached; both cards remain.';
    inspectedEdgeId.value = null;
    void loadGraph();
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'Could not detach the relationship.';
  }
}

const availableEndpoints = computed(() => {
  const items: Array<{ id: string; label: string; kind: string }> = [];
  for (const list of cardsByColumn.value.values()) for (const card of list) items.push({ id: card.resource_id, label: card.title, kind: card.subtype });
  for (const card of offBoardCards.value) items.push({ id: card.resource_id, label: card.title, kind: card.subtype });
  for (const node of graph.value.nodes) {
    if (node.kind !== 'resource' && !items.some((item) => item.id === node.id)) items.push({ id: node.id, label: node.name, kind: node.kind });
  }
  return items;
});

function openInspectedParty() {
  const node = selectedNode.value;
  if (node && node.kind !== 'resource') emit('navigate', `/parties/${encodeURIComponent(node.id)}`);
}

function onCanvasEnter() {
  const node = selectedNode.value;
  if (!node) return;
  if (node.kind === 'resource') void openEdit(node.id, true);
  else openInspectedParty();
}

function onBeforeUnload(event: BeforeUnloadEvent) {
  if (!editorDirty.value) return;
  event.preventDefault();
  (event as unknown as { returnValue: string }).returnValue = '';
}

function onWorkspaceChanged(event: Event) {
  const detail = (event as CustomEvent<{ operation?: string }>).detail;
  if (!detail || !['restore', 'erase_workspace', 'erase_party'].includes(detail.operation ?? '')) return;
  if (editorDirty.value) {
    if (globalThis.confirm('This workspace was replaced. Discard the unsaved card draft and reload?')) {
      editorDirty.value = false;
      editor.value.open = false;
    } else {
      return;
    }
  }
  void refreshBoard().then(() => void loadGraph());
}

const BOARD_CHANNEL = 'parley-workspace';
let boardChannel: BroadcastChannel | null = null;

function postBoardChanged() {
  if (boardChannel) boardChannel.postMessage({ type: 'resource-board-changed' });
}

function onBoardChannelMessage(event: MessageEvent) {
  const value = (event as MessageEvent<{ type?: unknown }>).data;
  if (!value || value.type !== 'resource-board-changed') return;
  if (editorDirty.value) return; // never clobber a live draft
  void refreshBoard().then(() => void loadGraph());
}

function retireSelectedCard() {
  if (!selectedCard.value) return;
  if (!globalThis.confirm(`Retire "${selectedCard.value.title}"? It leaves the board and picker.`)) return;
  void parley.retireResourceCard(selectedCard.value.resource_id).then(() => {
    if (editor.value.open && editor.value.mode === 'edit' && editor.value.card?.id === selectedCard.value?.resource_id) {
      editorDirty.value = false;
      editor.value.open = false;
    }
    selectedId.value = null;
    rootId.value = null;
    inspectedId.value = null;
    status.value = 'Card retired. It is removed from ordinary use.';
    postBoardChanged();
    void refreshBoard().then(() => void loadGraph());
  }).catch((reason: unknown) => {
    error.value = reason instanceof Error ? reason.message : 'Could not retire the card.';
  });
}

const breakpoint = ref<'wide' | 'narrow'>('wide');
const narrowView = ref<'board' | 'connections' | 'details'>('board');

function updateBreakpoint() {
  const media = globalThis.matchMedia;
  breakpoint.value = typeof media === 'function' && media('(max-width: 900px)').matches ? 'narrow' : 'wide';
}

onMounted(() => {
  globalThis.addEventListener('beforeunload', onBeforeUnload);
  globalThis.addEventListener('parley:workspace-changed', onWorkspaceChanged);
  updateBreakpoint();
  const media = globalThis.matchMedia;
  if (typeof media === 'function') {
    const query = media('(max-width: 900px)');
    if (typeof query.addEventListener === 'function') query.addEventListener('change', updateBreakpoint);
  }
  if (typeof BroadcastChannel !== 'undefined') {
    boardChannel = new BroadcastChannel(BOARD_CHANNEL);
    boardChannel.onmessage = onBoardChannelMessage;
  }
  void refreshBoard();
});

watch(() => props.routeId, (id) => {
  if (id && id !== selectedId.value) void selectCard(id);
});

defineExpose({ refresh: refreshBoard });
</script>

<template>
  <section
    class="resources-workspace"
    aria-labelledby="resources-heading"
  >
    <h2
      id="resources-heading"
      class="sr-only"
    >
      Resource board, connections and details
    </h2>

    <nav
      v-if="breakpoint === 'narrow'"
      class="narrow-tabs"
      aria-label="Resources views"
    >
      <button
        type="button"
        :aria-pressed="narrowView === 'board'"
        @click="narrowView = 'board'"
      >
        Board
      </button>
      <button
        type="button"
        :aria-pressed="narrowView === 'connections'"
        @click="narrowView = 'connections'"
      >
        Connections
      </button>
      <button
        type="button"
        :aria-pressed="narrowView === 'details'"
        @click="narrowView = 'details'"
      >
        Details
      </button>
    </nav>

    <!-- Board -->
    <section
      v-show="breakpoint !== 'narrow' || narrowView === 'board'"
      class="board-panel"
      aria-label="Resource board"
    >
      <header class="pane-bar">
        <h3>Your resources</h3>
      </header>
      <div
        class="board"
        @dragover.prevent="onDragOver"
      >
        <div
          v-for="column in snapshot?.columns ?? []"
          :key="column.id"
          class="lane"
          :data-column="column.id"
          @drop="onDrop($event, column.id)"
        >
          <h4>{{ column.title }} <small>{{ cardsByColumn.get(column.id)?.length ?? 0 }}</small></h4>
          <button
            type="button"
            class="add-card"
            @click="openCreate(column.id)"
          >
            + Add card
          </button>
          <ul class="tile-list">
            <li
              v-for="card in cardsByColumn.get(column.id) ?? []"
              :key="card.resource_id"
            >
              <div
                class="tile"
                :class="{ active: card.resource_id === selectedId }"
                :aria-pressed="card.resource_id === selectedId"
              >
                <button
                  type="button"
                  class="tile-body"
                  draggable="true"
                  :data-card-id="card.resource_id"
                  @click="selectCard(card.resource_id)"
                  @dragstart="onDragStart($event, card.resource_id)"
                >
                  <small>{{ card.subtype }}</small>
                  <strong>{{ card.title }}</strong>
                  <span
                    class="tile-counts"
                    :title="`${card.counts.files} files · ${card.counts.connections} connections · ${card.counts.deliveries} deliveries · ${card.counts.proposed} proposed links`"
                  >
                    {{ card.counts.files }} files · {{ card.counts.connections }} linked · {{ card.counts.deliveries }} delivered<template v-if="card.counts.proposed">
                      · {{ card.counts.proposed }} proposed
                    </template>
                  </span>
                </button>
                <div class="tile-actions">
                  <button
                    type="button"
                    @click="openEdit(card.resource_id)"
                  >
                    Edit
                  </button>
                  <select
                    :value="card.placement?.column_id ?? '__off__'"
                    :aria-label="`Move ${card.title} to column`"
                    @change="onColumnChange(card, $event)"
                  >
                    <option value="__off__">
                      Off board
                    </option>
                    <option
                      v-for="moveColumn in snapshot?.columns ?? []"
                      :key="moveColumn.id"
                      :value="moveColumn.id"
                    >
                      {{ moveColumn.title }}
                    </option>
                  </select>
                  <button
                    type="button"
                    @click="moveCard(card, null)"
                  >
                    Remove
                  </button>
                </div>
              </div>
            </li>
          </ul>
        </div>
      </div>
    </section>

    <!-- Graph -->
    <section
      v-show="breakpoint !== 'narrow' || narrowView === 'connections'"
      class="graph-panel"
      aria-label="Connections graph"
    >
      <header class="pane-bar">
        <h3>{{ rootName ? rootName + ' · connections' : 'Resource connections' }}</h3>
        <button
          v-if="rootId !== inspectedId && inspectedId"
          type="button"
          @click="returnToSelected"
        >
          Return to selected card
        </button>
      </header>
      <p
        v-if="graphLoading"
        class="pane-note"
        role="status"
      >
        Loading the current neighborhood…
      </p>
      <p
        v-if="graphError"
        class="pane-note error"
        role="alert"
      >
        {{ graphError }}
      </p>
      <p
        v-if="graph.omitted_nodes"
        class="pane-note"
      >
        Bounded neighborhood: {{ graph.omitted_nodes }} or more nodes omitted.
      </p>
      <div
        ref="canvasFrame"
        class="canvas-frame"
        tabindex="0"
        aria-label="Resource graph canvas"
        data-testid="resource-canvas"
        @keydown.enter="onCanvasEnter"
      >
        <VueFlow
          :nodes="graphNodes"
          :edges="graphEdges"
          fit-view-on-init
          :nodes-draggable="!layoutBusy"
          :nodes-connectable="false"
          elements-selectable
          @node-click="({ node }) => selectNode(String(node.id))"
          @edge-click="({ edge }) => { inspectedEdgeId = String(edge.id); inspectedId = null; }"
          @node-drag-stop="saveNodePosition"
        >
          <Background />
          <Controls />
        </VueFlow>
        <div
          v-if="editor.open && editor.graphMode"
          class="editor-anchor"
          :style="anchor"
        >
          <ResourceEditorForm
            ref="formRef"
            :key="formKey"
            :columns="snapshot?.columns ?? []"
            :mode="editor.mode"
            :card="editor.card"
            :card-summary="editor.summary"
            :initial-column-id="editor.columnId"
            :graph-mode="true"
            :available-endpoints="availableEndpoints"
            @saved="onSaved"
            @cancel="closeEditor"
            @update:dirty="editorDirty = $event"
            @navigate="emit('navigate', $event)"
          />
        </div>
      </div>
      <details
        class="semantic-list"
        open
      >
        <summary>Relationships · {{ graph.edges.length }}</summary>
        <ul aria-label="Semantic relationship list">
          <li
            v-for="edge in graph.edges"
            :key="edge.id"
          >
            <button
              type="button"
              :aria-pressed="inspectedEdgeId === edge.id"
              @click="inspectedEdgeId = edge.id; inspectedId = null"
            >
              {{ graph.nodes.find((node) => node.id === edge.from_id)?.name }} {{ edge.verb }} {{ graph.nodes.find((node) => node.id === edge.to_id)?.name }}
            </button>
          </li>
        </ul>
        <p
          v-if="!graph.edges.length"
          class="pane-note"
        >
          No relationships in this neighborhood yet.
        </p>
      </details>
      <article
        v-if="selectedEdge"
        class="edge-inspector"
      >
        <strong>{{ graph.nodes.find((node) => node.id === selectedEdge?.from_id)?.name }} → {{ selectedEdge?.verb }} → {{ graph.nodes.find((node) => node.id === selectedEdge?.to_id)?.name }}</strong>
        <p>Basis: {{ selectedEdge?.quote || 'owner-authored' }} · {{ selectedEdge?.speaker_kind }} · T{{ selectedEdge?.tier }}</p>
        <div class="edge-actions">
          <button
            type="button"
            @click="selectedEdge && detachEdge(selectedEdge.id)"
          >
            Detach
          </button>
        </div>
      </article>
      <div
        v-if="selectedNode"
        class="node-actions"
      >
        <button
          v-if="selectedNode.kind !== 'resource'"
          type="button"
          @click="openInspectedParty"
        >
          Open in Parties
        </button>
        <button
          v-else
          type="button"
          class="primary"
          @click="openEdit(selectedNode.id, true)"
        >
          Edit card
        </button>
        <button
          type="button"
          @click="exploreConnections(selectedNode.id)"
        >
          Explore connections
        </button>
      </div>
    </section>

    <!-- Inspector -->
    <section
      v-show="breakpoint !== 'narrow' || narrowView === 'details'"
      class="inspector-panel"
      aria-label="Resource inspector"
    >
      <template v-if="selectedCard || selectedNode">
        <header class="pane-bar">
          <h3>{{ selectedCard?.title ?? selectedNode?.name }}</h3>
        </header>
        <dl class="inspector-list">
          <div><dt>Type</dt><dd>{{ selectedCard?.subtype ?? selectedNode?.kind }}</dd></div>
          <div v-if="selectedCard">
            <dt>State</dt><dd>{{ selectedCard.readiness }}</dd>
          </div>
          <div v-if="selectedCard">
            <dt>Files</dt><dd>{{ selectedCard.counts.files }}</dd>
          </div>
          <div v-if="selectedCard">
            <dt>Connections</dt><dd>{{ selectedCard.counts.connections }}</dd>
          </div>
          <div v-if="selectedCard">
            <dt>Deliveries</dt><dd>{{ selectedCard.counts.deliveries }}</dd>
          </div>
        </dl>
        <div
          v-if="selectedCard && selectedCard.gaps.length"
          class="eligibility"
        >
          <strong>{{ selectedCard.placement?.column_id === 'available' ? 'Available, but still ineligible' : 'Incomplete resource' }}</strong>
          <p>Missing typed fields: {{ selectedCard.gaps.join(', ') }}. Labels or prose cannot satisfy them.</p>
        </div>
        <div
          v-if="selectedCard && !selectedCard.gaps.length"
          class="eligibility eligible"
        >
          <strong>Eligible</strong>
          <p>All typed fields for this subtype are present.</p>
        </div>
        <div
          v-if="selectedCard && selectedCard.counts.deliveries > 0"
          class="deliveries"
        >
          <h4>Deliveries</h4>
          <p
            v-if="deliveriesLoading"
            class="pane-note"
          >
            Loading delivery records…
          </p>
          <ul
            v-else
            aria-label="Delivery records"
          >
            <li
              v-for="delivery in deliveries"
              :key="delivery.send_id"
            >
              <span>{{ delivery.sent_at.slice(0, 10) }} · {{ delivery.recipient_name }} · {{ delivery.opportunity_title }}</span>
              <small>{{ delivery.material_kind }} · v{{ delivery.material_version }} · {{ delivery.material_id.slice(0, 8) }}</small>
              <button
                type="button"
                @click="emit('navigate', `/output/${encodeURIComponent(delivery.material_id)}`)"
              >
                Open material
              </button>
            </li>
          </ul>
          <button
            type="button"
            class="primary"
            @click="emit('navigate', '/output')"
          >
            Record delivery…
          </button>
        </div>
        <div class="inspector-actions">
          <button
            v-if="selectedCard"
            type="button"
            class="primary"
            @click="openEdit(selectedCard.resource_id)"
          >
            Edit card
          </button>
          <button
            v-if="selectedNode"
            type="button"
            @click="exploreConnections(selectedNode.id)"
          >
            Explore connections
          </button>
          <button
            v-if="selectedNode && selectedNode.kind !== 'resource'"
            type="button"
            @click="openInspectedParty"
          >
            Open in Parties
          </button>
          <button
            v-if="selectedCard"
            type="button"
            @click="moveCard(selectedCard, null)"
          >
            Remove from board
          </button>
          <button
            v-if="selectedCard"
            type="button"
            class="retire"
            @click="retireSelectedCard"
          >
            Retire card
          </button>
        </div>
      </template>
      <p
        v-if="!selectedCard && !selectedNode"
        class="pane-note"
      >
        Select a board card to inspect it.
      </p>
      <details
        class="picker"
        open
      >
        <summary>All resources · pick or place</summary>
        <ul aria-label="Resource picker">
          <li
            v-for="card in offBoardCards"
            :key="card.resource_id"
          >
            <button
              type="button"
              @click="selectCard(card.resource_id)"
            >
              {{ card.title }} · {{ card.subtype }} · {{ card.legacy ? 'legacy (off board)' : 'off board' }}
            </button>
            <select
              :aria-label="`Place ${card.title} on board`"
              @change="placeOffBoard(card, $event)"
            >
              <option value="">
                Place on board…
              </option>
              <option
                v-for="column in snapshot?.columns ?? []"
                :key="column.id"
                :value="column.id"
              >
                {{ column.title }}
              </option>
            </select>
          </li>
          <li
            v-if="!offBoardCards.length"
            class="pane-note"
          >
            No off-board resources right now.
          </li>
        </ul>
      </details>
    </section>

    <!-- Board Edit opens the shared content form over the board (spec §1). -->
    <dialog
      ref="boardDialog"
      class="board-editor-dialog"
      aria-label="Resource card editor"
      @cancel="onDialogCancel"
    >
      <ResourceEditorForm
        v-if="editor.open && !editor.graphMode"
        ref="formRef"
        :key="formKey"
        :columns="snapshot?.columns ?? []"
        :mode="editor.mode"
        :card="editor.card"
        :card-summary="editor.summary"
        :initial-column-id="editor.columnId"
        :graph-mode="false"
        :available-endpoints="availableEndpoints"
        @saved="onSaved"
        @cancel="closeEditor"
        @update:dirty="editorDirty = $event"
        @navigate="emit('navigate', $event)"
      />
    </dialog>

    <!-- Dirty draft switch: Save / Discard / Stay, per spec §4. -->
    <div
      v-if="pendingSwitch"
      class="draft-prompt"
      role="alertdialog"
      aria-label="Unsaved card draft"
    >
      <strong>Unsaved card draft</strong>
      <button
        type="button"
        @click="resolveEditorSwitch(true)"
      >
        Save
      </button>
      <button
        type="button"
        @click="resolveEditorSwitch(false)"
      >
        Discard unsaved changes
      </button>
      <button
        type="button"
        @click="stayOnEditor"
      >
        Stay
      </button>
    </div>

    <p
      class="workspace-status"
      role="status"
    >
      {{ loading ? 'Loading the board…' : status }}
    </p>
    <p
      v-if="error"
      class="workspace-status error"
      role="alert"
    >
      {{ error }}
    </p>
  </section>
</template>

<style scoped>
.resources-workspace { display: grid; grid-template-columns: minmax(17rem, 1fr) minmax(0, 2fr) minmax(15rem, 1fr); gap: .85rem; align-items: start; }
.resources-workspace > section, .resources-workspace .editor-host { background: white; border: 1px solid #ccd5cc; border-radius: .55rem; min-width: 0; }
.board-panel, .graph-panel, .inspector-panel { display: flex; flex-direction: column; }
.board { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: .5rem; padding: .6rem; background: #f7f8f1; }
.lane { background: #eef1e7; border: 1px solid #d8dfd2; border-radius: .5rem; padding: .5rem; min-width: 0; }
.lane h4 { margin: .2rem 0 .4rem; font-size: .95rem; }
.lane h4 small { color: #58665f; }
.add-card { border: 0; background: transparent; color: #2f5d49; font-weight: 600; padding: .2rem .3rem; }
.tile-list { list-style: none; padding: 0; margin: .3rem 0 0; }
.tile { border: 1px solid #c6cec1; border-top: 3px solid #ad9671; border-radius: .4rem; background: #fffefa; margin-bottom: .45rem; }
.tile.active { border-color: #30654d; outline: 1px solid #30654d; background: #f8fcf5; }
.tile-body { display: grid; gap: .15rem; width: 100%; text-align: left; border: 0; background: transparent; padding: .5rem; }
.tile-body strong { font-family: Georgia, serif; font-size: 1.05rem; }
.tile-counts { font-size: .78rem; color: #58665f; }
.tile-actions { display: flex; gap: .3rem; align-items: center; padding: 0 .45rem .45rem; flex-wrap: wrap; }
.tile-actions select { width: auto; }
.pane-bar { display: flex; align-items: center; justify-content: space-between; gap: .5rem; padding: .6rem .9rem; border-bottom: 1px solid #dce2db; }
.pane-bar h3 { margin: 0; font-size: 1rem; }
.pane-note { padding: 0 .9rem; color: #58665f; font-size: .85rem; }
.pane-note.error { color: #852521; }
.canvas-frame { height: clamp(22rem, 48vh, 38rem); border-block: 1px solid #d4dcd4; background: #eff3ed; }
.semantic-list { border-top: 1px solid #dce2db; padding: .4rem .9rem; }
.semantic-list ul { list-style: none; padding: 0; max-height: 10rem; overflow: auto; }
.semantic-list li button { width: 100%; text-align: left; border: 0; border-bottom: 1px solid #eef1ec; border-radius: 0; background: transparent; }
.edge-inspector { border-top: 1px solid #dce2db; padding: .6rem .9rem; }
.edge-actions, .node-actions, .inspector-actions { display: flex; gap: .4rem; flex-wrap: wrap; padding: .4rem .9rem .7rem; }
.node-actions { border-top: 1px solid #dce2db; }
.editor-host { margin: .7rem .9rem .9rem; }
.inspector-list { margin: .6rem .9rem; display: grid; gap: .3rem; }
.inspector-list > div { display: flex; justify-content: space-between; gap: 1rem; border-bottom: 1px solid #eef1ec; padding: .25rem 0; }
.inspector-list dt { color: #58665f; }
.inspector-list dd { margin: 0; font-weight: 600; }
.eligibility { margin: .5rem .9rem; padding: .5rem .7rem; background: #fdf1ef; border: 1px solid #e2b7b0; border-radius: .35rem; }
button.retire { color: #852521; border-color: #d9a49c; }
.eligibility.eligible { background: #eef5ef; border-color: #bcd6c2; }
.eligibility p { margin: .2rem 0 0; font-size: .85rem; }
.deliveries { border-top: 1px solid #dce2db; padding: .5rem .9rem; }
.deliveries ul { list-style: none; padding: 0; max-height: 10rem; overflow: auto; }
.deliveries li { display: grid; grid-template-columns: 1fr auto; gap: .15rem .5rem; align-items: center; border-bottom: 1px solid #eef1ec; padding: .3rem 0; }
.deliveries li small { grid-column: 1 / -1; }
.deliveries li button { justify-self: end; }
.picker { border-top: 1px solid #dce2db; padding: .5rem .9rem; }
.picker ul { list-style: none; padding: 0; max-height: 12rem; overflow: auto; }
.picker li { display: flex; gap: .4rem; align-items: center; justify-content: space-between; border-bottom: 1px solid #eef1ec; padding: .3rem 0; }
.narrow-tabs { display: flex; gap: .4rem; }
.workspace-status { grid-column: 1 / -1; margin: 0; color: #58665f; }
.workspace-status.error { color: #852521; }
.sr-only { position: absolute; clip-path: inset(50%); height: 1px; width: 1px; overflow: hidden; white-space: nowrap; }
/* Party neighbours and the graph root stay visually distinct (prototype palette). */
.resources-workspace :deep(.vue-flow__node.party-node) { background: #dbe9eb; }
.resources-workspace :deep(.vue-flow__node.root-node) { background: #f2e6cf; border: 2px solid #846e48; }
.editor-anchor { position: absolute; z-index: 5; inset-inline-start: .6rem; width: min(420px, calc(100% - 12px)); max-height: calc(100% - 1.5rem); overflow: auto; border: 2px solid #436b55; border-radius: 8px; background: #fffdf7; box-shadow: 0 5px 24px #223c3026; }
.board-editor-dialog { width: min(36rem, 94vw); max-height: 88dvh; color: #253e32; background: #fffef7; border: 1px solid #abbcac; border-radius: .55rem; padding: .9rem; }
.board-editor-dialog::backdrop { background: #172b2260; }
.draft-prompt { position: fixed; z-index: 15; top: 15%; left: 50%; transform: translateX(-50%); display: flex; align-items: center; gap: .6rem; padding: 1rem; background: #fffdf7; border: 2px solid #526f58; border-radius: 8px; box-shadow: 0 10px 40px #213c3030; flex-wrap: wrap; }
@media (max-width: 1100px) { .resources-workspace { grid-template-columns: minmax(15rem, 1fr) minmax(0, 1.8fr); } .inspector-panel { grid-column: 1 / -1; } }
@media (max-width: 900px) { .resources-workspace { grid-template-columns: 1fr; } .inspector-panel { grid-column: auto; } .board { grid-template-columns: repeat(3, minmax(9rem, 1fr)); overflow-x: auto; } }
</style>
