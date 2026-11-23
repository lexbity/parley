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
      role="status"
    >
      Loading parties…
    </p>
    <p
      v-else-if="!props.parties.length && !query && kind === 'all'"
      class="sheet-empty"
    >
      No parties yet. Add a person or organization, or import a CSV.
    </p>
    <p
      v-else-if="!visibleParties.length"
      class="sheet-empty"
    >
      No matching parties.
      <button
        type="button"
        @click="clearFilters"
      >
        Clear filters
      </button>
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
            :data-party-id="party.id"
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
                  :readonly="props.busy"
                  maxlength="250"
                  :aria-label="`Focus for ${party.name}`"
                  @keydown.enter.prevent="emit('save-focus', party.id)"
                  @keydown.esc.stop.prevent="emit('cancel-focus')"
                  @input="emit('update:focus-draft', ($event.target as HTMLInputElement).value)"
                >
                <div class="focus-actions">
                  <button
                    type="button"
                    :disabled="props.busy"
                    @click="emit('save-focus', party.id)"
                  >
                    Save
                  </button>
                  <button
                    type="button"
                    :disabled="props.busy"
                    @click="emit('cancel-focus')"
                  >
                    Cancel
                  </button>
                </div>
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
      v-if="visibleParties.length && rootPartyId && (rootKnown || parties.some(p => p.id === rootPartyId)) && (query || kind !== 'all') && !more && !visibleParties.some(p => p.id === rootPartyId)"
      class="sheet-empty"
    >
      Outside current filter <button @click="clearFilters">
        Clear filters
      </button>
    </p>
    <footer class="sheet-foot">
      <span aria-live="polite">{{ visibleParties.length }} {{ more ? 'loaded rows' : visibleParties.length === 1 ? 'party' : 'parties' }}</span>
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
.pane-bar, .sheet-actions { display: flex; gap: .4rem; flex-wrap: wrap; }
.sheet-actions :deep(button) { min-height: 2.5rem; }
.sheet-toolbar { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: .45rem; padding: .7rem .85rem; border-bottom: 1px solid #dce2db; }
.sheet-toolbar input { grid-column: 1 / -1; }
.sheet-toolbar select, .sheet-toolbar input { min-height: 2.5rem; }
.sheet-scroll { overflow: auto; max-height: min(55dvh, 36rem); }
table { table-layout: fixed; border-collapse: separate; border-spacing: 0; width: 100%; text-align: left; }
th, td { padding: .4rem .65rem; border-bottom: 1px solid #e3e9e2; vertical-align: middle; overflow-wrap: anywhere; }
th { position: sticky; top: 0; z-index: 1; font-weight: 600; color: #2e4739; background: #f2f5ed; }
th:first-child { width: 36%; }
th:nth-child(2) { width: 27%; }
tr.root { background: #e2ecdf; }
tr.root td:first-child { box-shadow: inset 3px 0 #367757; }
.row-party, .focus-cell { width: 100%; text-align: left; border: 0; background: transparent; padding: .3rem 0; overflow-wrap: anywhere; }
.row-party { font-weight: 650; }
.row-party[aria-pressed="true"], .row-party[aria-current="true"] { box-shadow: none; background: transparent; }
.focus-cell { color: #445b4e; }
.row-party:hover, .focus-cell:hover { text-decoration: underline; }
.focus-input { padding: .25rem .4rem; }
.focus-actions { display: flex; flex-wrap: wrap; gap: .25rem; margin-top: .3rem; }
.sheet-empty { margin: .75rem .85rem; color: #58665f; }
.sheet-foot { display: flex; justify-content: space-between; align-items: center; gap: .5rem; padding: .6rem .85rem; color: #58665f; font-size: .88rem; }
</style>
