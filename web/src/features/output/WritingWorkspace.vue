<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { parley, type Material, type OpportunitySummary, type Resource, type WritingInterest, type WritingMatch, type WritingTemplate } from '../../api/client';

const props = defineProps<{ opportunities: OpportunitySummary[]; resources: Resource[]; outputs: Material[] }>();
const emit = defineEmits<{ saved: [] }>();
const opportunityId = ref(''); const recipientId = ref(''); const interests = ref<WritingInterest[]>([]); const exchange = ref<{ current: Record<string, unknown> | null; versions: Array<Record<string, unknown>> }>({ current: null, versions: [] });
const templates = ref<WritingTemplate[]>([]); const families = ref<Array<{ id: string; name: string }>>([]); const model = ref(''); const modelDigest = ref<string | null>(null); const calibratedKinds = ref<string[]>([]); const match = ref<WritingMatch | null>(null); const jobId = ref(''); const proposal = ref<Record<string, unknown> | null>(null);
const proposalFor = ref<{ opportunityId: string; recipientId: string } | null>(null);
const authoringGeneration = ref(0);
const theirInterest = ref(''); const myInterest = ref(''); const exchangeTheyGet = ref(''); const exchangeIGet = ref(''); const familyName = ref('One ask'); const templateName = ref('One ask'); const templateBody = ref('{{their_name:name}},\nI can offer {{gift:guide}}.\nWould a short review be useful {{ask:next_step}}?'); const stage = ref('identified'); const channel = ref('email'); const templateId = ref(''); const slotValues = ref<Record<string, string>>({}); const responseMessage = ref(''); const errorMessage = ref(''); const busy = ref(false);
const selectedOpportunity = computed(() => props.opportunities.find((item) => item.id === opportunityId.value));
const participants = computed(() => selectedOpportunity.value?.participants ?? []);
const selectedTemplate = computed(() => templates.value.find((item) => item.id === templateId.value));
const slots = computed(() => [...new Set((selectedTemplate.value?.body.match(/\{\{[^}]+\}\}/g) ?? []).map((slot) => slot.slice(2, -2)))]);
const exchangeVersion = computed(() => Number(exchange.value.current?.version ?? 0));
const voiceSamples = ref<Array<{ id: string; text: string; channel: string | null; revision: number }>>([]); const voiceText = ref(''); const voiceChannel = ref('email'); const selectedVoiceIds = ref<string[]>([]); const requestedRules = ref(['R2']);
const templateCandidates = ref<Array<Record<string, unknown>>>([]);
const noMatch = ref(false);
const contextLoading = ref(false);
const contextDrafts = new Map<string, { theirs: string; mine: string; theyGet: string; iGet: string; slots: Record<string, string> }>();
const proposalIsCurrent = computed(() => proposalFor.value?.opportunityId === opportunityId.value && proposalFor.value?.recipientId === recipientId.value);
const shortlist = computed<unknown[]>(() => {
  try {
    const items: unknown = JSON.parse(match.value?.shortlist_json ?? '[]');
    return Array.isArray(items) ? items : [];
  } catch { return []; }
});

function resetMessage() { errorMessage.value = ''; responseMessage.value = ''; }
async function refreshAuthoring() {
  const opportunity = opportunityId.value;
  const recipient = recipientId.value;
  if (!opportunity || !recipient) return;
  const generation = authoringGeneration.value;
  contextLoading.value = true;
  try {
    const [nextInterests, nextExchange] = await Promise.all([
      parley.getWritingInterests(opportunity, recipient),
      parley.getWritingExchange(opportunity, recipient),
    ]);
    if (generation === authoringGeneration.value) {
      interests.value = nextInterests;
      exchange.value = nextExchange;
    }
  } catch (reason) {
    if (generation === authoringGeneration.value) errorMessage.value = reason instanceof Error ? reason.message : 'Could not load recipient evidence.';
  } finally {
    if (generation === authoringGeneration.value) contextLoading.value = false;
  }
}
watch(opportunityId, () => { recipientId.value = ''; }, { flush: 'sync' });
watch([opportunityId, recipientId], ([opportunity, recipient], [previousOpportunity, previousRecipient]) => {
  if (previousRecipient) {
    contextDrafts.set(JSON.stringify([previousOpportunity, previousRecipient]), {
      theirs: theirInterest.value, mine: myInterest.value, theyGet: exchangeTheyGet.value,
      iGet: exchangeIGet.value, slots: { ...slotValues.value },
    });
  }
  const draft = contextDrafts.get(JSON.stringify([opportunity, recipient]));
  theirInterest.value = draft?.theirs ?? '';
  myInterest.value = draft?.mine ?? '';
  exchangeTheyGet.value = draft?.theyGet ?? '';
  exchangeIGet.value = draft?.iGet ?? '';
  slotValues.value = draft?.slots ?? {};
  authoringGeneration.value++;
  interests.value = [];
  exchange.value = { current: null, versions: [] };
  match.value = null;
  noMatch.value = false;
  contextLoading.value = false;
  resetMessage();
  void refreshAuthoring();
});
watch([selectedTemplate, recipientId], () => { const recipient = participants.value.find((item) => item.id === recipientId.value); for (const slot of slots.value) if (slot.startsWith('their_name:') && recipient) slotValues.value[slot] = recipient.name; });
onMounted(async () => {
  try { [templates.value, families.value, voiceSamples.value] = await Promise.all([parley.listWritingTemplates(), parley.listTemplateFamilies(), parley.listVoiceSamples()]); const state = await parley.getEmbeddingModel(); model.value = state.model; modelDigest.value = state.model_digest; calibratedKinds.value = state.calibrated_kinds; }
  catch (reason) { errorMessage.value = reason instanceof Error ? reason.message : 'Could not load authoring settings.'; }
});

async function run(action: () => Promise<void>) { if (busy.value) return; busy.value = true; resetMessage(); try { await action(); } catch (reason) { errorMessage.value = reason instanceof Error ? reason.message : 'The action failed.'; } finally { busy.value = false; } }
async function addInterest(side: 'them' | 'me') { const statement = (side === 'them' ? theirInterest.value : myInterest.value).trim(); if (!statement || !opportunityId.value || !recipientId.value) return; const item = await parley.addWritingInterest(opportunityId.value, { recipient_id: recipientId.value, side, statement, knowledge_kind: side === 'them' ? 'hypothesis' : 'owner_report', ...(side === 'them' ? { check_suggestion: 'Ask the recipient or verify against a source before treating this as fact.' } : {}) }); interests.value.push(item); if (side === 'them') theirInterest.value = ''; else myInterest.value = ''; responseMessage.value = 'Interest saved with its evidence status.'; }
async function saveExchange() { if (!opportunityId.value || !recipientId.value) return; const ids = interests.value.filter((item) => item.status === 'active').map((item) => item.id); const saved = await parley.saveWritingExchange(opportunityId.value, { recipient_id: recipientId.value, they_get: exchangeTheyGet.value.trim(), i_get: exchangeIGet.value.trim(), interest_ids: ids }); await refreshAuthoring(); responseMessage.value = `Exchange version ${String(saved.version ?? exchangeVersion.value)} saved. Attestation is still required.`; }
async function attest() { if (!opportunityId.value || !recipientId.value) return; await parley.attestWritingExchange(opportunityId.value, recipientId.value); await refreshAuthoring(); responseMessage.value = 'This exchange version is attested. Any edit creates a new unattested version.'; }
async function saveTemplate() { let family = families.value.find((item) => item.name === familyName.value); if (!family) { family = await parley.createTemplateFamily(familyName.value.trim()); families.value.push(family); } const rules = templateBody.value.split(/\n+/).filter((line) => line.trim()).map((line) => [...new Set([...(line.includes('{{gift:') ? ['R1'] : []), ...(line.includes('{{proof:') ? ['R3'] : []), ...(line.includes('{{artifact:') ? ['R4'] : []), ...(line.includes('{{identity_link:') ? ['R7'] : []), ...(line.includes('{{capacity_line:') ? ['R6'] : []), ...(line.includes('{{ask:') ? ['R2'] : [])])]); const saved = await parley.createWritingTemplateVersion(family.id, { name: templateName.value.trim(), body: templateBody.value, stage_class: stage.value, channel: channel.value, sentence_rules: rules, proof_fallback: 'omit_sentence' }); templates.value.unshift(saved); templateId.value = saved.id; responseMessage.value = 'Immutable draft version saved. Review and validate it before activation.'; }
async function transitionTemplate(status: 'approve' | 'activate') { if (!templateId.value) return; if (status === 'approve') await parley.approveWritingTemplate(templateId.value); else await parley.activateWritingTemplate(templateId.value); templates.value = await parley.listWritingTemplates(); responseMessage.value = `Template ${status}d.`; }
async function setModel() { await parley.setEmbeddingModel(model.value.trim()); const state = await parley.getEmbeddingModel(); modelDigest.value = state.model_digest; calibratedKinds.value = state.calibrated_kinds; responseMessage.value = 'Embedding model changed. Matching is unavailable until thresholds are calibrated for its digest.'; }
async function addVoiceSample() { await parley.createVoiceSample(voiceText.value.trim(), voiceChannel.value); voiceSamples.value = await parley.listVoiceSamples(); voiceText.value = ''; responseMessage.value = 'Owner voice sample saved. Existing outputs were not rewritten.'; }
async function retireVoice(id: string) { await parley.retireVoiceSample(id); voiceSamples.value = await parley.listVoiceSamples(); responseMessage.value = 'Voice sample retired; its history is preserved.'; }
async function buildTemplateCandidate() {
  const job = await parley.buildWritingTemplates({ stage_class: stage.value, rungs: [1], rules: requestedRules.value, channel: channel.value, max_chars: channel.value === 'email' ? 1200 : 300, voice_sample_ids: selectedVoiceIds.value });
  const result = await poll(job.job_id);
  templateCandidates.value = Array.isArray(result?.variants)
    ? result.variants.flatMap(item => typeof item === 'object' && item !== null ? [item as Record<string, unknown>] : [])
    : [];
}
function useTemplateCandidate(candidate: Record<string, unknown>) { if (typeof candidate.name === 'string') templateName.value = candidate.name; if (typeof candidate.body === 'string') templateBody.value = candidate.body; responseMessage.value = 'Candidate copied into the editable template form. It remains a proposal until owner validation and activation.'; }
async function poll(id: string, generation?: number) {
  jobId.value = id;
  for (let attempt = 0; attempt < 360; attempt += 1) {
    const job = await parley.getRoleJob(id);
    if (generation !== undefined && generation !== authoringGeneration.value) return null;
    if (!['queued', 'running'].includes(job.status)) {
      if (job.status === 'succeeded') return job.result;
      throw new Error(job.last_error ?? `Agent job ended as ${job.status}.`);
    }
    await new Promise((resolve) => globalThis.setTimeout(resolve, 1000));
  }
  throw new Error('The job is still running. Its job ID is shown below.');
}
async function runMatch() {
  const opportunity = opportunityId.value;
  const recipient = recipientId.value;
  if (!opportunity || !recipient) return;
  const generation = authoringGeneration.value;
  match.value = null;
  noMatch.value = false;
  const job = await parley.startMatching(opportunity, recipient);
  const result = await poll(job.job_id, generation);
  if (!result || generation !== authoringGeneration.value) return;
  noMatch.value = result.no_match === true;
  if (noMatch.value) return;
  const next = await parley.getWritingMatch(opportunity, recipient);
  if (generation === authoringGeneration.value) match.value = next;
}
function matchedIds(): string[] {
  return shortlist.value.flatMap(entry => typeof entry === 'object' && entry !== null && 'asset_id' in entry && typeof entry.asset_id === 'string' ? [entry.asset_id] : []);
}
const proposalJob = ref('');
async function compose() {
  if (!opportunityId.value || !recipientId.value || selectedTemplate.value?.status !== 'active' || exchange.value.current?.fairness_attested !== true) {
    throw new Error('Choose an active template and attest the current exchange.');
  }
  const fills = slots.value.map(slot => {
    const value = slotValues.value[slot]?.trim();
    if (!value) throw new Error(`Fill the ${slot} slot before composing.`);
    const resource = props.resources.find(item => item.id === value && item.readiness === 'ready');
    return { slot, value, ...(resource ? { resource_id: resource.id } : {}) };
  });
  const origin = { opportunityId: opportunityId.value, recipientId: recipientId.value };
  const generation = authoringGeneration.value;
  const job = await parley.composeWritingMaterial(origin.opportunityId, {
    recipient_id: origin.recipientId, kind: 'outreach', template_version_id: selectedTemplate.value.id,
    exchange_version: exchangeVersion.value, slot_fills: fills, selected_match_ids: matchedIds(),
  });
  const result = await poll(job.job_id, generation);
  if (!result || generation !== authoringGeneration.value) return;
  proposal.value = result;
  proposalFor.value = origin;
  proposalJob.value = job.job_id;
}
async function saveDraft() {
  if (!proposalIsCurrent.value || !proposalJob.value || typeof proposal.value?.body !== 'string') return;
  await parley.saveWritingDraft(opportunityId.value, proposalJob.value, proposal.value.body);
  proposalJob.value = '';
  responseMessage.value = 'Draft saved to the material registry.';
  emit('saved');
}
</script>

<template>
  <section
    class="writing-workspace"
    aria-labelledby="writing-heading"
  >
    <h2
      id="writing-heading"
      tabindex="-1"
    >
      Composer, evidence and templates
    </h2>
    <p
      v-if="errorMessage"
      role="alert"
    >
      {{ errorMessage }}
    </p><p
      v-if="responseMessage"
      role="status"
    >
      {{ responseMessage }}
    </p>
    <div class="writing-grid">
      <section aria-label="Recipient exchange">
        <h3>Recipient and exchange</h3>
        <label>Opportunity<select
          v-model="opportunityId"
          :disabled="busy"
        ><option value="">Choose an opportunity</option><option
          v-for="item in opportunities"
          :key="item.id"
          :value="item.id"
        >{{ item.title }}</option></select></label>
        <label>Named participant<select
          v-model="recipientId"
          :disabled="busy || !opportunityId"
        ><option value="">Choose a participant</option><option
          v-for="person in participants"
          :key="person.id"
          :value="person.id"
        >{{ person.name }}</option></select></label>
        <p
          v-if="contextLoading"
          role="status"
        >
          Loading recipient evidence…
        </p>
        <ul>
          <li
            v-for="item in interests"
            :key="item.id"
          >
            <strong>{{ item.side === 'them' ? 'Their interest' : 'Owner interest' }} · {{ item.knowledge_kind }}:</strong> {{ item.statement }} <small v-if="item.check_suggestion">{{ item.check_suggestion }}</small>
          </li>
        </ul>
        <form @submit.prevent="run(() => addInterest('them'))">
          <label>What they may need<textarea
            v-model="theirInterest"
            required
          /></label><button :disabled="busy || contextLoading || !recipientId">
            Save as a hypothesis
          </button>
        </form>
        <form @submit.prevent="run(() => addInterest('me'))">
          <label>What you need<textarea
            v-model="myInterest"
            required
          /></label><button :disabled="busy || contextLoading || !recipientId">
            Save owner-reported interest
          </button>
        </form>
        <label>They get<textarea v-model="exchangeTheyGet" /></label><label>You get<textarea v-model="exchangeIGet" /></label>
        <button
          type="button"
          :disabled="busy || contextLoading || !recipientId || interests.length < 2 || !exchangeTheyGet.trim() || !exchangeIGet.trim()"
          @click="run(saveExchange)"
        >
          Save exchange version
        </button>
        <button
          type="button"
          :disabled="busy || !exchange.current || exchange.current.fairness_attested === true"
          @click="run(attest)"
        >
          Attest both sides benefit
        </button>
        <p v-if="exchange.current">
          Current exchange v{{ exchangeVersion }} · {{ exchange.current.fairness_attested ? 'attested' : 'needs owner attestation' }}
        </p>
      </section>
      <section aria-label="Matching and model">
        <h3>Local matching</h3><label>Embedding model<input v-model="model"></label><button
          type="button"
          :disabled="busy || !model.trim()"
          @click="run(setModel)"
        >
          Set local embedding model
        </button>
        <p>Model digest: {{ modelDigest ?? 'not loaded' }} · calibrated kinds: {{ calibratedKinds.join(', ') || 'none' }}</p><p>Without a matching model calibration, the system cannot rank resources.</p>
        <button
          type="button"
          :disabled="busy || !recipientId || !opportunityId || !modelDigest"
          @click="run(runMatch)"
        >
          Run exact matcher
        </button>
        <details v-if="match">
          <summary>Matched resources ({{ shortlist.length }})</summary>
          <pre>{{ JSON.stringify(shortlist, null, 2) }}</pre>
        </details>
        <p v-else-if="noMatch">
          No eligible resource exceeded its calibrated threshold; no slot was filled.
        </p>
      </section>
      <section
        id="output-templates"
        tabindex="-1"
        aria-label="Immutable templates"
      >
        <h3>Template versions</h3>
        <form @submit.prevent="run(addVoiceSample)">
          <label>Owner voice sample<textarea
            v-model="voiceText"
            required
          /></label>
          <label>Voice sample channel<input v-model="voiceChannel"></label>
          <button :disabled="busy || !voiceText.trim()">
            Save owner voice sample
          </button>
        </form>
        <ul>
          <li
            v-for="sample in voiceSamples"
            :key="sample.id"
          >
            <label><input
              v-model="selectedVoiceIds"
              type="checkbox"
              :value="sample.id"
            >Use sample {{ sample.channel ?? 'unspecified channel' }} · v{{ sample.revision }}</label>
            <button
              type="button"
              :disabled="busy"
              @click="run(() => retireVoice(sample.id))"
            >
              Retire sample
            </button>
          </li>
        </ul>
        <fieldset>
          <legend>Template Builder rules</legend>
          <label
            v-for="rule in ['R1','R2','R3','R4','R5','R6','R7']"
            :key="rule"
          ><input
            v-model="requestedRules"
            type="checkbox"
            :value="rule"
          >{{ rule }}</label>
          <button
            type="button"
            :disabled="busy || !requestedRules.length"
            @click="run(buildTemplateCandidate)"
          >
            Build template candidate
          </button>
          <ul>
            <li
              v-for="(candidate, index) in templateCandidates"
              :key="index"
            >
              <strong>{{ candidate.name }}</strong><p>{{ candidate.notes }}</p><button
                type="button"
                @click="useTemplateCandidate(candidate)"
              >
                Edit this candidate
              </button>
            </li>
          </ul>
        </fieldset>
        <label>Family<input v-model="familyName"></label><label>Version name<input v-model="templateName"></label><label>Stage<select v-model="stage"><option>identified</option><option>prepared</option><option>touched</option><option>responded</option><option>converging</option></select></label><label>Channel<input v-model="channel"></label><label>Template body<textarea
          v-model="templateBody"
          rows="6"
        /></label>
        <button
          type="button"
          :disabled="busy"
          @click="run(saveTemplate)"
        >
          Save as new draft version
        </button>
        <label>Version<select v-model="templateId"><option value="">Choose a version</option><option
          v-for="item in templates"
          :key="item.id"
          :value="item.id"
        >{{ item.name }} v{{ item.version }} · {{ item.status }}</option></select></label>
        <button
          type="button"
          :disabled="busy || !templateId || selectedTemplate?.status !== 'draft'"
          @click="run(() => transitionTemplate('approve'))"
        >
          Owner approve version
        </button><button
          type="button"
          :disabled="busy || !templateId || selectedTemplate?.status !== 'approved'"
          @click="run(() => transitionTemplate('activate'))"
        >
          Activate validated version
        </button>
      </section>
      <section aria-label="Compose and lint">
        <h3>Compose and review</h3><p v-if="slots.length">
          Resolve declared slots for this recipient:
        </p>
        <label
          v-for="slot in slots"
          :key="slot"
        >{{ slot }}<input
          v-model="slotValues[slot]"
          :list="slot.includes('proof') || slot.includes('gift') || slot.includes('artifact') ? 'ready-resources' : undefined"
        ></label>
        <datalist id="ready-resources">
          <option
            v-for="resource in resources.filter((item) => item.readiness === 'ready')"
            :key="resource.id"
            :value="resource.id"
          >
            {{ resource.title }}
          </option>
        </datalist>
        <button
          type="button"
          :disabled="busy || !recipientId || !modelDigest || selectedTemplate?.status !== 'active' || exchange.current?.fairness_attested !== true"
          @click="run(compose)"
        >
          Generate recipient-bound proposal
        </button>
        <p v-if="jobId">
          Durable job {{ jobId }}
        </p>
        <pre
          v-if="proposalIsCurrent && proposal?.body"
          class="proposal-body"
        >{{ proposal.body }}</pre>
        <p v-else-if="proposal">
          Switch back to the original recipient to review this proposal.
        </p>
        <button
          type="button"
          :disabled="busy || !proposalIsCurrent || !proposalJob || !proposal?.body"
          @click="run(saveDraft)"
        >
          Save proposal as draft
        </button>
        <p v-if="outputs.some((item) => item.opportunity_id === opportunityId && item.recipient_id === recipientId)">
          This participant has saved versions in the material registry.
        </p>
      </section>
    </div>
  </section>
</template>

<style scoped>
.writing-workspace { overflow-wrap: anywhere; }
.writing-workspace h2 { margin: 0 0 .75rem; font: 1.65rem/1.2 Georgia, serif; }
.writing-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); align-items: start; gap: .85rem; }
.writing-grid > section { padding: 1rem; background: #fffef9; border: 1px solid #c7d1c3; border-radius: .55rem; }
.writing-grid h3 { margin-top: 0; }
.writing-grid button { margin: .1rem .2rem .15rem 0; }
.writing-grid fieldset { min-width: 0; margin: .55rem 0; border: 1px solid #c7d1c3; border-radius: .35rem; padding: .65rem; }
.writing-grid form { margin: .55rem 0; }
.writing-grid pre { white-space: pre-wrap; overflow-wrap: anywhere; padding: .65rem; background: #edf2e8; }
.proposal-body { font: inherit; }
@media (max-width: 850px) { .writing-grid { grid-template-columns: minmax(0, 1fr); } }
</style>
