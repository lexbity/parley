<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { parley, type GraphEntity, type Material, type Opportunity, type Resource, type WritingInterest, type WritingMatch, type WritingTemplate } from '../../api/client';

const props = defineProps<{ opportunities: Opportunity[]; parties: GraphEntity[]; resources: Resource[]; outputs: Material[] }>();
const opportunityId = ref(''); const recipientId = ref(''); const interests = ref<WritingInterest[]>([]); const exchange = ref<{ current: Record<string, unknown> | null; versions: Array<Record<string, unknown>> }>({ current: null, versions: [] });
const templates = ref<WritingTemplate[]>([]); const families = ref<Array<{ id: string; name: string }>>([]); const model = ref(''); const modelDigest = ref<string | null>(null); const calibratedKinds = ref<string[]>([]); const match = ref<WritingMatch | null>(null); const jobId = ref(''); const proposal = ref<Record<string, unknown> | null>(null);
const theirInterest = ref(''); const myInterest = ref(''); const exchangeTheyGet = ref(''); const exchangeIGet = ref(''); const familyName = ref('One ask'); const templateName = ref('One ask'); const templateBody = ref('{{their_name:name}},\nI can offer {{gift:guide}}.\nWould a short review be useful {{ask:next_step}}?'); const stage = ref('identified'); const channel = ref('email'); const templateId = ref(''); const slotValues = ref<Record<string, string>>({}); const responseMessage = ref(''); const errorMessage = ref(''); const busy = ref(false);
const selectedOpportunity = computed(() => props.opportunities.find((item) => item.id === opportunityId.value));
const participants = computed(() => selectedOpportunity.value?.participant_ids.map((id) => props.parties.find((party) => party.id === id)).filter((party): party is GraphEntity => !!party) ?? []);
const selectedTemplate = computed(() => templates.value.find((item) => item.id === templateId.value));
const slots = computed(() => [...new Set((selectedTemplate.value?.body.match(/\{\{[^}]+\}\}/g) ?? []).map((slot) => slot.slice(2, -2)))]);
const exchangeVersion = computed(() => Number(exchange.value.current?.version ?? 0));
const voiceSamples = ref<Array<{ id: string; text: string; channel: string | null; revision: number }>>([]); const voiceText = ref(''); const voiceChannel = ref('email'); const selectedVoiceIds = ref<string[]>([]); const requestedRules = ref(['R2']); const templateCandidates = computed(() => Array.isArray(proposal.value?.variants) ? proposal.value.variants.flatMap((item) => typeof item === 'object' && item !== null ? [item as Record<string, unknown>] : []) : []);

function resetMessage() { errorMessage.value = ''; responseMessage.value = ''; }
async function refreshAuthoring() {
  if (!opportunityId.value || !recipientId.value) { interests.value = []; exchange.value = { current: null, versions: [] }; match.value = null; return; }
  try { [interests.value, exchange.value] = await Promise.all([parley.getWritingInterests(opportunityId.value, recipientId.value), parley.getWritingExchange(opportunityId.value, recipientId.value)]); }
  catch (reason) { errorMessage.value = reason instanceof Error ? reason.message : 'Could not load recipient evidence.'; }
}
watch(opportunityId, () => { recipientId.value = ''; match.value = null; refreshAuthoring(); });
watch(recipientId, () => { match.value = null; refreshAuthoring(); });
watch([selectedTemplate, recipientId], () => { const recipient = participants.value.find((item) => item.id === recipientId.value); for (const slot of slots.value) if (slot.startsWith('their_name:') && recipient) slotValues.value[slot] = recipient.name; });
onMounted(async () => {
  try { [templates.value, families.value, voiceSamples.value] = await Promise.all([parley.listWritingTemplates(), parley.listTemplateFamilies(), parley.listVoiceSamples()]); const state = await parley.getEmbeddingModel(); model.value = state.model; modelDigest.value = state.model_digest; calibratedKinds.value = state.calibrated_kinds; }
  catch (reason) { errorMessage.value = reason instanceof Error ? reason.message : 'Could not load authoring settings.'; }
});

async function run(action: () => Promise<void>) { busy.value = true; resetMessage(); try { await action(); } catch (reason) { errorMessage.value = reason instanceof Error ? reason.message : 'The action failed.'; } finally { busy.value = false; } }
async function addInterest(side: 'them' | 'me') { const statement = (side === 'them' ? theirInterest.value : myInterest.value).trim(); if (!statement || !opportunityId.value || !recipientId.value) return; const item = await parley.addWritingInterest(opportunityId.value, { recipient_id: recipientId.value, side, statement, knowledge_kind: side === 'them' ? 'hypothesis' : 'owner_report', ...(side === 'them' ? { check_suggestion: 'Ask the recipient or verify against a source before treating this as fact.' } : {}) }); interests.value.push(item); if (side === 'them') theirInterest.value = ''; else myInterest.value = ''; responseMessage.value = 'Interest saved with its evidence status.'; }
async function saveExchange() { if (!opportunityId.value || !recipientId.value) return; const ids = interests.value.filter((item) => item.status === 'active').map((item) => item.id); const saved = await parley.saveWritingExchange(opportunityId.value, { recipient_id: recipientId.value, they_get: exchangeTheyGet.value.trim(), i_get: exchangeIGet.value.trim(), interest_ids: ids }); await refreshAuthoring(); responseMessage.value = `Exchange version ${String(saved.version ?? exchangeVersion.value)} saved. Attestation is still required.`; }
async function attest() { if (!opportunityId.value || !recipientId.value) return; await parley.attestWritingExchange(opportunityId.value, recipientId.value); await refreshAuthoring(); responseMessage.value = 'This exchange version is attested. Any edit creates a new unattested version.'; }
async function saveTemplate() { let family = families.value.find((item) => item.name === familyName.value); if (!family) { family = await parley.createTemplateFamily(familyName.value.trim()); families.value.push(family); } const rules = templateBody.value.split(/\n+/).filter((line) => line.trim()).map((line) => [...new Set([...(line.includes('{{gift:') ? ['R1'] : []), ...(line.includes('{{proof:') ? ['R3'] : []), ...(line.includes('{{artifact:') ? ['R4'] : []), ...(line.includes('{{identity_link:') ? ['R7'] : []), ...(line.includes('{{capacity_line:') ? ['R6'] : []), ...(line.includes('{{ask:') ? ['R2'] : [])])]); const saved = await parley.createWritingTemplateVersion(family.id, { name: templateName.value.trim(), body: templateBody.value, stage_class: stage.value, channel: channel.value, sentence_rules: rules, proof_fallback: 'omit_sentence' }); templates.value.unshift(saved); templateId.value = saved.id; responseMessage.value = 'Immutable draft version saved. Review and validate it before activation.'; }
async function transitionTemplate(status: 'approve' | 'activate') { if (!templateId.value) return; if (status === 'approve') await parley.approveWritingTemplate(templateId.value); else await parley.activateWritingTemplate(templateId.value); templates.value = await parley.listWritingTemplates(); responseMessage.value = `Template ${status}d.`; }
async function setModel() { await parley.setEmbeddingModel(model.value.trim()); const state = await parley.getEmbeddingModel(); modelDigest.value = state.model_digest; calibratedKinds.value = state.calibrated_kinds; responseMessage.value = 'Embedding model changed. Matching is unavailable until thresholds are calibrated for its digest.'; }
async function addVoiceSample() { await parley.createVoiceSample(voiceText.value.trim(), voiceChannel.value); voiceSamples.value = await parley.listVoiceSamples(); voiceText.value = ''; responseMessage.value = 'Owner voice sample saved. Existing outputs were not rewritten.'; }
async function retireVoice(id: string) { await parley.retireVoiceSample(id); voiceSamples.value = await parley.listVoiceSamples(); responseMessage.value = 'Voice sample retired; its history is preserved.'; }
async function buildTemplateCandidate() { const job = await parley.buildWritingTemplates({ stage_class: stage.value, rungs: [1], rules: requestedRules.value, channel: channel.value, max_chars: channel.value === 'email' ? 1200 : 300, voice_sample_ids: selectedVoiceIds.value }); await poll(job.job_id); }
function useTemplateCandidate(candidate: Record<string, unknown>) { if (typeof candidate.name === 'string') templateName.value = candidate.name; if (typeof candidate.body === 'string') templateBody.value = candidate.body; responseMessage.value = 'Candidate copied into the editable template form. It remains a proposal until owner validation and activation.'; }
async function poll(id: string) { jobId.value = id; proposal.value = null; for (let attempt = 0; attempt < 360; attempt += 1) { const job = await parley.getRoleJob(id); if (!['queued', 'running'].includes(job.status)) { if (job.status === 'succeeded') proposal.value = job.result; else errorMessage.value = job.last_error ?? `Agent job ended as ${job.status}.`; return; } await new Promise((resolve) => globalThis.setTimeout(resolve, 1000)); } errorMessage.value = 'Agent job is still running. Its durable job id is retained so you can reopen it.'; }
async function runMatch() { if (!opportunityId.value || !recipientId.value) return; if (!modelDigest.value) throw new Error('No local embedding model digest is available. Configure the model in Settings first.'); const job = await parley.startMatching(opportunityId.value, recipientId.value); await poll(job.job_id); if (proposal.value) { try { match.value = await parley.getWritingMatch(opportunityId.value, recipientId.value); } catch (reason) { errorMessage.value = reason instanceof Error ? reason.message : 'Match result is not current.'; } } }
function matchedIds(): string[] { if (!match.value) return []; try { const value: unknown = JSON.parse(match.value.shortlist_json); if (!Array.isArray(value)) return []; return value.flatMap((entry) => typeof entry === 'object' && entry !== null && typeof (entry as { asset_id?: unknown }).asset_id === 'string' ? [(entry as { asset_id: string }).asset_id] : []); } catch { return []; } }
async function compose() { if (!opportunityId.value || !recipientId.value || !selectedTemplate.value || !exchangeVersion.value) throw new Error('Select an active template and save and attest an exchange first.'); const fills = slots.value.map((slot) => { const value = slotValues.value[slot]?.trim(); if (!value) throw new Error(`Fill the ${slot} slot before composing.`); const resource = props.resources.find((item) => item.id === value && item.readiness === 'ready'); return { slot, value, ...(resource ? { resource_id: resource.id } : {}) }; }); const job = await parley.composeWritingMaterial(opportunityId.value, { recipient_id: recipientId.value, kind: 'outreach', template_version_id: selectedTemplate.value.id, exchange_version: exchangeVersion.value, slot_fills: fills, selected_match_ids: matchedIds() }); await poll(job.job_id); }
async function saveDraft() { if (!proposal.value || !opportunityId.value || typeof proposal.value.body !== 'string') throw new Error('There is no completed Composer proposal to save.'); const saved = await parley.saveWritingDraft(opportunityId.value, jobId.value, proposal.value.body); responseMessage.value = `Draft saved as ${String(saved.material_id ?? 'a new material')}; it is not approved or released.`; }
</script>

<template>
  <section aria-labelledby="writing-heading">
    <h2 id="writing-heading">
      Composer, evidence and templates
    </h2>
    <p>Every working material belongs to one opportunity and one named participant. Generation creates a proposal; saving it creates a draft. Approval and release remain separate owner actions.</p>
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
    <div class="workspace-grid">
      <section aria-label="Recipient exchange">
        <h3>1. Recipient and exchange</h3>
        <label>Opportunity<select v-model="opportunityId"><option value="">Choose an opportunity</option><option
          v-for="item in opportunities"
          :key="item.id"
          :value="item.id"
        >{{ item.title }}</option></select></label>
        <label>Named participant<select v-model="recipientId"><option value="">Choose a participant</option><option
          v-for="person in participants"
          :key="person.id"
          :value="person.id"
        >{{ person.name }}</option></select></label>
        <p v-if="recipientId">
          Recipient {{ parties.find((item) => item.id === recipientId)?.name }} stays bound to this workspace.
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
          /></label><button :disabled="busy || !recipientId">
            Save as a hypothesis
          </button>
        </form>
        <form @submit.prevent="run(() => addInterest('me'))">
          <label>What you need<textarea
            v-model="myInterest"
            required
          /></label><button :disabled="busy || !recipientId">
            Save owner-reported interest
          </button>
        </form>
        <label>They get<textarea v-model="exchangeTheyGet" /></label><label>You get<textarea v-model="exchangeIGet" /></label>
        <button
          type="button"
          :disabled="busy || !recipientId || interests.length < 2"
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
        <h3>2. Local matching</h3><label>Embedding model<input v-model="model"></label><button
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
        <pre v-if="match">{{ JSON.stringify(JSON.parse(match.shortlist_json), null, 2) }}</pre>
        <p v-else-if="proposal?.no_match">
          No eligible resource exceeded its calibrated threshold; no slot was filled.
        </p>
      </section>
      <section aria-label="Immutable templates">
        <h3>3. Template versions</h3>
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
        <h3>4. Compose and review</h3><p v-if="slots.length">
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
          :disabled="busy || !recipientId || selectedTemplate?.status !== 'active' || exchange.current?.fairness_attested !== true"
          @click="run(compose)"
        >
          Generate recipient-bound proposal
        </button>
        <p v-if="jobId">
          Durable job {{ jobId }}
        </p><pre v-if="proposal">{{ JSON.stringify(proposal, null, 2) }}</pre><button
          type="button"
          :disabled="busy || !proposal?.body"
          @click="run(saveDraft)"
        >
          Save proposal as draft
        </button>
        <p v-if="outputs.some((item) => item.opportunity_id === opportunityId && item.recipient_id === recipientId)">
          Related drafts and versions appear in Output and Queue.
        </p>
      </section>
    </div>
  </section>
</template>
