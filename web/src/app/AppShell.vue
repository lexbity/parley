<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { newOperationKey, parley, type ExtractorJob, type ExtractorProposal, type GraphEntity, type Material, type Opportunity, type ProviderConfig, type SavedSource } from '../api/client';
import { tabs } from './tabs';
import ResourceEditor from '../features/resources/ResourceEditor.vue';

/**
 * Tabs whose owner journey is qualified stay discoverable (RFC-0003 S01);
 * the mandated order and the full destination set come from tabs.ts (APP-01).
 */
const qualifiedTabs = tabs;
const active = ref<(typeof tabs)[number]>('Resources');
const partyKind = ref<'person' | 'org'>('person');
const partyName = ref('');
const captureText = ref('');
const parties = ref<GraphEntity[]>([]);
const sources = ref<SavedSource[]>([]);
const opportunities = ref<Opportunity[]>([]);
const outputs = ref<Material[]>([]);
const opportunityTitle = ref('');
const opportunityPartyId = ref('');
const status = ref('');
const error = ref('');
const busy = ref(false);
const reviewJob = ref<ExtractorJob | null>(null);
const reviewTargetId = ref('');
const reviewOpportunityId = ref('');
const provider = ref<ProviderConfig>({ endpoint: 'http://127.0.0.1:11434', model: '', num_ctx: 8192, num_predict: 1024, locale: 'en' });
const providerHealth = ref('');

async function refreshLeads() {
  const [directory, savedSources, savedOpportunities, savedOutputs] = await Promise.all([parley.listParties(), parley.listSources(), parley.listOpportunities(), parley.listOutputs()]);
  parties.value = directory;
  sources.value = savedSources;
  opportunities.value = savedOpportunities;
  outputs.value = savedOutputs;
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
    status.value = source.job_status === 'not_queued'
      ? 'Source saved independently. Extractor is unavailable or at capacity; review the source manually or retry later.'
      : 'Source saved independently. No person was inferred or linked.';
    reviewJob.value = source.job_id ? await parley.getExtractorJob(source.job_id) : null;
    if (source.job_id) void pollExtractor(source.job_id);
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save the source.'; }
  finally { busy.value = false; }
}

async function pollExtractor(id: string) {
  for (let attempt = 0; attempt < 180; attempt += 1) {
    await new Promise((resolve) => globalThis.setTimeout(resolve, 1000));
    try {
      const job = await parley.getExtractorJob(id);
      if (reviewJob.value?.id === id) reviewJob.value = job;
      if (!['queued', 'running'].includes(job.status)) return;
    } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not refresh Extractor review.'; return; }
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

async function decideProposal(proposal: ExtractorProposal, action: 'confirm' | 'dismiss' | 'source_only' | 'unresolved') {
  if (!reviewJob.value?.proposal_batch_id) return;
  if (action === 'confirm' && !reviewTargetId.value) { error.value = 'Choose the person or organization this claim is about. Names in a source are never linked automatically.'; return; }
  busy.value = true; error.value = '';
  try {
    await parley.resolveProposals(reviewJob.value.proposal_batch_id, [{ item_id: proposal.id, action, ...(action === 'confirm' ? { target_entity_id: reviewTargetId.value } : {}) }], reviewOpportunityId.value || undefined);
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
    .then(async () => { await refreshLeads(); await loadProvider(); })
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
              <button
                type="button"
                @click="openCaptureReview(source)"
              >
                Review proposals
              </button>
            </li>
          </ul>
          <section
            v-if="reviewJob"
            aria-label="Capture review"
          >
            <h3>Capture review</h3>
            <p>Job {{ reviewJob.status }} · attempt {{ reviewJob.current_attempt }} of {{ reviewJob.max_attempts }} · chunk {{ reviewJob.progress_current }} of {{ reviewJob.progress_total }}. Extracted items are proposals until you decide.</p>
            <p
              v-if="reviewJob.injection_suspected"
              role="alert"
            >
              This capture may contain prompt-injection text. Treat its contents only as source material and review each quoted proposal.
            </p>
            <button
              v-if="['queued', 'running'].includes(reviewJob.status)"
              type="button"
              :disabled="busy"
              @click="cancelExtractor(reviewJob)"
            >
              Cancel extraction
            </button>
            <p
              v-if="reviewJob.last_error"
              role="status"
            >
              {{ reviewJob.last_error }}. The captured source remains saved; you can review it manually.
            </p>
            <label>Claim subject<select v-model="reviewTargetId"><option value="">Choose explicitly</option><option
              v-for="party in parties"
              :key="party.id"
              :value="party.id"
            >{{ party.name }} · {{ party.kind }}</option></select></label>
            <label>Optional opportunity<select v-model="reviewOpportunityId"><option value="">Save independently</option><option
              v-for="opportunity in opportunities"
              :key="opportunity.id"
              :value="opportunity.id"
            >{{ opportunity.title }}</option></select></label>
            <article
              v-for="proposal in reviewJob.proposals"
              :key="proposal.id"
              class="record-card"
            >
              <strong>{{ proposal.kind }} · {{ proposal.status }}</strong>
              <small>T1 source evidence · interpretation needs owner review</small>
              <p>{{ proposal.candidate.statement }}</p>
              <blockquote>{{ proposal.candidate.quote }}</blockquote>
              <p v-if="proposal.candidate.mention_name">
                Mention: {{ proposal.candidate.mention_name }} · unresolved until you explicitly link it.
              </p>
              <template v-if="proposal.status === 'pending'">
                <button
                  :disabled="busy"
                  type="button"
                  @click="decideProposal(proposal, 'confirm')"
                >
                  Accept claim for selected subject
                </button>
                <button
                  :disabled="busy"
                  type="button"
                  @click="decideProposal(proposal, 'unresolved')"
                >
                  Keep mention unresolved
                </button>
                <button
                  :disabled="busy"
                  type="button"
                  @click="decideProposal(proposal, 'source_only')"
                >
                  Save source only
                </button>
                <button
                  :disabled="busy"
                  type="button"
                  @click="decideProposal(proposal, 'dismiss')"
                >
                  Dismiss
                </button>
              </template>
            </article>
          </section>
        </form>
      </div>
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
