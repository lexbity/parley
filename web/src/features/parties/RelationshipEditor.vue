<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { parley, newOperationKey, type GraphEntity, type GraphSnapshot, type SavedSource } from '../../api/client';
import PartyFiles from './PartyFiles.vue';
import MarkdownPreview from '../../shared/MarkdownPreview.vue';
import { relationshipTypes, kindLabel } from './relations';

const props = defineProps<{ root: string; selected?: string; nodes: GraphSnapshot['nodes']; sources: SavedSource[]; correction?: GraphSnapshot['edges'][number] | null }>();
const emit = defineEmits<{ saved: [id: string]; cancel: [] }>();
const parentId = ref('');
const mode = ref<'create'|'link'>('create');
const parent = computed(() => props.nodes.find(n => n.id === parentId.value));
const correcting = computed(() => !!props.correction);
const options = computed(() => relationshipTypes.flatMap<{ kind: GraphEntity['kind']; direction: 'outgoing' | 'incoming'; verb: string }>(type => {
  if (type.from === parent.value?.kind) return [{ kind: type.to, direction:'outgoing' as const, verb:type.verb }];
  if (type.to === parent.value?.kind) return [{ kind: type.from, direction:'incoming' as const, verb:type.verb }];
  return [];
}));
const kind = ref<GraphEntity['kind']>('person');
const kinds = computed(() => [...new Set(options.value.map(o => o.kind))]);
const verb = ref('');
const assertions = computed(() => options.value.filter(o => o.kind === kind.value));
const assertion = computed(() => assertions.value.find(o => o.verb === verb.value));
const targetId = ref('');
const targetName = ref('');
const search = ref('');
const candidates = ref<GraphEntity[]>([]);
const target = computed(() => candidates.value.find(p => p.id === targetId.value) ?? props.nodes.find(n => n.id === targetId.value));
const sourceId = ref('');
const quote = ref('');
const notes = ref('');
const staged = ref<string[]>([]);
const fileBusy = ref(false);
const busy = ref(false);
const error = ref('');
const formVersion = ref(0);
const displayName = computed(() => mode.value === 'create' ? targetName.value.trim() : target.value?.name);
const sentence = computed(() => {
  if (!parent.value || !displayName.value || !assertion.value) return '';
  const [a,b] = assertion.value.direction === 'outgoing' ? [parent.value.name, displayName.value] : [displayName.value, parent.value.name];
  return a + ' ' + verb.value.replaceAll('_',' ') + ' ' + b;
});
watch(() => props.selected, id => { if (!correcting.value) parentId.value = id ?? props.root; }, { immediate:true });
watch(() => props.correction, edge => {
  if (!edge) return;
  parentId.value = edge.from_id;
  mode.value = 'link';
  const other = props.nodes.find(n => n.id === edge.to_id);
  if (other) kind.value = other.kind;
  targetId.value = edge.to_id; verb.value = edge.verb; sourceId.value = edge.occurrence_id; quote.value = edge.quote;
}, {immediate:true});
watch(kinds, values => { if (values[0] && !values.some(v => v === kind.value)) kind.value = values[0]; }, {immediate:true});
watch(assertions, values => { if (!values.some(v => v.verb === verb.value)) verb.value = values[0]?.verb ?? ''; }, {immediate:true});
let lookup = 0;
watch([mode, kind, search], ([nextMode, nextKind, query], _, cleanup) => {
  const own = ++lookup;
  candidates.value = [];
  if (nextMode !== 'link') return;
  const timer = globalThis.setTimeout(async () => {
    try {
      const result = await parley.queryDirectory({ kind: nextKind, q: query.trim(), limit: 50 });
      if (own === lookup) candidates.value = result.rows;
    } catch (reason) {
      if (own === lookup) error.value = reason instanceof Error ? reason.message : 'Search unavailable.';
    }
  }, 180);
  cleanup(() => { globalThis.clearTimeout(timer); lookup++; });
}, { immediate: true });
const matches = computed(() => [...new Map([...props.nodes, ...candidates.value].map(n => [n.id,n])).values()].filter(n => n.kind === kind.value && n.id !== parentId.value && (n.id === targetId.value || n.name.toLowerCase().includes(search.value.toLowerCase()))));
let lastIntent = '';
let operationKey = '';
async function save() {
  if (busy.value || !parent.value || !assertion.value || fileBusy.value) return;
  const p = parent.value;
  const definition = assertion.value;
  const common = { verb: verb.value, occurrence_id:sourceId.value, quote:quote.value };
  function key(input: unknown) {
    const serialized = JSON.stringify(input);
    if (serialized !== lastIntent) { lastIntent = serialized; operationKey = newOperationKey(); }
    return operationKey;
  }
  error.value = ''; busy.value = true;
  try {
    let id: string;
    if (mode.value === 'create') {
      const input = { ...common, kind:kind.value, name:targetName.value.trim(), related_entity_id:p.id, expected_parent_revision:p.revision,
        direction:definition.direction, notes:notes.value, staged_upload_ids:staged.value, view_scope:'leads', view_id:props.root,
        x:405, y:Math.max(0,props.nodes.length-1)*108 };
      id = (await parley.createRelatedEntity(input, key(input))).id;
    } else {
      if (!target.value || target.value.kind !== kind.value) throw new Error('Choose an existing card.');
      const [from,to] = definition.direction === 'outgoing' ? [p,target.value] : [target.value,p];
      const input = { ...common, from_id:from.id, to_id:to.id, expected_from_revision:from.revision, expected_to_revision:to.revision, expected_revision:props.correction?.revision };
      await parley.saveEdge(props.correction?.id ?? null, input, key([props.correction?.id,input]));
      id = target.value.id;
    }
    targetName.value = ''; notes.value = ''; staged.value = []; quote.value = ''; formVersion.value++; lastIntent = '';
    emit('saved', id);
  } catch(reason) { error.value = reason instanceof Error ? reason.message : 'Could not save the relationship.'; }
  finally { busy.value = false; }
}
</script>
<template>
  <form
    aria-label="Add a source-backed related entity"
    class="relation-form"
    @submit.prevent="save"
  >
    <fieldset :disabled="busy">
      <legend>{{ correcting ? 'Correct relationship' : 'Related cards' }}</legend>
      <label v-if="!correcting">Action<select v-model="mode"><option value="create">Add related card</option><option value="link">Link existing</option></select></label>
      <label>Related to<select
        v-model="parentId"
        required
      ><option
        v-for="n in nodes"
        :key="n.id"
        :value="n.id"
      >{{ n.name }} · {{ n.kind }}</option></select></label>
      <label>{{ mode === 'create' ? 'New entity type' : 'Target type' }}<select v-model="kind"><option
        v-for="k in kinds"
        :key="k"
        :value="k"
      >{{ k === 'person' && parent?.kind === 'org' ? 'Person (member of organization)' : kindLabel(k) }}</option></select></label>
      <label v-if="mode === 'create'">Name<input
        v-model="targetName"
        required
        maxlength="240"
      ></label>
      <template v-else>
        <label>Find existing card<input
          v-model="search"
          type="search"
        ></label>
        <label>Existing card<select
          v-model="targetId"
          required
        ><option value="">Choose a card</option><option
          v-for="n in matches"
          :key="n.id"
          :value="n.id"
        >{{ n.name }} · {{ n.focus || kindLabel(n.kind) }} · {{ n.id.slice(-8) }}</option></select></label>
      </template>
      <label>Relationship and direction<select
        v-model="verb"
        required
      ><option
        v-for="a in assertions"
        :key="a.verb"
        :value="a.verb"
      >{{ a.direction === 'outgoing' ? '→' : '←' }} {{ a.verb.replaceAll('_',' ') }}</option></select></label>
      <p
        v-if="sentence"
        class="sentence"
      >
        {{ sentence }}
      </p>
      <label>Saved source<select
        v-model="sourceId"
        required
      ><option value="">Choose a source</option><option
        v-for="s in sources"
        :key="s.occurrence_id"
        :value="s.occurrence_id"
      >Source {{ s.content_id.slice(0,12) }} · occurrence {{ s.occurrence_id.slice(0,8) }}</option></select></label>
      <label>Exact supporting quote<textarea
        v-model="quote"
        required
        rows="2"
      /></label>
      <template v-if="mode === 'create'">
        <label>Initial owner notes<textarea
          v-model="notes"
          rows="3"
        /></label>
        <details v-if="notes">
          <summary>Preview Markdown</summary><MarkdownPreview :source="notes" />
        </details>
        <PartyFiles
          :key="formVersion"
          @staged="staged = $event"
          @busy="fileBusy = $event"
        />
      </template>
      <button
        type="submit"
        :disabled="!sources.length || fileBusy || !sentence"
      >
        {{ mode === 'create' ? 'Save related entity and relationship' : correcting ? 'Save correction' : 'Link cards' }}
      </button>
      <button
        type="button"
        @click="emit('cancel')"
      >
        Cancel
      </button>
    </fieldset>
    <p
      v-if="error"
      role="alert"
    >
      {{ error }}
    </p>
  </form>
</template>
<style scoped>
.relation-form { padding-block:.6rem; }
fieldset { min-width:0; padding:.65rem; border:1px solid #bdcabb; border-radius:5px; }
.sentence { background:#edf3e9; padding:.45rem; }
fieldset label { display:grid; gap:.2rem; margin:.45rem 0; }
fieldset button { margin:.3rem .3rem .1rem 0; }
</style>
