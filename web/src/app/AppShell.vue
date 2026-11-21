<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { captureSource, createEntity, createResource, listEntities, listResources, listSources, type Entity, type Resource, type SavedSource } from '../api/client';
import { tabs } from './tabs';

const active = ref<(typeof tabs)[number]>('Resources');
const kind = ref<'gift'|'proof'|'artifact'>('gift');
const title = ref('');
const what = ref('');
const captureText = ref('');
const partyName = ref('');
const partyKind = ref<'person'|'org'>('person');
const resources = ref<Resource[]>([]);
const parties = ref<Entity[]>([]);
const sources = ref<SavedSource[]>([]);
const status = ref('');
const error = ref('');
const busy = ref(false);

async function refreshEntities() {
  const [entities, savedResources, savedSources] = await Promise.all([listEntities(), listResources(), listSources()]);
  resources.value = savedResources;
  sources.value = savedSources;
  parties.value = entities.filter((entity) => entity.kind === 'person' || entity.kind === 'org');
}
async function saveResource() {
  busy.value = true; error.value = ''; status.value = '';
  try {
    const saved = await createResource({ kind: kind.value, title: title.value.trim() || `${kind.value} draft`, body: { what: what.value.trim() } });
    resources.value.unshift(saved); title.value = ''; what.value = '';
    status.value = 'Draft saved. Add the remaining fields when you have them.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save the resource.'; }
  finally { busy.value = false; }
}
async function saveCapture() {
  busy.value = true; error.value = ''; status.value = '';
  try {
    const source = await captureSource(captureText.value);
    sources.value.unshift(source); captureText.value = '';
    status.value = 'Source saved independently. No person was inferred or linked.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save the source.'; }
  finally { busy.value = false; }
}
async function saveParty() {
  busy.value = true; error.value = ''; status.value = '';
  try {
    const party = await createEntity(partyKind.value, partyName.value.trim());
    parties.value.push(party); partyName.value = '';
    status.value = 'Lead created. No opportunity or outreach was created.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save the lead.'; }
  finally { busy.value = false; }
}
onMounted(() => { void refreshEntities().catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Workspace is unavailable.'; }); });
</script>

<template>
  <header>
    <strong>Parley</strong>
    <nav aria-label="Main navigation">
      <button
        v-for="tab in tabs"
        :key="tab"
        type="button"
        :disabled="tab !== 'Resources' && tab !== 'Leads'"
        :aria-current="active === tab ? 'page' : undefined"
        @click="active = tab"
      >
        {{ tab }}
      </button>
    </nav>
  </header>
  <main>
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
    <section
      v-if="active === 'Resources'"
      aria-labelledby="resources-heading"
    >
      <h1 id="resources-heading">
        Resources
      </h1>
      <div class="workspace-grid">
        <form @submit.prevent="saveResource">
          <h2>Add a resource</h2>
          <label>Type<select v-model="kind"><option value="gift">Gift</option><option value="proof">Proof</option><option value="artifact">Artifact</option></select></label>
          <label>Name<input
            v-model="title"
            autocomplete="off"
            placeholder="For example, onboarding walkthrough"
          ></label>
          <label>What can you offer?<textarea
            v-model="what"
            rows="4"
            placeholder="An incomplete draft is fine"
          /></label>
          <button
            :disabled="busy"
            type="submit"
          >
            Save draft
          </button>
          <p>Drafts stay editable and are not eligible for matching until required evidence is complete.</p>
        </form>
        <section aria-labelledby="saved-resources">
          <h2 id="saved-resources">
            Saved resources
          </h2>
          <p v-if="!resources.length">
            No resources yet. Add a draft to record something you can offer.
          </p>
          <ul>
            <li
              v-for="resource in resources"
              :key="resource.id"
            >
              {{ resource.title }} <small>{{ resource.readiness }} · {{ resource.kind }}</small>
            </li>
          </ul>
        </section>
      </div>
    </section>
    <section
      v-else-if="active === 'Leads'"
      aria-labelledby="leads-heading"
    >
      <h1 id="leads-heading">
        Leads
      </h1>
      <div class="workspace-grid">
        <form @submit.prevent="saveParty">
          <h2>Add a lead</h2>
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
        </form>
        <section>
          <h2>Directory</h2><p v-if="!parties.length">
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
          <h2>Sources inbox</h2>
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
