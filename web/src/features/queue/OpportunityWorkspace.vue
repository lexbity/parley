<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { newOperationKey, parley, type DirectoryEntity, type Material, type MaterialGateDTO, type Opportunity, type OpportunityExchange, type OpportunityInterest, type Resource } from '../../api/client';
import MaterialEditor, { type CurrentRelease, type MaterialApproval, type MaterialGate, type MaterialRecipient, type MaterialRecord, type MaterialSentence } from '../output/MaterialEditor.vue';

const props = withDefaults(defineProps<{ surface?: 'queue' | 'output'; opportunityId?: string; embedded?: boolean; refreshKey?:number }>(), { surface: 'queue', opportunityId:undefined, embedded:false, refreshKey:0 });
const opportunities = ref<Opportunity[]>([]);
const parties = ref<DirectoryEntity[]>([]);
const resources = ref<Resource[]>([]);
const interests = ref<OpportunityInterest[]>([]);
const exchange = ref<OpportunityExchange | null>(null);
const materials = ref<Material[]>([]);
const selectedOpportunityId = ref('');
const selectedMaterialId = ref('');
const selectedRecipientId = ref('');
const title = ref('');
const createParticipantIds = ref<string[]>([]);
const interestSide = ref<'me'|'them'>('them');
const interestStatement = ref('');
const theyGet = ref('');
const iGet = ref('');
const selectedInterestIds = ref<string[]>([]);
const fairnessAttested = ref(false);
const materialTitle = ref('');
const materialKind = ref<'outreach'|'follow_up'|'handoff'>('outreach');
const gate = ref<MaterialGateDTO | null>(null);
const release = ref<CurrentRelease | null>(null);
const error = ref('');
const status = ref('');
const busy = ref(false);

const selectedOpportunity = computed(() => opportunities.value.find((item) => item.id === selectedOpportunityId.value) ?? null);
const selectedMaterial = computed(() => materials.value.find((item) => item.id === selectedMaterialId.value) ?? null);
const claimReferences = computed(() => [
  ...resources.value.filter((item) => item.readiness === 'ready').map((item) => ({ kind:item.kind, id:item.id, label:item.title, tier:3 })),
  ...interests.value.filter((item) => item.status === 'active').map((item) => ({ kind:'interest', id:item.id, label:item.statement, tier:item.tier })),
  ...(selectedMaterial.value?.claim_references ?? []),
]);
function materialRecord(item: Material): MaterialRecord {
  if (!item.current_version) throw new Error('Material has no working version yet.');
  return {
    id: item.id,
    kind: item.kind,
    versionId: item.current_version.id,
    revision: item.revision,
    body: item.current_version.body,
    status: item.approval?.version_id === item.current_version.id ? 'approved' : 'draft',
    updatedAt: item.updated_at,
    sentences: item.current_version.sentences,
    slotFills: item.current_version.slot_fills,
    protectedTokens: item.current_version.protected_tokens,
  };
}

function materialGate(value: MaterialGateDTO): MaterialGate {
  const failures = value.results.filter((item) => !item.pass);
  return {
    evaluatedAt: value.evaluated_at,
    hardFailures: failures.filter((item) => item.severity === 'hard').map((item) => ({ id: item.id, detail: item.detail })),
    softFailures: failures.filter((item) => item.severity === 'soft').map((item) => ({ id: item.id, detail: item.detail })),
    copyEnabled: value.copy_enabled,
  };
}

function materialApproval(item: Material): MaterialApproval | null {
  const approval = item.approval;
  return approval ? { versionId: approval.version_id, recipientId: approval.recipient_id, approvedAt: approval.approved_at } : null;
}

async function loadWorkspace() {
  error.value = '';
  const [opportunityRows, directoryPage, resourceRows] = await Promise.all([
    parley.listOpportunities(),
    parley.searchDirectory({}, undefined, 1, 200),
    parley.listResources(),
  ]);
  opportunities.value = opportunityRows;
  parties.value = directoryPage.items.filter((item) => item.kind === 'person' || item.kind === 'org');
  resources.value = resourceRows;
  if (props.opportunityId) selectedOpportunityId.value = props.opportunityId;
  else if (!selectedOpportunityId.value || !opportunityRows.some((item) => item.id === selectedOpportunityId.value)) selectedOpportunityId.value = opportunityRows[0]?.id ?? '';
}

async function loadOpportunity() {
  if (!selectedOpportunityId.value) {
    interests.value = [];
    exchange.value = null;
    materials.value = [];
    selectedMaterialId.value = '';
    return;
  }
  const id = selectedOpportunityId.value;
  const [interestRows, exchangeRow, materialRows] = await Promise.all([
    parley.listOpportunityInterests(id),
    parley.getOpportunityExchange(id),
    parley.listOpportunityMaterials(id),
  ]);
  if (selectedOpportunityId.value !== id) return;
  interests.value = interestRows;
  exchange.value = exchangeRow;
  materials.value = materialRows;
  if (!materials.value.some((item) => item.id === selectedMaterialId.value)) selectedMaterialId.value = materials.value[0]?.id ?? '';
  if (exchangeRow) {
    theyGet.value = exchangeRow.they_get;
    iGet.value = exchangeRow.i_get;
    selectedInterestIds.value = [...exchangeRow.interest_ids];
    fairnessAttested.value = exchangeRow.fairness_attested;
  } else {
    theyGet.value = '';
    iGet.value = '';
    selectedInterestIds.value = [];
    fairnessAttested.value = false;
  }
}

async function loadGate() {
  const material = selectedMaterial.value;
  if (!material?.current_version) { gate.value = null; return; }
  try {
    gate.value = await parley.getMaterialGate(material.id, material.current_version.id);
  } catch (cause) {
    gate.value = null;
    error.value = cause instanceof Error ? cause.message : 'Current gate status is unavailable.';
  }
  if (material.approval?.version_id !== material.current_version.id) release.value = null;
}

watch(selectedOpportunityId, () => { void loadOpportunity().then(loadGate).catch((cause: unknown) => { error.value = cause instanceof Error ? cause.message : 'Opportunity details could not be loaded.'; }); });
watch(() => props.opportunityId, (id) => { if (id) selectedOpportunityId.value = id; });
watch(selectedMaterialId, () => { void loadGate(); });
watch(() => props.refreshKey, () => { if(props.refreshKey) void loadWorkspace().catch((cause:unknown)=>{error.value=cause instanceof Error?cause.message:'Workspace could not be refreshed.';}); });

async function createOpportunity() {
  if (!title.value.trim()) return;
  error.value = ''; busy.value = true;
  try {
    const created = await parley.createOpportunity({ title: title.value.trim(), participant_ids: [...createParticipantIds.value] }, newOperationKey());
    title.value = ''; createParticipantIds.value = [];
    await loadWorkspace();
    selectedOpportunityId.value = created.id;
    status.value = 'Opportunity saved. No message was created or sent.';
  } catch (cause) { error.value = cause instanceof Error ? cause.message : 'Opportunity could not be saved.'; }
  finally { busy.value = false; }
}

async function addInterest() {
  const opportunity = selectedOpportunity.value;
  if (!opportunity || !interestStatement.value.trim()) return;
  error.value = ''; busy.value = true;
  try {
    const saved = await parley.createInterest(opportunity.id, { side: interestSide.value, statement: interestStatement.value.trim() }, newOperationKey());
    interests.value = [...interests.value, saved];
    interestStatement.value = '';
    status.value = 'Owner-authored interest saved as tier 3.';
  } catch (cause) { error.value = cause instanceof Error ? cause.message : 'Interest could not be saved.'; }
  finally { busy.value = false; }
}

async function saveExchange() {
  const opportunity = selectedOpportunity.value;
  if (!opportunity) return;
  error.value = ''; busy.value = true;
  try {
    exchange.value = await parley.updateExchange(opportunity.id, {
      expected_revision: exchange.value?.version ?? 0,
      they_get: theyGet.value,
      i_get: iGet.value,
      interest_ids: [...selectedInterestIds.value],
      fairness_attested: fairnessAttested.value,
    }, newOperationKey());
    status.value = fairnessAttested.value ? 'Exchange and owner fairness attestation saved.' : 'Exchange saved. Copy remains blocked until fairness is explicitly attested.';
    await loadGate();
  } catch (cause) { error.value = cause instanceof Error ? cause.message : 'Exchange could not be saved.'; }
  finally { busy.value = false; }
}

async function createMaterial() {
  const opportunity = selectedOpportunity.value;
  if (!opportunity || !selectedRecipientId.value || !materialTitle.value.trim()) return;
  error.value = ''; busy.value = true;
  try {
    const created = await parley.createMaterial(opportunity.id, { recipient_id: selectedRecipientId.value, kind: materialKind.value, title: materialTitle.value.trim() }, newOperationKey());
    materials.value = [...materials.value.filter((item) => item.id !== created.id), created];
    selectedMaterialId.value = created.id;
    materialTitle.value = '';
    status.value = 'Working material saved as a draft for the selected recipient.';
  } catch (cause) { error.value = cause instanceof Error ? cause.message : 'Material could not be created.'; }
  finally { busy.value = false; }
}

async function startFirstVersion() {
  const item = selectedMaterial.value;
  if (!item || item.current_version) return;
  await saveVersion({ materialId:item.id,expectedRevision:item.revision,body:'',sentences:[],slotFills:{},protectedTokens:[] });
}

async function saveVersion(input: { materialId:string;expectedRevision:number;body:string;sentences:MaterialSentence[];slotFills:Record<string,string>;protectedTokens:string[] }) {
  error.value = ''; busy.value = true;
  try {
    const saved = await parley.saveMaterialVersion(input.materialId, { expected_revision:input.expectedRevision,body:input.body,sentences:input.sentences,slot_fills:input.slotFills,protected_tokens:input.protectedTokens }, newOperationKey());
    materials.value = materials.value.map((item) => item.id === saved.id ? saved : item);
    release.value = null;
    status.value = 'New immutable working version saved. Approval and release must be reviewed again.';
    await loadGate();
  } catch (cause) { error.value = cause instanceof Error ? cause.message : 'Version was not saved; review the current revision.'; }
  finally { busy.value = false; }
}

async function approve(input: { materialId:string;versionId:string;expectedRevision:number;overrideReasons:Record<string,string> }) {
  error.value = ''; busy.value = true;
  try {
    await parley.approveMaterialVersion(input.materialId, { expected_revision:input.expectedRevision,version_id:input.versionId,override_reasons:input.overrideReasons }, newOperationKey());
    materials.value = await parley.listOpportunityMaterials(selectedOpportunityId.value);
    status.value = 'Exact version approved for its named recipient.';
    await loadGate();
  } catch (cause) { error.value = cause instanceof Error ? cause.message : 'Approval was refused; inspect the current gate report.'; await loadGate(); }
  finally { busy.value = false; }
}

async function requestCopy(input: { materialId:string;versionId:string;recipientId:string }) {
  error.value = ''; busy.value = true;
  try {
    const grant = await parley.issueCopyGrant(input.materialId, { version_id:input.versionId,recipient_id:input.recipientId }, newOperationKey());
    release.value = { grantId:grant.grant_id,versionId:grant.version_id,recipientId:grant.recipient_id,digest:grant.digest,body:grant.body,acknowledgementState:'ready' };
    status.value = 'Approved text is authorized for an explicit clipboard action. No send or reply timer exists.';
  } catch (cause) { release.value = null; error.value = cause instanceof Error ? cause.message : 'Approved copy is unavailable.'; }
  finally { busy.value = false; }
}

async function acknowledgeCopy(input: { grantId:string;success:boolean;operationKey:string }) {
  error.value = '';
  try {
    const receipt = await parley.acknowledgeCopy(input.grantId,input.success,input.operationKey);
    if (release.value?.grantId === input.grantId) release.value = { ...release.value, acknowledgementState:receipt.status === 'acknowledged' ? 'recorded' : receipt.status === 'failed' ? 'failed' : 'unknown' };
    status.value = receipt.status === 'acknowledged' ? 'Clipboard promise succeeded and its receipt was recorded. Mark sent remains a separate action.' : 'Clipboard attempt failed; the approved preview remains available.';
  } catch (cause) {
    if (release.value?.grantId === input.grantId) release.value = { ...release.value, acknowledgementState:'unknown' };
    error.value = cause instanceof Error ? `${cause.message} The acknowledgement can be retried with the same operation.` : 'Copy acknowledgement is unknown; retry the same operation.';
  }
}

onMounted(() => { void parley.connect().then(loadWorkspace).catch((cause: unknown) => { error.value = cause instanceof Error ? cause.message : 'Workspace could not be loaded.'; }); });
</script>

<template>
  <section :aria-label="props.embedded ? `Materials for ${selectedOpportunity?.title ?? 'opportunity'}` : props.surface === 'queue' ? 'Opportunity queue workspace' : 'Output registry workspace'">
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
    <div
      class="workspace-columns"
      :class="{ embedded: props.embedded }"
    >
      <aside v-if="!props.embedded">
        <h2>{{ props.surface === 'queue' ? 'Opportunities' : 'Output' }}</h2>
        <ul>
          <li
            v-for="item in opportunities"
            :key="item.id"
          >
            <button
              type="button"
              :aria-current="selectedOpportunityId === item.id ? 'true' : undefined"
              @click="selectedOpportunityId = item.id"
            >
              {{ item.title }} · {{ item.status }}
            </button>
          </li>
        </ul>
        <p v-if="!opportunities.length">
          No opportunities yet. Add one below; a lead alone never creates outreach.
        </p>
        <form @submit.prevent="createOpportunity">
          <h3>Create opportunity</h3>
          <label>Title<input
            v-model="title"
            required
            maxlength="500"
          ></label>
          <fieldset>
            <legend>Participants</legend><label
              v-for="party in parties"
              :key="party.id"
              class="inline"
            ><input
              v-model="createParticipantIds"
              type="checkbox"
              :value="party.id"
            >{{ party.name }} · {{ party.kind }}</label>
          </fieldset>
          <button
            type="submit"
            :disabled="busy"
          >
            Create opportunity
          </button>
        </form>
      </aside>
      <div
        v-if="selectedOpportunity"
        class="work-area"
      >
        <section
          v-if="!props.embedded"
          aria-label="Mutual value exchange"
        >
          <h2>{{ selectedOpportunity.title }} · Participants</h2>
          <p v-if="!selectedOpportunity.participants.length">
            Add a participant before creating recipient-facing material.
          </p>
          <label>Interest side<select v-model="interestSide"><option value="them">Their stated or hypothesized interest</option><option value="me">My interest</option></select></label>
          <label>Interest statement<textarea
            v-model="interestStatement"
            rows="2"
          /></label><button
            type="button"
            :disabled="busy || !interestStatement.trim()"
            @click="addInterest"
          >
            Save interest
          </button>
          <ul>
            <li
              v-for="interest in interests"
              :key="interest.id"
            >
              T{{ interest.tier }} · {{ interest.side }}: {{ interest.statement }}
            </li>
          </ul>
          <label>They get<input v-model="theyGet"></label><label>I get<input v-model="iGet"></label>
          <fieldset>
            <legend>Interests linked to this exchange</legend><label
              v-for="interest in interests"
              :key="interest.id"
              class="inline"
            ><input
              v-model="selectedInterestIds"
              type="checkbox"
              :value="interest.id"
            >{{ interest.side }}: {{ interest.statement }}</label>
          </fieldset>
          <label class="inline"><input
            v-model="fairnessAttested"
            type="checkbox"
          >I believe both sides come out ahead, including if they never reply.</label>
          <button
            type="button"
            :disabled="busy"
            @click="saveExchange"
          >
            Save exchange version
          </button>
        </section>
        <section aria-label="Opportunity materials">
          <h2>Material for a named recipient</h2>
          <label>Recipient<select v-model="selectedRecipientId"><option value="">Choose participant</option><option
            v-for="participant in selectedOpportunity.participants"
            :key="participant.id"
            :value="participant.id"
          >{{ participant.name }} · {{ participant.kind }}</option></select></label>
          <label>Kind<select v-model="materialKind"><option value="outreach">Outreach</option><option value="follow_up">Follow-up</option><option value="handoff">Handoff</option></select></label>
          <label>Material title<input
            v-model="materialTitle"
            maxlength="240"
          ></label><button
            type="button"
            :disabled="busy || !selectedRecipientId || !materialTitle.trim()"
            @click="createMaterial"
          >
            Create draft material
          </button>
          <ul>
            <li
              v-for="item in materials"
              :key="item.id"
            >
              <button
                type="button"
                :aria-current="item.id === selectedMaterialId ? 'true' : undefined"
                @click="selectedMaterialId = item.id"
              >
                {{ item.title }} · {{ item.recipient_name }} · {{ item.current_version ? `v${item.current_version.sequence}` : 'no version' }}
              </button>
            </li>
          </ul>
        </section>
        <section
          v-if="selectedMaterial && !selectedMaterial.current_version"
          aria-label="Material without a working version"
        >
          <p>This material has no working version yet.</p>
          <button
            type="button"
            :disabled="busy"
            @click="startFirstVersion"
          >
            Start an incomplete draft version
          </button>
        </section>
        <section
          v-if="selectedMaterial && selectedMaterial.current_version && gate"
          aria-label="Shared Queue and Output material editor"
        >
          <h2>{{ props.surface === 'queue' ? 'Queue material' : 'Output material' }}</h2>
          <MaterialEditor
            :opportunity-id="selectedMaterial.opportunity_id"
            :opportunity-title="selectedOpportunity.title"
            :recipient="{id:selectedMaterial.recipient_id,kind:selectedMaterial.recipient_kind,name:selectedMaterial.recipient_name} satisfies MaterialRecipient"
            :material="materialRecord(selectedMaterial)"
            :claim-references="claimReferences"
            :gate="materialGate(gate)"
            :approval="materialApproval(selectedMaterial)"
            :current-release="release"
            :busy="busy ? 'save' : null"
            :operation-error="error"
            @save-version="saveVersion"
            @approve="approve"
            @request-copy="requestCopy"
            @acknowledge-copy="acknowledgeCopy"
          />
        </section>
        <p v-else-if="selectedMaterial">
          The current gate is loading. Release controls remain unavailable.
        </p>
      </div>
      <p
        v-else
        class="work-area"
      >
        Choose or create an opportunity.
      </p>
    </div>
  </section>
</template>

<style scoped>
.workspace-columns{display:grid;grid-template-columns:minmax(240px,310px) minmax(0,1fr);gap:1rem;align-items:start}.workspace-columns.embedded{display:block}.work-area{min-width:0}.workspace-columns>aside,.work-area>section{background:#fff;border:1px solid #d7dfd9;border-radius:.5rem;padding:1rem;margin-bottom:1rem}label{display:grid;gap:.25rem;margin:.65rem 0}.inline{display:flex;align-items:center;gap:.5rem}.inline input{width:auto}button,input,select,textarea{font:inherit}button{min-height:2.5rem}textarea{width:100%;box-sizing:border-box}@media(max-width:767px){.workspace-columns{grid-template-columns:1fr}}
</style>
