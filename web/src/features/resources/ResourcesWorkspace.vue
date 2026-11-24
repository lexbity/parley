<script setup lang="ts">
/* global DragEvent, Event, HTMLElement, HTMLDialogElement, HTMLInputElement, HTMLSelectElement, MediaQueryList, BeforeUnloadEvent, BroadcastChannel, Element, MessageEvent */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { ApiError, newOperationKey, parley, type DeliveryRow, type EntityAttachment, type EntityCard, type GraphSnapshot, type PlacementView, type ResourceBoardSnapshot, type ResourceColumn } from '../../api/client';
import { commands } from '../../api/commands';
import { registerScoped, resourceMarkReadyAction, useCommandRegistry, type UiCommand } from '../../app/commands/registry';
import { kindLabel } from '../parties/relations';
import MarkdownPreview from '../../shared/MarkdownPreview.vue';
import ResourceEditorForm from './ResourceEditorForm.vue';
import ResourceGraph, { type PendingRelationship } from './ResourceGraph.vue';

const props = defineProps<{ routeId?: string | null; active?: boolean }>();
const emit = defineEmits<{ navigate: [path: string]; changed: [] }>();

type CardSummary = ResourceBoardSnapshot['cards'][number];
type GraphNode = GraphSnapshot['nodes'][number];
type GraphEdge = GraphSnapshot['edges'][number];
type View = 'board' | 'connections' | 'details';
type Editor = { mode: 'create'; column: string; title: string } | { mode: 'edit'; card: EntityCard; graph: boolean; summary: CardSummary | null };
const emptyGraph = (): GraphSnapshot => ({ nodes: [], edges: [], omitted_nodes: 0, omitted_edges: 0 });
const snapshot = ref<ResourceBoardSnapshot | null>(null);
const graph = ref(emptyGraph());
const inspectorGraph = ref<GraphSnapshot | null>(null);
const selectedId = ref<string | null>(null);
const rootId = ref<string | null>(null);
const inspectedId = ref<string | null>(null);
const edgeId = ref<string | null>(null);
const narrow = ref(false);
const view = ref<View>('board');
const pickerOpen = ref(false);
const query = ref('');
const graphFilter = ref<'all' | 'resources' | 'parties' | 'activity'>('all');
const boardLoading = ref(false);
const graphLoading = ref(false);
const detailsLoading = ref(false);
const boardError = ref('');
const graphError = ref('');
const detailsError = ref('');
const error = ref('');
const status = ref('');
const inspectedCard = ref<EntityCard | null>(null);
const attachments = ref<EntityAttachment[]>([]);
const deliveries = ref<DeliveryRow[]>([]);
const editor = ref<Editor | null>(null);
const editorKey = ref(0);
const pendingRelationship = ref<PendingRelationship | null>(null);
const editorDirty = ref(false);
const editorLoading = ref(false);
const pendingSwitch = ref<(() => void) | null>(null);
const form = ref<InstanceType<typeof ResourceEditorForm> | null>(null);
const confirmDialog = ref<HTMLDialogElement | null>(null);
const editorDialog = ref<HTMLDialogElement | null>(null);
const pickerDialog = ref<HTMLDialogElement | null>(null);
const pickerSearch = ref<HTMLElement | null>(null);
const detailsHeading = ref<HTMLElement | null>(null);
const boardHeading = ref<HTMLElement | null>(null);
const layoutBusy = ref(false);
const moveBusy = ref(false);
const boardWriteBusy = ref(false);
const composer = ref<{ kind: 'list' } | null>(null);
const composerTitle = ref('');
const displacedCardTitle = ref('');
const composerError = ref('');
const composerInput = ref<HTMLElement | null>(null);
const addList = ref<HTMLElement | null>(null);
const cardDrafts = ref<Record<string, string>>({});
const cardErrors = ref<Record<string, string>>({});
const collapsedLists = ref<string[]>([]);
const laneMenu = ref('');
const renamingColumnId = ref('');
const columnRenameDraft = ref('');
const renameInput = ref<HTMLInputElement | null>(null);
const titleEditId = ref('');
const titleDraft = ref('');
const titleError = ref('');
const titleBusy = ref(false);
const titleInput = ref<HTMLInputElement | null>(null);
const systemRemovalDialog = ref<HTMLDialogElement | null>(null);
const systemRemovalTarget = ref<ResourceColumn | null>(null);
const systemRemovalTyped = ref('');
const systemRemovalError = ref('');
const systemRemovalInput = ref<HTMLInputElement | null>(null);
let systemRemovalTrigger: HTMLElement | null = null;
let titleBase: EntityCard | null = null;
let cardIntent: { signature: string; key: string } | null = null;
let renameIntent: { signature: string; key: string } | null = null;
let eraseIntent: { signature: string; key: string } | null = null;
let composerTrigger: HTMLElement | null = null;
let columnRenameTrigger: HTMLElement | null = null;
let titleEditTrigger: HTMLElement | null = null;
let composerIntent: { signature: string; key: string } | null = null;
let removalIntent: { signature: string; key: string } | null = null;
let retirementIntent: { signature: string; key: string } | null = null;
const draggedId = ref('');
const dropTarget = ref('');
const partyEndpoints = ref<Array<{ id: string; label: string; kind: string; revision?: number }>>([]);
const relationshipToReveal = ref<string | null>(null);
const edgeCorrection = ref<{ verb: string; basis: string; reverse: boolean } | null>(null);
const edgeBusy = ref(false);
const graphRemovalDialog = ref<HTMLDialogElement | null>(null);
const graphRemovalTarget = ref<string | null>(null);
const graphRemovalEdge = ref('');
let graphRemovalTrigger: HTMLElement | null = null;
const versionDialog = ref<HTMLDialogElement | null>(null);
const sentVersion = ref<Awaited<ReturnType<typeof parley.getDeliveredResourceVersion>> | null>(null);
const versionLoading = ref(false);
const versionError = ref('');
const sentVersionNumber = ref(0);
const filtersOpen = ref(false);
let invocation: HTMLElement | null = null;
let promptInvocation: HTMLElement | null = null;
let pickerInvocation: HTMLElement | null = null;
let boardGeneration = 0;
let graphGeneration = 0;
let detailGeneration = 0;
let editorGeneration = 0;
let partyGeneration = 0;
let media: MediaQueryList | null = null;
let channel: BroadcastChannel | null = null;
let channelEpoch: string | undefined;
let pendingRefresh = false;
let initialBoardScroll = 0;
let eventCursor: string | null = null;
let eventPolling = false;
let eventTimer: ReturnType<typeof globalThis.setInterval> | undefined;

const cardsByColumn = computed(() => {
  const columns = new Map(snapshot.value?.columns.map(column => [column.id, [] as CardSummary[]]));
  for (const card of snapshot.value?.cards ?? []) {
    if (card.placement) columns.get(card.placement.column_id)?.push(card);
  }
  return columns;
});
const allResources = computed<CardSummary[]>(() => [
  ...(snapshot.value?.cards ?? []),
  ...(snapshot.value?.off_board ?? []).map(card => ({ ...card, placement: null })),
]);
const resourceById = computed(() => new Map(allResources.value.map(card => [card.resource_id, card])));
const nodeById = computed(() => new Map(graph.value.nodes.map(node => [node.id, node])));
const rootName = computed(() => nodeById.value.get(rootId.value ?? '')?.name ?? resourceById.value.get(rootId.value ?? '')?.title ?? '');
const selectedNode = computed(() => nodeById.value.get(inspectedId.value ?? ''));
const selectedSummary = computed(() => resourceById.value.get(inspectedId.value ?? ''));
const selectedEdge = computed(() => graph.value.edges.find(edge => edge.id === edgeId.value) ?? inspectorGraph.value?.edges.find(edge => edge.id === edgeId.value));
function removableGraphEdges(id: string) {
  const root = rootId.value;
  return graph.value.edges.filter(edge => edge.owner_authored && !edge.record
    && ((edge.from_id === root && edge.to_id === id) || (edge.to_id === root && edge.from_id === id)));
}
const graphRemovalChoices = computed(() => graphRemovalTarget.value ? removableGraphEdges(graphRemovalTarget.value) : []);
const filteredResources = computed(() => {
  const search = query.value.trim().toLocaleLowerCase();
  return allResources.value.filter(card => `${card.title} ${resourceLabel(card.subtype)}`.toLocaleLowerCase().includes(search));
});
const endpointSearchError = ref('');
const endpoints = computed(() => {
  const nodes = new Map(graph.value.nodes.map(node => [node.id, { id: node.id, label: node.name, kind: kindLabel(node.kind), revision: node.revision }]));
  const resources = allResources.value.map(card => ({
    id: card.resource_id, label: card.title, kind: resourceLabel(card.subtype), revision: nodes.get(card.resource_id)?.revision,
  }));
  const people = [...nodes.values()].filter(node => isParty(nodeById.value.get(node.id)));
  return [...new Map([...resources, ...people, ...partyEndpoints.value].map(item => [item.id, item])).values()];
});
const activityNodeIds = computed(() => new Set(graph.value.edges.filter(edge => edge.record).flatMap(edge => [edge.from_id, edge.to_id])));
const visibleNodes = computed(() => graph.value.nodes.filter(node => node.id === rootId.value || graphFilter.value === 'all'
  || (graphFilter.value === 'resources' && isResource(node)) || (graphFilter.value === 'parties' && isParty(node))
  || (graphFilter.value === 'activity' && activityNodeIds.value.has(node.id))));
const visibleNodeIds = computed(() => new Set(visibleNodes.value.map(node => node.id)));
const visibleEdges = computed(() => graph.value.edges.filter(edge => visibleNodeIds.value.has(edge.from_id) && visibleNodeIds.value.has(edge.to_id) && (graphFilter.value !== 'activity' || edge.record)));
const boardViewport = ref<HTMLElement | null>(null);
const position = computed(() => selectedSummary.value?.placement ?? null);
const selectedIsFiltered = computed(() => Boolean(selectedNode.value && !visibleNodeIds.value.has(selectedNode.value.id)));
const columnTitle = computed(() => snapshot.value?.columns.find(column => column.id === position.value?.column_id)?.title ?? 'Off board');
const inColumn = computed(() => cardsByColumn.value.get(position.value?.column_id ?? '') ?? []);
const orderIndex = computed(() => inColumn.value.findIndex(card => card.resource_id === inspectedId.value));
const graphEditor = computed(() => editor.value?.mode === 'edit' && editor.value.graph);
const inspectedName = computed(() => inspectedCard.value?.title ?? selectedNode.value?.name ?? selectedSummary.value?.title ?? '');
const inspectedType = computed(() => inspectedCard.value?.subtype ?? selectedSummary.value?.subtype ?? selectedNode.value?.kind ?? '');
const relatedEdges = computed(() => (inspectorGraph.value?.edges ?? graph.value.edges).filter(edge => !edge.record?.send_id && ( edge.from_id === inspectedId.value || edge.to_id === inspectedId.value)));

function resourceLabel(subtype: string) {
  return ({ gift: 'Offer', proof: 'Proof', skill: 'Skill', artifact: 'Artifact', interest: 'Interest', note: 'Note' } as Record<string, string>)[subtype] ?? kindLabel(subtype);
}
function isParty(node: GraphNode | undefined) {
  return node?.kind === 'person' || node?.kind === 'org' || node?.kind === 'project';
}
function isResource(node: GraphNode | undefined) {
  return node?.kind === 'resource' || Boolean(node && resourceById.value.has(node.id));
}
function nameOf(id: string) { return nodeById.value.get(id)?.name ?? inspectorGraph.value?.nodes.find(node => node.id === id)?.name ?? resourceById.value.get(id)?.title ?? 'Unavailable card'; }
function message(reason: unknown, fallback: string) { return reason instanceof Error ? reason.message : fallback; }
function focused() {
  const target = globalThis.document.activeElement;
  return target instanceof HTMLElement ? target : null;
}
function storageKey() {
  return parley.workspaceEpoch && `parley.resources.${parley.workspaceId}.${parley.workspaceEpoch}`;
}
function remember() {
  const key = storageKey();
  if (!key || props.active === false) return;
  try {
    globalThis.sessionStorage.setItem(key, JSON.stringify({
      selected: selectedId.value, root: rootId.value, inspected: inspectedId.value, edge: edgeId.value, view: view.value, filter: graphFilter.value, collapsed: collapsedLists.value, scrollTop: boardViewport.value?.scrollTop ?? 0,
    }));
  } catch { /* View state remains in memory. */ }
}
function restore() {
  const key = storageKey();
  if (!key || props.routeId) return;
  try {
    const data = JSON.parse(globalThis.sessionStorage.getItem(key) ?? '{}');
    selectedId.value = typeof data.selected === 'string' ? data.selected : null;
    rootId.value = typeof data.root === 'string' ? data.root : null;
    inspectedId.value = typeof data.inspected === 'string' ? data.inspected : null;
    edgeId.value = typeof data.edge === 'string' ? data.edge : null;
    if (['board', 'connections', 'details'].includes(data.view)) view.value = data.view;
    if (['all', 'resources', 'parties', 'activity'].includes(data.filter)) graphFilter.value = data.filter;
    if (Array.isArray(data.collapsed)) collapsedLists.value = data.collapsed.filter((id: unknown) => typeof id === 'string');
    if (typeof data.scrollTop === 'number') initialBoardScroll = data.scrollTop;
  } catch { /* Use the first visible card. */ }
}

async function refreshBoard() {
  const own = ++boardGeneration;
  boardLoading.value = true;
  boardError.value = '';
  try {
    const next = await parley.getResourceBoard({ includeOffboard: true });
    if (own !== boardGeneration) return;
    const cards = new Map(next.cards.map(card => [card.resource_id, card]));
    if (!snapshot.value) snapshot.value = next;
    let cursor = next.next_cursor;
    const visited = new Set<string>();
    while (cursor) {
      if (visited.has(cursor)) throw new Error('The resource board returned the same page twice.');
      visited.add(cursor);
      const page = await parley.getResourceBoard({ cursor });
      if (own !== boardGeneration) return;
      if (page.columns.length !== next.columns.length || page.columns.some((column, index) => {
        const first = next.columns[index];
        return first?.id !== column.id || first.revision !== column.revision;
      })) {
        throw new Error('The board changed while loading. Retry to get the latest order.');
      }
      for (const card of page.cards) cards.set(card.resource_id, card);
      cursor = page.next_cursor;
    }
    snapshot.value = { ...next, cards: [...cards.values()] };
    if (!boardWriteBusy.value) retainDisplacedDrafts(next.columns.map(column => column.id));
    eventCursor = String(next.event_cursor);
    if (initialBoardScroll) {
      await nextTick();
      if (boardViewport.value) boardViewport.value.scrollTop = initialBoardScroll;
      initialBoardScroll = 0;
    }
    if (!rootId.value && next.cards[0]) setBoardSelection(next.cards[0].resource_id, false);
  } catch (reason) {
    if (own === boardGeneration) boardError.value = message(reason, 'Could not load the resource board.');
  } finally {
    if (own === boardGeneration) boardLoading.value = false;
  }
}

async function loadGraph() {
  const own = ++graphGeneration;
  const id = rootId.value;
  graphError.value = '';
  graphLoading.value = Boolean(id);
  if (!id) { graph.value = emptyGraph(); return; }
  try {
    const next = await parley.getGraph({ center_id: id, view_scope: 'resources', view_id: id });
    if (own !== graphGeneration) return;
    graph.value = next;
    if (relationshipToReveal.value) {
      const link = next.edges.find(edge => edge.id === relationshipToReveal.value);
      relationshipToReveal.value = null;
      if (link) {
        edgeId.value = link.id;
        view.value = 'details';
      }
    }
    if (edgeId.value && !inspectorGraph.value?.edges.some(edge => edge.id === edgeId.value) && !next.edges.some(edge => edge.id === edgeId.value)) {
      edgeId.value = null;
      inspectedId.value = id;
    }
  } catch (reason) {
    if (own === graphGeneration) graphError.value = message(reason, 'Could not load connections.');
  } finally {
    if (own === graphGeneration) graphLoading.value = false;
  }
}

async function loadDetails() {
  const own = ++detailGeneration;
  const id = inspectedId.value;
  const node = selectedNode.value;
  const summary = selectedSummary.value;
  const resource = Boolean(summary || isResource(node) || id && !node && (id === props.routeId || id === selectedId.value));
  detailsError.value = '';
  detailsLoading.value = Boolean(id && (resource || isParty(node)));
  if (!id || !detailsLoading.value) return;
  const [card, files, history, neighbors] = await Promise.allSettled([
    resource ? parley.getEntityCard(id) : parley.getEntityNotes(id).then(notes => ({ notes })),
    !summary || summary.counts.files ? parley.listAttachments(id) : Promise.resolve([]),
    resource && (!summary || summary.counts.deliveries) ? parley.getEntityCardDeliveries(id) : Promise.resolve([]),
    id !== rootId.value ? parley.getGraph({ center_id: id, view_scope: 'resources', view_id: id }) : Promise.resolve(null),
  ]);
  if (own !== detailGeneration) return;
  if (card.status === 'fulfilled') {
    if ('id' in card.value) inspectedCard.value = card.value;
    else partyNotes.value = card.value.notes.markdown;
  }
  if (files.status === 'fulfilled') attachments.value = files.value;
  if (history.status === 'fulfilled') deliveries.value = history.value;
  if (neighbors.status === 'fulfilled') inspectorGraph.value = neighbors.value;
  const failure = [card, files, history, neighbors].find(result => result.status === 'rejected');
  if (failure?.status === 'rejected') {
    const reason = failure.reason;
    detailsError.value = reason instanceof ApiError && reason.status === 404
      ? ({ RESOURCE_RETIRED: 'This resource was retired.', RESOURCE_ERASED: 'This resource was erased.' }[reason.code] ?? 'This record or its details are unavailable.')
      : message(reason, 'Could not load all card details.');
  }
  detailsLoading.value = false;
}
const partyNotes = ref('');
watch([inspectedId, () => selectedNode.value?.kind, () => selectedSummary.value?.etag], () => {
  inspectedCard.value = null;
  inspectorGraph.value = null;
  partyNotes.value = '';
  attachments.value = [];
  deliveries.value = [];
  void loadDetails();
});
watch(rootId, () => { graph.value = emptyGraph(); void loadGraph(); });
watch([rootId, selectedId, inspectedId, edgeId, view, graphFilter], remember);

function setBoardSelection(id: string, showDetails = true) {
  selectedId.value = id;
  rootId.value = id;
  inspectedId.value = id;
  edgeId.value = null;
  if (showDetails && narrow.value) {
    view.value = 'details';
    void nextTick(() => detailsHeading.value?.focus());
  }
}
function chooseBoard(id: string) {
  if (id === selectedId.value) {
    if (narrow.value) { changeView('details'); void nextTick(() => detailsHeading.value?.focus()); }
    return;
  }
  requestSwitch(() => {
    setBoardSelection(id);
    emit('navigate', `/resources/${encodeURIComponent(id)}`);
  });
}
function chooseResource(id: string) {
  requestSwitch(() => {
    closePicker();
    if (resourceById.value.get(id)?.placement) setBoardSelection(id);
    else {
      rootId.value = id;
      inspectedId.value = id;
      edgeId.value = null;
      if (narrow.value) view.value = 'details';
    }
    emit('navigate', `/resources/${encodeURIComponent(id)}`);
    void nextTick(() => detailsHeading.value?.focus());
  });
}
function explore(id: string) {
  requestSwitch(() => {
    rootId.value = id;
    inspectedId.value = id;
    edgeId.value = null;
    view.value = 'connections';
  });
}
function inspectEdge(id: string) {
  requestSwitch(() => {
    edgeId.value = id;
    edgeCorrection.value = null;
    view.value = 'details';
    void nextTick(() => detailsHeading.value?.focus());
  });
}
function activateNode(id: string) {
  const node = nodeById.value.get(id);
  if (!node) {
    const neighbor = inspectorGraph.value?.nodes.find(item => item.id === id);
    if (neighbor) explore(neighbor.id);
    return;
  }
  if (isResource(node)) { openEdit(id, true); return; }
  requestSwitch(() => {
    inspectedId.value = id;
    edgeId.value = null;
    view.value = 'details';
    void nextTick(() => detailsHeading.value?.focus());
  });
}
function changeView(target: View) {
  view.value = target;
}
function dismissEditor(restoreFocus = true) {
  editorGeneration += 1;
  editorLoading.value = false;
  editor.value = null;
  pendingRelationship.value = null;
  editorDirty.value = false;
  if (editorDialog.value?.open) editorDialog.value.close();
  if (restoreFocus) void nextTick(() => restoreInvocation(invocation));
  if (pendingRefresh) { pendingRefresh = false; void refresh(); }
}
function restoreInvocation(target: HTMLElement | null) {
  if (target?.isConnected && target.getClientRects().length) target.focus();
  else (view.value === 'details' ? detailsHeading.value : boardHeading.value)?.focus();
}
function requestSwitch(action: () => void) {
  if (form.value?.saving || pendingSwitch.value) return;
  if (editorDirty.value) {
    promptInvocation = focused();
    pendingSwitch.value = action;
    void nextTick(() => { if (confirmDialog.value?.isConnected) confirmDialog.value.showModal(); });
    return;
  }
  dismissEditor(false);
  action();
}
function finishSwitch() {
  const action = pendingSwitch.value;
  pendingSwitch.value = null;
  if (confirmDialog.value?.open) confirmDialog.value.close();
  dismissEditor(!action);
  action?.();
}
async function saveAndSwitch() {
  if (!await form.value?.save()) stay(true);
}
function stay(showErrors = false) {
  pendingSwitch.value = null;
  if (confirmDialog.value?.open) confirmDialog.value.close();
  if (showErrors && graphEditor.value) view.value = 'connections';
  void nextTick(() => showErrors ? form.value?.focusTitle() : restoreInvocation(promptInvocation));
}
function closeEditor() { requestSwitch(() => restoreInvocation(invocation)); }
function openCreate(column: string, draft = '') {
  const trigger = composerTrigger;
  const title = draft.trim();

  requestSwitch(() => {
    invocation = trigger;
    editorKey.value += 1;
    editor.value = { mode: 'create', column, title };
    composer.value = null;
    composerTitle.value = '';
    composerError.value = '';
    composerIntent = null;
    void showModalEditor();
  });
}
async function showModalEditor() {
  await nextTick();
  if (editorDialog.value?.isConnected && !editorDialog.value.open) editorDialog.value.showModal();
  form.value?.focusTitle();
}
function openEdit(id: string, inGraph = false, addRelationship = false) {
  if (editor.value?.mode === 'edit' && editor.value.card.id === id && editor.value.graph === inGraph) {
    if (addRelationship) form.value?.focusRelationship();
    else form.value?.focusTitle();
    return;
  }
  const trigger = focused();
  requestSwitch(() => {
    const summary = resourceById.value.get(id) ?? null;
    if (!inGraph && summary?.placement) setBoardSelection(id, false);
    inspectedId.value = id;
    edgeId.value = null;
    if (inGraph) view.value = 'connections';
    invocation = trigger;
    const own = ++editorGeneration;
    editorLoading.value = true;
    error.value = '';
    void parley.getEntityCard(id).then(card => {
      if (own !== editorGeneration) return;
      editorKey.value += 1;
      editor.value = { mode: 'edit', card, graph: inGraph, summary };
      if (inGraph) void nextTick(() => addRelationship ? form.value?.focusRelationship() : form.value?.focusTitle());
      else void showModalEditor();
    }).catch(reason => {
      if (own === editorGeneration) error.value = message(reason, 'Could not open the card.');
    }).finally(() => {
      if (own === editorGeneration) editorLoading.value = false;
    });
  });
}
function onSaved(result: { entity: EntityCard; placement: PlacementView | null; createdRelated: { edge_id: string; to_id: string } | null }) {
  const create = editor.value?.mode === 'create';
  const draftSwitch = Boolean(pendingSwitch.value);
  if (create) setBoardSelection(result.entity.id, !draftSwitch);
  else if (result.createdRelated) relationshipToReveal.value = result.createdRelated.edge_id;
  if (draftSwitch) finishSwitch();
  else dismissEditor();
  status.value = result.createdRelated ? 'Card and relationship saved.' : 'Card saved.';
  notify();
  void refresh().then(() => {
    if (create && !draftSwitch && narrow.value && view.value === 'details' && inspectedId.value === result.entity.id) {
      void nextTick(() => detailsHeading.value?.focus());
    }
  });
}
async function refresh() {
  const priorRoot = rootId.value;
  await refreshBoard();
  if (priorRoot === rootId.value) await loadGraph();
  await loadDetails();
}
function notify() {
  channel?.postMessage({ type: 'resource-board-changed', epoch: parley.workspaceEpoch });
  emit('changed');
}
async function pollChanges() {
  if (props.active === false || globalThis.document.visibilityState !== 'visible' || eventPolling || !eventCursor || boardLoading.value) return;
  eventPolling = true;
  try {
    const page = await parley.getEvents(eventCursor);
    eventCursor = page.next_cursor;
    const changed = page.items.some(item =>
      ['resource', 'resource_column', 'entity', 'entity_attachment', 'edge', 'claim', 'send', 'material', 'opportunity'].includes(item.entity_type));
    if (!changed) return;
    if (editorDirty.value) pendingRefresh = true;
    else await refresh();
  } catch (reason) {
    if (reason instanceof ApiError && [409, 410].includes(reason.status)) {
      if (editorDirty.value) pendingRefresh = true;
      else await refresh();
    }
  } finally { eventPolling = false; }
}

function onBroadcast(event: MessageEvent) {
  if (event.data?.type !== 'resource-board-changed' || event.data.epoch !== parley.workspaceEpoch) return;
  if (editorDirty.value) pendingRefresh = true;
  else void refresh();
}

function openPicker() {
  pickerInvocation = focused();
  pickerOpen.value = true;
  void nextTick(() => {
    if (pickerDialog.value?.isConnected) pickerDialog.value.showModal();
    pickerSearch.value?.focus();
  });
}
function closePicker() {
  pickerOpen.value = false;
  if (pickerDialog.value?.open) pickerDialog.value.close();
  void nextTick(() => restoreInvocation(pickerInvocation));
}

function startComposer(next: NonNullable<typeof composer.value>, event: Event) {
  const trigger = event.currentTarget instanceof HTMLElement ? event.currentTarget : focused();
  requestSwitch(() => {
    composerTrigger = trigger;
    composer.value = next;
    composerTitle.value = '';
    composerError.value = '';
    composerIntent = null;
    focusComposer();
  });
}
function setRenameInput(element: unknown) {
  renameInput.value = element instanceof HTMLInputElement ? element : null;
}
function setTitleInput(element: unknown) {
  titleInput.value = element instanceof HTMLInputElement ? element : null;
}
function focusComposer() {
  void nextTick(() => {
    composerInput.value?.focus();
  });
}
function cancelComposer(restoreFocus = true) {
  if (boardWriteBusy.value) return;
  composer.value = null;
  composerTitle.value = '';
  composerError.value = '';
  composerIntent = null;
  if (restoreFocus) void nextTick(() => restoreInvocation(composerTrigger));
}
function retainDisplacedDrafts(living: string[]) {
  const gone = Object.keys(cardDrafts.value).filter(id => !living.includes(id) && cardDrafts.value[id]);
  for (const id of Object.keys(cardDrafts.value)) {
    if (!living.includes(id)) {
      if (cardDrafts.value[id] && !displacedCardTitle.value) displacedCardTitle.value = cardDrafts.value[id];
      delete cardDrafts.value[id];
      delete cardErrors.value[id];
      collapsedLists.value = collapsedLists.value.filter(item => item !== id);
    }
  }
  if (gone.length) displacedCardTitle.value = gone.map(id => cardDrafts.value[id] ?? '').find(Boolean) ?? displacedCardTitle.value;
}
const cardInputs = new Map<string, HTMLInputElement>();
function assignCardInput(columnId: string, element: unknown) {
  if (element instanceof HTMLInputElement) cardInputs.set(columnId, element);
  else cardInputs.delete(columnId);
}
function focusCardInput(columnId: string) {
  void nextTick(() => cardInputs.get(columnId)?.focus());
}
function adoptDisplacedDraft(columnId: string) {
  if (!displacedCardTitle.value || cardDrafts.value[columnId]) return;
  cardDrafts.value[columnId] = displacedCardTitle.value;
  displacedCardTitle.value = '';
}
function clearCardDraft(columnId: string) {
  cardDrafts.value[columnId] = '';
  cardErrors.value[columnId] = '';
}
async function addCard(columnId: string) {
  const title = (cardDrafts.value[columnId] ?? '').trim();
  if (!title || boardWriteBusy.value || boardLoading.value) return;
  const epoch = parley.workspaceEpoch;
  const signature = JSON.stringify([epoch, columnId, title]);
  if (cardIntent?.signature !== signature) cardIntent = { signature, key: newOperationKey() };
  const { key } = cardIntent;
  boardWriteBusy.value = true;
  cardErrors.value[columnId] = '';
  error.value = '';
  try {
    await parley.commitResourceCard({ create: { title, subtype: 'artifact', body: {}, placement: { column_id: columnId } } }, key);
    if (epoch !== parley.workspaceEpoch) return;
    cardIntent = null;
    cardDrafts.value[columnId] = '';
    status.value = 'Card added.';
    notify();
    await refreshBoard();
  } catch (reason) {
    if (epoch === parley.workspaceEpoch) cardErrors.value[columnId] = message(reason, 'Could not add the card.');
  } finally {
    boardWriteBusy.value = false;
    if (epoch === parley.workspaceEpoch && snapshot.value?.columns.some(column => column.id === columnId)) focusCardInput(columnId);
  }
}
async function saveComposer() {
  const draft = composer.value;
  const title = composerTitle.value.trim();
  if (!draft || draft.kind !== 'list' || !title || boardWriteBusy.value || boardLoading.value) return;
  const epoch = parley.workspaceEpoch;
  const signature = JSON.stringify([epoch, draft, title]);
  if (composerIntent?.signature !== signature) composerIntent = { signature, key: newOperationKey() };
  const { key } = composerIntent;
  boardWriteBusy.value = true;
  composerError.value = '';
  error.value = '';
  try {
    await parley.createResourceColumn({ title }, key);
    if (epoch !== parley.workspaceEpoch) return;
    status.value = 'List added.';
    composerIntent = null;
    composerTitle.value = '';
    notify();
    await refreshBoard();
  } catch (reason) {
    if (epoch === parley.workspaceEpoch) composerError.value = message(reason, 'Could not add the list.');
  } finally {
    boardWriteBusy.value = false;
    if (composer.value === draft) focusComposer();
  }
}
function toggleLaneMenu(columnId: string) {
  laneMenu.value = laneMenu.value === columnId ? '' : columnId;
}
function closeLaneMenu() {
  laneMenu.value = '';
}
function onDocumentPointer(event: Event) {
  if (!laneMenu.value) return;
  if (event.target instanceof Element && event.target.closest('[data-lane-menu]')) return;
  closeLaneMenu();
}
function startColumnRename(column: ResourceColumn, event: Event) {
  const trigger = event.currentTarget instanceof HTMLElement ? event.currentTarget : focused();
  closeLaneMenu();
  requestSwitch(() => {
    columnRenameTrigger = trigger;
    renamingColumnId.value = column.id;
    columnRenameDraft.value = column.title;
    void nextTick(() => renameInput.value?.focus());
  });
}
function cancelColumnRename() {
  renamingColumnId.value = '';
  columnRenameDraft.value = '';
  void nextTick(() => restoreInvocation(columnRenameTrigger));
}
async function saveColumnRename() {
  const column = snapshot.value?.columns.find(item => item.id === renamingColumnId.value);
  const title = columnRenameDraft.value.trim();
  if (!column || !title || boardWriteBusy.value || boardLoading.value) return;
  const epoch = parley.workspaceEpoch;
  const signature = JSON.stringify([epoch, column.id, column.revision, title]);
  if (renameIntent?.signature !== signature) renameIntent = { signature, key: newOperationKey() };
  boardWriteBusy.value = true;
  error.value = '';
  try {
    await parley.renameResourceColumn(column.id, { title, expected_revision: column.revision }, renameIntent.key);
    if (epoch !== parley.workspaceEpoch) return;
    renameIntent = null;
    status.value = 'List renamed.';
    notify();
    await refreshBoard();
    if (epoch === parley.workspaceEpoch) {
      renamingColumnId.value = '';
      columnRenameDraft.value = '';
      await nextTick();
      restoreInvocation(columnRenameTrigger);
    }
  } catch (reason) {
    if (epoch !== parley.workspaceEpoch) return;
    error.value = message(reason, 'Could not rename the list.');
    if (reason instanceof ApiError && [404, 412].includes(reason.status)) {
      await refreshBoard();
      cancelColumnRename();
    }
  } finally {
    boardWriteBusy.value = false;
  }
}
function requestColumnRemoval(column: ResourceColumn, event: Event) {
  const trigger = event.currentTarget instanceof HTMLElement ? event.currentTarget : focused();
  closeLaneMenu();
  if (column.kind === 'system') {
    requestSwitch(() => {
      systemRemovalTrigger = trigger;
      systemRemovalTarget.value = column;
      systemRemovalTyped.value = '';
      systemRemovalError.value = '';
      void nextTick(() => {
        const dialog = systemRemovalDialog.value;
        if (dialog?.isConnected && typeof dialog.showModal === 'function') dialog.showModal();
        systemRemovalInput.value?.focus();
      });
    });
    return;
  }
  requestSwitch(() => { void confirmColumnRemoval(column, trigger); });
}
function cancelSystemRemoval() {
  if (systemRemovalDialog.value?.open) systemRemovalDialog.value.close();
  systemRemovalTarget.value = null;
  systemRemovalTyped.value = '';
  systemRemovalError.value = '';
  restoreInvocation(systemRemovalTrigger);
}
async function confirmSystemRemoval() {
  const column = systemRemovalTarget.value;
  if (!column || boardWriteBusy.value) return;
  if (systemRemovalTyped.value !== column.title) {
    systemRemovalError.value = 'Type the list name exactly as shown to confirm.';
    return;
  }
  if (systemRemovalDialog.value?.open) systemRemovalDialog.value.close();
  systemRemovalTarget.value = null;
  await confirmColumnRemoval(column, systemRemovalTrigger, true);
}
async function confirmColumnRemoval(column: ResourceColumn, trigger: HTMLElement | null, skipConfirm = false) {
  const count = cardsByColumn.value.get(column.id)?.length ?? 0;
  const description = count ? ` ${count === 1 ? 'Its card will' : 'Its cards will'} move to Find resource, where you can add them to another list.` : '';
  if (boardWriteBusy.value || boardLoading.value || (!skipConfirm && !globalThis.confirm(`Remove “${column.title}”?${description}`))) {
    restoreInvocation(trigger);
    return;
  }
  const epoch = parley.workspaceEpoch;
  const signature = JSON.stringify([epoch, column.id, column.revision]);
  if (removalIntent?.signature !== signature) removalIntent = { signature, key: newOperationKey() };
  boardWriteBusy.value = true;
  error.value = '';
  try {
    const result = await parley.removeResourceColumn(column.id, { expected_revision: column.revision }, removalIntent.key);
    if (epoch !== parley.workspaceEpoch) return;
    status.value = result.off_board_count ? 'List removed. Its cards are in Find resource.' : 'List removed.';
    removalIntent = null;
    notify();
    await refresh();
  } catch (reason) {
    if (epoch !== parley.workspaceEpoch) return;
    error.value = message(reason, 'Could not remove the list.');
    if (reason instanceof ApiError && [404, 412].includes(reason.status)) await refreshBoard();
  } finally {
    boardWriteBusy.value = false;
    if (!snapshot.value?.columns.some(item => item.id === column.id)) {
      retainDisplacedDrafts(snapshot.value?.columns.map(item => item.id) ?? []);
      await nextTick();
      restoreInvocation(addList.value);
    } else if (trigger?.isConnected) {
      restoreInvocation(trigger);
    }
  }
}

let failedMove: { signature: string; key: string } | null = null;
async function moveCard(card: CardSummary, column: string | null, before: string | null = null) {
  if (boardWriteBusy.value || moveBusy.value || boardLoading.value || before === card.resource_id || !column && !card.placement) return;
  const trigger = focused();
  const source = card.placement?.column_id;
  const input = {
    resource_id: card.resource_id,
    destination_column_id: column,
    before_resource_id: before,
    expected_placement_revision: card.placement?.revision ?? 0,
    expected_source_column_revision: snapshot.value?.columns.find(item => item.id === source)?.revision ?? null,
    expected_destination_column_revision: snapshot.value?.columns.find(item => item.id === column)?.revision ?? null,
  };
  const signature = JSON.stringify(input);
  const key = failedMove?.signature === signature ? failedMove.key : newOperationKey();
  moveBusy.value = true;
  error.value = '';
  try {
    await parley.moveResourceCard(input, key);
    failedMove = null;
    status.value = column ? `${card.title} moved to ${snapshot.value?.columns.find(item => item.id === column)?.title}.` : `${card.title} moved off board.`;
    notify();
    await refreshBoard();
    if (trigger && !trigger.isConnected) {
      await nextTick();
      const tile = Array.from(globalThis.document.querySelectorAll<HTMLElement>('[data-resource-id]')).find(element => element.dataset.resourceId === card.resource_id);
      restoreInvocation(tile ?? trigger);
    }
  } catch (reason) {
    failedMove = reason instanceof TypeError ? { signature, key } : null;
    error.value = message(reason, 'Could not move the card.');
    if (reason instanceof ApiError && reason.status === 412) await refreshBoard();
  } finally {
    moveBusy.value = false;
    if (pendingRefresh && !editorDirty.value) {
      pendingRefresh = false;
      void refresh();
    }
  }
}
function changeColumn(card: CardSummary, event: Event) {
  const select = event.target as HTMLSelectElement;
  const destination = select.value || null;
  select.value = card.placement?.column_id ?? '';
  if (destination !== (card.placement?.column_id ?? null)) void moveCard(card, destination);
}
function moveRelative(offset: number) {
  const card = selectedSummary.value;
  if (!card?.placement) return;
  const index = orderIndex.value;
  const anchor = inColumn.value[index + (offset > 0 ? 2 : -1)]?.resource_id ?? null;
  void moveCard(card, card.placement.column_id, anchor);
}
function startDrag(event: DragEvent, id: string) {
  draggedId.value = id;
  if (event.dataTransfer) {
    event.dataTransfer.effectAllowed = 'move';
    event.dataTransfer.setData('text/plain', id);
  }
}
function endDrag() { draggedId.value = ''; dropTarget.value = ''; }
function drop(event: DragEvent, column: string, before: string | null = null) {
  const card = resourceById.value.get(draggedId.value);
  if (!card || event.dataTransfer?.getData('text/plain') !== card.resource_id) return;
  endDrag();
  if (!before && card.placement?.column_id === column && cardsByColumn.value.get(column)?.at(-1)?.resource_id === card.resource_id) return;
  void moveCard(card, column, before);
}
async function savePosition(node: { id: string; position: { x: number; y: number } }) {
  const current = nodeById.value.get(node.id);
  const root = rootId.value;
  if (!root || !current || layoutBusy.value) return;
  layoutBusy.value = true;
  try {
    await parley.saveGraphLayout({ view_scope: 'resources', view_id: root, positions: [
      { entity_id: node.id, ...node.position, expected_revision: current.position_revision },
    ] });
  } catch (reason) { graphError.value = message(reason, 'Could not save graph position.'); }
  finally {
    layoutBusy.value = false;
    if (root === rootId.value) await loadGraph();
  }
}
async function openSentVersion(resourceId: string, sendId: string, materialId: string, version: number) {
  sentVersionNumber.value = version;
  versionError.value = '';
  versionLoading.value = true;
  sentVersion.value = null;
  if (!versionDialog.value?.open) versionDialog.value?.showModal();
  try { sentVersion.value = await parley.getDeliveredResourceVersion(resourceId, sendId, materialId, version); }
  catch (reason) { versionError.value = message(reason, 'Could not open the sent version.'); }
  finally { versionLoading.value = false; }
}
function openParty(id: string) { emit('navigate', `/parties/${encodeURIComponent(id)}`); }
function sentence(edge: GraphEdge) { return `${nameOf(edge.from_id)} ${edge.verb.replaceAll('_', ' ')} ${nameOf(edge.to_id)}`; }
async function correctEdge(edge: GraphEdge) {
  if (!edgeCorrection.value || edgeBusy.value || !edge.owner_authored) return;
  const { verb, basis, reverse } = edgeCorrection.value;
  edgeBusy.value = true;
  try {
    await parley.correctOwnerEdge(edge.id, { expected_revision: edge.revision, verb, basis, reverse });
    edgeCorrection.value = null;
    status.value = 'Relationship updated.';
    notify();
    await refresh();
  } catch (reason) { error.value = message(reason, 'Could not correct the relationship.'); }
  finally { edgeBusy.value = false; }
}
function removeRelationshipFromGraph(id: string) {
  const trigger = focused();
  requestSwitch(() => {
    const choices = removableGraphEdges(id);
    if (choices.length === 1 && choices[0]) {
      void detachEdge(choices[0]).then(() => restoreGraphActionFocus(trigger));
    } else if (choices.length > 1) {
      graphRemovalTrigger = trigger;
      graphRemovalTarget.value = id;
      graphRemovalEdge.value = '';
      error.value = '';
      void nextTick(() => graphRemovalDialog.value?.showModal());
    }
  });
}
function restoreGraphActionFocus(trigger: HTMLElement | null) {
  void nextTick(() => {
    if (trigger?.isConnected) trigger.focus();
    else globalThis.document.querySelector<HTMLElement>('[data-testid="resource-canvas"]')?.focus();
  });
}
function closeGraphRemoval() {
  graphRemovalDialog.value?.close();
  graphRemovalTarget.value = null;
  restoreGraphActionFocus(graphRemovalTrigger);
}
async function removeChosenRelationship() {
  const edge = graphRemovalChoices.value.find(item => item.id === graphRemovalEdge.value);
  if (!edge) return;
  if (await detachEdge(edge, false)) closeGraphRemoval();
}
async function detachEdge(edge: GraphEdge, confirm = true) {
  if (edgeBusy.value || !edge.owner_authored || edge.record) return false;
  if (confirm && !globalThis.confirm(`Remove “${sentence(edge)}”? Both cards will remain.`)) return false;
  error.value = '';
  edgeBusy.value = true;
  try {
    await parley.removeEdge(edge.id, edge.revision, newOperationKey());
    if (edgeId.value === edge.id) edgeId.value = null;
    status.value = 'Relationship removed.';
    notify();
    await refresh();
    return true;
  } catch (reason) {
    error.value = message(reason, 'Could not remove the relationship.');
    if (reason instanceof ApiError && [404, 412].includes(reason.status)) await refresh();
    return false;
  } finally { edgeBusy.value = false; }
}
async function download(file: EntityAttachment) {
  const id = inspectedId.value;
  if (!id) return;
  try {
    const content = await parley.getAttachmentContent(id, file.id);
    const url = globalThis.URL.createObjectURL(new globalThis.Blob([content.bytes], { type: content.contentType }));
    const link = globalThis.document.createElement('a');
    link.href = url;
    link.download = content.filename;
    link.click();
    globalThis.setTimeout(() => globalThis.URL.revokeObjectURL(url), 30_000);
  } catch (reason) { detailsError.value = message(reason, 'Could not download this file.'); }
}
function archive(card: CardSummary) {
  requestSwitch(() => { void confirmArchive(card); });
}
async function confirmArchive(card: CardSummary) {
  if (boardWriteBusy.value || !globalThis.confirm(`Archive “${card.title}”? This removes it from the board and picker. Its delivery history is preserved.`)) return;
  const trigger = focused();
  const epoch = parley.workspaceEpoch;
  const signature = JSON.stringify([epoch, card.resource_id]);
  if (retirementIntent?.signature !== signature) retirementIntent = { signature, key: newOperationKey() };
  boardWriteBusy.value = true;
  error.value = '';
  try {
    await parley.retireResourceCard(card.resource_id, retirementIntent.key);
    if (epoch !== parley.workspaceEpoch) return;
    retirementIntent = null;
    if (selectedId.value === card.resource_id) selectedId.value = null;
    if (rootId.value === card.resource_id) rootId.value = null;
    if (inspectedId.value === card.resource_id) inspectedId.value = null;
    if (props.routeId === card.resource_id) emit('navigate', '/resources');
    status.value = 'Card archived.';
    notify();
    await refresh();
    await nextTick();
    if (trigger && !trigger.isConnected) restoreInvocation(boardHeading.value);
  } catch (reason) { error.value = message(reason, 'Could not archive the card.'); }
  finally { boardWriteBusy.value = false; }
}
function erase(card: CardSummary) {
  requestSwitch(() => { void confirmErase(card); });
}
async function confirmErase(card: CardSummary) {
  if (boardWriteBusy.value || !globalThis.confirm(`Erase “${card.title}” permanently? This deletes the card, its files and its relationships. Delivered or cited cards refuse erasure — archive those instead.`)) return;
  const trigger = focused();
  const epoch = parley.workspaceEpoch;
  const signature = JSON.stringify([epoch, card.resource_id]);
  if (eraseIntent?.signature !== signature) eraseIntent = { signature, key: newOperationKey() };
  boardWriteBusy.value = true;
  error.value = '';
  try {
    await parley.eraseResourceCard(card.resource_id, eraseIntent.key);
    if (epoch !== parley.workspaceEpoch) return;
    eraseIntent = null;
    if (selectedId.value === card.resource_id) selectedId.value = null;
    if (rootId.value === card.resource_id) rootId.value = null;
    if (inspectedId.value === card.resource_id) inspectedId.value = null;
    if (props.routeId === card.resource_id) emit('navigate', '/resources');
    status.value = 'Card erased.';
    notify();
    await refresh();
    await nextTick();
    if (trigger && !trigger.isConnected) restoreInvocation(boardHeading.value);
  } catch (reason) {
    error.value = message(reason, 'Could not erase the card.');
    if (reason instanceof ApiError && reason.status === 409) status.value = '';
  } finally { boardWriteBusy.value = false; }
}
async function startTitleEdit(card: CardSummary, event: Event) {
  if (titleBusy.value) return;
  if (titleEditId.value === card.resource_id) {
    titleInput.value?.focus();
    return;
  }
  const trigger = event.currentTarget instanceof HTMLElement ? event.currentTarget : focused();
  requestSwitch(() => {
    titleEditTrigger = trigger;
    titleEditId.value = card.resource_id;
    titleDraft.value = card.title;
    titleError.value = '';
    titleBase = null;
    void nextTick(() => titleInput.value?.focus());
    void parley.getEntityCard(card.resource_id).then(loaded => {
      if (titleEditId.value !== card.resource_id) return;
      titleBase = loaded;
      titleDraft.value = loaded.title;
    }).catch(reason => {
      if (titleEditId.value === card.resource_id) titleError.value = message(reason, 'Could not load this card for renaming.');
    });
  });
}
function cancelTitleEdit() {
  titleEditId.value = '';
  titleDraft.value = '';
  titleError.value = '';
  titleBase = null;
  void nextTick(() => restoreInvocation(titleEditTrigger));
}
async function saveTitleEdit() {
  const summary = resourceById.value.get(titleEditId.value);
  const title = titleDraft.value.trim();
  if (!summary || titleBusy.value) return cancelTitleEdit();
  if (!title || title.length > 240) {
    titleError.value = title ? 'Keep the title within 240 characters.' : 'Enter a title.';
    return;
  }
  if (!titleBase) {
    titleError.value = 'Loading this card…';
    return;
  }
  if (title === titleBase.title) return cancelTitleEdit();
  const epoch = parley.workspaceEpoch;
  titleBusy.value = true;
  titleError.value = '';
  try {
    await parley.commitResourceCard({ edit: { resource_id: summary.resource_id, expected_revision: titleBase.revision, expected_notes_rev: titleBase.notes.revision, title } }, newOperationKey());
    if (epoch !== parley.workspaceEpoch) return;
    titleEditId.value = '';
    titleDraft.value = '';
    titleBase = null;
    status.value = 'Card renamed.';
    notify();
    await refreshBoard();
    if (epoch === parley.workspaceEpoch) await nextTick();
    restoreInvocation(titleEditTrigger);
  } catch (reason) {
    if (epoch !== parley.workspaceEpoch) return;
    titleError.value = message(reason, 'Could not rename the card.');
    if (reason instanceof ApiError && [404, 412].includes(reason.status)) await refreshBoard();
  } finally { titleBusy.value = false; }
}
function isCollapsed(column: ResourceColumn) {
  return !(cardsByColumn.value.get(column.id)?.length) && collapsedLists.value.includes(column.id);
}
function expandList(columnId: string) {
  collapsedLists.value = collapsedLists.value.filter(id => id !== columnId);
}
function collapseList(columnId: string) {
  if (!collapsedLists.value.includes(columnId)) collapsedLists.value = [...collapsedLists.value, columnId];
  closeLaneMenu();
}
async function searchParties(query: string) {
  const own = ++partyGeneration;
  endpointSearchError.value = '';
  if (!query.trim()) { partyEndpoints.value = []; return; }
  try {
    const result = await parley.queryDirectory({ q: query.trim(), limit: 40 });
    if (own !== partyGeneration) return;
    const found = result.rows.filter(node => node.kind === 'org' || node.kind === 'person' || node.kind === 'project');
    partyEndpoints.value = [...new Map([...partyEndpoints.value, ...found.map(node => ({
      id: node.id, label: node.name, kind: kindLabel(node.kind), revision: node.revision,
    }))].map(node => [node.id, node])).values()];
  } catch (reason) {
    if (own === partyGeneration) endpointSearchError.value = message(reason, 'Could not search parties.');
  }
}
function onBeforeUnload(event: BeforeUnloadEvent) {
  const drafting = Object.values(cardDrafts.value).some(title => title.trim());
  if (!editorDirty.value && !drafting && !composerTitle.value.trim() && !displacedCardTitle.value) return;
  event.preventDefault();
  event.returnValue = true;
}
function workspaceChanged() {
  pendingRefresh = false;
  failedMove = null;
  if (channelEpoch !== parley.workspaceEpoch) {
    composer.value = null;
    composerTitle.value = '';
    displacedCardTitle.value = '';
    composerError.value = '';
    composerIntent = null;
    cardIntent = null;
    renameIntent = null;
    eraseIntent = null;
    removalIntent = null;
    retirementIntent = null;
    cardDrafts.value = {};
    cardErrors.value = {};
    collapsedLists.value = [];
    laneMenu.value = '';
    renamingColumnId.value = '';
    columnRenameDraft.value = '';
    if (titleEditId.value) cancelTitleEdit();
    if (systemRemovalDialog.value?.open) cancelSystemRemoval();
    if (graphRemovalDialog.value?.open) closeGraphRemoval();
  }
  if (editorDirty.value) form.value?.quarantine();
  else dismissEditor(false);
  if (channelEpoch !== parley.workspaceEpoch) {
    channelEpoch = parley.workspaceEpoch;
    selectedId.value = null;
    rootId.value = null;
    inspectedId.value = null;
    edgeId.value = null;
    snapshot.value = null;
  }
  void refresh();
}
function updateBreakpoint() {
  const active = focused();
  if (media?.matches && !narrow.value) {
    if (graphEditor.value || active?.closest('.graph-panel')) view.value = 'connections';
    else if (active?.closest('.inspector-panel')) view.value = 'details';
    else view.value = 'board';
  }
  narrow.value = media?.matches ?? false;
}
function applyRoute(id: string) {
  setBoardSelection(id, false);
  view.value = 'details';
}
watch(() => props.routeId, id => { if (id && id !== rootId.value) applyRoute(id); });

// --- U06 scoped command front doors -------------------------------------
// The selected resource card is frozen (id + content revision) before the
// readiness command is exposed; any change to that pair is a new intent.
const commandRegistry = useCommandRegistry();
let disposeResourceActions: (() => void) | undefined;
let scopedResourceFingerprint: string | null = null;
const selectedResourceId = computed(() => props.routeId ?? selectedId.value ?? inspectedId.value);
function refreshScopedResourceActions() {
  const id = selectedResourceId.value;
  const card = id ? resourceById.value.get(id) : undefined;
  const fingerprint = id && card && card.readiness !== 'retired'
    ? JSON.stringify([parley.workspaceEpoch, id, card.content_revision])
    : null;
  if (fingerprint === scopedResourceFingerprint) return;
  scopedResourceFingerprint = fingerprint;
  disposeResourceActions?.();
  disposeResourceActions = undefined;
  if (!fingerprint) return;
  const selectedCard = resourceById.value.get(id!)!;
  const actions: UiCommand[] = [resourceMarkReadyAction({
    resourceId: id!,
    contentRevision: selectedCard.content_revision,
    intentFingerprint: fingerprint,
    invoke: (commandId, args, key) => commands.invoke(commandId, { version: 1, args, dry_run: false }, key),
    onCommitted: (result) => {
      status.value = result.dry_run ? 'Resource readiness dry run completed.' : 'Resource is ready.';
      error.value = '';
      void refreshBoard();
    },
  })];
  disposeResourceActions = registerScoped(commandRegistry, actions);
}
watch([snapshot, selectedResourceId], refreshScopedResourceActions);

onMounted(() => {
  media = globalThis.matchMedia?.('(max-width: 850px)') ?? null;
  updateBreakpoint();
  media?.addEventListener('change', updateBreakpoint);
  globalThis.addEventListener('beforeunload', onBeforeUnload);
  globalThis.addEventListener('parley:workspace-reloaded', workspaceChanged);
  globalThis.addEventListener('parley:workspace-changed', workspaceChanged);
  globalThis.addEventListener('parley:workspace-quarantined', workspaceChanged);
  globalThis.document.addEventListener('pointerdown', onDocumentPointer);
  channelEpoch = parley.workspaceEpoch;
  if (typeof BroadcastChannel !== 'undefined') {
    channel = new BroadcastChannel('parley-resource-board');
    channel.onmessage = onBroadcast;
  }
  eventTimer = globalThis.setInterval(pollChanges, 2500);
  if (props.routeId) applyRoute(props.routeId);
  else restore();
  void refreshBoard();
});
onBeforeUnmount(() => {
  boardGeneration += 1; graphGeneration += 1; detailGeneration += 1; editorGeneration += 1; partyGeneration += 1;
  disposeResourceActions?.();
  media?.removeEventListener('change', updateBreakpoint);
  globalThis.removeEventListener('beforeunload', onBeforeUnload);
  globalThis.removeEventListener('parley:workspace-reloaded', workspaceChanged);
  globalThis.removeEventListener('parley:workspace-changed', workspaceChanged);
  globalThis.removeEventListener('parley:workspace-quarantined', workspaceChanged);
  globalThis.document.removeEventListener('pointerdown', onDocumentPointer);
  channel?.close();
  if (eventTimer) globalThis.clearInterval(eventTimer);
});
const hasDraft = computed(() => editorDirty.value || Boolean(form.value?.saving));
const columns = computed(() => snapshot.value?.columns ?? []);
defineExpose({ refresh, requestSwitch, hasDraft, columns });
</script>

<template>
  <section
    class="resources-workspace"
    aria-label="Resource workspace"
  >
    <nav
      v-if="narrow"
      class="resource-views"
      aria-label="Resources views"
    >
      <button
        v-for="pane in (['board', 'connections', 'details'] as const)"
        :key="pane"
        type="button"
        :aria-pressed="view === pane"
        :aria-controls="'resource-' + pane"
        @click="changeView(pane)"
      >
        {{ pane === 'board' ? 'Board' : pane === 'connections' ? 'Connections' : 'Details' }}
      </button>
    </nav>

    <section
      v-show="!narrow || view === 'board'"
      id="resource-board"
      class="board-panel"
      aria-label="Resource board"
    >
      <header class="panel-header">
        <h2
          ref="boardHeading"
          tabindex="-1"
        >
          Your resources
        </h2>
        <div class="actions">
          <button
            type="button"
            @click="openPicker"
          >
            Find resource
          </button>
        </div>
      </header>
      <div
        v-if="boardError"
        class="notice"
        role="alert"
      >
        {{ boardError }} <button
          type="button"
          @click="refreshBoard"
        >
          Retry board
        </button>
      </div>
      <div
        v-if="displacedCardTitle"
        class="notice"
        role="status"
      >
        The list for “{{ displacedCardTitle }}” was removed. Choose Add card in another list to continue this draft.
        <button
          type="button"
          @click="displacedCardTitle = ''"
        >
          Discard draft
        </button>
      </div>
      <div
        ref="boardViewport"
        class="board"
        :aria-busy="boardLoading"
        @scroll.passive="remember"
      >
        <template
          v-for="column in snapshot?.columns ?? []"
          :key="column.id"
        >
          <button
            v-if="isCollapsed(column)"
            type="button"
            class="lane-pill"
            :class="{ 'drop-lane': dropTarget === column.id }"
            :aria-label="'Expand empty list ' + column.title"
            @click="expandList(column.id)"
            @dragover.prevent="draggedId && (dropTarget = column.id)"
            @drop.prevent="expandList(column.id); drop($event, column.id)"
          >
            <span class="pill-name">{{ column.title }}</span>
            <span class="pill-count">0</span>
          </button>
          <section
            v-else
            class="lane"
            :class="{ 'drop-lane': dropTarget === column.id }"
            :aria-label="column.title"
            @dragover.prevent="draggedId && (dropTarget = column.id)"
            @drop.prevent="drop($event, column.id)"
          >
            <header
              class="lane-header"
              data-lane-menu
            >
              <form
                v-if="renamingColumnId === column.id"
                class="lane-rename"
                @submit.prevent="saveColumnRename"
                @keydown.esc.stop.prevent="cancelColumnRename"
              >
                <input
                  :ref="setRenameInput"
                  v-model="columnRenameDraft"
                  :aria-label="'Rename list ' + column.title"
                  maxlength="100"
                  :disabled="boardWriteBusy"
                >
              </form>
              <template v-else>
                <h3>{{ column.title }} <small>{{ cardsByColumn.get(column.id)?.length ?? 0 }}</small></h3>
                <div class="lane-tools">
                  <button
                    type="button"
                    class="lane-menu-button"
                    :aria-label="'List actions for ' + column.title"
                    aria-haspopup="menu"
                    :aria-expanded="laneMenu === column.id"
                    :disabled="boardLoading || boardWriteBusy || moveBusy"
                    @click.stop="toggleLaneMenu(column.id)"
                  >
                    <span aria-hidden="true">…</span>
                  </button>
                  <ul
                    v-if="laneMenu === column.id"
                    class="lane-menu"
                    role="menu"
                    :aria-label="'List actions for ' + column.title"
                  >
                    <li role="none">
                      <button
                        role="menuitem"
                        type="button"
                        @click="startColumnRename(column, $event)"
                      >
                        Rename list
                      </button>
                    </li>
                    <li role="none">
                      <button
                        role="menuitem"
                        type="button"
                        :disabled="boardWriteBusy || moveBusy"
                        @click="requestColumnRemoval(column, $event)"
                      >
                        Remove list
                      </button>
                    </li>
                    <li
                      v-if="!cardsByColumn.get(column.id)?.length"
                      role="none"
                    >
                      <button
                        role="menuitem"
                        type="button"
                        @click="collapseList(column.id)"
                      >
                        Collapse
                      </button>
                    </li>
                  </ul>
                </div>
              </template>
            </header>
            <ul class="tile-list">
              <li
                v-for="card in cardsByColumn.get(column.id) ?? []"
                :key="card.resource_id"
                class="tile"
                :class="{ selected: card.resource_id === selectedId, 'drop-card': dropTarget === card.resource_id }"
                @dragover.stop.prevent="draggedId && (dropTarget = card.resource_id)"
                @drop.stop.prevent="drop($event, column.id, card.resource_id)"
              >
                <div
                  class="tile-body"
                  :draggable="titleEditId !== card.resource_id"
                  :data-resource-id="card.resource_id"
                  :role="titleEditId === card.resource_id ? undefined : 'button'"
                  :tabindex="titleEditId === card.resource_id ? -1 : 0"
                  :aria-pressed="titleEditId === card.resource_id ? undefined : card.resource_id === selectedId"
                  @click="chooseBoard(card.resource_id)"
                  @keydown.enter.prevent="titleEditId !== card.resource_id && chooseBoard(card.resource_id)"
                  @dragstart="startDrag($event, card.resource_id)"
                  @dragend="endDrag"
                >
                  <small>{{ resourceLabel(card.subtype) }}</small>
                  <input
                    v-if="titleEditId === card.resource_id"
                    :ref="setTitleInput"
                    v-model="titleDraft"
                    :aria-label="'Rename card ' + card.title"
                    maxlength="240"
                    :disabled="titleBusy"
                    @click.stop
                    @keydown.enter.prevent="saveTitleEdit"
                    @keydown.esc.prevent="cancelTitleEdit"
                  >
                  <button
                    v-else
                    type="button"
                    class="tile-title"
                    :aria-label="'Rename ' + card.title"
                    @dblclick.stop="startTitleEdit(card, $event)"
                    @keydown.enter.prevent.stop="startTitleEdit(card, $event)"
                  >
                    {{ card.title }}
                  </button>
                  <p
                    v-if="titleEditId === card.resource_id && titleError"
                    role="alert"
                  >
                    {{ titleError }}
                  </p>
                  <span class="tile-counts">{{ card.counts.files }} {{ card.counts.files === 1 ? 'file' : 'files' }} · {{ card.counts.connections }} linked<span v-if="card.counts.proposed"> · {{ card.counts.proposed }} proposed</span></span>
                </div>
                <div class="tile-actions">
                  <button
                    type="button"
                    :aria-label="'Edit ' + card.title"
                    @click="openEdit(card.resource_id)"
                  >
                    Edit
                  </button>
                  <button
                    type="button"
                    class="retire"
                    :aria-label="'Archive ' + card.title"
                    :disabled="boardWriteBusy || moveBusy"
                    @click="archive(card)"
                  >
                    Archive
                  </button>
                  <button
                    type="button"
                    class="retire"
                    :aria-label="'Delete ' + card.title"
                    :disabled="boardWriteBusy || moveBusy"
                    @click="erase(card)"
                  >
                    Delete
                  </button>
                </div>
              </li>
            </ul>
            <form
              class="card-inline"
              :aria-label="'Add card to ' + column.title"
              @submit.prevent="addCard(column.id)"
            >
              <input
                :ref="element => assignCardInput(column.id, element)"
                v-model="cardDrafts[column.id]"
                :aria-label="'Add card to ' + column.title"
                placeholder="Enter a card title"
                maxlength="240"
                :disabled="boardWriteBusy"
                @focus="adoptDisplacedDraft(column.id)"
                @keydown.esc.prevent="clearCardDraft(column.id)"
              >
              <p
                v-if="cardErrors[column.id]"
                role="alert"
              >
                {{ cardErrors[column.id] }}
              </p>
              <div class="card-inline-actions">
                <button
                  class="primary"
                  type="submit"
                  :disabled="!(cardDrafts[column.id] ?? '').trim() || boardLoading || boardWriteBusy"
                >
                  Add card
                </button>
                <button
                  type="button"
                  :disabled="boardWriteBusy"
                  @click="openCreate(column.id, cardDrafts[column.id])"
                >
                  More options
                </button>
              </div>
            </form>
          </section>
        </template>
        <div
          v-if="snapshot"
          class="new-list"
        >
          <form
            v-if="composer?.kind === 'list'"
            class="board-composer"
            aria-label="New resource list"
            @submit.prevent="saveComposer"
            @keydown.esc.stop.prevent="cancelComposer()"
          >
            <input
              ref="composerInput"
              v-model="composerTitle"
              aria-label="List name"
              placeholder="Enter a list name"
              maxlength="100"
              :disabled="boardWriteBusy"
            >
            <p
              v-if="composerError"
              role="alert"
            >
              {{ composerError }}
            </p>
            <div class="actions">
              <button
                type="submit"
                class="primary"
                :disabled="!composerTitle.trim() || boardLoading || boardWriteBusy"
              >
                Add list
              </button>
              <button
                type="button"
                :disabled="boardWriteBusy"
                @click="cancelComposer()"
              >
                Cancel
              </button>
            </div>
          </form>
          <button
            v-show="composer?.kind !== 'list'"
            ref="addList"
            type="button"
            :disabled="boardWriteBusy || (composer !== null && !!composerTitle.trim())"
            @click="startComposer({ kind: 'list' }, $event)"
          >
            + Add another list
          </button>
        </div>
      </div>
      <p
        v-if="boardLoading && !snapshot"
        class="panel-note"
      >
        Loading resource board…
      </p>
      <p
        v-else-if="snapshot?.columns.length"
        class="panel-note"
      >
        Drag cards to organize them, or move them from Details.
      </p>
    </section>

    <div
      v-show="!narrow || view !== 'board'"
      class="connections-panel"
    >
      <section
        v-show="!narrow || view === 'connections'"
        id="resource-connections"
        class="graph-panel"
        aria-label="Connections graph"
      >
        <header class="panel-header">
          <h2>{{ rootName ? rootName + ' · connections' : 'Resource connections' }}</h2>
          <div class="actions">
            <button
              v-if="selectedId && selectedId !== rootId"
              type="button"
              @click="explore(selectedId)"
            >
              Return to selected card
            </button>
            <button
              v-if="graph.nodes.length > 1"
              type="button"
              :aria-expanded="filtersOpen"
              @click="filtersOpen = !filtersOpen"
            >
              Filter
            </button>
          </div>
        </header>
        <label
          v-if="filtersOpen && graph.nodes.length > 1"
          class="graph-filter"
        >Show
          <select v-model="graphFilter">
            <option value="all">All connections</option><option value="resources">Resources</option><option value="parties">Parties</option><option value="activity">Material and delivery</option>
          </select>
        </label>
        <p
          v-if="graphError"
          class="notice"
          role="alert"
        >
          {{ graphError }} <button
            type="button"
            @click="loadGraph"
          >
            Retry connections
          </button>
        </p>
        <p
          v-else-if="graphLoading && !graph.nodes.length"
          class="graph-empty"
          role="status"
        >
          Loading connections…
        </p>
        <p
          v-else-if="!graph.nodes.length"
          class="graph-empty"
        >
          {{ rootId ? 'This card is unavailable.' : 'Select a card to see its connections.' }}
        </p>
        <ResourceGraph
          v-if="rootId && graph.nodes.length"
          :root-id="rootId"
          :nodes="visibleNodes"
          :edges="visibleEdges"
          :inspected-id="inspectedId"
          :edge-id="edgeId"
          :editing="graphEditor"
          :pending="pendingRelationship"
          :busy="layoutBusy"
          :active="active !== false && (!narrow || view === 'connections')"
          :can-add-relationship="isResource"
          :removing="edgeBusy"
          :label-for-node="node => resourceLabel(resourceById.get(node.id)?.subtype ?? node.kind)"
          @activate="activateNode"
          @add-relationship="id => openEdit(id, true, true)"
          @remove-relationship="removeRelationshipFromGraph"
          @inspect-edge="inspectEdge"
          @position="savePosition"
        >
          <ResourceEditorForm
            v-if="editor?.mode === 'edit' && editor.graph"
            ref="form"
            :key="editorKey"
            mode="edit"
            :card="editor.card"
            :card-summary="editor.summary"
            :columns="snapshot?.columns ?? []"
            :available-endpoints="endpoints"
            :endpoint-search-error="endpointSearchError"
            graph-mode
            @saved="onSaved"
            @cancel="closeEditor"
            @update:dirty="editorDirty = $event"
            @search-endpoints="searchParties"
            @preview-relationship="pendingRelationship = $event"
          />
        </ResourceGraph>
        <p
          v-if="graph.omitted_nodes || graph.omitted_edges || graph.nodes.length !== visibleNodes.length"
          class="panel-note"
        >
          <span v-if="graph.omitted_nodes || graph.omitted_edges">{{ graph.omitted_nodes }} more cards and {{ graph.omitted_edges }} more relationships outside this view. </span>
          <span v-if="graph.nodes.length !== visibleNodes.length">{{ graph.nodes.length - visibleNodes.length }} cards hidden by filters.</span>
        </p>
        <details
          v-if="graph.nodes.length"
          class="graph-list"
        >
          <summary>Cards and relationships</summary>
          <ul aria-label="Graph cards">
            <li
              v-for="node in visibleNodes"
              :key="node.id"
            >
              <button
                type="button"
                :aria-pressed="inspectedId === node.id && !edgeId"
                @click="activateNode(node.id)"
              >
                {{ nameOf(node.id) }} · {{ resourceLabel(resourceById.get(node.id)?.subtype ?? node.kind) }}
              </button>
            </li>
          </ul>
          <ul
            v-if="visibleEdges.length"
            aria-label="Semantic relationship list"
          >
            <li
              v-for="edge in visibleEdges"
              :key="edge.id"
            >
              <button
                type="button"
                :aria-pressed="edgeId === edge.id"
                @click="inspectEdge(edge.id)"
              >
                {{ sentence(edge) }}
              </button>
            </li>
          </ul>
          <p v-else>
            No relationships in this view.
          </p>
        </details>
      </section>

      <section
        v-show="!narrow || view === 'details'"
        id="resource-details"
        class="inspector-panel"
        aria-label="Resource inspector"
      >
        <template v-if="selectedEdge">
          <small class="eyebrow">Relationship</small>
          <h2
            ref="detailsHeading"
            tabindex="-1"
          >
            {{ sentence(selectedEdge) }}
          </h2>
          <blockquote v-if="selectedEdge.quote">
            {{ selectedEdge.quote }}
          </blockquote>
          <div
            v-if="selectedEdge.record"
            class="record-detail"
          >
            <p v-if="selectedEdge.record.kind === 'delivery'">
              Recorded by the owner on {{ selectedEdge.captured_at.slice(0, 10) }} · {{ selectedEdge.record.opportunity_title }} · Version {{ selectedEdge.record.material_version }}
            </p>
            <p v-else>
              {{ selectedEdge.record.opportunity_title }} · Used in material version {{ selectedEdge.record.material_version }}
            </p>
            <div class="actions">
              <button
                v-if="selectedEdge.record.send_id"
                type="button"
                @click="openSentVersion(selectedEdge.from_id, selectedEdge.record.send_id, selectedEdge.record.material_id, selectedEdge.record.material_version)"
              >
                View sent version
              </button>
              <button
                v-else
                type="button"
                @click="emit('navigate', '/output/' + encodeURIComponent(selectedEdge.record.material_id))"
              >
                Open material
              </button>
              <button
                type="button"
                @click="emit('navigate', '/queue/' + encodeURIComponent(selectedEdge.record.opportunity_id))"
              >
                Open opportunity
              </button>
            </div>
          </div>
          <p v-else-if="selectedEdge.owner_authored">
            Owner note · {{ selectedEdge.captured_at.slice(0, 10) }} · Revision {{ selectedEdge.revision }}
          </p>
          <p v-else>
            {{ selectedEdge.source_title || 'Captured source' }} · {{ selectedEdge.speaker_kind }} · Tier {{ selectedEdge.tier }} · {{ selectedEdge.captured_at.slice(0, 10) }}
          </p>
          <p v-if="selectedEdge.source_url">
            {{ selectedEdge.source_url }}
          </p>
          <form
            v-if="edgeCorrection"
            class="edge-form"
            @submit.prevent="correctEdge(selectedEdge)"
          >
            <label>Relationship verb<input
              v-model="edgeCorrection.verb"
              required
              maxlength="120"
              :disabled="edgeBusy"
            ></label>
            <label>Basis<textarea
              v-model="edgeCorrection.basis"
              required
              maxlength="2000"
              :disabled="edgeBusy"
            /></label>
            <label class="check"><input
              v-model="edgeCorrection.reverse"
              type="checkbox"
              :disabled="edgeBusy"
            > Reverse direction</label>
            <div class="actions">
              <button
                type="submit"
                class="primary"
                :disabled="edgeBusy"
              >
                Save correction
              </button><button
                type="button"
                :disabled="edgeBusy"
                @click="edgeCorrection = null"
              >
                Cancel correction
              </button>
            </div>
          </form>
          <div
            v-else-if="!selectedEdge.record"
            class="actions"
          >
            <button
              v-if="selectedEdge.owner_authored"
              type="button"
              @click="edgeCorrection = { verb: selectedEdge.verb, basis: selectedEdge.quote, reverse: false }"
            >
              Correct relationship
            </button>
            <button
              v-if="selectedEdge.owner_authored"
              type="button"
              :disabled="edgeBusy"
              @click="detachEdge(selectedEdge)"
            >
              Detach
            </button>
            <button
              type="button"
              @click="activateNode(selectedEdge.from_id)"
            >
              Open {{ nameOf(selectedEdge.from_id) }}
            </button>
            <button
              type="button"
              @click="activateNode(selectedEdge.to_id)"
            >
              Open {{ nameOf(selectedEdge.to_id) }}
            </button>
          </div>
        </template>
        <template v-else-if="inspectedName">
          <small class="eyebrow">{{ resourceLabel(inspectedType) }}<template v-if="selectedSummary"> · {{ columnTitle }}</template></small>
          <h2
            ref="detailsHeading"
            tabindex="-1"
          >
            {{ inspectedName }}
          </h2>
          <p v-if="selectedIsFiltered">
            This card is hidden by the graph filter.
          </p>
          <div
            v-if="inspectedCard?.notes.markdown || partyNotes"
            class="notes-preview"
          >
            <MarkdownPreview :source="inspectedCard?.notes.markdown || partyNotes" />
          </div>
          <p
            v-if="detailsLoading"
            role="status"
          >
            Loading card details…
          </p>
          <p
            v-if="detailsError"
            class="notice"
            role="alert"
          >
            {{ detailsError }} <button
              type="button"
              @click="loadDetails"
            >
              Retry details
            </button>
          </p>
          <div class="actions">
            <button
              v-if="selectedSummary && !graphEditor"
              type="button"
              :disabled="editorLoading"
              @click="openEdit(selectedSummary.resource_id, true)"
            >
              Open card editor
            </button>
            <button
              v-if="isParty(selectedNode) && inspectedId"
              type="button"
              @click="openParty(inspectedId)"
            >
              Open in Parties
            </button>
            <button
              v-if="selectedNode?.kind === 'material'"
              type="button"
              @click="emit('navigate', '/output/' + encodeURIComponent(selectedNode.id))"
            >
              Open in Output
            </button>
            <button
              v-if="selectedNode?.kind === 'opportunity'"
              type="button"
              @click="emit('navigate', '/queue/' + encodeURIComponent(selectedNode.id))"
            >
              Open in Queue
            </button>
            <button
              v-if="inspectedId && inspectedId !== rootId"
              type="button"
              @click="explore(inspectedId)"
            >
              Explore connections
            </button>
          </div>
          <div
            v-if="selectedSummary"
            class="placement"
          >
            <label>Resource board column
              <select
                :aria-label="'Move ' + selectedSummary.title + ' to column'"
                :value="position?.column_id ?? ''"
                :disabled="moveBusy || boardLoading"
                @change="changeColumn(selectedSummary, $event)"
              >
                <option value="">Off board</option>
                <option
                  v-for="column in snapshot?.columns ?? []"
                  :key="column.id"
                  :value="column.id"
                >{{ column.title }}</option>
              </select>
            </label>
            <div
              v-if="position"
              class="actions"
            >
              <button
                type="button"
                :disabled="moveBusy || orderIndex <= 0"
                @click="moveRelative(-1)"
              >
                Move earlier
              </button>
              <button
                type="button"
                :disabled="moveBusy || orderIndex < 0 || orderIndex >= inColumn.length - 1"
                @click="moveRelative(1)"
              >
                Move later
              </button>
              <button
                type="button"
                :disabled="moveBusy || boardLoading"
                @click="moveCard(selectedSummary, null)"
              >
                Remove from board
              </button>
            </div>
          </div>
          <details
            v-if="relatedEdges.length"
            class="inspector-group"
          >
            <summary>{{ relatedEdges.length }} {{ relatedEdges.length === 1 ? 'relationship' : 'relationships' }} and their sources</summary>
            <ul>
              <li
                v-for="edge in relatedEdges"
                :key="edge.id"
              >
                <button
                  type="button"
                  class="text-button"
                  @click="inspectEdge(edge.id)"
                >
                  {{ sentence(edge) }}
                </button><small v-if="edge.quote">{{ edge.quote }}</small>
              </li>
            </ul>
          </details>
          <details
            v-if="attachments.length"
            class="inspector-group"
          >
            <summary>{{ attachments.length }} {{ attachments.length === 1 ? 'file' : 'files' }}</summary>
            <ul aria-label="Attached files">
              <li
                v-for="file in attachments"
                :key="file.id"
              >
                {{ file.filename }} · {{ Math.ceil(file.size / 1024) }} KB · {{ file.share_permission ? 'Shareable' : 'Private' }}
                <button
                  type="button"
                  :aria-label="'Download ' + file.filename"
                  @click="download(file)"
                >
                  Download
                </button>
              </li>
            </ul>
          </details>
          <details
            v-if="deliveries.length"
            class="inspector-group"
          >
            <summary>{{ deliveries.length }} recorded {{ deliveries.length === 1 ? 'delivery' : 'deliveries' }}</summary>
            <ul aria-label="Delivery records">
              <li
                v-for="delivery in deliveries"
                :key="`${delivery.send_id}:${delivery.material_id}:${delivery.material_version}`"
              >
                {{ delivery.sent_at.slice(0, 10) }} · {{ delivery.recipient_name }} · {{ delivery.opportunity_title }}
                <small>{{ delivery.material_kind }} · version {{ delivery.material_version }}</small>
                <div class="actions">
                  <button
                    type="button"
                    @click="emit('navigate', '/queue/' + encodeURIComponent(delivery.opportunity_id))"
                  >
                    Open opportunity
                  </button>
                  <button
                    v-if="inspectedId"
                    type="button"
                    @click="openSentVersion(inspectedId, delivery.send_id, delivery.material_id, delivery.material_version)"
                  >
                    View sent version
                  </button>
                </div>
              </li>
            </ul>
          </details>
          <details
            v-if="selectedSummary"
            class="inspector-group"
          >
            <summary>Readiness and card actions</summary>
            <p v-if="selectedSummary.gaps.length">
              <strong>{{ position?.column_id === 'available' ? 'Available, but still ineligible' : 'Incomplete resource' }}</strong>
            </p>
            <p v-if="selectedSummary.gaps.length">
              Missing typed fields: {{ selectedSummary.gaps.join(', ') }}
            </p>
            <p v-else>
              {{ selectedSummary.readiness === 'ready' ? 'Ready' : 'Draft' }}
            </p>
            <div class="inspector-actions">
              <button
                type="button"
                @click="archive(selectedSummary)"
              >
                Archive card
              </button>
              <button
                type="button"
                class="retire"
                @click="erase(selectedSummary)"
              >
                Delete card
              </button>
            </div>
          </details>
        </template>
        <template v-else>
          <h2
            ref="detailsHeading"
            tabindex="-1"
          >
            Details
          </h2>
          <p>{{ detailsError || (detailsLoading ? 'Loading card details…' : inspectedId ? 'This card is unavailable.' : 'Select a resource to see its details.') }}</p>
        </template>
      </section>
    </div>

    <dialog
      ref="versionDialog"
      class="resource-dialog"
      aria-labelledby="sent-version-title"
    >
      <header class="panel-header">
        <h2 id="sent-version-title">
          Sent version {{ sentVersionNumber }}
        </h2><button
          type="button"
          @click="versionDialog?.close()"
        >
          Close
        </button>
      </header>
      <div class="picker-content">
        <p
          v-if="versionLoading"
          role="status"
        >
          Loading sent version…
        </p>
        <p
          v-else-if="versionError"
          role="alert"
        >
          {{ versionError }}
        </p>
        <template v-else-if="sentVersion">
          <p>{{ sentVersion.resourceTitle }} · Resource revision {{ sentVersion.resourceRevision }}</p>
          <p v-if="sentVersion.filename">
            Attachment · {{ sentVersion.filename }}
          </p>
          <pre class="sent-text">{{ sentVersion.body }}</pre>
        </template>
      </div>
    </dialog>
    <dialog
      ref="pickerDialog"
      class="resource-dialog"
      aria-labelledby="resource-picker-title"
      @cancel.prevent="closePicker"
    >
      <template v-if="pickerOpen">
        <header class="panel-header">
          <h2 id="resource-picker-title">
            Find resource
          </h2><button
            type="button"
            @click="closePicker"
          >
            Close
          </button>
        </header>
        <div class="picker-content">
          <label>Search by title or type<input
            ref="pickerSearch"
            v-model="query"
            type="search"
            autocomplete="off"
          ></label>
          <ul
            v-if="filteredResources.length"
            aria-label="Resource picker"
          >
            <li
              v-for="card in filteredResources"
              :key="card.resource_id"
            >
              <button
                type="button"
                class="text-button"
                @click="chooseResource(card.resource_id)"
              >
                {{ card.title }}
              </button>
              <small>{{ resourceLabel(card.subtype) }} · {{ snapshot?.columns.find(column => column.id === card.placement?.column_id)?.title ?? 'Off board' }}</small>
              <select
                v-if="!card.placement"
                :aria-label="'Place ' + card.title + ' on board'"
                :disabled="moveBusy || boardLoading"
                value=""
                @change="changeColumn(card, $event)"
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
          </ul>
          <p v-else>
            {{ allResources.length ? 'No resources match your search.' : 'No resources yet.' }}
          </p>
        </div>
      </template>
    </dialog>
    <dialog
      ref="graphRemovalDialog"
      class="resource-dialog"
      aria-labelledby="graph-remove-title"
      @cancel.prevent="!edgeBusy && closeGraphRemoval()"
    >
      <form
        v-if="graphRemovalTarget"
        class="board-composer"
        @submit.prevent="removeChosenRelationship"
      >
        <h2 id="graph-remove-title">
          Remove a relationship
        </h2>
        <label>Relationship
          <select
            v-model="graphRemovalEdge"
            aria-label="Relationship"
            required
            autofocus
            :disabled="edgeBusy"
          >
            <option
              value=""
              disabled
            >Choose a relationship</option>
            <option
              v-for="edge in graphRemovalChoices"
              :key="edge.id"
              :value="edge.id"
            >{{ sentence(edge) }}</option>
          </select>
        </label>
        <p v-if="graphRemovalChoices.length === 0">
          No removable relationships remain between these cards.
        </p>
        <p
          v-if="error"
          role="alert"
        >
          {{ error }}
        </p>
        <div class="actions">
          <button
            type="submit"
            :disabled="edgeBusy || !graphRemovalChoices.some(edge => edge.id === graphRemovalEdge)"
          >
            Remove relationship
          </button>
          <button
            type="button"
            :disabled="edgeBusy"
            @click="closeGraphRemoval"
          >
            Cancel
          </button>
        </div>
      </form>
    </dialog>
    <dialog
      ref="systemRemovalDialog"
      class="resource-dialog"
      aria-labelledby="system-remove-title"
      @cancel.prevent="cancelSystemRemoval"
    >
      <form
        v-if="systemRemovalTarget"
        class="board-composer"
        @submit.prevent="confirmSystemRemoval"
      >
        <h2 id="system-remove-title">
          Remove “{{ systemRemovalTarget.title }}”?
        </h2>
        <p>
          This is a system list. Removing it cannot be undone — the workspace
          never recreates it. Its cards stay available in Find resource.
        </p>
        <label>Type “{{ systemRemovalTarget.title }}” to confirm
          <input
            ref="systemRemovalInput"
            v-model="systemRemovalTyped"
            aria-label="Confirm list name"
            autocomplete="off"
            :disabled="boardWriteBusy"
          >
        </label>
        <p
          v-if="systemRemovalError"
          role="alert"
        >
          {{ systemRemovalError }}
        </p>
        <div class="actions">
          <button
            type="submit"
            class="danger"
            :disabled="boardWriteBusy || systemRemovalTyped !== systemRemovalTarget.title"
          >
            Remove system list
          </button>
          <button
            type="button"
            :disabled="boardWriteBusy"
            @click="cancelSystemRemoval"
          >
            Cancel
          </button>
        </div>
      </form>
    </dialog>
    <dialog
      ref="editorDialog"
      class="resource-dialog editor-dialog"
      aria-label="Resource card editor"
      @cancel.prevent="closeEditor"
    >
      <ResourceEditorForm
        v-if="editor && !graphEditor"
        ref="form"
        :key="editorKey"
        :mode="editor.mode"
        :card="editor.mode === 'edit' ? editor.card : null"
        :card-summary="editor.mode === 'edit' ? editor.summary : null"
        :initial-column-id="editor.mode === 'create' ? editor.column : null"
        :columns="snapshot?.columns ?? []"
        :initial-title="editor.mode === 'create' ? editor.title : ''"
        @saved="onSaved"
        @cancel="closeEditor"
        @update:dirty="editorDirty = $event"
      />
    </dialog>
    <dialog
      ref="confirmDialog"
      role="alertdialog"
      class="resource-dialog draft-dialog"
      aria-labelledby="resource-draft-title"
      @cancel.prevent="stay()"
    >
      <div
        v-if="pendingSwitch"
        class="draft-prompt"
      >
        <h2 id="resource-draft-title">
          Unsaved card draft
        </h2>
        <p>Save your changes before leaving this editor?</p>
        <div class="actions">
          <button
            type="button"
            class="primary"
            :disabled="form?.saving"
            @click="saveAndSwitch"
          >
            Save
          </button>
          <button
            type="button"
            :disabled="form?.saving"
            @click="finishSwitch"
          >
            Discard unsaved changes
          </button>
          <button
            type="button"
            autofocus
            :disabled="form?.saving"
            @click="stay()"
          >
            Stay
          </button>
        </div>
      </div>
    </dialog>
    <p
      class="workspace-status"
      role="status"
    >
      {{ editorLoading ? 'Opening card editor…' : status }}
    </p>
    <p
      v-if="error"
      class="workspace-status notice"
      role="alert"
    >
      {{ error }}
    </p>
  </section>
</template>

<style scoped>
.resources-workspace {
  --panel: #fffdf7;
  --line: #c7d1c2;
  --muted: #4e6558;
  display: grid;
  grid-template-columns: minmax(0, 1.12fr) minmax(0, 1fr);
  gap: 1rem;
  align-items: start;
  min-width: 0;
}
.resources-workspace > .board-panel, .connections-panel {
  background: var(--panel);
  border: 1px solid var(--line);
  border-radius: .6rem;
  min-width: 0;
}
.resources-workspace > .board-panel { padding: 0; }
.resources-workspace section.lane,
.resources-workspace .graph-panel,
.resources-workspace .inspector-panel { border: 0; border-radius: 0; background: none; padding: 0; min-width: 0; }
.panel-header { display: flex; justify-content: space-between; align-items: center; flex-wrap: wrap; gap: .6rem; padding: .85rem 1rem; border-bottom: 1px solid var(--line); }
.panel-header h2 { margin: 0; overflow-wrap: anywhere; }
.actions { display: flex; flex-wrap: wrap; align-items: center; gap: .4rem; }
.actions button, .panel-header button { min-height: 2.5rem; }
.board { display: flex; align-items: flex-start; min-width: 0; max-height: min(65dvh, 41rem); overflow: auto; gap: .65rem; padding: .7rem; background: #f4f6ed; }
.lane, .new-list, .lane-pill { flex: 0 0 clamp(13rem, 31%, 17rem); min-width: 0; }
.new-list > button { width: 100%; border-style: dashed; text-align: start; background: #fffdf8; min-height: 2.75rem; }
.lane-header { display: flex; align-items: start; justify-content: space-between; gap: .3rem; margin-bottom: .55rem; }
.lane-rename { flex: 1; margin: .15rem 0; }
.lane-rename input { width: 100%; font-size: 1.03rem; }
.lane-tools { position: relative; display: grid; }
.lane-menu-button { min-height: 2rem; min-width: 2rem; padding: .1rem .45rem; background: transparent; border-color: transparent; font-size: 1.05rem; line-height: 1; color: #344f40; }
.lane-menu { position: absolute; right: 0; top: calc(100% + .15rem); z-index: 5; list-style: none; margin: 0; padding: .25rem; background: var(--panel); border: 1px solid var(--line); border-radius: .45rem; box-shadow: 0 8px 22px #263f2d2e; display: grid; min-width: 10rem; }
.lane-menu button { width: 100%; text-align: left; border: 0; background: transparent; min-height: 2.1rem; font-size: .9rem; }
.lane-menu button:not(:disabled):hover { background: #eaf0eb; }
.lane-pill { writing-mode: vertical-rl; text-orientation: mixed; display: inline-flex; align-items: center; gap: .4rem; justify-content: center; background: #e9eee2; border: 1px solid #d3dbcc; border-radius: .5rem; padding: .55rem .3rem; font: 1rem Georgia, serif; color: #213b2f; }
.lane-pill .pill-count { writing-mode: horizontal-tb; font-variant-numeric: tabular-nums; font-size: .85rem; color: var(--muted); }
.board-composer { background: #fffefb; border: 1px solid #c3cdbf; border-radius: .4rem; display: grid; gap: .5rem; padding: .55rem; }
.board-composer input { min-width: 0; width: 100%; }
.board-composer .actions button { font-size: .85rem; min-height: 2.25rem; }
.board-composer p { margin: 0; color: #832d25; overflow-wrap: anywhere; }
.resources-workspace section.lane {
  background: #e9eee2;
  border: 1px solid #d3dbcc;
  border-radius: .5rem;
  padding: .55rem;
}
.lane h3 { font-size: 1.03rem; margin: .25rem .1rem; overflow-wrap: anywhere; }
.lane h3 small { margin-left: .18rem; font-variant-numeric: tabular-nums; font-weight: 500; }
.tile-list { list-style: none; margin: 0; padding: 0; }
.tile { display: grid; border: 1px solid #c3cdbf; border-top: 3px solid #a48e68; border-radius: .4rem; background: #fffefb; margin-bottom: .55rem; min-width: 0; padding-bottom: .35rem; }
.tile.selected { border-color: #30654d; box-shadow: 0 0 0 1px #30654d; background: #f5f9f0; }
.tile-body { display: grid; gap: .32rem; min-width: 0; text-align: left; border: 0; box-shadow: none; background: transparent; padding: .65rem .6rem .4rem; overflow-wrap: anywhere; }
.tile-body[aria-pressed="true"] { background: transparent; box-shadow: none; }
.tile-body small, .eyebrow { color: var(--muted); font: .88rem/1.4 ui-monospace, monospace; }
.tile-body strong, .tile-title { font: 1.23rem/1.2 Georgia, serif; }
.tile-title { text-align: left; min-height: 0; width: 100%; padding: 0; border: 0; background: transparent; border-color: transparent; overflow-wrap: anywhere; }
.tile-title:hover { text-decoration: underline; text-decoration-color: #a48e68; text-underline-offset: .18em; }
.tile-body input { width: 100%; font: 1.23rem/1.2 Georgia, serif; }
.tile-body p { margin: 0; color: #832d25; font-size: .88rem; }
.tile-counts { color: var(--muted); font-size: .9rem; }
.tile-actions { display: flex; justify-content: flex-end; flex-wrap: wrap; gap: .3rem; padding: 0 .37rem; }
.tile-actions button { min-height: 2rem; font-size: .82rem; }
.card-inline { background: #fffefb; border: 1px dashed #c3cdbf; border-radius: .4rem; display: grid; gap: .35rem; padding: .4rem; }
.card-inline input { min-width: 0; width: 100%; border-style: dashed; border-color: transparent; background: transparent; }
.card-inline input:focus { border-color: #a48e68; background: #fffefb; }
.card-inline p { margin: 0; color: #832d25; overflow-wrap: anywhere; font-size: .88rem; }
.card-inline-actions { display: flex; gap: .35rem; }
.card-inline-actions button { min-height: 2.1rem; font-size: .85rem; }
.text-button { text-align: left; background: none; color: #245640; }
.drop-lane { outline: 2px dashed #30654d; outline-offset: -3px; }
.drop-card { box-shadow: 0 -3px #30654d; }
.panel-note { margin: 0; padding: .65rem 1rem; color: var(--muted); }
.graph-filter { display: flex; gap: .55rem; align-items: center; margin: 0; padding: .55rem 1rem; }
.graph-filter select { width: auto; }
.graph-empty { display: grid; place-items: center; min-height: 15rem; background: #edf2e8; margin: 0; padding: 1rem; text-align: center; color: var(--muted); }
.graph-list, .inspector-group { padding: .65rem 0; border-top: 1px solid var(--line); }
.graph-list { padding-inline: 1rem; }
.graph-list ul, .inspector-group ul, .picker-content ul { list-style: none; padding: 0; margin: .55rem 0 0; }
.graph-list li + li, .inspector-group li + li { border-top: 1px solid #e2e8dc; }
.graph-list li button { text-align: left; width: 100%; border: 0; }
.resources-workspace .inspector-panel { padding: 1rem; border-top: 1px solid var(--line); overflow-wrap: anywhere; }
.inspector-panel h2 { margin: .25rem 0 .65rem; font: 1.75rem/1.18 Georgia, serif; }
.inspector-panel blockquote, .notes-preview { background: #eef2e8; margin: .5rem 0 .85rem; padding: .55rem .8rem; overflow-wrap: anywhere; }
.notes-preview :deep(pre) { overflow: auto; }
.placement { margin: .7rem 0; }
.placement label { display: flex; align-items: center; flex-wrap: wrap; gap: .55rem; }
.placement select { width: auto; }
.inspector-group li { padding: .45rem .05rem; }
.inspector-group li small { display: block; }
.inspector-group .text-button { border: 0; padding-left: 0; }
.resource-views { display: flex; gap: .35rem; flex-wrap: wrap; }
.resource-views button { flex: 1; min-height: 2.7rem; }
.workspace-status { grid-column: 1 / -1; color: var(--muted); margin: 0; }
.workspace-status:empty { display: none; }
.notice { color: #832d25; padding: .45rem 1rem; }
.resource-dialog { width: min(37rem, calc(100vw - 1.5rem)); max-height: calc(100dvh - 1.5rem); color: inherit; background: var(--panel); border: 1px solid var(--line); border-radius: .6rem; padding: 0; }
.resource-dialog::backdrop { background: #152a2175; }
.picker-content, .draft-prompt { padding: .8rem 1rem; }
.picker-content li { display: grid; grid-template-columns: minmax(0, 1fr) max-content; gap: .25rem .55rem; padding: .45rem 0; border-top: 1px solid var(--line); }
.picker-content li button { border: 0; padding: .25rem 0; overflow-wrap: anywhere; }
.picker-content li small { grid-column: 1; }
.picker-content li select { grid-column: 2; grid-row: 1 / span 2; align-self: center; max-width: 10rem; }
.sent-text { white-space: pre-wrap; overflow-wrap: anywhere; }
.retire { color: #832d25; }
.danger { color: #832d25; border-color: #832d25; }
.inspector-actions { display: flex; flex-wrap: wrap; gap: .4rem; margin-top: .55rem; }
.edge-form { padding: 0; border: 0; background: none; }
.check { display: flex; }
@media (max-width: 850px) {
  .resources-workspace { grid-template-columns: minmax(0, 1fr); }
  .resources-workspace .inspector-panel { border-top: 0; }
}
@media (max-width: 530px) {
  .board { display: grid; grid-template-columns: minmax(0, 1fr); max-height: none; }
}
</style>
