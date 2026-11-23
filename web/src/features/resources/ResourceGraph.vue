<script setup lang="ts">
/* global HTMLElement, ResizeObserver, KeyboardEvent */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from 'vue';
import { Background, Controls, Handle, MarkerType, Position, VueFlow, type Edge, type Node, type VueFlowInstance } from '@xyflow/vue';
import { parley, type GraphSnapshot } from '../../api/client';

export type PendingRelationship = { parentId: string; title: string; kind: string; verb: string; incoming: boolean };
type GraphNode = GraphSnapshot['nodes'][number];
const props = defineProps<{
  rootId: string;
  nodes: GraphNode[];
  edges: GraphSnapshot['edges'];
  inspectedId: string | null;
  edgeId: string | null;
  editing: boolean;
  pending?: PendingRelationship | null;
  busy: boolean;
  active: boolean;
  canAddRelationship: (node: GraphNode) => boolean;
  removing: boolean;
  labelForNode: (node: GraphNode) => string;
}>();
const emit = defineEmits<{
  activate: [id: string];
  'inspect-edge': [id: string];
  'add-relationship': [id: string];
  'remove-relationship': [id: string];
  position: [node: { id: string; position: { x: number; y: number } }];
}>();
const frame = ref<HTMLElement | null>(null);
const editor = ref<HTMLElement | null>(null);
const flow = shallowRef<VueFlowInstance | null>(null);
const anchor = ref({ x: 8, y: 8 });
const positions = new Map<string, { x: number; y: number }>();
let initializedRoot: string | null = null;
let observer: ResizeObserver | undefined;
const nodes = computed<Node[]>(() => {
  const removable = new Set<string>();
  for (const edge of props.edges) {
    if (!edge.owner_authored || edge.record) continue;
    if (edge.from_id === props.rootId) removable.add(edge.to_id);
    if (edge.to_id === props.rootId) removable.add(edge.from_id);
  }
  const neighbors = props.nodes.filter(node => node.id !== props.rootId);
  const result: Node[] = props.nodes.map(node => {
    const root = node.id === props.rootId;
    const index = neighbors.indexOf(node);
    const fallback = root
      ? { x: 0, y: Math.max(0, Math.min(neighbors.length, 7) - 1) * 47 }
      : { x: 360 + Math.floor(index / 7) * 285, y: (index % 7) * 94 };
    if (!positions.has(node.id)) {
      const occupied = [...positions.values()];
      const position = { ...fallback };
      while (!root && occupied.some(point => point.x === position.x && point.y === position.y)) {
        position.y += 94;
        if (position.y >= 7 * 94) { position.x += 285; position.y = 0; }
      }
      positions.set(node.id, position);
    }
    return {
      id: node.id, type: 'resource-card',
      position: node.position_revision ? { x: node.x, y: node.y } : positions.get(node.id) ?? fallback,
      data: {
        title: node.name, kind: props.labelForNode(node), root, party: ['person', 'org', 'project'].includes(node.kind),
        canAdd: props.canAddRelationship(node), canRemove: !root && removable.has(node.id),
      },
      selected: props.inspectedId === node.id && !props.edgeId,
      ariaLabel: `${node.name}, ${props.labelForNode(node)}`,
      draggable: !props.busy && node.kind !== 'material' && node.kind !== 'opportunity',
    };
  });
  const draft = props.pending;
  const parent = draft && result.find(node => node.id === draft.parentId);
  if (draft && parent) {
    result.push({
      id: 'pending-resource', type: 'resource-card',
      position: { x: parent.position.x + (draft.parentId === props.rootId ? 360 : 240), y: parent.position.y + 96 },
      data: { title: draft.title, kind: draft.kind, pending: true, party: ['Person', 'Organization'].includes(draft.kind) },
      selectable: false, draggable: false, focusable: false,
    });
  }
  return result;
});
const edges = computed<Edge[]>(() => {
  const result: Edge[] = props.edges.map(edge => ({
  id: edge.id, type: 'straight', source: edge.from_id, target: edge.to_id,
  label: edge.verb.replaceAll('_', ' '), markerEnd: { type: MarkerType.ArrowClosed, width: 16, height: 16, color: '#607f69' },
  selected: props.edgeId === edge.id,
  style: { stroke: props.edgeId === edge.id ? '#245742' : '#78947f', strokeWidth: props.edgeId === edge.id ? 2.5 : 1.5 },
  labelStyle: { fill: '#294c38', fontFamily: 'monospace', fontSize: 13 },
  labelBgStyle: { fill: '#eaf0e6' },
  }));
  const draft = props.pending;
  if (draft && props.nodes.some(node => node.id === draft.parentId)) {
    result.push({
      id: 'pending-resource-edge', type: 'straight', label: draft.verb,
      source: draft.incoming ? 'pending-resource' : draft.parentId,
      target: draft.incoming ? draft.parentId : 'pending-resource',
      style: { stroke: '#7e6845', strokeDasharray: '5 4' },
      selectable: false, focusable: false,
    });
  }
  return result;
});
function viewportKey(root: string) {
  const epoch = parley.workspaceEpoch;
  return epoch && `parley.resources.graph.${parley.workspaceId}.${epoch}.${root}`;
}
async function restoreViewport() {
  if (!props.active || !flow.value || initializedRoot === props.rootId || !frame.value?.clientWidth || !nodes.value.length) return;
  const instance = flow.value;
  const root = props.rootId;
  const key = viewportKey(root);
  initializedRoot = root;
  try {
    const previous = key ? JSON.parse(globalThis.sessionStorage.getItem(key) ?? 'null') : null;
    if (previous && [previous.x, previous.y, previous.zoom].every(Number.isFinite)) {
      await instance.setViewport(previous);
      return;
    }
  } catch { /* Fit the graph when the saved viewport is unavailable. */ }
  if (nodes.value.length === 1) await instance.fitView({ padding: .32, maxZoom: 1.1 });
  else if (nodes.value.length <= 8) await instance.fitView({ padding: .16, maxZoom: 1.1 });
  else await instance.fitView({ nodes: nodes.value.slice(0, 8).map(node => ({ id: node.id })), padding: .16, maxZoom: 1.1 });
}
function rememberViewport() {
  const key = viewportKey(props.rootId);
  if (!key || initializedRoot !== props.rootId || !props.active || !flow.value) return;
  try { globalThis.sessionStorage.setItem(key, JSON.stringify(flow.value.getViewport())); }
  catch { /* The current canvas still has its viewport. */ }
}
function alignEditor() {
  if (!props.editing || !frame.value || !editor.value) return;
  const bounds = frame.value.getBoundingClientRect();
  const node = Array.from(frame.value.querySelectorAll<HTMLElement>('.vue-flow__node')).find(item => item.dataset.id === props.inspectedId);
  const rect = node?.getBoundingClientRect();
  const width = editor.value.offsetWidth;
  anchor.value = {
    x: Math.max(8, Math.min((rect?.left ?? bounds.left) - bounds.left, bounds.width - width - 8)),
    y: Math.max(8, Math.min((rect?.top ?? bounds.top) - bounds.top, bounds.height - 295)),
  };
}
function cancelEditor() { editor.value?.querySelector<HTMLElement>('.editor-actions button[type=button]')?.click(); }
function onViewport() { alignEditor(); rememberViewport(); }
function editFocusedNode(event: KeyboardEvent) {
  const target = event.target;
  if (!(target instanceof HTMLElement) || target.closest('.node-actions, .node-editor')) return;
  const id = target.closest<HTMLElement>('.vue-flow__node')?.dataset.id;
  if (id) { event.preventDefault(); event.stopPropagation(); emit('activate', id); }
  else if (target === frame.value && props.inspectedId) emit('activate', props.inspectedId);
}
watch(() => props.rootId, () => { positions.clear(); initializedRoot = null; });
watch(() => [props.editing, props.inspectedId, props.active], async () => {
  await nextTick();
  alignEditor();
  void restoreViewport();
});
onMounted(() => {
  if (frame.value && typeof ResizeObserver !== 'undefined') {
    observer = new ResizeObserver(() => { alignEditor(); void restoreViewport(); });
    observer.observe(frame.value);
  }
});
onBeforeUnmount(() => observer?.disconnect());
</script>

<template>
  <div
    ref="frame"
    class="canvas"
    tabindex="0"
    aria-label="Resource graph canvas"
    data-testid="resource-canvas"
    @keydown.enter.capture="editFocusedNode"
  >
    <VueFlow
      :key="rootId"
      :nodes="nodes"
      :edges="edges"
      :min-zoom=".25"
      :max-zoom="2"
      :nodes-draggable="!busy && !editing"
      :nodes-connectable="false"
      :delete-key-code="null"
      :zoom-on-scroll="false"
      @init="instance => { flow = instance; restoreViewport(); }"
      @nodes-initialized="restoreViewport"
      @move-end="onViewport"
      @move="alignEditor"
      @node-click="({ node }) => emit('activate', node.id)"
      @edge-click="({ edge }) => emit('inspect-edge', edge.id)"
      @node-drag-stop="({ node }) => emit('position', node)"
    >
      <template #node-resource-card="{ id, data, selected }">
        <div
          class="graph-card"
          :class="{ root: data.root, party: data.party, selected, provisional: data.pending }"
        >
          <Handle
            type="target"
            :position="data.root ? Position.Right : Position.Left"
          />
          <div
            v-if="data.pending"
            class="preview-name"
          >
            <small>{{ data.kind }} · Unsaved</small><strong>{{ data.title }}</strong>
          </div>
          <button
            v-else
            type="button"
            tabindex="-1"
            class="node-title"
            :aria-label="'Open ' + data.title"
          >
            <small>{{ data.kind }}</small><strong>{{ data.title }}</strong>
          </button>
          <div
            v-if="!data.pending && (data.canAdd || data.canRemove)"
            class="node-actions nodrag nopan"
          >
            <button
              v-if="data.canAdd"
              type="button"
              :aria-label="'Add relationship from ' + data.title"
              :disabled="busy || removing"
              @click.stop="emit('add-relationship', id)"
              @keydown.stop
            >
              <span aria-hidden="true">+</span>
            </button>
            <button
              v-if="data.canRemove"
              type="button"
              :aria-label="'Remove relationship to ' + data.title"
              :disabled="busy || removing"
              @click.stop="emit('remove-relationship', id)"
              @keydown.stop
            >
              <span aria-hidden="true">−</span>
            </button>
          </div>
          <Handle
            type="source"
            :position="data.root ? Position.Right : Position.Left"
          />
        </div>
      </template>
      <Background
        color="#acbfac"
        :gap="16"
      />
      <Controls :show-interactive="false" />
    </VueFlow>
    <div
      v-if="editing"
      ref="editor"
      class="node-editor"
      :style="{ left: anchor.x + 'px', top: anchor.y + 'px' }"
      @keydown.esc.stop.prevent="cancelEditor"
    >
      <slot />
    </div>
  </div>
</template>

<style scoped>
.canvas {
  min-height: 21rem;
  height: clamp(24rem, 47vh, 33rem);
  position: relative;
  background: #eaf0e6;
}
.graph-card {
  width: 184px;
  min-height: 65px;
  background: #dfe9da;
  border: 1px solid #b2c3b0;
  border-radius: .42rem;
  display: flex;
  align-items: stretch;
  text-align: center;
  color: #213b2f;
}
.node-title, .graph-card .preview-name { display: grid; width: 100%; align-content: center; gap: .15rem; border: 0; background: transparent; padding: 8px 9px; }
.node-title { min-width: 0; }
.node-actions { flex: none; display: grid; align-content: center; gap: .2rem; padding: .2rem; }
.node-actions button { display: grid; place-items: center; padding: 0; width: 2rem; height: 2rem; font-size: 1.2rem; background: #fffefb; border: 1px solid #b2c3b0; }
.graph-card small { display: block; font: 12px ui-monospace, monospace; color: #344f40; }
.graph-card strong { display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; overflow-wrap: anywhere; font: 19px/1.15 Georgia, serif; }
.graph-card.root { background: #eee2c9; border-color: #927b53; border-width: 2px; }
.graph-card.provisional { border: 2px dashed #7e6845; }
.graph-card.party { background: #d7e7ea; }
.graph-card.selected { outline: 2px solid #30654d; outline-offset: 1px; }
.graph-card :deep(.vue-flow__handle) { opacity: 0; }
.canvas :deep(.vue-flow__node:focus-visible) { outline: 2px solid #245742; outline-offset: 4px; }
.node-editor {
  background: #fffdf7;
  border: 2px solid #456e56;
  border-radius: .55rem;
  width: min(30rem, calc(100% - 16px));
  max-height: calc(100% - 16px);
  overflow: auto;
  position: absolute;
  z-index: 6;
  box-shadow: 0 8px 28px #263f2d3d;
}
@media (max-width: 850px) {
  .canvas { height: clamp(22rem, 57dvh, 38rem); }
  .canvas:has(.node-editor) { height: auto; }
  .canvas:has(.node-editor) > :deep(.vue-flow) { height: 11rem; }
  .node-editor { position: relative; inset: auto !important; width: 100%; max-height: none; box-shadow: none; }
}
</style>
