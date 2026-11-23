<script setup lang="ts">
/* global HTMLElement, HTMLDetailsElement, HTMLSelectElement */
import { computed, nextTick, ref, shallowRef, watch } from 'vue';
import { Background, Controls, Handle, Position, MiniMap, VueFlow, type Edge, type Node, type VueFlowInstance } from '@xyflow/vue';
import { newOperationKey, parley, type GraphSnapshot, type SavedSource } from '../../api/client';

import RelationshipEditor from '../parties/RelationshipEditor.vue';
import { kindLabel } from '../parties/relations';

const props = defineProps<{ focusEntityId?: string; viewRootId?: string; rootName?: string; sources?: SavedSource[]; refresh?: number; editorOpen?: boolean; canOpen?: boolean }>();
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
  label: edge.from_id === props.viewRootId ? edge.verb.replaceAll('_', ' ') + ' →' : '← ' + edge.verb.replaceAll('_', ' '),
  labelStyle: { fill: '#365341', fontFamily: 'monospace', fontSize: 13 },
  labelBgStyle: { fill: '#eff3eb' }, style: { stroke: edge.tier === 2 ? '#8b6725' : '#71957f', strokeWidth: 1.6 },
})));
const selectedEdge = computed(() => snapshot.value.edges.find((edge) => edge.id === selectedEdgeId.value) ?? null);

async function load() {
  const ownGeneration = ++generation.value;
  const centerId = props.viewRootId;
  if (!centerId) {
    snapshot.value = { nodes: [], edges: [], omitted_nodes: 0, omitted_edges: 0 };
    loading.value = false;
    return;
  }
  loading.value = true; error.value = '';
  try {
    const viewRoot = props.viewRootId ?? props.focusEntityId ?? 'directory';
    const [result, savedSources] = await Promise.all([
      parley.getGraph({ center_id: centerId, view_scope: 'leads', view_id: viewRoot }),
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
  setAnchor(id);
  emit('select-node', id);
}
function setAnchor(id: string) {
  const node = Array.from(frame.value?.querySelectorAll('[data-id]') ?? []).find(el => el.getAttribute('data-id') === id);
  if (!node || !frame.value) return;
  const bounds = frame.value.getBoundingClientRect();
  const rect = node.getBoundingClientRect();
  anchor.value = {
    left: Math.max(6, Math.min(rect.left - bounds.left, bounds.width - 410)) + 'px',
    top: Math.max(6, Math.min(rect.top - bounds.top, bounds.height - 360)) + 'px',
  };
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
defineExpose({ openRelated, inspectEdge, reload: load });
watch(() => props.focusEntityId, id => { if (id) relatedEntityId.value = id; });
watch(() => props.viewRootId, () => {
  snapshot.value = { nodes: [], edges: [], omitted_nodes: 0, omitted_edges: 0 };
  selectedEdgeId.value = '';
});
watch(() => [props.viewRootId, props.refresh], () => { void load(); }, { immediate: true });
watch(() => props.sources, current => { if (current) savedSourcesForGraph.value = current; });
</script>

<template>
  <section
    class="network-pane"
    aria-label="Party network"
  >
    <header class="pane-bar">
      <h2 id="relationship-graph-heading">
        {{ rootName ? rootName + ' · neighborhood' : 'Party network' }}
      </h2>
      <button
        v-if="focusEntityId && canOpen !== false"
        type="button"
        @click="setAnchor(focusEntityId); emit('open-card')"
      >
        Open selected card
      </button>
    </header>
    <p
      v-if="loading"
      role="status"
    >
      Loading the current graph neighborhood…
    </p>
    <p
      v-if="error"
      role="alert"
    >
      {{ error }}
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
        tabindex="0"
        aria-label="Relationship graph canvas"
        data-testid="relationship-canvas"
      >
        <VueFlow
          v-if="viewRootId && nodes.length"
          :key="viewRootId"
          :nodes="nodes"
          :edges="edges"
          :fit-view="true"
          :fit-view-options="{ padding: 0.12, maxZoom: 1.35 }"
          :nodes-draggable="!layoutBusy"
          :nodes-connectable="false"
          elements-selectable
          aria-label="Relationship graph"
          @node-drag-stop="savePosition"
          @init="instance => flow = instance"
          @node-click="({ node }) => focusNode(node.id)"
          @edge-click="({ edge }) => inspectEdge(edge.id)"
        >
          <template #node-card="{ id, data }">
            <div
              :class="['graph-card', { home: data.root, inspecting: data.selected }]"
              @keydown.enter.stop="focusNode(id)"
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
          <Controls />
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
      <slot name="summary" />
      <div class="pane-bar">
        <span>{{ nodes.length }} visible · {{ edges.length }} relationships</span>
      </div>
      <article
        v-if="selectedEdge"
        class="evidence-strip"
      >
        <strong>{{ selectedEdge.verb }}</strong><blockquote>{{ selectedEdge.quote }}</blockquote>
      </article>
      <details
        ref="relations"
        class="graph-inspector"
      >
        <summary>Relationships and evidence</summary><h4>Relationship list</h4>
        <p v-if="!snapshot.edges.length">
          No accepted relationships in this neighborhood yet. A name or search result does not create a relationship.
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
 .network-pane { min-width:0; overflow:hidden; }
.graph-inspector, .evidence-strip { padding:.7rem 1rem; }
.canvas-frame { position:relative; }
.anchored-editor { position:absolute; z-index:5; width:min(400px,calc(100% - 12px)); max-height:calc(100% - 3rem); overflow:auto; border:2px solid #436b55; border-radius:8px; background:#fffdf7; box-shadow:0 5px 24px #223c3026; }
.empty-network { position:absolute; inset:42% 0 auto; text-align:center; pointer-events:none; }
.graph-card { display:grid; align-content:center; gap:2px; width:225px; min-height:60px; padding:8px; border:1px solid #acbeab; background:#e4ecdf; border-radius:5px; text-align:center; color:#233d30; }
.graph-card small { font:12px monospace; }
.graph-card strong { font:19px Georgia,serif; overflow-wrap:anywhere; }
.graph-card.home { border:2px solid #426b51; background:#d8e8db; }
.graph-card.inspecting { outline:2px solid #37694f; outline-offset:-2px; }
.graph-card :deep(.vue-flow__handle) { opacity:0; }
.graph-layout { display:block; }
.canvas-frame { height:clamp(23rem,51vh,41rem); min-width:0; border-block:1px solid #d4dcd4; background:#eff3ed; }
.canvas-frame:focus-visible { outline:3px solid #32735b; outline-offset:2px; }
blockquote { margin:.5rem 0; padding:.5rem .75rem; border-left:3px solid #32735b; }
@media(max-width:767px) { .graph-layout { grid-template-columns:1fr; } .canvas-frame { height:22rem; } }
</style>
