<script setup lang="ts">
import { computed, watch } from 'vue';
import type { GraphEntity } from '../../api/client';

const props = defineProps<{
  parties: GraphEntity[];
  rootPartyId: string | null;
  selectedRecordId: string | null;
  query: string;
  kind: 'all' | 'person' | 'org';
  sort: 'name' | 'type' | 'focus';
  focusEdit: string | null;
  focusDraft: string;
  busy: boolean;
  addOpen?: boolean;
  serverPaged?: boolean;
  loading?: boolean;
  rootKnown?: boolean;
  more?: boolean;
}>();

const emit = defineEmits<{
  'update:query': [value: string];
  'update:kind': [value: 'all' | 'person' | 'org'];
  'update:sort': [value: 'name' | 'type' | 'focus'];
  'update:focus-draft': [value: string];
  'add-party': [];
  more: [];
  'first-visible': [id: string];
  'select-row': [id: string];
  'edit-focus': [id: string];
  'save-focus': [id: string];
  'cancel-focus': [];
}>();

const visibleParties = computed(() => {
  if (props.serverPaged) return props.parties;
  const needle = props.query.trim().toLocaleLowerCase();
  return props.parties
    .filter((party) => (props.kind === 'all' || party.kind === props.kind)
      && (!needle || `${party.name} ${party.focus} ${party.kind}`.toLocaleLowerCase().includes(needle)))
    .sort((a, b) => {
      const left = props.sort === 'type' ? a.kind : props.sort === 'focus' ? (a.focus ?? '') : a.name;
      const right = props.sort === 'type' ? b.kind : props.sort === 'focus' ? (b.focus ?? '') : b.name;
      return left.toLocaleLowerCase().localeCompare(right.toLocaleLowerCase()) || a.id.localeCompare(b.id);
    });
});

watch(() => visibleParties.value[0]?.id, id => { if (id) emit('first-visible', id); }, { immediate: true });
function clearFilters() { emit('update:kind','all'); emit('update:query',''); }
function typeLabel(kind: string): string {
  return kind === 'org' ? 'Organization' : kind === 'person' ? 'Person' : 'Project';
}
</script>

<template>
  <section
    class="party-sheet"
    aria-labelledby="party-sheet-heading"
  >
    <header class="pane-bar">
      <h2 id="party-sheet-heading">
        Party sheet
      </h2>
      <div class="sheet-actions">
        <button
          type="button"
          class="primary"
          :aria-expanded="addOpen"
          @click="emit('add-party')"
        >
          Add party
        </button>
        <slot name="import" />
      </div>
    </header>
    <div class="sheet-toolbar">
      <label
        class="sr-only"
        for="party-search"
      >Find a party</label>
      <input
        id="party-search"
        :value="props.query"
        type="search"
        placeholder="Find a party"
        @input="emit('update:query', ($event.target as HTMLInputElement).value)"
      >
      <label
        class="sr-only"
        for="party-kind"
      >Party type</label>
      <select
        id="party-kind"
        :value="props.kind"
        @change="emit('update:kind', ($event.target as HTMLSelectElement).value as 'all' | 'person' | 'org')"
      >
        <option value="all">
          All types
        </option>
        <option value="person">
          People
        </option>
        <option value="org">
          Organizations
        </option>
      </select>
      <label
        class="sr-only"
        for="party-sort"
      >Sort by</label>
      <select
        id="party-sort"
        :value="props.sort"
        @change="emit('update:sort', ($event.target as HTMLSelectElement).value as 'name' | 'type' | 'focus')"
      >
        <option value="name">
          Sort by name
        </option>
        <option value="type">
          Sort by type
        </option>
        <option value="focus">
          Sort by focus
        </option>
      </select>
    </div>
    <p
      v-if="loading && !props.parties.length"
      class="sheet-empty"
    >
      Loading parties…
    </p>
    <p
      v-else-if="!props.parties.length && !query && kind === 'all'"
      class="sheet-empty"
    >
      No parties yet. Add a person or organization. People do not need an organization.
    </p>
    <p
      v-else-if="!visibleParties.length"
      class="sheet-empty"
    >
      No matching parties.
    </p>
    <div
      v-else
      class="sheet-scroll"
    >
      <table>
        <thead>
          <tr>
            <th scope="col">
              Party
            </th><th scope="col">
              Type
            </th><th scope="col">
              Focus
            </th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="party in visibleParties"
            :key="party.id"
            :class="{ root: party.id === props.rootPartyId }"
          >
            <td>
              <button
                type="button"
                class="row-party"
                :aria-pressed="party.id === props.rootPartyId"
                :aria-current="party.id === props.selectedRecordId ? 'true' : undefined"
                @click="emit('select-row', party.id)"
              >
                {{ party.name }}
              </button>
            </td>
            <td>{{ typeLabel(party.kind) }}</td>
            <td>
              <template v-if="props.focusEdit === party.id">
                <input
                  class="focus-input"
                  :value="props.focusDraft"
                  maxlength="250"
                  :aria-label="`Focus for ${party.name}`"
                  @keydown.enter.prevent="emit('save-focus', party.id)"
                  @keydown.esc.prevent="emit('cancel-focus')"
                  @input="emit('update:focus-draft', ($event.target as HTMLInputElement).value)"
                >
                <button
                  type="button"
                  :disabled="props.busy"
                  @click="emit('save-focus', party.id)"
                >
                  Save
                </button>
                <button
                  type="button"
                  @click="emit('cancel-focus')"
                >
                  Cancel
                </button>
              </template>
              <template v-else>
                <button
                  type="button"
                  class="focus-cell"
                  :disabled="props.busy"
                  @click="emit('edit-focus', party.id)"
                >
                  {{ party.focus || 'Add focus' }}
                </button>
              </template>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
    <p
      v-if="rootPartyId && (rootKnown || parties.some(p => p.id === rootPartyId)) && (query || kind !== 'all') && !more && !visibleParties.some(p => p.id === rootPartyId)"
      class="sheet-empty"
    >
      Outside current filter <button @click="clearFilters">
        Clear filters
      </button>
    </p>
    <footer class="sheet-foot">
      {{ visibleParties.length }} {{ more ? 'loaded rows' : 'party rows' }} · name, type and focus
      <button
        v-if="more"
        :disabled="loading"
        @click="emit('more')"
      >
        Load more
      </button>
    </footer>
  </section>
</template>

<style scoped>
.party-sheet { display: flex; flex-direction: column; min-width: 0; min-height: 0; }
.sheet-actions { display: flex; gap: .4rem; flex-wrap: wrap; }
.sheet-toolbar { display: flex; gap: .45rem; padding: .6rem .8rem; border-bottom: 1px solid #dce2db; flex-wrap: wrap; }
.sheet-toolbar input { min-width: 8rem; flex: 1 1 10rem; }
.sheet-toolbar select { width: auto; }
.sheet-scroll { overflow: auto; max-height: 52vh; }
table { border-collapse: collapse; width: 100%; text-align: left; }
th, td { padding: .45rem .6rem; border-bottom: 1px solid #e3e9e2; vertical-align: middle; overflow-wrap:anywhere; }
th { font-size:.92rem; font-weight:550; color:#2e4739; background:#f2f5ed; }
tr.root { background: #e2ecdf; }
.row-party { width: 100%; text-align: left; font-weight:600; border: 0; background: transparent; padding: .2rem .3rem; }
.row-party[aria-pressed="true"] { background: #deebe2; }
.focus-cell { border: 0; background: transparent; color: #58665f; padding: .2rem .3rem; }
.focus-input { width: 10rem; padding: .25rem .4rem; }
.sheet-empty, .sheet-foot { margin: .6rem .8rem; color: #58665f; }
.sheet-foot { margin-top: auto; border-top: 1px solid #dce2db; padding: .5rem .8rem; font-size: .8rem; }
.sr-only { position: absolute; clip-path: inset(50%); height: 1px; width: 1px; overflow: hidden; white-space: nowrap; }
</style>