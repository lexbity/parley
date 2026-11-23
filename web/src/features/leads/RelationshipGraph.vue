<script setup lang="ts">
/* global HTMLElement, HTMLDetailsElement, HTMLSelectElement, KeyboardEvent, ResizeObserver */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from 'vue';
import { Background, Controls, Handle, Position, MiniMap, VueFlow, type Edge, type Node, type VueFlowInstance } from '@xyflow/vue';
import { newOperationKey, parley, type GraphSnapshot, type SavedSource } from '../../api/client';

import RelationshipEditor from '../parties/RelationshipEditor.vue';
import { kindLabel } from '../parties/relations';

const props = defineProps<{ focusEntityId?: string; viewRootId?: string; rootName?: string; sources?: SavedSource[]; refresh?: number; editorOpen?: boolean; canOpen?: boolean; active?: boolean }>();
const emit = defineEmits<{ 'select-node': [id: string]; 'select-edge': [id: string]; 'related-saved': []; 'nodes-loaded': [nodes: GraphSnapshot['nodes']]; 'open-card': []; 'inspect-edge': [edge: GraphSnapshot['edges'][number]] }>();
const snapshot = ref<GraphSnapshot>({ nodes: [], edges: [], omitted_nodes: 0, omitted_edges: 0 });
const flow = shallowRef<VueFlowInstance | null>(null);
const selectedEdgeId = ref('');
const loading = ref(false);
const error = ref('');
const generation = ref(0);
const savedSourcesForGraph = ref<SavedSource[]>([]);
const relatedEntityId = ref('');
const relationStatus = ref('');
const relationForm = ref(true);
const correction = ref<GraphSnapshot['edges'][number] | null>(null);
const layoutBusy = ref(false);
const frame = ref<HTMLElement | null>(null);
const relations = ref<HTMLDetailsElement | null>(null);
const anchor = ref({ left: '0.6rem', top: '0.6rem' });
let initializedRoot: string | undefined;
let resizeObserver: ResizeObserver | undefined;
const nodes = computed<Node[]>(() => {
  const neighbors = snapshot.value.nodes.filter(node => node.id !== props.viewRootId);
  return snapshot.value.nodes.map(node => {
    const root = node.id === props.viewRootId;
    const defaultPosition = root ? { x: 0, y: Math.max(0, neighbors.length - 1) * 54 } : { x: 405, y: neighbors.indexOf(node) * 108 };
    return {
      id: node.id, type: 'card', position: node.position_revision ? { x: node.x, y: node.y } : defaultPosition,
      data: { label: node.name, kind: kindLabel(node.kind), root, selected: node.id === props.focusEntityId },
      ariaLabel: node.name + ', ' + kindLabel(node.kind), selectable: true, draggable: !layoutBusy.value,
    };
  });
});
const edges = computed<Edge[]>(() => snapshot.value.edges.map(edge => ({
  id: edge.id, type: 'straight', source: edge.from_id, target: edge.to_id,
  label: edge.verb.replaceAll('_', ' ') + (edge.from_id === props.viewRootId ? ' →' : ''),
  markerEnd: { type: 'arrowclosed', color: edge.tier === 2 ? '#8b6725' : '#71957f' },
  labelStyle: { fill: '#365341', fontFamily: 'monospace', fontSize: 13 },
  labelBgStyle: { fill: '#eff3eb' }, style: { stroke: edge.tier === 2 ? '#8b6725' : '#71957f', strokeWidth: 1.6 },
})));
const selectedEdge = computed(() => snapshot.value.edges.find((edge) => edge.id === selectedEdgeId.value) ?? null);

async function load() {
  const ownGeneration = ++generation.value;
  const centerId = props.viewRootId;
  if (!centerId) {
    snapshot.value = { nodes: [], edges: [], omitted_nodes: 0, omitted_edges: 0 };
    emit('nodes-loaded', []);
    loading.value = false; error.value = '';
    return;
  }
  loading.value = true; error.value = '';
  try {
    const [result, savedSources] = await Promise.all([
      parley.getGraph({ center_id: centerId, view_scope: 'leads', view_id: centerId }),
      props.sources ?? parley.listSources(),
    ]);
    if (ownGeneration !== generation.value || centerId !== props.viewRootId) return;
    snapshot.value = result;
    savedSourcesForGraph.value = savedSources;
    emit('nodes-loaded', result.nodes);
    if (!relatedEntityId.value || !result.nodes.some((node) => node.id === relatedEntityId.value)) relatedEntityId.value = centerId ?? result.nodes[0]?.id ?? '';
    if (!result.edges.some((edge) => edge.id === selectedEdgeId.value)) selectedEdgeId.value = '';
  } catch (reason) {
    if (ownGeneration === generation.value) error.value = reason instanceof Error ? reason.message : 'Could not load relationship evidence.';
  } finally { if (ownGeneration === generation.value) loading.value = false; }
}

async function relatedSaved(id: string) {
  await load();
  relationStatus.value = correction.value ? 'Relationship corrected.' : 'The entity and source-attributed directed relationship were saved together.';
  correction.value = null;
  relationForm.value = false;
  emit('related-saved');
  focusNode(id);
}
async function savePosition(event: { node: { id: string; position: { x: number; y: number } } }) {
  const current = snapshot.value.nodes.find((node) => node.id === event.node.id);
  if (!current || layoutBusy.value) return;
  layoutBusy.value = true; error.value = '';
  try {
    await parley.saveGraphLayout({
      view_scope: 'leads', view_id: props.viewRootId ?? props.focusEntityId ?? 'directory',
      positions: [{ entity_id: current.id, x: event.node.position.x, y: event.node.position.y, expected_revision: current.position_revision }],
    }, newOperationKey());
    await load();
  } catch (reason) {
    error.value = reason instanceof Error ? `${reason.message} The saved layout is unchanged; reload the graph before retrying.` : 'Layout could not be saved. The saved layout is unchanged.';
    await load();
  } finally { layoutBusy.value = false; }
}

function focusNode(id: string) {
  selectedEdgeId.value = '';
  emit('select-node', id);
}
function anchorTo(id: string) {
  const node = Array.from(frame.value?.querySelectorAll('[data-id]') ?? []).find(el => el.getAttribute('data-id') === id);
  if (!node || !frame.value) return;
  const bounds = frame.value.getBoundingClientRect();
  const rect = node.getBoundingClientRect();
  anchor.value = {
    left: Math.max(8, Math.min(rect.left - bounds.left, bounds.width - 408)) + 'px',
    top: Math.max(8, Math.min(rect.top - bounds.top, bounds.height - 320)) + 'px',
  };
}
function alignEditor() {
  if (props.editorOpen && props.focusEntityId) anchorTo(props.focusEntityId);
}
function viewportKey(root: string) {
  return parley.workspaceEpoch && `parley.parties.graph.${parley.workspaceId}.${parley.workspaceEpoch}.${root}`;
}
async function restoreViewport() {
  const instance = flow.value;
  const root = props.viewRootId;
  if (props.active === false || !root || initializedRoot === root || !instance || !frame.value?.clientWidth || !nodes.value.length) return;
  initializedRoot = root;
  try {
    const key = viewportKey(root);
    const saved = key ? JSON.parse(globalThis.sessionStorage.getItem(key) ?? 'null') : null;
    if (saved && [saved.x, saved.y, saved.zoom].every(Number.isFinite)) {
      await instance.setViewport(saved);
      return;
    }
  } catch { /* The graph can still fit the current neighborhood. */ }
  await instance.fitView({ nodes: nodes.value.slice(0, 8).map(node => ({ id: node.id })), padding: .16, maxZoom: 1.15 });
}
function rememberViewport() {
  const root = props.viewRootId;
  if (!root || root !== initializedRoot || !flow.value || props.active === false) return;
  const key = viewportKey(root);
  if (!key) return;
  try { globalThis.sessionStorage.setItem(key, JSON.stringify(flow.value.getViewport())); } catch { /* Keep the live viewport. */ }
}
function openFocusedNode(event: KeyboardEvent) {
  const target = event.target;
  if (!(target instanceof HTMLElement)) return;
  const id = target.closest<HTMLElement>('.vue-flow__node')?.dataset.id ?? (target === frame.value ? props.focusEntityId : undefined);
  if (!id) return;
  event.preventDefault();
  event.stopPropagation();
  focusNode(id);
}
function inspectEdge(id: string) {
  const edge = snapshot.value.edges.find(item => item.id === id);
  if (edge) { selectedEdgeId.value = id; emit('inspect-edge', edge); }
}
function openRelated(edge?: GraphSnapshot['edges'][number]) {
  correction.value = edge ?? null;
  relationForm.value = true;
  if (!relations.value) return;
  relations.value.open = true;
  void nextTick(() => relations.value?.querySelector<HTMLSelectElement>('form select')?.focus());
}
defineExpose({ anchorTo, openRelated, inspectEdge, reload: load });
watch(() => props.focusEntityId, id => { if (id) relatedEntityId.value = id; });
watch(() => props.viewRootId, () => {
  initializedRoot = undefined;
  flow.value = null;
  snapshot.value = { nodes: [], edges: [], omitted_nodes: 0, omitted_edges: 0 };
  emit('nodes-loaded', []);
  if (relations.value) relations.value.open = false;
  correction.value = null;
  relationStatus.value = '';
  relationForm.value = true;
  selectedEdgeId.value = '';
});
watch(() => [props.editorOpen, props.focusEntityId, props.active], async () => {
  await nextTick();
  alignEditor();
  void restoreViewport();
});
watch(() => [props.viewRootId, props.refresh], () => { void load(); }, { immediate: true });
watch(() => props.sources, current => { if (current) savedSourcesForGraph.value = current; });
onMounted(() => {
  if (frame.value && typeof ResizeObserver !== 'undefined') {
    resizeObserver = new ResizeObserver(() => { alignEditor(); void restoreViewport(); });
    resizeObserver.observe(frame.value);
  }
});
onBeforeUnmount(() => { generation.value++; resizeObserver?.disconnect(); });
</script>

<template>
  <section
    class="network-pane"
    aria-label="Party network"
  >
    <header class="pane-bar">
      <h2
        id="relationship-graph-heading"
        tabindex="-1"
      >
        {{ rootName ? rootName + ' · neighborhood' : 'Party network' }}
      </h2>
      <button
        v-if="focusEntityId && canOpen !== false"
        type="button"
        @click="emit('open-card')"
      >
        Open selected card
      </button>
    </header>
    <p
      v-if="loading && !nodes.length"
      role="status"
      class="graph-notice"
    >
      Loading the current graph neighborhood…
    </p>
    <p
      v-if="error"
      role="alert"
      class="graph-notice"
    >
      {{ error }}
      <button
        type="button"
        @click="load"
      >
        Retry
      </button>
    </p>
    <p v-if="snapshot.omitted_nodes">
      Showing a bounded neighborhood; {{ snapshot.omitted_nodes }} or more nodes were omitted. Narrow the directory selection.
    </p>
    <p v-if="snapshot.omitted_edges">
      Showing the first 400 relationships; {{ snapshot.omitted_edges }} more are omitted.
    </p>
    <div class="graph-layout">
      <div
        ref="frame"
        class="canvas-frame"
        :class="{ editing: editorOpen }"
        tabindex="0"
        aria-label="Relationship graph canvas"
        data-testid="relationship-canvas"
        @keydown.enter.capture="openFocusedNode"
      >
        <VueFlow
          v-if="viewRootId && nodes.length"
          :key="viewRootId"
          :nodes="nodes"
          :edges="edges"
          :nodes-draggable="!layoutBusy && !editorOpen"
          :nodes-connectable="false"
          :delete-key-code="null"
          :zoom-on-scroll="false"
          aria-label="Relationship graph"
          @node-drag-stop="savePosition"
          @init="instance => flow = instance"
          @nodes-initialized="restoreViewport"
          @move="alignEditor"
          @move-end="rememberViewport"
          @node-click="({ node }) => focusNode(node.id)"
          @edge-click="({ edge }) => inspectEdge(edge.id)"
        >
          <template #node-card="{ data }">
            <div
              :class="['graph-card', { home: data.root, inspecting: data.selected }]"
            >
              <Handle
                type="target"
                :position="data.root ? Position.Right : Position.Left"
              />
              <small>{{ data.kind }}</small><strong>{{ data.label }}</strong>
              <Handle
                type="source"
                :position="data.root ? Position.Right : Position.Left"
              />
            </div>
          </template>
          <Background
            color="#bbcbbb"
            :gap="16"
          />
          <Controls :show-interactive="false" />
          <MiniMap
            v-if="nodes.length > 12"
            pannable
            zoomable
          />
        </VueFlow>
        <div
          v-if="editorOpen"
          class="anchored-editor"
          :style="anchor"
        >
          <slot name="editor" />
        </div>
        <p
          v-if="!viewRootId"
          class="empty-network"
        >
          Select a party to see its neighborhood.
        </p>
      </div>
      <details
        v-if="viewRootId"
        ref="relations"
        class="graph-inspector"
      >
        <summary>Relationships and evidence <span class="graph-count">{{ edges.length }}</span></summary>
        <h4>Relationship list</h4>
        <p v-if="!snapshot.edges.length">
          No accepted relationships in this neighborhood yet.
        </p>
        <ul aria-label="Attributed relationships">
          <li
            v-for="edge in snapshot.edges"
            :key="edge.id"
          >
            <button
              type="button"
              :aria-pressed="selectedEdgeId === edge.id"
              @click="inspectEdge(edge.id)"
            >
              {{ snapshot.nodes.find((node) => node.id === edge.from_id)?.name }} {{ edge.verb }} {{ snapshot.nodes.find((node) => node.id === edge.to_id)?.name }}
            </button>
            <small> · T{{ edge.tier }} · {{ edge.speaker_kind }} · {{ edge.captured_at }}</small>
          </li>
        </ul>
        <article
          v-if="selectedEdge"
          aria-label="Selected relationship evidence"
        >
          <strong>{{ snapshot.nodes.find((node) => node.id === selectedEdge?.from_id)?.name }} → {{ selectedEdge.verb }} → {{ snapshot.nodes.find((node) => node.id === selectedEdge?.to_id)?.name }}</strong>
          <p>Speaker: {{ selectedEdge.speaker_kind }} · Tier {{ selectedEdge.tier }} · {{ selectedEdge.captured_at }}</p>
          <blockquote>{{ selectedEdge.quote }}</blockquote>
          <p>{{ selectedEdge.source_title ?? 'Captured source' }}<span v-if="selectedEdge.source_url"> · {{ selectedEdge.source_url }}</span></p>
          <p v-if="selectedEdge.tier === 1">
            The source supports that this was stated; it does not independently verify the interpretation.
          </p>
        </article>
        <h4>Entities in this view</h4>
        <ul aria-label="Graph entity list">
          <li
            v-for="node in snapshot.nodes"
            :key="node.id"
          >
            <button
              type="button"
              :aria-pressed="relatedEntityId === node.id"
              @click="focusNode(node.id)"
            >
              {{ node.name }} · {{ node.kind }}
            </button>
          </li>
        </ul>
        <template v-if="parley.supports('party.relations') && viewRootId">
          <RelationshipEditor
            v-if="relationForm"
            :key="correction?.id ?? viewRootId"
            :root="viewRootId"
            :selected="focusEntityId"
            :nodes="snapshot.nodes"
            :sources="savedSourcesForGraph"
            :correction="correction"
            @saved="relatedSaved"
            @cancel="relationForm = false; correction = null"
          />
          <button
            v-else
            @click="openRelated()"
          >
            Add or link a card
          </button>
          <p
            v-if="relationStatus"
            role="status"
          >
            {{ relationStatus }}
          </p>
        </template>
      </details>
    </div>
  </section>
</template>

<style scoped>
.network-pane { min-width: 0; overflow: hidden; }
.network-pane .pane-bar { flex-wrap: wrap; border-bottom: 0; }
.network-pane .pane-bar button { min-height: 2.5rem; }
.graph-inspector, .graph-notice { padding: .7rem 1rem; }
.graph-inspector summary { font-weight: 600; }
.graph-count { margin-left: .25rem; font-weight: 400; color: #58665f; }
.canvas-frame { position: relative; }
.anchored-editor { position:absolute; z-index:5; width:min(400px,calc(100% - 12px)); max-height:calc(100% - 3rem); overflow:auto; border:2px solid #436b55; border-radius:8px; background:#fffdf7; box-shadow:0 5px 24px #223c3026; }
.empty-network { position:absolute; inset:42% 0 auto; padding: 0 1rem; text-align:center; pointer-events:none; }
.graph-card { display:grid; align-content:center; gap:2px; width:225px; min-height:60px; padding:8px; border:1px solid #acbeab; background:#e4ecdf; border-radius:5px; text-align:center; color:#233d30; }
.graph-card small { font:12px monospace; }
.graph-card strong { font:19px Georgia,serif; overflow-wrap:anywhere; }
.graph-card.home { border:2px solid #426b51; background:#d8e8db; }
.graph-card.inspecting { outline:2px solid #37694f; outline-offset:-2px; }
.graph-card :deep(.vue-flow__handle) { opacity:0; }
.canvas-frame { height: clamp(21rem, 49vh, 37rem); min-width:0; border-block:1px solid #d4dcd4; background:#eef2e8; }
.canvas-frame:focus-visible { outline: 3px solid #32735b; outline-offset: -3px; }
blockquote { margin:.5rem 0; padding:.5rem .75rem; border-left:3px solid #32735b; }
@media (max-width: 767px) {
  .canvas-frame { height: clamp(17rem, 48dvh, 23rem); }
  .canvas-frame.editing { height: auto; }
  .canvas-frame.editing :deep(.vue-flow) { display: none; }
  .anchored-editor { position: static; width: auto; max-height: none; border: 0; border-radius: 0; box-shadow: none; }
}
</style>
