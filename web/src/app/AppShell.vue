<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { newOperationKey, parley, type GraphEntity, type SavedSource } from '../api/client';
import { tabs } from './tabs';
import ResourceEditor from '../features/resources/ResourceEditor.vue';

/**
 * Tabs whose owner journey is qualified stay discoverable (RFC-0003 S01);
 * the mandated order and the full destination set come from tabs.ts (APP-01).
 */
const qualifiedTabs = tabs.filter((tab) => tab === 'Resources' || tab === 'Leads');
const active = ref<(typeof tabs)[number]>('Resources');
const partyKind = ref<'person' | 'org'>('person');
const partyName = ref('');
const captureText = ref('');
const parties = ref<GraphEntity[]>([]);
const sources = ref<SavedSource[]>([]);
const status = ref('');
const error = ref('');
const busy = ref(false);

async function refreshLeads() {
  const [directory, savedSources] = await Promise.all([parley.listParties(), parley.listSources()]);
  parties.value = directory;
  sources.value = savedSources;
}

async function saveParty() {
  busy.value = true; error.value = ''; status.value = '';
  try {
    const party = await parley.createEntity({ kind: partyKind.value, name: partyName.value.trim() }, newOperationKey());
    parties.value.push(party); partyName.value = '';
    status.value = 'Lead created. No opportunity or outreach was created.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save the lead.'; }
  finally { busy.value = false; }
}

async function saveCapture() {
  busy.value = true; error.value = ''; status.value = '';
  try {
    const source = await parley.saveSource({ text: captureText.value, title: null, url: null });
    sources.value.unshift(source); captureText.value = '';
    status.value = 'Source saved independently. No person was inferred or linked.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save the source.'; }
  finally { busy.value = false; }
}

onMounted(() => {
  void parley.connect()
    .then(refreshLeads)
    .catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Workspace is unavailable.'; });
});
</script>

<template>
  <header>
    <strong>Parley</strong>
    <nav aria-label="Main navigation">
      <button
        v-for="tab in qualifiedTabs"
        :key="tab"
        type="button"
        :aria-current="active === tab ? 'page' : undefined"
        @click="active = tab"
      >
        {{ tab }}
      </button>
    </nav>
  </header>
  <main>
    <h1>{{ active }}</h1>
    <p
      v-if="status"
      role="status"
    >
      {{ status }}
    </p>
    <p
      v-if="error"
      role="alert"
    >
      {{ error }}
    </p>
    <div v-show="active === 'Resources'">
      <ResourceEditor />
    </div>
    <section
      v-if="active === 'Leads'"
      aria-labelledby="leads-heading"
    >
      <h2 id="leads-heading">
        Lead directory and sources
      </h2>
      <div class="workspace-grid">
        <form @submit.prevent="saveParty">
          <h3>Add a lead</h3>
          <label>Type<select v-model="partyKind"><option value="person">Person</option><option value="org">Organization</option></select></label>
          <label>Name<input
            v-model="partyName"
            required
            autocomplete="off"
          ></label>
          <button
            :disabled="busy"
            type="submit"
          >
            Add lead
          </button>
          <p>No opportunity or outreach is created by saving a lead.</p>
        </form>
        <section>
          <h3>Directory</h3><p v-if="!parties.length">
            No leads yet. Add a person or organization. People do not need an organization.
          </p>
          <ul>
            <li
              v-for="party in parties"
              :key="party.id"
            >
              {{ party.name }} <small>{{ party.kind }}</small>
            </li>
          </ul>
        </section>
        <form
          class="capture"
          @submit.prevent="saveCapture"
        >
          <h3>Sources inbox</h3>
          <p>Paste captured text. Saving a source does not decide who it is about or create outreach.</p>
          <label>Captured text<textarea
            v-model="captureText"
            required
            minlength="1"
            rows="8"
          /></label>
          <button
            :disabled="busy"
            type="submit"
          >
            Save source
          </button>
          <ul>
            <li
              v-for="source in sources"
              :key="source.occurrence_id"
            >
              Saved source {{ source.content_id.slice(0, 12) }} · occurrence {{ source.occurrence_id.slice(0, 8) }}
            </li>
          </ul>
        </form>
      </div>
    </section>
  </main>
</template>

<style>
:root { color: #18221f; background: #f4f6f3; font: 16px/1.5 system-ui, sans-serif; }
body { margin: 0; } header { display: flex; align-items: center; gap: 2rem; padding: .75rem 2rem; background: white; border-bottom: 1px solid #d7dfd9; }
nav { display: flex; gap: .5rem; } button, input, select, textarea { font: inherit; } button { cursor: pointer; padding: .45rem .8rem; }
button[aria-current="page"] { background: #173e32; color: white; } main { max-width: 1100px; margin: 2rem auto; padding: 0 1rem; }
.workspace-grid { display: grid; grid-template-columns: repeat(auto-fit,minmax(300px,1fr)); gap: 1.5rem; align-items: start; }
form, section section { background: white; border: 1px solid #d7dfd9; border-radius: .5rem; padding: 1rem; }
label { display: grid; gap: .3rem; margin: .8rem 0; } input, select, textarea { width: 100%; box-sizing: border-box; padding: .5rem; border: 1px solid #82918a; border-radius: .25rem; }
small { color: #58665f; } .capture { grid-column: 1 / -1; } @media(max-width:650px) { header { align-items: start; flex-direction: column; gap: .5rem; padding: 1rem; } nav { flex-wrap: wrap; } }
</style>
