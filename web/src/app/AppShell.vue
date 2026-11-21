<script setup lang="ts">
import { onMounted, ref, watch } from 'vue';
import { parley, type ExtractorJob, type ExtractorProposal, type GraphEntity, type Material, type Opportunity, type ProviderConfig, type SavedSource } from '../api/client';
import { tabs } from './tabs';
import ResourceEditor from '../features/resources/ResourceEditor.vue';
import Directory from '../features/leads/Directory.vue';

/**
 * Tabs whose owner journey is qualified stay discoverable (RFC-0003 S01);
 * the mandated order and the full destination set come from tabs.ts (APP-01).
 * The Leads workspace is the S02 directory component; the S04 Extractor
 * review state is owned here so its statuses stay workspace-level; Queue,
 * Output and Settings carry the S03 records and the loopback provider.
 */
const qualifiedTabs = tabs;
const active = ref<(typeof tabs)[number]>('Resources');
const parties = ref<GraphEntity[]>([]);
const opportunities = ref<Opportunity[]>([]);
const outputs = ref<Material[]>([]);
const opportunityTitle = ref('');
const opportunityPartyId = ref('');
const status = ref('');
const error = ref('');
const busy = ref(false);
const reviewJob = ref<ExtractorJob | null>(null);
const provider = ref<ProviderConfig>({ endpoint: 'http://127.0.0.1:11434', model: '', num_ctx: 8192, num_predict: 1024, locale: 'en' });
const providerHealth = ref('');

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

function startCaptureReview(source: SavedSource): void {
  if (!source.job_id) return;
  void pollExtractor(source.job_id);
}

/** Fetch now, then keep fetching once a second until the job reaches a
 * terminal state or the five-minute poll budget runs out. */
async function pollExtractor(id: string) {
  for (let attempt = 0; attempt < 180; attempt += 1) {
    try {
      const job = await parley.getExtractorJob(id);
      if (reviewJob.value === null || reviewJob.value.id === id) reviewJob.value = job;
      if (!['queued', 'running'].includes(job.status)) return;
    } catch (reason) {
      error.value = reason instanceof Error ? reason.message : 'Could not refresh Extractor review.';
      return;
    }
    await new Promise((resolve) => globalThis.setTimeout(resolve, 1000));
  }
}

async function openCaptureReview(source: SavedSource) {
  try { reviewJob.value = await parley.getCaptureProposals(source.occurrence_id); }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'There is no Extractor review for this source yet.'; }
}

async function cancelExtractor(job: ExtractorJob) {
  busy.value = true; error.value = '';
  try { await parley.cancelExtractorJob(job.id); reviewJob.value = await parley.getExtractorJob(job.id); status.value = 'Extractor cancellation was recorded; any late result is discarded.'; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not cancel this Extractor job.'; }
  finally { busy.value = false; }
}

async function decideProposal(proposal: ExtractorProposal, action: 'confirm' | 'dismiss' | 'source_only' | 'unresolved', targetEntityId: string, opportunityId: string) {
  if (!reviewJob.value?.proposal_batch_id) return;
  if (action === 'confirm' && !targetEntityId) { error.value = 'Choose the person or organization this claim is about. Names in a source are never linked automatically.'; return; }
  busy.value = true; error.value = '';
  try {
    await parley.resolveProposals(reviewJob.value.proposal_batch_id, [{ item_id: proposal.id, action, ...(action === 'confirm' ? { target_entity_id: targetEntityId } : {}) }], opportunityId || undefined);
    reviewJob.value = await parley.getExtractorJob(reviewJob.value.id);
    status.value = action === 'confirm' ? 'Claim accepted with its source evidence.' : action === 'unresolved' ? 'Mention saved without linking it to a person or organization.' : `Proposal ${action.replace('_', ' ')}.`;
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save the review decision.'; }
  finally { busy.value = false; }
}

async function loadProvider() {
  try { provider.value = await parley.getProvider(); } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not load provider settings.'; }
}

async function saveProvider() {
  busy.value = true; error.value = ''; providerHealth.value = '';
  try { await parley.updateProvider(provider.value); status.value = 'Local Extractor provider settings saved. Check model health before using extraction.'; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save provider settings.'; }
  finally { busy.value = false; }
}

async function checkProvider() {
  busy.value = true; error.value = '';
  try { const result = await parley.checkProvider(); providerHealth.value = result.message; if (!result.available) error.value = result.message; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not check the local model.'; }
  finally { busy.value = false; }
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
    .then(async () => { await refreshWork(); await loadProvider(); })
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
    <Directory
      v-if="active === 'Leads'"
      :opportunities="opportunities"
      :review-job="reviewJob"
      @review-source="openCaptureReview"
      @source-saved="startCaptureReview"
      @decide="decideProposal"
      @cancel-job="cancelExtractor"
    />
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
    <section
      v-if="active === 'Settings'"
      aria-labelledby="settings-heading"
    >
      <h2 id="settings-heading">
        Local agent settings
      </h2>
      <p>Extractor runs only through a loopback provider. Manual capture, review, and other workspace actions remain available when the model is unavailable.</p>
      <form @submit.prevent="saveProvider">
        <label>Local endpoint<input
          v-model="provider.endpoint"
          required
          autocomplete="off"
        ></label>
        <label>Installed model tag<input
          v-model="provider.model"
          required
          autocomplete="off"
        ></label>
        <label>Context budget<input
          v-model.number="provider.num_ctx"
          type="number"
          min="2048"
          max="131072"
          required
        ></label>
        <label>Output budget<input
          v-model.number="provider.num_predict"
          type="number"
          min="64"
          max="8192"
          required
        ></label>
        <label>Evaluated language tag<input
          v-model="provider.locale"
          required
          maxlength="40"
        ></label>
        <button
          type="submit"
          :disabled="busy"
        >
          Save provider
        </button>
        <button
          type="button"
          :disabled="busy"
          @click="checkProvider"
        >
          Check local model
        </button>
        <p
          v-if="providerHealth"
          role="status"
        >
          {{ providerHealth }}
        </p>
      </form>
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
small { color: #58665f; } .record-card { background:white; border:1px solid #d7dfd9; border-radius:.5rem; margin:.75rem 0; padding:1rem; } .output-list { padding:0; list-style:none; } @media(max-width:650px) { header { align-items: start; flex-direction: column; gap: .5rem; padding: 1rem; } nav { flex-wrap: wrap; } }
</style>
