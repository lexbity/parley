<script setup lang="ts">
/* global HTMLElement, HTMLDialogElement, HTMLTextAreaElement, MouseEvent, Node, Event, CustomEvent */
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue';
import {
  newOperationKey, parley,
  type ContactThread, type GateReport, type Material, type MaterialInput, type Opportunity, type OpportunitySummary,
  type ReleasedMaterial, type Resource, type SentenceBinding, type SlotFill,
  type StructuredReadiness, type WritingInterest, type WritingMatch, type WritingTemplate,
} from '../../api/client';
import { observeRoleJob } from '../../app/inference/activity';
import { writeClipboardWithDeadline } from '../../shared/clipboard';
import ContactThreadPanel from '../queue/ContactThreadPanel.vue';
import SearchableSelect from '../../shared/SearchableSelect.vue';
import type { SelectOption } from '../../shared/select-options';

/**
 * The Output workspace (AR-001 screens 35–41). One opportunity/recipient scope
 * owns the left library (matched resources, drafts) and the right editor; the
 * editor toolbar opens one review dialog per owner intent. Matching, exchange,
 * templates, compose and release all keep their server-owned determinism: the
 * workspace only freezes owner selections and re-reads committed facts.
 */
const props = defineProps<{
  resources: Resource[];
  outputs: Material[];
  active: boolean;
  recordId: string | null;
  location: string | null;
}>();
const emit = defineEmits<{ changed: []; navigate: [path: string] }>();

const emailCharLimit = 1200;
const opportunityId = ref('');
const recipientId = ref('');
const interests = ref<WritingInterest[]>([]);
const exchange = ref<{ current: Record<string, unknown> | null; versions: Array<Record<string, unknown>> }>({ current: null, versions: [] });
const match = ref<WritingMatch | null>(null);
const embedding = ref<{ model: string; model_digest: string | null; profile_revision: number | null; calibrated_kinds: string[]; readiness: StructuredReadiness }>({ model: '', model_digest: null, profile_revision: null, calibrated_kinds: [], readiness: { status: 'unqualified', diagnostic: null, checked_at: null, probe_id: null } });
const embeddingDraft = ref('');
const embedBusy = ref(false);
const templates = ref<WritingTemplate[]>([]);
const families = ref<Array<{ id: string; name: string }>>([]);
const voiceSamples = ref<Array<{ id: string; text: string; channel: string | null; revision: number }>>([]);
const selectedVoiceIds = ref<string[]>([]);
const requestedRules = ref(['R1', 'R2']);

interface WorkingDraft {
  body: string;
  resourceId: string;
  templateId: string;
  fills: Record<string, string>;
  fillResources: Record<string, string>;
  materialId: string | null;
}
const baseDraft = (): WorkingDraft => ({ body: '', resourceId: '', templateId: '', fills: {}, fillResources: {}, materialId: null });
const draftsByScope = reactive(new Map<string, WorkingDraft>());
const scopeKey = computed(() => `${opportunityId.value}\u0000${recipientId.value}`);
const draft = computed<WorkingDraft>(() => {
  const key = scopeKey.value;
  let value = draftsByScope.get(key);
  if (!value) { value = baseDraft(); draftsByScope.set(key, value); }
  return value;
});

const leftTab = ref<'resources' | 'drafts'>('resources');
const mobilePane = ref<'resources' | 'editor'>('editor');
const query = ref('');
const selection = ref('');
const manualMatching = ref(false);
const menu = ref('');
const popover = ref<HTMLElement | null>(null);
const modal = ref('');
const modalEl = ref<HTMLDialogElement | null>(null);
const editorEl = ref<HTMLTextAreaElement | null>(null);
const notice = ref('');
const errorMessage = ref('');
const busy = ref(false);
const scopeGeneration = ref(0);
const consumedRecord = ref<string | null>(null);

// Identity and typeahead no longer scan a whole-domain client cache: the
// loaded window serves the picker, and any id outside it is resolved by an
// authoritative single-record read.
const opportunities = ref<OpportunitySummary[]>([]);
const scopedRecord = ref<Opportunity | null>(null);
async function resolveOpportunity(id: string): Promise<Opportunity | null> {
  try { return await parley.getOpportunity(id); } catch { return null; }
}
async function loadOpportunityWindow(q = '') {
  const page = await parley.listOpportunitiesPage({ q, limit: 50 });
  opportunities.value = page.items;
}
async function searchOpportunities(q: string): Promise<SelectOption[]> {
  await loadOpportunityWindow(q.trim());
  return opportunities.value.map(item => ({ value: item.id, label: item.title }));
}
const participants = ref<Array<{ id: string; name: string; kind: 'person' | 'org' }>>([]);
async function loadParticipants(ids: string[]) {
  const results = await Promise.allSettled(ids.map(id => parley.getEntity(id)));
  participants.value = results.flatMap(result =>
    result.status === 'fulfilled' && (result.value.kind === 'person' || result.value.kind === 'org')
      ? [{ id: result.value.id, name: result.value.name, kind: result.value.kind }]
      : []);
}
const selectedOpportunity = computed(() => scopedRecord.value && {
  id: scopedRecord.value.id,
  title: scopedRecord.value.title,
  state: scopedRecord.value.state,
  revision: scopedRecord.value.revision,
  participants: participants.value,
  tags: [] as string[],
  next_task: null,
  next_deadline: null,
  missing_material: false,
} satisfies OpportunitySummary | null);
const opportunityOptions = computed<SelectOption[]>(() => opportunities.value.map(item => ({ value: item.id, label: item.title })));
const recipient = computed(() => participants.value.find((item) => item.id === recipientId.value) ?? null);
const boundTemplate = computed(() => templates.value.find((item) => item.id === draft.value.templateId) ?? null);
const boundMaterial = computed(() => (draft.value.materialId ? props.outputs.find((item) => item.id === draft.value.materialId) ?? null : null));
const boundResource = computed(() => props.resources.find((item) => item.id === draft.value.resourceId) ?? null);
const exchangeVersion = computed(() => Number(exchange.value.current?.version ?? 0));
const exchangeAttested = computed(() => exchange.value.current?.fairness_attested === true);
const dirty = computed(() => draft.value.body !== (boundMaterial.value?.body ?? ''));
/** Explicit re-binding counts as a save-able change even when wording is untouched. */
const bindingsDirty = ref(false);
const words = computed(() => draft.value.body.trim().split(/\s+/).filter(Boolean).length);
const channel = computed(() => boundTemplate.value?.channel ?? 'email');
const charLimit = computed(() => channel.value === 'email' ? emailCharLimit : 300);
const channelLabel = computed(() => channel.value.charAt(0).toUpperCase() + channel.value.slice(1));
const editorTitle = computed(() => boundMaterial.value
  ? `${boundMaterial.value.kind.charAt(0).toUpperCase() + boundMaterial.value.kind.slice(1)} for ${boundMaterial.value.recipient_name}`
  : boundTemplate.value?.name ?? 'Untitled draft');
const stateLabel = computed(() => {
  if (dirty.value) return 'Unsaved changes';
  if (!boundMaterial.value) return 'New draft';
  return `${boundMaterial.value.status.charAt(0).toUpperCase() + boundMaterial.value.status.slice(1)} · v${boundMaterial.value.version}`;
});
const scopeMaterials = computed(() => props.outputs
  .filter((item) => item.opportunity_id === opportunityId.value && item.recipient_id === recipientId.value)
  .sort((left, right) => right.version - left.version));
const templateSlots = computed(() => slotsOf(boundTemplate.value));
const pendingEffectiveFills = computed<SlotFill[]>(() => pendingTemplateSlots.value.map((slot) => {
  const resourceId = pendingFillResources.value[slot];
  const resource = resourceId ? props.resources.find((item) => item.id === resourceId) : undefined;
  const value = resource ? resolvedResourceName(resource) : (pendingFills.value[slot] ?? '').trim();
  return { slot, value, ...(resourceId ? { resource_id: resourceId } : {}) };
}));
const slotsResolved = computed(() => templateSlots.value.every((slot) => (draft.value.fills[slot] ?? '').trim().length > 0));
const slotsVerbatim = computed(() => templateSlots.value.every((slot) => {
  const value = (draft.value.fills[slot] ?? '').trim();
  return value.length > 0 && draft.value.body.includes(value);
}));
const canSave = computed(() => (dirty.value || bindingsDirty.value) && draft.value.body.trim().length > 0
  && boundTemplate.value?.status === 'active' && slotsResolved.value && slotsVerbatim.value && exchangeVersion.value > 0);
const saveLabel = computed(() => (dirty.value || bindingsDirty.value) ? 'Save draft' : boundMaterial.value ? 'Saved' : 'Save draft');

const matchDigest = computed(() => embedding.value.model_digest);
const embeddingProfileRevisionUnavailable = computed(() => embedding.value.profile_revision === null);
const assessment = computed<'current' | 'stale' | 'missing'>(() => {
  if (!match.value) return 'missing';
  return matchDigest.value !== null && match.value.embed_model_digest === matchDigest.value ? 'current' : 'stale';
});
const ranked = computed(() => assessment.value === 'current' && !manualMatching.value);
const listing = computed(() => ranked.value || manualMatching.value);
interface ShortlistEntry { id: string; kind: string; score?: number }
const shortlist = computed<ShortlistEntry[]>(() => {
  try {
    const items: unknown = JSON.parse(match.value?.shortlist_json ?? '[]');
    if (!Array.isArray(items)) return [];
    return items.flatMap((entry) => {
      if (typeof entry !== 'object' || entry === null) return [];
      const record = entry as Record<string, unknown>;
      if (typeof record.id !== 'string') return [];
      return [{
        id: record.id,
        kind: typeof record.kind === 'string' ? record.kind : 'gift',
        score: typeof record.score === 'number' && Number.isFinite(record.score) ? record.score : undefined,
      }];
    });
  } catch { return []; }
});
const matchedResources = computed(() => shortlist.value.flatMap((entry) => {
  const resource = props.resources.find((item) => item.id === entry.id);
  return resource ? [{ resource, score: entry.score }] : [];
}));
const manualResources = computed(() => props.resources.filter((item) => item.readiness === 'ready'));
const listedResources = computed<Array<{ resource: Resource; score: number | undefined }>>(() =>
  ranked.value ? matchedResources.value : manualResources.value.map((resource) => ({ resource, score: undefined })));
const visibleResources = computed(() => listedResources.value.filter(({ resource }) =>
  [resource.title, resource.kind].join(' ').toLowerCase().includes(query.value.trim().toLowerCase())));
const selectedEntry = computed(() => listedResources.value.find(({ resource }) => resource.id === selection.value) ?? null);

const scopeDraftCount = computed(() => scopeMaterials.value.length);
const gateReport = ref<GateReport | null>(null);
const gateReasons = ref<Record<string, string>>({});
const gateMatchesReasons = ref(false);
const failedSoft = computed(() => gateReport.value?.checks.filter((check) => check.severity === 'soft' && !check.pass) ?? []);
const hardFailures = computed(() => gateReport.value?.checks.filter((check) => check.severity === 'hard' && !check.pass) ?? []);
const overridesComplete = computed(() => failedSoft.value.every((check) => (gateReasons.value[check.id] ?? '').trim().length >= 10));
const canApprove = computed(() => boundMaterial.value?.status === 'draft' && gateReport.value !== null && gateReport.value.copy_enabled && gateMatchesReasons.value && hardFailures.value.length === 0 && overridesComplete.value);
const canRelease = computed(() => boundMaterial.value?.status === 'approved' || boundMaterial.value?.status === 'released');
const pendingAck = ref<ReleasedMaterial | null>(null);
const deliveredReleases = ref<Record<string, ReleasedMaterial>>({});

const lintFindings = ref<Array<{ code: string; level: string; message: string }>>([]);
const lintAdvisory = ref<Array<{ issue: string; suggested_rewrite: string }>>([]);
const lintJob = ref('');
const proposal = ref('');
const proposalJob = ref('');
const proposalFor = ref<{ opportunityId: string; recipientId: string } | null>(null);
const proposalIsCurrent = computed(() => proposalFor.value?.opportunityId === opportunityId.value && proposalFor.value?.recipientId === recipientId.value);
const templateCandidates = ref<Array<Record<string, unknown>>>([]);
const familyName = ref('One ask');
const templateName = ref('One ask');
const templateStage = ref('identified');
const templateChannel = ref('email');
const templateBody = ref('{{their_name:name}},\nI can offer {{gift:guide}}.\nWould a short review be useful {{ask:next_step}}?');
const libraryTemplateId = ref('');
const libraryTemplate = computed(() => templates.value.find((item) => item.id === libraryTemplateId.value) ?? null);
const pendingTemplateId = ref('');
const pendingTemplate = computed(() => templates.value.find((item) => item.id === pendingTemplateId.value) ?? null);
const pendingTemplateSlots = computed(() => slotsOf(pendingTemplate.value));
const pendingFills = ref<Record<string, string>>({});
const pendingFillResources = ref<Record<string, string>>({});
const pendingHypothesis = ref('');
const pendingOwnerInterest = ref('');
const pendingTheyGet = ref('');
const pendingYouGet = ref('');
const attestationAccepted = ref(false);
const voiceText = ref('');
const voiceChannel = ref('email');

const toolGroups: Array<{ group: string; commands: Array<[id: string, label: string, detail: string]> }> = [
  { group: 'Insert', commands: [
    ['resource', 'Insert resource', 'Review a match and insert at the cursor'],
    ['slots', 'Template and fills', 'Choose an active version and resolve its slots'],
    ['templates', 'Template library', 'Review, build, or manage reusable versions'],
  ] },
  { group: 'Context', commands: [
    ['exchange', 'Interests and exchange', 'Review both sides and attest the exchange'],
    ['voice', 'Voice samples', 'Select or manage owner samples'],
    ['matching', 'Matching and calibration', 'Refresh results or choose manually'],
  ] },
  { group: 'Review', commands: [
    ['compose', 'Generate a proposal', 'Review generated wording before applying it'],
    ['lint', 'Review writing advice', 'See advisory Linter findings'],
    ['gate', 'Run gate checks', 'Review the exact saved version before approval'],
    ['versions', 'Version history', 'Open a saved version for this recipient'],
    ['release', 'Copy or download approved text', 'Use exact approved material bytes'],
  ] },
];
const captions: Record<string, string> = {
  resource: 'Choose a resource', slots: 'Template and fills', templates: 'Template library',
  exchange: 'Interests and exchange', voice: 'Voice samples', matching: 'Matching and calibration',
  compose: 'Generate a proposal', lint: 'Writing advice', gate: 'Gate checks',
  versions: 'Version history', release: 'Release approved material',
};

function slotsOf(template: WritingTemplate | null): string[] {
  if (!template) return [];
  return [...new Set((template.body.match(/\{\{[^}]+\}\}/g) ?? []).map((slot) => slot.slice(2, -2)))];
}
function resolvedResourceName(resource: Resource): string {
  if (resource.kind === 'gift') {
    const what = resource.body['what'];
    if (typeof what === 'string' && what.trim()) return what;
  }
  return resource.title;
}
function kindLabel(kind: string): string {
  return kind.charAt(0).toUpperCase() + kind.slice(1);
}
function setNotice(message: string) { notice.value = message; }
function resetMessages() { notice.value = ''; errorMessage.value = ''; }

async function run(action: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  resetMessages();
  try { await action(); }
  catch (reason) { errorMessage.value = reason instanceof Error ? reason.message : 'The action failed.'; }
  finally { busy.value = false; }
}

async function refreshScopeFacts() {
  const opportunity = opportunityId.value;
  const recipient = recipientId.value;
  if (!opportunity || !recipient) return;
  const generation = scopeGeneration.value;
  match.value = null;
  interests.value = [];
  exchange.value = { current: null, versions: [] };
  try {
    const [nextInterests, nextExchange] = await Promise.all([
      parley.getWritingInterests(opportunity, recipient),
      parley.getWritingExchange(opportunity, recipient),
    ]);
    if (generation !== scopeGeneration.value) return;
    interests.value = nextInterests;
    exchange.value = nextExchange;
  } catch (reason) {
    if (generation === scopeGeneration.value) errorMessage.value = reason instanceof Error ? reason.message : 'Could not load recipient evidence.';
    return;
  }
  try {
    const next = await parley.getWritingMatch(opportunity, recipient);
    if (generation === scopeGeneration.value) match.value = next;
  } catch { /* No completed match is a normal state: the library offers manual selection. */ }
}

async function selectScope(nextOpportunity: string, nextRecipient?: string) {
  if (!nextOpportunity) return;
  const record = scopedRecord.value?.id === nextOpportunity ? scopedRecord.value : await resolveOpportunity(nextOpportunity);
  if (!record) { errorMessage.value = 'That opportunity is no longer available.'; return; }
  const target = nextRecipient ?? opportunities.value.find(item => item.id === nextOpportunity)?.participants[0]?.id ?? record.participant_ids[0] ?? '';
  if (nextOpportunity === opportunityId.value && target === recipientId.value && scopedRecord.value) return;
  scopedRecord.value = record;
  opportunityId.value = nextOpportunity;
  recipientId.value = target;
  await loadParticipants(record.participant_ids);
  manualMatching.value = false;
  query.value = '';
  selection.value = '';
  scopeGeneration.value += 1;
  resetMessages();
  void refreshScopeFacts();
}

watch(recipientId, () => {
  selection.value = draft.value.resourceId;
});

function loadMaterial(material: Material) {
  const key = `${material.opportunity_id}\u0000${material.recipient_id}`;
  selectScope(material.opportunity_id, material.recipient_id);
  const current = draftsByScope.get(key) ?? baseDraft();
  current.body = material.body;
  current.materialId = material.id;
  current.resourceId = '';
  bindingsDirty.value = false;
  draftsByScope.set(key, current);
  mobilePane.value = 'editor';
  leftTab.value = 'drafts';
  resetMessages();
}

async function saveDraft() {
  if (!canSave.value || !opportunityId.value || !recipientId.value || !boundTemplate.value) return;
  const opportunity = opportunityId.value;
  const template = boundTemplate.value;
  const fills: SlotFill[] = templateSlots.value.map((slot) => {
    const value = (draft.value.fills[slot] ?? '').trim();
    const resourceId = draft.value.fillResources[slot];
    return { slot, value, ...(resourceId ? { resource_id: resourceId } : {}) };
  });
  const bindings = deriveSentenceBindings(draft.value.body, fills);
  const input: MaterialInput = {
    recipient_id: recipientId.value,
    kind: 'outreach',
    template_version_id: template.id,
    body: draft.value.body,
    slot_fills: fills,
    sentence_bindings: bindings,
    exchange_version: exchangeVersion.value,
  };
  const saved = boundMaterial.value
    ? await parley.editMaterial(boundMaterial.value.id, input)
    : await parley.createMaterial(opportunity, input);
  draft.value.materialId = saved.id;
  bindingsDirty.value = false;
  setNotice(`Draft v${saved.version} saved for ${saved.recipient_name}. Owner review is still required before release.`);
  emit('changed');
}

/**
 * Owner-review classification for every sentence (UCR-001 U07): resource
 * sentences bind their exact resource, ask sentences bind the ladder ask, and
 * everything else stays generic. The deterministic gate re-checks these
 * bindings against current evidence at approval time.
 */
function deriveSentenceBindings(body: string, fills: SlotFill[]): SentenceBinding[] {
  const sentences: string[] = [];
  let start = 0;
  for (const [index, character] of [...body].entries()) {
    if (['.', '!', '?', '\n'].includes(character)) {
      const value = body.slice(start, index + 1).trim();
      if (value) sentences.push(value);
      start = index + 1;
    }
  }
  const rest = body.slice(start).trim();
  if (rest) sentences.push(rest);
  const resourceFills = fills.filter((fill) => ['gift', 'proof', 'artifact'].includes(fill.slot.split(':')[0] ?? '') && fill.resource_id);
  const askFill = fills.find((fill) => (fill.slot.split(':')[0] ?? '') === 'ask');
  let askBound = false;
  return sentences.map((sentence, index) => {
    const resourceFill = resourceFills.find((fill) => sentence.includes(fill.value));
    if (resourceFill) {
      const resource = props.resources.find((item) => item.id === resourceFill.resource_id);
      return {
        sentence_index: index,
        claim_type: resourceFill.slot.startsWith('gift') ? 'offer' : 'self_fact',
        ref_kind: 'resource',
        ref_id: resourceFill.resource_id ?? null,
        ref_revision: resource?.revision ?? null,
        approved_wording: sentence,
        ask_rung: null,
      };
    }
    if (askFill && !askBound && sentence.includes(askFill.value)) {
      askBound = true;
      // The deterministic gate grounds ask sentences at an explicit rung (H3,
      // H7): the editor's single ask starts at rung 1 and must end in '?'.
      return { sentence_index: index, claim_type: 'ask', ref_kind: 'ladder', ref_id: null, ref_revision: null, approved_wording: sentence, ask_rung: sentence.trimEnd().endsWith('?') ? 1 : null };
    }
    return { sentence_index: index, claim_type: 'generic', ref_kind: null, ref_id: null, ref_revision: null, approved_wording: sentence, ask_rung: null };
  });
}

function toggleMenu(group: string) { menu.value = menu.value === group ? '' : group; }
function dismissMenu() {
  const trigger = popover.value?.querySelector<HTMLElement>('[aria-expanded="true"]');
  menu.value = '';
  trigger?.focus();
}
function closeOutside(event: MouseEvent) {
  if (!popover.value?.contains(event.target as Node | null)) menu.value = '';
}

async function openTool(name: string) {
  menu.value = '';
  modal.value = name;
  if (name === 'exchange') {
    pendingHypothesis.value = latestInterest('them');
    pendingOwnerInterest.value = latestInterest('me');
    pendingTheyGet.value = String(exchange.value.current?.they_get ?? '');
    pendingYouGet.value = String(exchange.value.current?.i_get ?? '');
    attestationAccepted.value = false;
  } else if (name === 'slots') {
    pendingTemplateId.value = draft.value.templateId;
    pendingFills.value = { ...draft.value.fills };
    pendingFillResources.value = { ...draft.value.fillResources };
    for (const slot of pendingTemplateSlots.value) {
      if (slot.startsWith('their_name') && !pendingFills.value[slot]) {
        pendingFills.value = { ...pendingFills.value, [slot]: recipient.value?.name ?? '' };
      }
    }
  } else if (name === 'gate') {
    gateReport.value = boundMaterial.value?.gate ?? null;
    gateReasons.value = {};
    gateMatchesReasons.value = false;
    if (boundMaterial.value?.status === 'draft') void evaluateGate();
  } else if (name === 'voice') {
    selectedVoiceIds.value = selectedVoiceIds.value.filter((id) => voiceSamples.value.some((sample) => sample.id === id));
  }
  await nextTick();
  const root = modalEl.value;
  if (!root || root.open) return;
  // jsdom lacks showModal; the open attribute keeps dialog content testable.
  if (typeof root.showModal === 'function') root.showModal();
  else root.setAttribute('open', '');
}
function closeTool() {
  const root = modalEl.value;
  if (root && typeof root.close === 'function' && root.open) {
    // The close event owns the shared cleanup, including the templates route.
    root.close();
    return;
  }
  root?.removeAttribute('open');
  onDialogClose();
}
function onDialogClose() {
  modal.value = '';
  if (props.location === 'templates') emit('navigate', '/output');
}

function latestInterest(side: 'them' | 'me'): string {
  const items = interests.value.filter((item) => item.side === side && item.status === 'active');
  return items.at(-1)?.statement ?? '';
}

function resourceSharing(resource: Resource): string {
  if (resource.kind === 'artifact') {
    return resource.body['public'] === true ? 'Approved for external sharing' : 'Private to the workspace';
  }
  return 'Eligible to share with this recipient';
}

async function insertResource() {
  const entry = selectedEntry.value;
  if (!entry) return;
  const insertion = resolvedResourceName(entry.resource);
  const editor = editorEl.value;
  const start = editor?.selectionStart ?? draft.value.body.length;
  const end = editor?.selectionEnd ?? start;
  const body = draft.value.body;
  const spacer = start > 0 && !/\s$/.test(body.slice(0, start)) ? ' ' : '';
  draft.value.body = body.slice(0, start) + spacer + insertion + body.slice(end);
  draft.value.resourceId = entry.resource.id;
  closeTool();
  mobilePane.value = 'editor';
  await nextTick();
  editorEl.value?.focus();
  const caret = start + spacer.length + insertion.length;
  editorEl.value?.setSelectionRange(caret, caret);
  setNotice(`Inserted ${entry.resource.title}. Resolve its slot fill before saving.`);
}

function applyFills() {
  bindingsDirty.value = true;
  draft.value.templateId = pendingTemplateId.value;
  draft.value.fills = Object.fromEntries(pendingEffectiveFills.value.map((fill) => [fill.slot, fill.value]));
  draft.value.fillResources = { ...pendingFillResources.value };
  const firstResource = Object.values(pendingFillResources.value)[0];
  if (firstResource) draft.value.resourceId = firstResource;
  setNotice(`Template and fills selected for ${recipient.value?.name ?? 'the recipient'}.`);
  closeTool();
}
watch(pendingTemplateId, () => {
  const next: Record<string, string> = {};
  for (const slot of pendingTemplateSlots.value) {
    if (slot.startsWith('their_name')) next[slot] = recipient.value?.name ?? '';
  }
  pendingFills.value = { ...next, ...pendingFills.value };
});

async function saveExchange() {
  if (!opportunityId.value || !recipientId.value) return;
  const hypothesis = pendingHypothesis.value.trim();
  const ownerInterest = pendingOwnerInterest.value.trim();
  const added: string[] = [];
  if (hypothesis && hypothesis !== latestInterest('them')) {
    const item = await parley.addWritingInterest(opportunityId.value, {
      recipient_id: recipientId.value, side: 'them', statement: hypothesis, knowledge_kind: 'hypothesis',
      check_suggestion: 'Ask the recipient or verify against a source before treating this as fact.',
    });
    added.push(item.id);
  }
  if (ownerInterest && ownerInterest !== latestInterest('me')) {
    const item = await parley.addWritingInterest(opportunityId.value, {
      recipient_id: recipientId.value, side: 'me', statement: ownerInterest, knowledge_kind: 'owner_report',
    });
    added.push(item.id);
  }
  await parley.saveWritingExchange(opportunityId.value, {
    recipient_id: recipientId.value,
    they_get: pendingTheyGet.value.trim(),
    i_get: pendingYouGet.value.trim(),
    interest_ids: [...interests.value.filter((item) => item.status === 'active').map((item) => item.id), ...added],
  });
  await refreshScopeFacts();
  setNotice('Exchange version saved. Review both sides before attesting.');
}

async function attestExchange() {
  if (!opportunityId.value || !recipientId.value || !attestationAccepted.value || exchangeAttested.value) return;
  await parley.attestWritingExchange(opportunityId.value, recipientId.value);
  await refreshScopeFacts();
  setNotice(`Exchange attested for ${recipient.value?.name ?? 'the recipient'}.`);
  closeTool();
}

async function saveVoiceSample() {
  await parley.createVoiceSample(voiceText.value.trim(), voiceChannel.value.trim());
  voiceSamples.value = await parley.listVoiceSamples();
  voiceText.value = '';
  setNotice('Owner voice sample saved. Existing outputs were not rewritten.');
}
async function retireVoice(id: string) {
  await parley.retireVoiceSample(id);
  voiceSamples.value = await parley.listVoiceSamples();
  selectedVoiceIds.value = selectedVoiceIds.value.filter((sample) => sample !== id);
  setNotice('Voice sample retired; its history is preserved.');
}

async function setEmbeddingModel() {
  await parley.setEmbeddingModel(embeddingDraft.value.trim(), embedding.value.profile_revision);
  embedding.value = await parley.getEmbeddingModel();
  setNotice('Embedding model changed. Verify it, then calibrate thresholds for its digest.');
}

async function verifyEmbeddingModel() {
  if (embedBusy.value) return;
  embedBusy.value = true;
  try {
    const result = await parley.checkEmbedProvider();
    if (result.embed && ['queued', 'running'].includes(result.embed.status)) {
      setNotice('Embedding model verification queued. It runs in the background.');
    } else if (result.embed?.status === 'ready') {
      setNotice('Embedding model is verified.');
    } else {
      setNotice(result.message || 'Embedding model verification did not complete.');
    }
    // Refresh after the probe settles: the matching lane is separate, so the
    // verifier converges within its own readiness window.
    await new Promise((resolve) => globalThis.setTimeout(resolve, 2400));
    embedding.value = await parley.getEmbeddingModel();
  } catch (reason) {
    setNotice(reason instanceof Error ? reason.message : 'Could not verify the embedding model.');
  } finally {
    embedBusy.value = false;
  }
}

function chooseManually() {
  manualMatching.value = true;
  selection.value = props.resources.find((item) => item.readiness === 'ready')?.id ?? '';
  if (modalEl.value?.open) closeTool();
  mobilePane.value = 'resources';
  leftTab.value = 'resources';
  setNotice('Showing eligible resources in manual selection order.');
}

async function runMatch() {
  const opportunity = opportunityId.value;
  const recipientLocal = recipientId.value;
  if (!opportunity || !recipientLocal) return;
  const generation = scopeGeneration.value;
  match.value = null;
  const job = await parley.startMatching(opportunity, recipientLocal);
  await pollJob(job.job_id, generation);
  const next = await parley.getWritingMatch(opportunity, recipientLocal);
  if (generation !== scopeGeneration.value) return;
  match.value = next;
  manualMatching.value = false;
  selection.value = '';
  setNotice('Matching results refreshed for the selected recipient.');
  closeTool();
}

async function pollJob(id: string, generation: number): Promise<Record<string, unknown> | null> {
  return new Promise((resolve, reject) => {
    const epoch = parley.workspaceEpoch;
    let stopped = false;
    const finish = (errorOrNull: Error | null, result?: Record<string, unknown> | null) => {
      if (stopped) return;
      stopped = true;
      stop();
      if (errorOrNull) reject(errorOrNull);
      else resolve(result ?? null);
    };
    const stop = observeRoleJob(
      id,
      () => {
        if (generation !== scopeGeneration.value || epoch !== parley.workspaceEpoch) {
          finish(new Error('The view or workspace changed while the job ran.'));
        }
      },
      (job) => {
        if (job.status === 'succeeded') finish(null, (job.result ?? null) as Record<string, unknown> | null);
        else finish(new Error(job.last_error ?? `Agent job ended as ${job.status}.`));
      },
      (reason) => finish(reason instanceof Error ? reason : new Error(String(reason))),
    );
    globalThis.setTimeout(() => {
      finish(new Error('The job is still running. Keep this view open; it is listed in Queue activity.'));
    }, 360_000);
  });
}

async function buildTemplateCandidate() {
  const job = await parley.buildWritingTemplates({
    stage_class: templateStage.value, rungs: [1], rules: requestedRules.value,
    channel: templateChannel.value, max_chars: templateChannel.value === 'email' ? emailCharLimit : 300,
    voice_sample_ids: selectedVoiceIds.value,
  });
  const result = await pollJob(job.job_id, scopeGeneration.value);
  templateCandidates.value = Array.isArray(result?.variants)
    ? result.variants.flatMap((item) => typeof item === 'object' && item !== null ? [item as Record<string, unknown>] : [])
    : [];
}
function useTemplateCandidate(candidate: Record<string, unknown>) {
  if (typeof candidate.name === 'string') templateName.value = candidate.name;
  if (typeof candidate.body === 'string') templateBody.value = candidate.body;
  setNotice('Candidate copied into the editable template form. It remains a proposal until owner validation and activation.');
}

async function saveTemplateVersion() {
  let family = families.value.find((item) => item.name === familyName.value.trim());
  if (!family) {
    family = await parley.createTemplateFamily(familyName.value.trim());
    families.value = [...families.value, family];
  }
  const rules = templateBody.value.split(/\n+/).filter((line) => line.trim()).map((line) => [...new Set([
    ...(line.includes('{{gift:') ? ['R1'] : []),
    ...(line.includes('{{proof:') ? ['R3'] : []),
    ...(line.includes('{{artifact:') ? ['R4'] : []),
    ...(line.includes('{{identity_link:') ? ['R7'] : []),
    ...(line.includes('{{capacity_line:') ? ['R6'] : []),
    ...(line.includes('{{ask:') ? ['R2'] : []),
  ])]);
  const saved = await parley.createWritingTemplateVersion(family.id, {
    name: templateName.value.trim(), body: templateBody.value, stage_class: templateStage.value,
    channel: templateChannel.value, sentence_rules: rules, proof_fallback: 'omit_sentence',
  });
  templates.value = [saved, ...templates.value];
  libraryTemplateId.value = saved.id;
  setNotice('Immutable draft version saved. Review and validate it before activation.');
}
async function transitionTemplate(status: 'approve' | 'activate') {
  if (!libraryTemplate.value) return;
  if (status === 'approve') await parley.approveWritingTemplate(libraryTemplate.value.id);
  else await parley.activateWritingTemplate(libraryTemplate.value.id);
  templates.value = await parley.listWritingTemplates();
  draft.value.templateId = status === 'activate' ? libraryTemplate.value.id : draft.value.templateId;
  pendingTemplateId.value = status === 'activate' ? libraryTemplate.value.id : pendingTemplateId.value;
  setNotice(`Template version ${status}d.`);
}

async function composeProposal() {
  if (!opportunityId.value || !recipientId.value || !boundTemplate.value || boundTemplate.value.status !== 'active' || !exchangeAttested.value) {
    throw new Error('Choose an active template and attest the current exchange.');
  }
  const template = boundTemplate.value;
  const fills: Array<{ slot: string; value: string; resource_id?: string }> = templateSlots.value.map((slot) => {
    const value = (draft.value.fills[slot] ?? '').trim();
    if (!value) throw new Error(`Fill the ${slot} slot before composing.`);
    const resource = props.resources.find((item) => item.id === draft.value.fillResources[slot] && item.readiness === 'ready');
    return { slot, value, ...(resource ? { resource_id: resource.id } : {}) };
  });
  const origin = { opportunityId: opportunityId.value, recipientId: recipientId.value };
  const generation = scopeGeneration.value;
  const job = await parley.composeWritingMaterial(opportunityId.value, {
    recipient_id: recipientId.value, kind: 'outreach', template_version_id: template.id,
    exchange_version: exchangeVersion.value, slot_fills: fills,
    selected_match_ids: ranked.value ? shortlist.value.map((entry) => entry.id) : [],
  });
  const result = await pollJob(job.job_id, generation);
  if (!result || generation !== scopeGeneration.value) return;
  if (typeof result.body !== 'string') throw new Error('The composer job did not return reviewable wording.');
  proposal.value = result.body;
  proposalJob.value = job.job_id;
  proposalFor.value = origin;
}

async function applyProposal() {
  if (!proposalIsCurrent.value || !proposal.value) return;
  draft.value.body = proposal.value;
  proposal.value = '';
  proposalJob.value = '';
  setNotice('Proposal applied to your working draft as unsaved changes.');
  closeTool();
}

async function evaluateGate() {
  const material = boundMaterial.value;
  if (!material) return;
  const validReasons = Object.fromEntries(Object.entries(gateReasons.value).filter(([, reason]) => reason.trim().length >= 10));
  gateReport.value = await parley.evaluateMaterialGate(material.id, validReasons);
  gateMatchesReasons.value = true;
}
watch(gateReasons, () => { gateMatchesReasons.value = false; }, { deep: true });

async function approveMaterial() {
  const material = boundMaterial.value;
  if (!material || !canApprove.value) return;
  const updated = await parley.approveMaterial(material.id, Object.fromEntries(failedSoft.value.map((check) => [check.id, gateReasons.value[check.id] ?? ''])));
  gateReport.value = updated.gate;
  setNotice(`Approved exact material v${updated.version}. Release stays a separate owner action.`);
  emit('changed');
}

async function releaseMaterial(mode: 'copy' | 'download') {
  const material = boundMaterial.value;
  if (!material || !canRelease.value || busy.value) return;
  busy.value = true;
  errorMessage.value = '';
  let issued: ReleasedMaterial | null = null;
  let delivered = false;
  try {
    issued = await parley.releaseMaterial(material.id);
    if (issued.material_id !== material.id || issued.version !== material.version || issued.recipient_id !== material.recipient_id) {
      throw new Error('The authorized version or recipient changed. No content was released.');
    }
    const digest = await globalThis.crypto.subtle.digest('SHA-256', new globalThis.TextEncoder().encode(issued.body));
    const actual = Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
    if (actual !== issued.content_sha256 || actual !== material.content_sha256) throw new Error('The authorized text failed its SHA-256 integrity check. No content was released.');
    if (mode === 'copy') {
      await writeClipboardWithDeadline(issued.body);
      delivered = true;
      try {
        await parley.acknowledgeRelease(issued.release_id, true);
        deliveredReleases.value = { ...deliveredReleases.value, [material.id]: issued };
        setNotice(`Copy confirmed for ${material.recipient_name}, version ${issued.version}. Mark sent remains a separate action.`);
      } catch {
        pendingAck.value = issued;
        setNotice('Clipboard accepted the text; its acknowledgement is pending. Reconcile before marking sent.');
      }
    } else {
      const blob = new globalThis.Blob([issued.body], { type: 'text/plain;charset=utf-8' });
      const objectUrl = globalThis.URL.createObjectURL(blob);
      const link = globalThis.document.createElement('a');
      link.href = objectUrl;
      link.download = `parley-${filenamePart(material.recipient_name)}-${material.kind}-v${issued.version}.txt`;
      link.rel = 'noopener';
      globalThis.document.body.append(link); link.click(); link.remove();
      globalThis.setTimeout(() => globalThis.URL.revokeObjectURL(objectUrl), 0);
      delivered = true;
      try {
        await parley.acknowledgeRelease(issued.release_id, true);
        deliveredReleases.value = { ...deliveredReleases.value, [material.id]: issued };
        setNotice(`Download issued for ${material.recipient_name}, version ${issued.version}. The browser cannot confirm a disk save.`);
      } catch {
        pendingAck.value = issued;
        setNotice('Download was issued; its acknowledgement is pending. Reconcile before marking sent.');
      }
    }
  } catch (reason) {
    if (issued && !delivered) await parley.acknowledgeRelease(issued.release_id, false).catch(() => undefined);
    errorMessage.value = reason instanceof Error ? reason.message : `Could not ${mode} this approved version.`;
  } finally { busy.value = false; }
}

async function reconcileAck() {
  if (!pendingAck.value) return;
  await parley.acknowledgeRelease(pendingAck.value.release_id, true);
  const issued = pendingAck.value;
  pendingAck.value = null;
  deliveredReleases.value = { ...deliveredReleases.value, [issued.material_id]: issued };
  setNotice('The original release acknowledgement is now recorded.');
}



function filenamePart(value: string): string {
  const cleaned = value.normalize('NFKC').replace(/[^\p{L}\p{N}._-]+/gu, '-').replace(/^-+|-+$/g, '').slice(0, 80);
  return cleaned || 'recipient';
}

async function runLint() {
  const material = boundMaterial.value;
  if (!material) return;
  lintFindings.value = [];
  lintAdvisory.value = [];
  const response = await parley.lintWritingMaterial(material.id, material.version, material.content_sha256, newOperationKey());
  lintJob.value = typeof response.job_id === 'string' ? response.job_id : '';
  lintFindings.value = Array.isArray(response.deterministic_findings)
    ? response.deterministic_findings.flatMap((finding) => {
      if (typeof finding !== 'object' || finding === null) return [];
      const record = finding as Record<string, unknown>;
      if (typeof record.code !== 'string' || typeof record.message !== 'string') return [];
      return [{ code: record.code, level: typeof record.level === 'string' ? record.level : 'warn', message: record.message }];
    })
    : [];
  if (lintJob.value) {
    const result = await pollJob(lintJob.value, scopeGeneration.value);
    if (!result) return;
    lintAdvisory.value = Array.isArray(result.findings)
      ? result.findings.flatMap((finding) => {
        if (typeof finding !== 'object' || finding === null) return [];
        const record = finding as Record<string, unknown>;
        if (typeof record.issue !== 'string') return [];
        return [{ issue: record.issue, suggested_rewrite: typeof record.suggested_rewrite === 'string' ? record.suggested_rewrite : '' }];
      })
      : [];
  }
}

async function loadAuthoringFacts() {
  const [nextTemplates, nextFamilies, nextSamples, nextEmbedding] = await Promise.all([
    parley.listWritingTemplates(), parley.listTemplateFamilies(), parley.listVoiceSamples(), parley.getEmbeddingModel(),
  ]);
  templates.value = nextTemplates;
  families.value = nextFamilies;
  voiceSamples.value = nextSamples;
  embedding.value = nextEmbedding;
  embeddingDraft.value = nextEmbedding.model;
}

/** Deep links (/output/{id}) address one opportunity scope or one material. */
async function consumeDeepLink(id: string | null) {
  if (!id || consumedRecord.value === id) return;
  try {
    const material = await parley.getMaterial(id);
    consumedRecord.value = id;
    loadMaterial(material);
    void openTool('versions');
    return;
  } catch { /* Not a material id; try an opportunity id. */ }
  if (await resolveOpportunity(id)) {
    consumedRecord.value = id;
    await selectScope(id);
    mobilePane.value = 'editor';
  }
}
watch(() => props.recordId, id => { void consumeDeepLink(id); });
watch(() => props.location, (location) => {
  if (location === 'templates') void openTool('templates');
});

let threadTimer: ReturnType<typeof globalThis.setInterval> | undefined;
let threadReadInFlight = false;
const threads = ref<ContactThread[]>([]);
async function pollThreads() {
  if (!props.active || threadReadInFlight || globalThis.document.visibilityState !== 'visible') return;
  threadReadInFlight = true;
  try { threads.value = await parley.listThreads(); }
  catch { /* Keep the latest server-confirmed thread state during transport recovery. */ }
  finally { threadReadInFlight = false; }
}
function onVisibilityChange() {
  if (globalThis.document.visibilityState === 'visible') void pollThreads();
}

function onEntityChanged(event: Event) {
  if (!props.active || globalThis.document.visibilityState === 'hidden') return;
  const detail = (event as CustomEvent<{ entityType?: string; entityId?: string }>).detail ?? {};
  if (detail.entityType === 'opportunity' && detail.entityId && detail.entityId !== opportunityId.value) return;
  void loadOpportunityWindow();
  if (scopedRecord.value) void refreshScopeFacts();
}
onMounted(async () => {
  globalThis.addEventListener('parley:entity-changed', onEntityChanged);
  threadTimer = globalThis.setInterval(() => { void pollThreads(); }, 2000);
  globalThis.document.addEventListener('visibilitychange', onVisibilityChange);
  try {
    await Promise.all([loadAuthoringFacts(), loadOpportunityWindow()]);
  } catch (reason) { errorMessage.value = reason instanceof Error ? reason.message : 'Could not load authoring settings.'; }
  if (!opportunityId.value && opportunities.value.length) await selectScope(opportunities.value[0]!.id);
  if (props.location === 'templates') void openTool('templates');
  await consumeDeepLink(props.recordId);
});
onBeforeUnmount(() => {
  globalThis.removeEventListener('parley:entity-changed', onEntityChanged);
  globalThis.document.removeEventListener('visibilitychange', onVisibilityChange);
  if (threadTimer) globalThis.clearInterval(threadTimer);
});
</script>

<template>
  <section
    class="ow"
    aria-label="Output workspace"
    @click="closeOutside"
  >
    <nav
      class="ow-mobile-nav"
      aria-label="Output panes"
    >
      <button
        type="button"
        :aria-pressed="mobilePane === 'resources'"
        @click="mobilePane = 'resources'"
      >
        Resources
      </button>
      <button
        type="button"
        :aria-pressed="mobilePane === 'editor'"
        @click="mobilePane = 'editor'"
      >
        Editor
      </button>
    </nav>
    <div class="ow-split">
      <aside
        class="ow-panel ow-library"
        aria-labelledby="ow-library-heading"
        :data-mobile-active="mobilePane === 'resources'"
      >
        <div class="ow-library-context">
          <div class="ow-between">
            <h2 id="ow-library-heading">
              Opportunity workspace
            </h2>
            <span
              v-if="selectedOpportunity"
              class="ow-pill"
            >{{ selectedOpportunity.state }}</span>
          </div>
          <SearchableSelect
            id="output-opportunity"
            label="Opportunity"
            placeholder="Filter opportunities"
            :model-value="opportunityId"
            :options="opportunityOptions"
            :search="searchOpportunities"
            :selected-label="selectedOpportunity?.title"
            :disabled="busy || !opportunityOptions.length"
            @update:model-value="selectScope($event)"
          />
          <label>Recipient<select
            :value="recipientId"
            :disabled="busy || !participants.length"
            @change="selectScope(opportunityId, ($event.target as HTMLSelectElement).value)"
          ><option
            v-for="person in participants"
            :key="person.id"
            :value="person.id"
          >{{ person.name }}</option></select></label>
        </div>
        <div
          class="ow-tabs"
          aria-label="Opportunity items"
        >
          <button
            type="button"
            :aria-pressed="leftTab === 'resources'"
            @click="leftTab = 'resources'"
          >
            Matched resources
          </button>
          <button
            type="button"
            :aria-pressed="leftTab === 'drafts'"
            @click="leftTab = 'drafts'"
          >
            Drafts <span
              v-if="scopeDraftCount"
              class="ow-number"
            >{{ scopeDraftCount }}</span>
          </button>
        </div>
        <template v-if="leftTab === 'resources'">
          <div
            v-if="listing"
            class="ow-list"
          >
            <div class="ow-between ow-list-summary">
              <span>{{ manualMatching ? 'Manual selection' : `${visibleResources.length} eligible matches` }}</span>
              <button
                type="button"
                class="ow-text"
                @click="openTool('matching')"
              >
                {{ manualMatching ? 'Matching settings' : 'Refresh matches' }}
              </button>
            </div>
            <label class="ow-search"><span class="sr-only">Filter resources</span><input
              v-model="query"
              type="search"
              placeholder="Filter resources"
            ></label>
            <ul
              v-if="visibleResources.length"
              class="ow-items"
              aria-label="Eligible resources"
            >
              <li
                v-for="{ resource, score } in visibleResources"
                :key="resource.id"
              >
                <button
                  type="button"
                  class="ow-resource"
                  :aria-pressed="selection === resource.id"
                  @click="selection = resource.id"
                >
                  <span class="ow-row-title"><strong>{{ resource.title }}</strong><span class="ow-kind">{{ kindLabel(resource.kind) }}</span></span>
                  <small>{{ boundResource?.id === resource.id ? 'Used in draft' : manualMatching ? 'Eligible to share' : score !== undefined ? `${Math.round(score * 100)}% match` : 'Eligible to share' }}</small>
                </button>
              </li>
            </ul>
            <p
              v-else
              class="ow-empty"
              role="status"
            >
              No resources match this filter.
            </p>
            <div
              v-if="selectedEntry"
              class="ow-list-actions"
            >
              <button
                type="button"
                class="ow-text"
                @click="openTool('resource')"
              >
                Review selected resource <span aria-hidden="true">↗</span>
              </button>
            </div>
          </div>
          <div
            v-else
            class="ow-list ow-unavailable"
          >
            <h3>{{ assessment === 'stale' ? 'Matching needs attention' : 'This scope has no assessment yet' }}</h3>
            <p>{{ assessment === 'stale' ? 'The embedding profile changed. Refresh the assessment after calibration or choose an eligible resource manually.' : 'Run matching for this opportunity and recipient when you’re ready.' }}</p>
            <div class="ow-actions">
              <button
                type="button"
                @click="openTool('matching')"
              >
                {{ assessment === 'stale' ? 'Review calibration' : 'Run matching' }}
              </button>
              <button
                type="button"
                class="ow-text"
                @click="chooseManually"
              >
                Choose manually
              </button>
            </div>
          </div>
        </template>
        <div
          v-else
          class="ow-list"
        >
          <p
            v-if="!scopeMaterials.length"
            class="ow-empty"
          >
            No saved drafts for {{ recipient?.name ?? 'this recipient' }} yet.
          </p>
          <ul
            v-else
            class="ow-items"
            aria-label="Saved drafts"
          >
            <li
              v-for="material in scopeMaterials"
              :key="material.id"
            >
              <button
                type="button"
                class="ow-resource"
                :aria-pressed="draft.materialId === material.id"
                @click="loadMaterial(material)"
              >
                <span class="ow-row-title"><strong>{{ kindLabel(material.kind) }}</strong><span class="ow-kind">v{{ material.version }}</span></span>
                <small>{{ material.recipient_name }} · {{ kindLabel(material.status) }}</small>
              </button>
            </li>
          </ul>
          <button
            type="button"
            class="ow-text"
            :disabled="!scopeMaterials.length"
            @click="openTool('versions')"
          >
            View version history
          </button>
        </div>
      </aside>

      <section
        class="ow-panel ow-writing"
        aria-labelledby="ow-editor-heading"
        :data-mobile-active="mobilePane === 'editor'"
      >
        <header class="ow-editor-head">
          <div class="ow-between ow-editor-heading">
            <div>
              <h2 id="ow-editor-heading">
                {{ editorTitle }}
              </h2>
              <p>{{ recipient?.name ?? 'No recipient selected' }} <span aria-hidden="true">·</span> {{ channelLabel }} <span aria-hidden="true">·</span> Rung 1</p>
            </div>
            <span
              class="ow-pill"
              :class="{ 'ow-pill-warn': dirty }"
            >{{ stateLabel }}</span>
          </div>
        </header>
        <div class="ow-toolbar">
          <div
            ref="popover"
            class="ow-menus"
            aria-label="Editor tools"
            @keydown.esc.stop="dismissMenu"
          >
            <div
              v-for="{ group, commands } in toolGroups"
              :key="group"
              class="ow-menu"
            >
              <button
                type="button"
                class="ow-menu-trigger"
                :aria-expanded="menu === group"
                :aria-controls="'ow-menu-' + group"
                @click="toggleMenu(group)"
              >
                {{ group }} <span
                  class="ow-chevron"
                  aria-hidden="true"
                />
              </button>
              <div
                v-if="menu === group"
                :id="'ow-menu-' + group"
                class="ow-menu-popover"
                :aria-label="group + ' options'"
              >
                <button
                  v-for="[id, label, detail] in commands"
                  :key="id"
                  type="button"
                  @click="openTool(id)"
                >
                  <strong>{{ label }}</strong><small>{{ detail }}</small>
                </button>
              </div>
            </div>
          </div>
          <button
            class="primary ow-save"
            type="button"
            :disabled="!canSave || busy"
            :title="canSave ? '' : 'Bind an active template, resolve its fills in the wording, and save an exchange version first.'"
            @click="run(saveDraft)"
          >
            {{ saveLabel }}
          </button>
        </div>
        <div class="ow-paper">
          <label
            class="sr-only"
            for="ow-message"
          >Email draft</label>
          <textarea
            id="ow-message"
            ref="editorEl"
            v-model="draft.body"
            :maxlength="charLimit"
            placeholder="Write your message…"
            spellcheck="false"
          />
        </div>
        <footer class="ow-editor-foot">
          <div class="ow-bindings">
            <button
              type="button"
              class="ow-text"
              @click="openTool('slots')"
            >
              {{ boundTemplate ? `${boundTemplate.name} · v${boundTemplate.version}` : 'Choose template' }}
            </button>
            <span aria-hidden="true">·</span>
            <button
              type="button"
              class="ow-text"
              @click="openTool('exchange')"
            >
              {{ exchangeAttested ? 'Exchange attested' : exchangeVersion ? 'Attestation needed' : 'Set exchange' }}
            </button>
          </div>
          <span>{{ words }} words <span aria-hidden="true">·</span> {{ draft.body.length }} / {{ charLimit.toLocaleString('en-US') }}</span>
        </footer>
      </section>
    </div>
    <p
      v-if="errorMessage"
      role="alert"
    >
      {{ errorMessage }}
    </p>
    <p
      v-else-if="notice && !modal"
      class="ow-status"
      role="status"
    >
      {{ notice }}
    </p>

    <dialog
      ref="modalEl"
      class="ow-dialog"
      aria-labelledby="ow-dialog-title"
      @close="onDialogClose"
    >
      <header class="ow-between">
        <div>
          <h2 id="ow-dialog-title">
            {{ captions[modal] ?? 'Editor tools' }}
          </h2>
          <p>{{ selectedOpportunity?.title ?? '' }} · {{ recipient?.name ?? '' }}</p>
        </div>
        <button
          type="button"
          aria-label="Close"
          @click="closeTool"
        >
          Close
        </button>
      </header>

      <template v-if="modal === 'resource'">
        <div
          v-if="selectedEntry"
          class="ow-tool-card"
        >
          <div class="ow-between">
            <strong>{{ selectedEntry.resource.title }}</strong>
            <span class="ow-kind">{{ kindLabel(selectedEntry.resource.kind) }}</span>
          </div>
          <dl>
            <dt>Matched need</dt><dd>{{ kindLabel(selectedEntry.resource.kind) }} for this recipient</dd>
            <dt>Basis</dt><dd>{{ selectedEntry.score !== undefined ? `Calibrated match score ${Math.round(selectedEntry.score * 100)}%` : 'Manual selection by the owner' }}</dd>
            <dt>Sharing</dt><dd>{{ resourceSharing(selectedEntry.resource) }}</dd>
          </dl>
        </div>
        <p v-else>
          Choose an eligible resource from the opportunity list first.
        </p>
        <div class="ow-actions">
          <button
            class="primary"
            type="button"
            :disabled="!selectedEntry"
            @click="insertResource"
          >
            Insert at cursor
          </button>
        </div>
      </template>

      <template v-else-if="modal === 'slots'">
        <label>Active template<select
          v-model="pendingTemplateId"
        ><option value="">Choose a template</option><option
          v-for="item in templates.filter((template) => template.status === 'active')"
          :key="item.id"
          :value="item.id"
        >{{ item.name }} · v{{ item.version }}</option></select></label>
        <template v-if="pendingTemplate">
          <label
            v-for="slot in pendingTemplateSlots"
            :key="slot"
          >{{ slot }}<select
            v-if="['gift', 'proof', 'artifact'].includes(slot.split(':')[0] ?? '')"
            v-model="pendingFillResources[slot]"
          ><option value="">Choose a resource</option><option
            v-for="resource in props.resources.filter((item) => item.readiness === 'ready' && item.kind === slot.split(':')[0])"
            :key="resource.id"
            :value="resource.id"
          >{{ resolvedResourceName(resource) }}</option></select><input
            v-else
            v-model="pendingFills[slot]"
            :placeholder="slot.startsWith('their_name') ? recipient?.name : slot.startsWith('ask') ? 'A short walkthrough' : ''"
          ></label>
          <p
            v-if="pendingEffectiveFills.some((fill) => fill.value && !draft.body.includes(fill.value))"
            role="status"
          >
            Every filled value must appear verbatim in the draft wording before it can be saved.
          </p>
        </template>
        <div class="ow-actions">
          <button
            class="primary"
            type="button"
            :disabled="!pendingTemplate || !pendingEffectiveFills.every((fill) => fill.value)"
            @click="applyFills"
          >
            Apply selections
          </button>
        </div>
      </template>

      <template v-else-if="modal === 'templates'">
        <div
          v-for="item in templates"
          :key="item.id"
          class="ow-tool-card"
        >
          <div class="ow-between">
            <button
              type="button"
              class="ow-text"
              @click="libraryTemplateId = item.id"
            >
              <strong>{{ item.name }} · v{{ item.version }}</strong>
            </button>
            <span class="ow-pill">{{ item.status }}</span>
          </div>
          <p>Reusable skeleton · {{ item.stage_class }} · {{ item.channel }}</p>
        </div>
        <p
          v-if="!templates.length"
          class="ow-empty"
          role="status"
        >
          No template versions yet. Author the first draft below.
        </p>
        <label>Family<input
          v-model="familyName"
        ></label>
        <label>Version name<input
          v-model="templateName"
        ></label>
        <label>Stage<select v-model="templateStage"><option>identified</option><option>prepared</option><option>touched</option><option>responded</option><option>converging</option></select></label>
        <label>Channel<select v-model="templateChannel"><option value="email">email</option><option value="sms">sms</option></select></label>
        <label>Template body<textarea
          v-model="templateBody"
          rows="5"
        /></label>
        <div class="ow-actions">
          <button
            type="button"
            :disabled="busy || !templateName.trim() || !templateBody.trim()"
            @click="run(saveTemplateVersion)"
          >
            Save as new draft version
          </button>
          <button
            type="button"
            :disabled="busy || libraryTemplate?.status !== 'draft'"
            @click="run(() => transitionTemplate('approve'))"
          >
            Owner approve version
          </button>
          <button
            type="button"
            class="primary"
            :disabled="busy || libraryTemplate?.status !== 'approved'"
            @click="run(() => transitionTemplate('activate'))"
          >
            Activate validated version
          </button>
        </div>
        <fieldset>
          <legend>Template Builder (local model)</legend>
          <label
            v-for="rule in ['R1', 'R2', 'R3', 'R4', 'R5', 'R6', 'R7']"
            :key="rule"
          ><input
            v-model="requestedRules"
            type="checkbox"
            :value="rule"
          >{{ rule }}</label>
          <label>Voice samples<select
            v-model="selectedVoiceIds"
            multiple
          ><option
            v-for="sample in voiceSamples"
            :key="sample.id"
            :value="sample.id"
          >Sample · {{ sample.channel ?? 'unspecified channel' }} · v{{ sample.revision }}</option></select></label>
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
              <strong>{{ candidate.name }}</strong>
              <p>{{ candidate.notes }}</p>
              <button
                type="button"
                @click="useTemplateCandidate(candidate)"
              >
                Edit this candidate
              </button>
            </li>
          </ul>
        </fieldset>
      </template>

      <template v-else-if="modal === 'exchange'">
        <div class="ow-form-grid">
          <label>What they may need <small>Hypothesis</small><textarea
            v-model="pendingHypothesis"
            rows="2"
          /></label>
          <label>What you need <small>Owner-reported</small><textarea
            v-model="pendingOwnerInterest"
            rows="2"
          /></label>
          <label>They get<textarea
            v-model="pendingTheyGet"
            rows="2"
          /></label>
          <label>You get<textarea
            v-model="pendingYouGet"
            rows="2"
          /></label>
        </div>
        <p v-if="exchangeVersion">
          Current exchange v{{ exchangeVersion }} · {{ exchangeAttested ? 'attested' : 'needs owner attestation' }}
        </p>
        <label
          v-if="exchangeVersion && !exchangeAttested"
          class="ow-choice"
        ><input
          v-model="attestationAccepted"
          type="checkbox"
        > I reviewed this saved exchange and attest both sides benefit.</label>
        <div class="ow-actions">
          <button
            type="button"
            :disabled="busy || !pendingHypothesis.trim() || !pendingOwnerInterest.trim() || !pendingTheyGet.trim() || !pendingYouGet.trim()"
            @click="run(saveExchange)"
          >
            Save exchange version
          </button>
          <button
            v-if="exchangeVersion && !exchangeAttested"
            class="primary"
            type="button"
            :disabled="busy || !attestationAccepted"
            @click="run(attestExchange)"
          >
            Attest saved exchange
          </button>
        </div>
      </template>

      <template v-else-if="modal === 'voice'">
        <ul
          v-if="voiceSamples.length"
          class="ow-items"
        >
          <li
            v-for="sample in voiceSamples"
            :key="sample.id"
            class="ow-tool-card"
          >
            <label class="ow-choice"><input
              v-model="selectedVoiceIds"
              type="checkbox"
              :value="sample.id"
            >Use for proposals · {{ sample.channel ?? 'unspecified channel' }} · v{{ sample.revision }}</label>
            <p>{{ sample.text }}</p>
            <button
              type="button"
              :disabled="busy"
              @click="run(() => retireVoice(sample.id))"
            >
              Retire sample
            </button>
          </li>
        </ul>
        <p
          v-else
          class="ow-empty"
          role="status"
        >
          No owner voice samples yet.
        </p>
        <label>Owner voice sample<textarea
          v-model="voiceText"
          rows="3"
          placeholder="Add a short example of your own writing"
        /></label>
        <label>Channel<input
          v-model="voiceChannel"
        ></label>
        <div class="ow-actions">
          <button
            class="primary"
            type="button"
            :disabled="busy || !voiceText.trim()"
            @click="run(saveVoiceSample)"
          >
            Save voice preferences
          </button>
        </div>
      </template>

      <template v-else-if="modal === 'matching'">
        <div class="ow-tool-card">
          <strong>{{ assessment === 'stale' ? 'Recalibration required' : assessment === 'current' ? 'Current calibrated assessment' : 'Assessment missing' }}</strong>
          <p>{{ assessment === 'stale' ? 'The previous results belong to another embedding profile.' : 'Matching uses current recipient evidence and eligible resources.' }}</p>
          <p>Model {{ embedding.model || 'not set' }} · digest {{ embedding.model_digest ?? 'not verified' }} · embedding {{ embedding.readiness.status }} · calibrated kinds: {{ embedding.calibrated_kinds.join(', ') || 'none' }}</p>
          <p v-if="embedding.readiness.status === 'failed' && embedding.readiness.diagnostic">
            Embedding model check failed: {{ embedding.readiness.diagnostic }}
          </p>
          <p v-if="!embedding.model_digest">
            Verify the embedding model before matching; without a verification and calibration, the system cannot rank resources.
          </p>
          <p v-else-if="embedding.calibrated_kinds.length === 0">
            The embedding model is verified but not calibrated. Calibrate thresholds before ranking.
          </p>
        </div>
        <label>Embedding model<input
          v-model="embeddingDraft"
        ></label>
        <div class="ow-actions">
          <button
            class="primary"
            type="button"
            :disabled="busy || !embeddingDraft.trim()"
            @click="run(setEmbeddingModel)"
          >
            Set local embedding model
          </button>
          <button
            type="button"
            :disabled="embedBusy || embeddingProfileRevisionUnavailable"
            @click="run(verifyEmbeddingModel)"
          >
            Verify embedding model
          </button>
          <button
            type="button"
            :disabled="busy || !embedding.model_digest || !recipientId"
            @click="run(runMatch)"
          >
            Run matching
          </button>
          <button
            type="button"
            @click="chooseManually"
          >
            Choose manually
          </button>
        </div>
      </template>

      <template v-else-if="modal === 'compose'">
        <div class="ow-tool-card">
          <p>Uses {{ boundTemplate ? `${boundTemplate.name} · v${boundTemplate.version}` : 'the selected template' }}, {{ boundResource ? boundResource.title : 'the selected resource' }}, and {{ exchangeAttested ? 'the attested exchange' : 'an attested exchange' }}.</p>
        </div>
        <label
          v-if="proposal"
        >Proposal preview<textarea
          v-model="proposal"
          rows="7"
        /></label>
        <div class="ow-actions">
          <button
            v-if="!proposal"
            class="primary"
            type="button"
            :disabled="busy || !boundTemplate || boundTemplate.status !== 'active' || !slotsResolved || !exchangeAttested || !embedding.model_digest"
            @click="run(composeProposal)"
          >
            Generate
          </button>
          <button
            v-else
            class="primary"
            type="button"
            :disabled="busy || !proposalIsCurrent"
            @click="run(applyProposal)"
          >
            Apply to draft
          </button>
        </div>
        <p
          v-if="proposal && !proposalIsCurrent"
          role="status"
        >
          Switch back to the original recipient to apply this proposal.
        </p>
      </template>

      <template v-else-if="modal === 'lint'">
        <div class="ow-tool-card">
          <strong>Advisory · {{ boundMaterial ? `saved version v${boundMaterial.version}` : 'unsaved wording' }}</strong>
          <p>Deterministic findings stay advisory; the release gate enforces them separately.</p>
        </div>
        <ul
          v-if="lintFindings.length"
          class="ow-checks"
        >
          <li
            v-for="finding in lintFindings"
            :key="finding.code + finding.message"
          >
            <span class="ow-kind">{{ finding.level }}</span>{{ finding.code }} · {{ finding.message }}
          </li>
        </ul>
        <ul
          v-if="lintAdvisory.length"
          class="ow-checks"
        >
          <li
            v-for="(finding, index) in lintAdvisory"
            :key="index"
          >
            <span class="ow-kind">advisory</span>{{ finding.issue }} <em>Suggested: {{ finding.suggested_rewrite }}</em>
          </li>
        </ul>
        <div class="ow-actions">
          <button
            class="primary"
            type="button"
            :disabled="busy || !boundMaterial"
            @click="run(runLint)"
          >
            Review advice for saved version
          </button>
        </div>
        <p
          v-if="!boundMaterial"
          role="status"
        >
          Save the current wording as a version to review its advice.
        </p>
      </template>

      <template v-else-if="modal === 'gate'">
        <div
          v-if="!boundMaterial"
          class="ow-tool-card"
        >
          <strong>Unsaved material</strong>
          <p>Save the current wording as a version to run the deterministic gate.</p>
        </div>
        <div
          v-else
          class="ow-tool-card"
        >
          <div class="ow-between">
            <strong>Saved material v{{ boundMaterial.version }}</strong>
            <span
              class="ow-pill"
              :class="{ 'ow-pill-warn': !canApprove }"
            >{{ canApprove ? 'Passed' : 'Needs attention' }}</span>
          </div>
          <p v-if="!gateReport">
            The server has not returned a gate snapshot for this version yet.
          </p>
          <ul
            v-else
            class="ow-checks"
            aria-label="Gate report"
          >
            <li
              v-for="check in gateReport.checks"
              :key="check.id"
            >
              <span class="ow-kind">{{ check.pass || check.overridden ? 'Pass' : 'Review' }}</span>{{ check.id }} · {{ check.detail }}
            </li>
          </ul>
          <label
            v-for="check in failedSoft"
            :key="check.id"
          >Override reason for {{ check.id }} (at least 10 characters)<textarea
            v-model="gateReasons[check.id]"
            minlength="10"
            maxlength="1000"
            :aria-label="`Override reason for ${check.id}`"
          /></label>
          <p
            v-if="hardFailures.length"
            role="alert"
          >
            Hard failures block approval: {{ hardFailures.map((check) => check.id).join(', ') }}. Correct the named evidence or material first.
          </p>
          <p v-if="dirty">
            Save the current wording as a new version before approving it.
          </p>
        </div>
        <div class="ow-actions">
          <button
            v-if="boundMaterial?.status === 'draft' && gateReport"
            type="button"
            :disabled="busy"
            @click="run(evaluateGate)"
          >
            Recalculate current gate report
          </button>
          <button
            class="primary"
            type="button"
            :disabled="busy || !canApprove"
            @click="run(approveMaterial)"
          >
            {{ boundMaterial?.status === 'approved' ? 'Version approved' : 'Approve exact version' }}
          </button>
          <button
            v-if="dirty"
            type="button"
            :disabled="busy || !canSave"
            @click="run(saveDraft)"
          >
            Save new version
          </button>
        </div>
      </template>

      <template v-else-if="modal === 'versions'">
        <div
          v-for="material in scopeMaterials"
          :key="material.id"
          class="ow-tool-card"
        >
          <div class="ow-between">
            <strong>{{ kindLabel(material.kind) }} · v{{ material.version }}</strong>
            <span class="ow-pill">{{ material.status }}</span>
          </div>
          <label>Saved wording<textarea
            :value="material.body"
            readonly
            rows="4"
          /></label>
          <button
            type="button"
            :aria-pressed="draft.materialId === material.id"
            @click="loadMaterial(material); closeTool()"
          >
            {{ draft.materialId === material.id ? 'Open in editor' : 'Open this version' }}
          </button>
        </div>
        <p
          v-if="!scopeMaterials.length"
          class="ow-empty"
          role="status"
        >
          No saved versions for this recipient.
        </p>
      </template>

      <template v-else-if="modal === 'release'">
        <div
          v-if="boundMaterial && canRelease"
          class="ow-tool-card"
        >
          <strong>{{ boundMaterial.status === 'approved' ? 'Approved' : 'Released' }} material v{{ boundMaterial.version }}</strong>
          <p>Release for {{ boundMaterial.recipient_name }} · {{ channelLabel }}.</p>
        </div>
        <p
          v-else
          class="ow-unavailable"
        >
          Save the current material and approve that exact version in Gate checks to enable release.
        </p>
        <div class="ow-actions">
          <button
            class="primary"
            type="button"
            :disabled="busy || !boundMaterial || !canRelease"
            @click="releaseMaterial('copy')"
          >
            Copy approved text
          </button>
          <button
            type="button"
            :disabled="busy || !boundMaterial || !canRelease"
            @click="releaseMaterial('download')"
          >
            Download approved text
          </button>
          <button
            v-if="pendingAck"
            type="button"
            :disabled="busy"
            @click="run(reconcileAck)"
          >
            Reconcile original release acknowledgement
          </button>
        </div>
        <ContactThreadPanel
          v-if="boundMaterial"
          :opportunity-id="boundMaterial.opportunity_id"
          :material="boundMaterial"
          :delivered="deliveredReleases[boundMaterial.id] ?? null"
          :threads="threads"
          @changed="emit('changed')"
        />
      </template>
      <p
        v-if="notice && modal"
        class="ow-status"
        role="status"
      >
        {{ notice }}
      </p>
    </dialog>
  </section>
</template>

<style scoped>
.ow {
  --ow-line: #d4ded5;
  --ow-ink: #19352b;
  --ow-muted: #52665a;
  --ow-accent: #265e4b;
  --ow-wash: #eef3ed;
  --ow-paper: #fffefa;
  color: var(--ow-ink);
  overflow-wrap: anywhere;
}
.ow-split {
  display: grid;
  grid-template-columns: minmax(310px, .82fr) minmax(0, 1.96fr);
  align-items: stretch;
  gap: 1rem;
}
.ow-panel {
  border: 1px solid var(--ow-line);
  border-radius: .6rem;
  background: var(--ow-paper);
  min-width: 0;
}
.ow-library-context { padding: 1rem 1.1rem .75rem; }
.ow-library-context h2 { margin: 0; font-size: 1.04rem; }
.ow-library-context label { font-size: .88rem; font-weight: 600; }
.ow-between { display: flex; justify-content: space-between; align-items: center; gap: .65rem; }
.ow-pill, .ow-kind {
  display: inline-flex;
  align-items: center;
  align-self: center;
  flex-shrink: 0;
  width: max-content;
  padding: .16rem .48rem;
  border-radius: .28rem;
  background: #e9f0e8;
  color: #2e5945;
  font-size: .78rem;
  font-weight: 670;
}
.ow-pill-warn { background: #faedd4; color: #664719; }
.ow-kind { background: #f0f2ed; color: #41584c; font-size: .72rem; }
.ow-tabs {
  display: flex;
  gap: .9rem;
  padding-inline: 1.1rem;
  border-top: 1px solid var(--ow-line);
  border-bottom: 1px solid var(--ow-line);
}
.ow-tabs button {
  background: transparent;
  border: 0;
  box-shadow: none;
  border-radius: 0;
  border-bottom: 2px solid transparent;
  padding-inline: .18rem;
  padding-block: .68rem;
  font-weight: 600;
  color: var(--ow-muted);
}
.ow-tabs button[aria-pressed='true'] { border-bottom-color: var(--ow-accent); color: var(--ow-accent); }
.ow-number { border-radius: 1rem; padding: 0 .35rem; font-size: .75rem; background: var(--ow-wash); }
.ow-list { padding: .75rem 1.1rem .9rem; }
.ow-list-summary { color: var(--ow-muted); font-size: .88rem; }
button.ow-text {
  max-width: 100%;
  min-height: 2rem;
  padding: .1rem .05rem;
  color: var(--ow-accent);
  border: 0;
  background: transparent;
  text-align: left;
  font-weight: 620;
}
button.ow-text:hover { background: transparent; text-decoration: underline; text-underline-offset: .22em; }
button.ow-text:disabled { color: #7d8f84; }
.ow-search input { background: #fcfdf9; }
.ow-items { list-style: none; margin: .65rem 0 0; padding: 0; display: grid; gap: .45rem; }
button.ow-resource {
  display: grid;
  width: 100%;
  justify-items: start;
  text-align: left;
  padding: .62rem .7rem;
  row-gap: .27rem;
  border-color: var(--ow-line);
  background: #fff;
  color: var(--ow-muted);
  font-size: .86rem;
}
button.ow-resource[aria-pressed='true'] {
  box-shadow: inset 3px 0 var(--ow-accent);
  border-color: #93b39f;
  background: #edf3ec;
}
.ow-row-title { display: flex; justify-content: space-between; gap: .45rem; align-items: start; width: 100%; color: var(--ow-ink); }
.ow-row-title strong { font-size: .94rem; line-height: 1.35; }
.ow-resource small { font-size: .79rem; color: #385a48; }
.ow-list-actions { margin-top: .45rem; }
.ow-empty, .ow-unavailable { background: #f8f5e9; border-radius: .4rem; padding: .75rem; color: #57471e; margin-block: .5rem; }
.ow-list.ow-unavailable { margin: .9rem 1.1rem; }
.ow-writing { display: flex; flex-direction: column; min-height: 650px; }
.ow-editor-head { padding: 1.12rem 1.35rem 1rem; }
.ow-editor-heading { align-items: start; }
.ow-editor-heading h2 { font: normal clamp(1.32rem, 2.2vw, 1.95rem)/1.14 Georgia, serif; margin: 0; letter-spacing: -.018em; }
.ow-editor-heading p, .ow-dialog header p { margin: .4rem 0 0; color: var(--ow-muted); font-size: .91rem; }
.ow-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: .6rem;
  background: #f5f7f2;
  border-block: 1px solid var(--ow-line);
  padding: .5rem 1.35rem;
}
.ow-menus { display: flex; flex-wrap: wrap; gap: .38rem; min-width: 0; }
.ow-menu { position: relative; }
.ow-menu-trigger { font-weight: 610; }
.ow-chevron { border: solid var(--ow-accent); border-width: 0 1.5px 1.5px 0; width: .42rem; height: .42rem; transform: rotate(45deg); margin: -.18rem .16rem 0; }
button.ow-menu-trigger[aria-expanded='true'] { border-color: #799d8b; background: #e7f0e7; }
.ow-menu-popover {
  position: absolute;
  z-index: 3;
  top: calc(100% + .4rem);
  left: 0;
  width: 20rem;
  padding: .34rem;
  background: #fffefa;
  border: 1px solid #b7c8ba;
  border-radius: .45rem;
  box-shadow: 0 12px 26px #18352924;
}
.ow-menu-popover button { display: grid; width: 100%; justify-content: stretch; justify-items: start; gap: .15rem; padding: .54rem .6rem; border: 0; background: transparent; text-align: left; }
.ow-menu-popover button:hover, .ow-menu-popover button:focus-visible { background: #e8f0e7; }
.ow-menu-popover small { font-size: .84rem; }
.ow-save { flex-shrink: 0; }
.ow-save:disabled { background: #e3eee6; color: #3a6050; border-color: #ccded1; opacity: 1; }
.ow-paper { display: flex; flex: 1; padding: 1.8rem clamp(1.35rem, 4vw, 3.7rem) 1rem; }
.ow-paper textarea {
  min-height: 360px;
  resize: vertical;
  flex: 1;
  padding: .35rem .25rem;
  border: 1px solid transparent;
  background: transparent;
  color: var(--ow-ink);
  font-size: 1.05rem;
  line-height: 1.72;
  border-radius: .18rem;
}
.ow-paper textarea:focus-visible { outline-offset: .18rem; }
.ow-editor-foot {
  margin-top: auto;
  border-top: 1px solid var(--ow-line);
  padding: .5rem 1.35rem;
  display: flex;
  align-items: center;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: .3rem .75rem;
  color: var(--ow-muted);
  font-size: .86rem;
}
.ow-bindings { display: flex; gap: .42rem; align-items: center; flex-wrap: wrap; }
.ow-mobile-nav { display: none; }
.ow-dialog { width: min(39rem, calc(100vw - 1.5rem)); color: var(--ow-ink); max-height: calc(100dvh - 2rem); }
.ow-dialog h2 { margin: 0; font: normal 1.55rem Georgia, serif; }
.ow-dialog header { border-bottom: 1px solid var(--ow-line); padding-bottom: .8rem; margin-bottom: .75rem; }
.ow-dialog header button { align-self: start; }
.ow-tool-card { margin: .6rem 0; padding: .8rem .9rem; border: 1px solid var(--ow-line); border-radius: .4rem; background: #f5f8f2; }
.ow-tool-card p:last-child, .ow-checks { margin-bottom: 0; }
.ow-tool-card dl { display: grid; grid-template-columns: max-content minmax(0, 1fr); gap: .15rem .6rem; }
.ow-tool-card dt { font-weight: 650; }
.ow-actions { display: flex; flex-wrap: wrap; align-items: center; gap: .45rem; margin-top: .8rem; }
.ow-form-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: .35rem .8rem; }
.ow-choice { display: flex; align-items: start; gap: .45rem; }
.ow-checks { display: grid; list-style: none; gap: .55rem; padding: 0; }
.ow-checks li { display: flex; align-items: start; gap: .5rem; }
.ow-status { margin: .55rem 0 0; color: var(--ow-accent); }
.ow fieldset { min-width: 0; margin: .55rem 0; border: 1px solid var(--ow-line); border-radius: .35rem; padding: .65rem; }
@media (max-width: 980px) {
  .ow-split { grid-template-columns: minmax(265px, .9fr) minmax(0, 1.55fr); }
  .ow-paper { padding: 1.3rem 1.15rem 1rem; }
}
@media (max-width: 720px) {
  .ow-mobile-nav { display: flex; gap: .45rem; margin-bottom: .65rem; }
  .ow-split { display: block; }
  .ow-panel[data-mobile-active='false'] { display: none; }
  .ow-dialog { width: calc(100vw - 1.5rem); }
  .ow-writing { min-height: 565px; }
  .ow-paper textarea { min-height: 285px; }
  .ow-menu-popover { position: fixed; top: auto; margin-top: .45rem; left: 5vw; width: 90vw; }
  .ow-editor-head, .ow-toolbar, .ow-editor-foot { padding-inline: .7rem; }
  .ow-menus { gap: .23rem; }
  .ow-menu-trigger, .ow-save { padding-inline: .4rem; font-size: .88rem; }
  .ow-form-grid { grid-template-columns: minmax(0, 1fr); }
}
</style>
