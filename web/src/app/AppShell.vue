<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { acknowledgeRelease, captureSource, createEntity, createOpportunity, createResource, listEntities, listOpportunities, listOutputs, listResources, listSources, releaseMaterial, type Entity, type Material, type Opportunity, type Resource, type SavedSource } from '../api/client';
import { tabs } from './tabs';

const active = ref<(typeof tabs)[number]>('Resources');
const kind = ref<'gift'|'proof'|'artifact'>('gift');
const title = ref('');
const what = ref('');
const captureText = ref('');
const partyName = ref('');
const partyKind = ref<'person'|'org'>('person');
const opportunityTitle = ref('');
const opportunityPartyId = ref('');
const resources = ref<Resource[]>([]);
const parties = ref<Entity[]>([]);
const sources = ref<SavedSource[]>([]);
const opportunities = ref<Opportunity[]>([]);
const outputs = ref<Material[]>([]);
const status = ref('');
const error = ref('');
const busy = ref(false);

async function refreshEntities() {
  const [entities, savedResources, savedSources, savedOpportunities, savedOutputs] = await Promise.all([listEntities(), listResources(), listSources(), listOpportunities(), listOutputs()]);
  resources.value = savedResources;
  sources.value = savedSources;
  parties.value = entities.filter((entity) => entity.kind === 'person' || entity.kind === 'org');
  opportunities.value = savedOpportunities;
  outputs.value = savedOutputs;
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
async function saveOpportunity() {
  busy.value = true; error.value = ''; status.value = '';
  try {
    const opportunity = await createOpportunity(opportunityTitle.value.trim(), opportunityPartyId.value);
    opportunities.value.unshift(opportunity); opportunityTitle.value = '';
    status.value = 'Opportunity created with the selected participant. No message was created or sent.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not create the opportunity.'; }
  finally { busy.value = false; }
}
async function copyMaterial(material: Material) {
  busy.value = true; error.value = ''; status.value = '';
  let releaseId: string | undefined;
  try {
    const release = await releaseMaterial(material.id);
    releaseId = release.release_id;
    await navigator.clipboard.writeText(release.body);
    await acknowledgeRelease(release.release_id, true);
    status.value = `Approved version ${release.version} copied for ${material.recipient_name}. Marking it sent is a separate action.`;
  } catch (reason) {
    if (releaseId) await acknowledgeRelease(releaseId, false).catch(() => undefined);
    error.value = reason instanceof Error ? reason.message : 'Could not copy this material.';
  } finally { busy.value = false; }
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
        :disabled="tab === 'Settings'"
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
    <section v-else-if="active === 'Queue'" aria-labelledby="queue-heading">
      <h1 id="queue-heading">Queue</h1>
      <div class="workspace-grid">
        <form @submit.prevent="saveOpportunity">
          <h2>Create an opportunity</h2>
          <label>Opportunity title<input v-model="opportunityTitle" required maxlength="500" autocomplete="off"></label>
          <label>Participant<select v-model="opportunityPartyId" required><option disabled value="">Choose a person or organization</option><option v-for="party in parties" :key="party.id" :value="party.id">{{ party.name }} · {{ party.kind }}</option></select></label>
          <button :disabled="busy || !parties.length" type="submit">Create opportunity</button>
          <p>A lead alone creates no opportunity. The selected participant is visible on this card.</p>
        </form>
        <section aria-labelledby="opportunity-list-heading">
          <h2 id="opportunity-list-heading">Opportunities</h2>
          <p v-if="!opportunities.length">No opportunities yet. Choose a lead and create one to bring the records together.</p>
          <article v-for="opportunity in opportunities" :key="opportunity.id" class="record-card">
            <h3>{{ opportunity.title }}</h3>
            <p>Participant IDs: {{ opportunity.participant_ids.join(', ') }}</p>
            <p>{{ opportunity.state }} · {{ opportunity.material_ids.length }} material(s)</p>
            <ul><li v-for="output in outputs.filter((item) => item.opportunity_id === opportunity.id)" :key="output.id">{{ output.recipient_name }} · version {{ output.version }} · {{ output.status }}</li></ul>
          </article>
        </section>
      </div>
    </section>
    <section v-else-if="active === 'Output'" aria-labelledby="output-heading">
      <h1 id="output-heading">Output</h1>
      <p>Materials remain bound to one opportunity, recipient, and immutable version. Copy is available only after approval and current gate checks.</p>
      <p v-if="!outputs.length">No materials yet. Create an opportunity, prepare its exchange, then author a material for a named participant.</p>
      <ul class="output-list"><li v-for="material in outputs" :key="material.id" class="record-card">
        <strong>{{ material.recipient_name }}</strong> · {{ material.kind }} · {{ material.status }} · version {{ material.version }}
        <p>{{ material.body }}</p>
        <button v-if="material.status === 'approved'" :disabled="busy" type="button" @click="copyMaterial(material)">Copy approved version</button>
        <small>Opportunity {{ material.opportunity_id }} · SHA-256 {{ material.content_sha256 }}</small>
      </li></ul>
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
small { color: #58665f; } .capture { grid-column: 1 / -1; } .record-card { background: white; border: 1px solid #d7dfd9; border-radius: .5rem; margin: .75rem 0; padding: 1rem; } .output-list { padding: 0; list-style: none; } @media(max-width:650px) { header { align-items: start; flex-direction: column; gap: .5rem; padding: 1rem; } nav { flex-wrap: wrap; } }
</style>
