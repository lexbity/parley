<script setup lang="ts">
import { onMounted, ref, watch } from 'vue';
import { parley, type GraphEntity, type Material, type Opportunity } from '../api/client';
import { tabs } from './tabs';
import ResourceEditor from '../features/resources/ResourceEditor.vue';
import Directory from '../features/leads/Directory.vue';

/**
 * Tabs whose owner journey is qualified stay discoverable (RFC-0003 S01);
 * the mandated order and the full destination set come from tabs.ts (APP-01).
 * The Leads workspace is the S02 directory component; Queue and Output carry
 * the S03 opportunity and approved-material records.
 */
const qualifiedTabs = tabs.filter((tab) => ['Resources', 'Leads', 'Queue', 'Output'].includes(tab));
const active = ref<(typeof tabs)[number]>('Resources');
const parties = ref<GraphEntity[]>([]);
const opportunities = ref<Opportunity[]>([]);
const outputs = ref<Material[]>([]);
const opportunityTitle = ref('');
const opportunityPartyId = ref('');
const status = ref('');
const error = ref('');
const busy = ref(false);

async function refreshWork() {
  const [directory, savedOpportunities, savedOutputs] = await Promise.all([
    parley.listParties(),
    parley.listOpportunities(),
    parley.listOutputs(),
  ]);
  parties.value = directory;
  opportunities.value = savedOpportunities;
  outputs.value = savedOutputs;
}

async function saveOpportunity() {
  busy.value = true; error.value = ''; status.value = '';
  try {
    const opportunity = await parley.createOpportunity(opportunityTitle.value.trim(), opportunityPartyId.value);
    opportunities.value.unshift(opportunity); opportunityTitle.value = ''; opportunityPartyId.value = '';
    status.value = 'Opportunity created with the selected participant. No message was created or sent.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not create the opportunity.'; }
  finally { busy.value = false; }
}

async function copyMaterial(material: Material) {
  busy.value = true; error.value = ''; status.value = '';
  let releaseId: string | undefined;
  try {
    const release = await parley.releaseMaterial(material.id);
    releaseId = release.release_id;
    await globalThis.navigator.clipboard.writeText(release.body);
    await parley.acknowledgeRelease(release.release_id, true);
    status.value = `Approved version ${release.version} copied for ${material.recipient_name}. Marking it sent is a separate action.`;
  } catch (reason) {
    if (releaseId) await parley.acknowledgeRelease(releaseId, false).catch(() => undefined);
    error.value = reason instanceof Error ? reason.message : 'Could not copy this material.';
  } finally { busy.value = false; }
}

onMounted(() => {
  void parley.connect()
    .then(refreshWork)
    .catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Workspace is unavailable.'; });
});

// The directory workspace records leads through its own component; the
// opportunity forms re-read shared records when their tab becomes visible.
watch(active, (tab) => {
  if (tab === 'Queue' || tab === 'Output') void refreshWork().catch(() => undefined);
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
    <Directory v-if="active === 'Leads'" />
    <section
      v-if="active === 'Queue'"
      aria-labelledby="queue-heading"
    >
      <h2 id="queue-heading">
        Opportunities
      </h2>
      <div class="workspace-grid">
        <form @submit.prevent="saveOpportunity">
          <h3>Create an opportunity</h3>
          <label>Opportunity title<input
            v-model="opportunityTitle"
            required
            maxlength="500"
          ></label>
          <label>Participant<select
            v-model="opportunityPartyId"
            required
          ><option
            disabled
            value=""
          >Choose a lead</option><option
            v-for="party in parties"
            :key="party.id"
            :value="party.id"
          >{{ party.name }} · {{ party.kind }}</option></select></label>
          <button
            :disabled="busy || !parties.length"
            type="submit"
          >
            Create opportunity
          </button>
          <p>A lead alone does not create an opportunity or outreach.</p>
        </form>
        <section aria-label="Opportunity list">
          <h3>All opportunities</h3>
          <p v-if="!opportunities.length">
            No opportunities yet. Select a lead to bring records together.
          </p>
          <article
            v-for="opportunity in opportunities"
            :key="opportunity.id"
            class="record-card"
          >
            <h4>{{ opportunity.title }}</h4>
            <p>{{ opportunity.state }} · {{ opportunity.material_ids.length }} material(s)</p>
            <ul>
              <li
                v-for="output in outputs.filter((item) => item.opportunity_id === opportunity.id)"
                :key="output.id"
              >
                {{ output.recipient_name }} · v{{ output.version }} · {{ output.status }}
              </li>
            </ul>
          </article>
        </section>
      </div>
    </section>
    <section
      v-if="active === 'Output'"
      aria-labelledby="output-heading"
    >
      <h2 id="output-heading">
        Approved materials
      </h2>
      <p>Each material is bound to one opportunity, recipient and immutable version. Release checks run against current evidence.</p>
      <p v-if="!outputs.length">
        No materials yet. Create an opportunity, prepare its exchange, then author material for a named participant.
      </p>
      <ul class="output-list">
        <li
          v-for="material in outputs"
          :key="material.id"
          class="record-card"
        >
          <strong>{{ material.recipient_name }}</strong> · {{ material.kind }} · v{{ material.version }} · {{ material.status }}
          <p>Opportunity {{ material.opportunity_id }} · SHA-256 {{ material.content_sha256 }}</p>
          <button
            type="button"
            :disabled="busy || material.status !== 'approved'"
            @click="copyMaterial(material)"
          >
            Copy approved text
          </button>
        </li>
      </ul>
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
small { color: #58665f; } .capture { grid-column: 1 / -1; } .record-card { background:white; border:1px solid #d7dfd9; border-radius:.5rem; margin:.75rem 0; padding:1rem; } .output-list { padding:0; list-style:none; } @media(max-width:650px) { header { align-items: start; flex-direction: column; gap: .5rem; padding: 1rem; } nav { flex-wrap: wrap; } }
</style>
