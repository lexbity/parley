<script setup lang="ts">
/* global document, HTMLElement */
import { computed, onMounted, ref, watch } from 'vue';
import { Background, Controls, MiniMap, VueFlow, type Edge, type Node } from '@xyflow/vue';
import { newOperationKey, parley, type GraphSnapshot, type SavedSource } from '../../api/client';

const props = defineProps<{ focusEntityId?: string }>();
const snapshot = ref<GraphSnapshot>({ nodes: [], edges: [], omitted_nodes: 0, omitted_edges: 0 });
const selectedEdgeId = ref('');
const loading = ref(false);
const error = ref('');
const generation = ref(0);
const sources = ref<SavedSource[]>([]);
const relatedEntityId = ref('');
const relatedKind = ref<'person' | 'project' | 'resource'>('person');
const relatedName = ref('');
const occurrenceId = ref('');
const quote = ref('');
const mutationBusy = ref(false);
const relationStatus = ref('');
const relationError = ref('');
const layoutBusy = ref(false);
const nodes = computed<Node[]>(() => snapshot.value.nodes.map((node) => ({
  id: node.id, type: 'default', position: { x: node.x, y: node.y }, data: { label: `${node.name}\n${node.kind}` },
  selectable: true, draggable: !layoutBusy.value,
})));
const edges = computed<Edge[]>(() => snapshot.value.edges.map((edge) => ({
  id: edge.id, source: edge.from_id, target: edge.to_id, label: edge.verb, animated: false,
  data: { evidenceId: edge.occurrence_id, quote: edge.quote, speaker: edge.speaker_kind, tier: edge.tier },
  labelStyle: { fill: '#18221f', fontWeight: 600 },
  style: { stroke: edge.tier === 2 ? '#8b6725' : '#326c53', strokeWidth: 2 },
})));
const selectedEdge = computed(() => snapshot.value.edges.find((edge) => edge.id === selectedEdgeId.value) ?? null);

async function load() {
  const ownGeneration = ++generation.value;
  const centerId = props.focusEntityId;
  loading.value = true; error.value = '';
  try {
    const [result, savedSources] = await Promise.all([
      parley.getGraph({ center_id: centerId, view_scope: 'leads', view_id: centerId ?? 'directory' }),
      parley.listSources(),
    ]);
    if (ownGeneration !== generation.value || centerId !== props.focusEntityId) return;
    snapshot.value = result;
    sources.value = savedSources;
    if (!relatedEntityId.value || !result.nodes.some((node) => node.id === relatedEntityId.value)) relatedEntityId.value = centerId ?? result.nodes[0]?.id ?? '';
    if (!result.edges.some((edge) => edge.id === selectedEdgeId.value)) selectedEdgeId.value = '';
  } catch (reason) {
    if (ownGeneration === generation.value) error.value = reason instanceof Error ? reason.message : 'Could not load relationship evidence.';
  } finally { if (ownGeneration === generation.value) loading.value = false; }
}

async function addRelatedEntity() {
  const parent = snapshot.value.nodes.find((node) => node.id === relatedEntityId.value);
  const source = sources.value.find((item) => item.occurrence_id === occurrenceId.value);
  if (!parent || !source) { relationError.value = 'Choose an existing related entity and a saved source.'; return; }
  const allowed = (relatedKind.value === 'person' && parent.kind === 'org')
    || (relatedKind.value === 'project' && parent.kind === 'org')
    || (relatedKind.value === 'resource' && parent.kind === 'person');
  if (!allowed) { relationError.value = 'The selected entity type cannot form this directed relationship. Choose an organization for a person/project or a person for a resource.'; return; }
  mutationBusy.value = true; relationError.value = ''; relationStatus.value = '';
  try {
    const verb = relatedKind.value === 'person' ? 'member_of' : relatedKind.value === 'project' ? 'works_with' : 'offers_to';
    await parley.createRelatedEntity({
      kind: relatedKind.value, name: relatedName.value.trim(), related_entity_id: parent.id, verb,
      occurrence_id: source.occurrence_id, quote: quote.value,
      view_scope: 'leads', view_id: props.focusEntityId ?? 'directory', x: parent.x + 260, y: parent.y,
    }, newOperationKey());
    relatedName.value = ''; quote.value = '';
    await load();
    relationStatus.value = 'The entity and source-attributed directed relationship were saved together.';
  } catch (reason) { relationError.value = reason instanceof Error ? reason.message : 'Could not save the related entity and edge.'; }
  finally { mutationBusy.value = false; }
}

async function savePosition(event: { node: { id: string; position: { x: number; y: number } } }) {
  const current = snapshot.value.nodes.find((node) => node.id === event.node.id);
  if (!current || layoutBusy.value) return;
  layoutBusy.value = true; error.value = '';
  try {
    await parley.saveGraphLayout({
      view_scope: 'leads', view_id: props.focusEntityId ?? 'directory',
      positions: [{ entity_id: current.id, x: event.node.position.x, y: event.node.position.y, expected_revision: current.position_revision }],
    }, newOperationKey());
    await load();
  } catch (reason) {
    error.value = reason instanceof Error ? `${reason.message} The saved layout is unchanged; reload the graph before retrying.` : 'Layout could not be saved. The saved layout is unchanged.';
    await load();
  } finally { layoutBusy.value = false; }
}

function focusNode(id: string) {
  const node = snapshot.value.nodes.find((candidate) => candidate.id === id);
  if (!node) return;
  const canvas = document.querySelector<HTMLElement>('[data-testid="relationship-canvas"]');
  canvas?.focus();
}

watch(() => props.focusEntityId, () => { void load(); });
onMounted(() => { void load(); });
</script>

<template>
  <section aria-labelledby="relationship-graph-heading">
    <h3 id="relationship-graph-heading">
      Relationship graph and evidence list
    </h3>
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
        class="canvas-frame"
        tabindex="0"
        aria-label="Relationship graph canvas"
        data-testid="relationship-canvas"
      >
        <VueFlow
          :nodes="nodes"
          :edges="edges"
          fit-view-on-init
          :nodes-draggable="!layoutBusy"
          :nodes-connectable="false"
          elements-selectable
          aria-label="Relationship graph"
          @node-drag-stop="savePosition"
          @edge-click="(_, edge) => selectedEdgeId = edge.id"
        >
          <Background />
          <Controls />
          <MiniMap
            pannable
            zoomable
          />
        </VueFlow>
      </div>
      <div>
        <h4>Relationship list</h4>
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
              @click="selectedEdgeId = edge.id"
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
              @click="focusNode(node.id)"
            >
              {{ node.name }} · {{ node.kind }}
            </button>
          </li>
        </ul>
        <form @submit.prevent="addRelatedEntity">
          <h4>Add a source-backed related entity</h4>
          <label>Related to<select
            v-model="relatedEntityId"
            required
          ><option
            v-for="node in snapshot.nodes"
            :key="node.id"
            :value="node.id"
          >{{ node.name }} · {{ node.kind }}</option></select></label>
          <label>New entity type<select v-model="relatedKind"><option value="person">Person (member of organization)</option><option value="project">Project (works with organization)</option><option value="resource">Resource (offered to person)</option></select></label>
          <label>Name<input
            v-model="relatedName"
            required
            maxlength="240"
            autocomplete="off"
          ></label>
          <label>Saved source<select
            v-model="occurrenceId"
            required
          ><option value="">Choose a source</option><option
            v-for="source in sources"
            :key="source.occurrence_id"
            :value="source.occurrence_id"
          >Source {{ source.content_id.slice(0, 12) }} · occurrence {{ source.occurrence_id.slice(0, 8) }}</option></select></label>
          <label>Exact supporting quote<textarea
            v-model="quote"
            required
            minlength="1"
            rows="3"
          /></label>
          <button
            type="submit"
            :disabled="mutationBusy || !snapshot.nodes.length || !sources.length"
          >
            Save related entity and relationship
          </button>
          <p>The service verifies the quote and commits the node, edge, evidence and initial position atomically.</p>
          <p
            v-if="relationStatus"
            role="status"
          >
            {{ relationStatus }}
          </p>
          <p
            v-if="relationError"
            role="alert"
          >
            {{ relationError }}
          </p>
        </form>
      </div>
    </div>
  </section>
</template>

<style scoped>
.graph-layout { display:grid; grid-template-columns:minmax(0, 3fr) minmax(16rem, 2fr); gap:1rem; }
.canvas-frame { height:32rem; min-width:0; border:1px solid #82918a; border-radius:.4rem; background:#fff; }
.canvas-frame:focus-visible { outline:3px solid #32735b; outline-offset:2px; }
blockquote { margin:.5rem 0; padding:.5rem .75rem; border-left:3px solid #32735b; }
@media(max-width:767px) { .graph-layout { grid-template-columns:1fr; } .canvas-frame { height:22rem; } }
</style>
