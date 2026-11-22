<script setup lang="ts">
import { onMounted, ref, watch } from 'vue';
import { parley, type AgentChat, type AgentChatSummary, type BriefItem, type GraphEntity, type RoleJob, type VocabularyFluency } from '../../api/client';

const props = defineProps<{ parties: GraphEntity[] }>();
const scopeKind = ref<'party' | 'segment'>('party');
const scopeId = ref('');
const segments = ref<Array<{ id: string; name: string; description: string | null; revision: number }>>([]);
const segmentName = ref('');
const memberId = ref('');
const chatMessage = ref('');
const chat = ref<AgentChat | null>(null);
const chatHistory = ref<AgentChatSummary[]>([]);
const selectedChatId = ref('');
const fluency = ref<VocabularyFluency[]>([]);
const items = ref<BriefItem[]>([]);
const correction = ref<Record<string, string>>({});
const busy = ref(false);
const error = ref('');
const status = ref('');

let loadGeneration = 0;

async function load() {
  const own = ++loadGeneration;
  if (!scopeId.value) { if (own === loadGeneration) items.value = []; return; }
  const [loadedItems, loadedFluency, chats] = await Promise.all([
    parley.getBriefItems(scopeKind.value, scopeId.value),
    parley.listFluency(),
    parley.listAgentChats({ role: 'briefer', scope_kind: scopeKind.value, scope_id: scopeId.value }),
  ]);
  if (own !== loadGeneration) return;
  items.value = loadedItems;
  fluency.value = loadedFluency;
  chatHistory.value = chats;
  if (selectedChatId.value && chats.some((item) => item.id === selectedChatId.value)) {
    const loadedChat = await parley.getAgentChat(selectedChatId.value);
    if (own !== loadGeneration) return;
    chat.value = loadedChat;
  } else if (own === loadGeneration) {
    chat.value = null;
    selectedChatId.value = '';
  }
}

async function loadSegments() { segments.value = await parley.listSegments(); }

async function createSegment() {
  busy.value = true; error.value = ''; status.value = '';
  try {
    const segment = await parley.createSegment(segmentName.value.trim());
    segmentName.value = ''; await loadSegments(); scopeKind.value = 'segment'; scopeId.value = segment.id;
    status.value = 'Segment created. Add parties explicitly; membership does not create opportunities or outreach.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not create segment.'; }
  finally { busy.value = false; }
}

async function addMember() {
  if (!scopeId.value || !memberId.value) return;
  busy.value = true; error.value = '';
  try { await parley.updateSegmentParty(scopeId.value, memberId.value, 'add'); memberId.value = ''; status.value = 'Party added to the segment.'; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not add segment member.'; }
  finally { busy.value = false; }
}

async function generate() {
  if (!scopeId.value) return;
  const scope = { kind: scopeKind.value, id: scopeId.value };
  busy.value = true; error.value = ''; status.value = '';
  try {
    const { job_id } = await parley.generateBrief(scopeKind.value, scopeId.value);
    status.value = 'Briefer is preparing evidence based on saved sources in this scope.';
    for (let attempt = 0; attempt < 180; attempt += 1) {
      await new Promise((resolve) => globalThis.setTimeout(resolve, 1000));
      const job: RoleJob = await parley.getRoleJob(job_id);
      if (!['queued', 'running'].includes(job.status)) {
        // The job belongs to the scope that started it; if the owner switched
        // scope, never attach its outcome to the current scope.
        if (scope.kind !== scopeKind.value || scope.id !== scopeId.value) return;
        await load();
        if (job.status !== 'succeeded') error.value = job.last_error ?? 'Briefer could not validate its proposals.';
        else status.value = 'Brief proposals are ready for owner review; no proposal is accepted automatically.';
        return;
      }
    }
    if (scope.kind === scopeKind.value && scope.id === scopeId.value) error.value = 'Briefer is still running. Its durable job can be checked again later.';
  } catch (reason) { if (scope.kind === scopeKind.value && scope.id === scopeId.value) error.value = reason instanceof Error ? reason.message : 'Could not generate a brief.'; }
  finally { busy.value = false; }
}

async function decide(item: BriefItem, decision: 'confirm' | 'correct' | 'dismiss') {
  busy.value = true; error.value = ''; status.value = '';
  try {
    await parley.decideBriefItem(item.id, { decision, expected_revision: item.revision, ...(decision === 'correct' ? { corrected_text: correction.value[item.id] ?? '' } : {}) });
    await load();
    status.value = decision === 'confirm' ? 'Confirmed as an owner statement.' : decision === 'correct' ? 'Correction saved as a new attributable owner statement.' : 'Dismissed for this party scope.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save the brief decision.'; }
  finally { busy.value = false; }
}

async function createResearchChat() {
  if (!scopeId.value) return;
  busy.value = true; error.value = '';
  try {
    chat.value = await parley.createAgentChat({ role: 'briefer', scope_kind: scopeKind.value, scope_id: scopeId.value });
    selectedChatId.value = chat.value.id;
    await load();
    status.value = 'A separate research conversation is scoped to this party or segment.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not start this scoped research chat.'; }
  finally { busy.value = false; }
}

async function openResearchChat() {
  if (!selectedChatId.value) return;
  busy.value = true; error.value = '';
  try { chat.value = await parley.getAgentChat(selectedChatId.value); }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not reopen this research chat.'; }
  finally { busy.value = false; }
}

async function sendResearchTurn() {
  if (!chat.value || !chatMessage.value.trim()) return;
  busy.value = true; error.value = '';
  try {
    const result = await parley.addAgentTurn(chat.value.id, chatMessage.value.trim());
    chatMessage.value = '';
    await openResearchChat();
    status.value = 'Your scoped question is saved; any Briefer response remains a proposal.';
    for (let attempt = 0; attempt < 180; attempt += 1) {
      await new Promise((resolve) => globalThis.setTimeout(resolve, 1000));
      const job = await parley.getRoleJob(result.job_id);
      if (!['queued', 'running'].includes(job.status)) {
        chat.value = await parley.getAgentChat(result.chat_id);
        if (job.status !== 'succeeded') error.value = job.last_error ?? 'Briefer could not validate a response.';
        return;
      }
    }
    error.value = 'Briefer is still running. This scoped conversation is saved.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not send the research question.'; }
  finally { busy.value = false; }
}

async function changeFluency(term: string, statusValue: VocabularyFluency['status']) {
  const existing = fluency.value.find((item) => item.term.toLowerCase() === term.toLowerCase());
  if (!existing) return;
  busy.value = true; error.value = '';
  try { const updated = await parley.updateFluency(term, statusValue, existing.revision); fluency.value = fluency.value.map((item) => item.term === updated.term ? updated : item); }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save vocabulary fluency.'; }
  finally { busy.value = false; }
}

watch([scopeKind, scopeId], () => { void load().catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Could not load this brief.'; }); });
onMounted(() => { void loadSegments().catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Could not load segments.'; }); });
</script>

<template>
  <section aria-labelledby="brief-heading">
    <h2 id="brief-heading">
      Brief by party or segment
    </h2>
    <label>Scope type
      <select v-model="scopeKind"><option value="party">Party</option><option value="segment">Segment</option></select>
    </label>
    <label>{{ scopeKind === 'party' ? 'Party' : 'Segment' }}
      <select
        v-if="scopeKind === 'party'"
        v-model="scopeId"
      ><option value="">Choose a party</option><option
        v-for="item in props.parties"
        :key="item.id"
        :value="item.id"
      >{{ item.name }} · {{ item.kind }}</option></select>
    </label>
    <label v-if="scopeKind === 'segment'">Segment
      <select v-model="scopeId"><option value="">Choose a segment</option><option
        v-for="item in segments"
        :key="item.id"
        :value="item.id"
      >{{ item.name }}</option></select>
    </label>
    <form
      v-if="scopeKind === 'segment'"
      @submit.prevent="createSegment"
    >
      <label>New segment<input
        v-model="segmentName"
        required
        maxlength="200"
      ></label>
      <button :disabled="busy">
        Create segment
      </button>
    </form>
    <form
      v-if="scopeKind === 'segment' && scopeId"
      @submit.prevent="addMember"
    >
      <label>Add party to segment<select
        v-model="memberId"
        required
      ><option value="">Choose a person or organization</option><option
        v-for="item in props.parties"
        :key="item.id"
        :value="item.id"
      >{{ item.name }} · {{ item.kind }}</option></select></label>
      <button :disabled="busy">
        Add member
      </button>
    </form>
    <button
      type="button"
      :disabled="busy || !scopeId"
      @click="generate"
    >
      Generate brief proposals
    </button>
    <section
      v-if="scopeId"
      aria-label="Scoped research chat"
    >
      <h3>Research chat · {{ scopeKind }}</h3>
      <p>This transcript is separate from the owner Interviewer and sees only this party or segment scope.</p>
      <label>Saved scoped chats<select v-model="selectedChatId"><option value="">Choose a saved chat</option><option
        v-for="item in chatHistory"
        :key="item.id"
        :value="item.id"
      >{{ item.last_turn ?? 'No question yet' }}</option></select></label>
      <button
        type="button"
        :disabled="busy"
        @click="createResearchChat"
      >
        Start scoped chat
      </button>
      <button
        type="button"
        :disabled="busy || !selectedChatId"
        @click="openResearchChat"
      >
        Open saved chat
      </button>
      <div
        v-if="chat"
        aria-label="Scoped transcript"
      >
        <ol>
          <li
            v-for="turn in chat.turns"
            :key="turn.id"
          >
            <strong>{{ turn.speaker === 'owner' ? 'You' : 'Briefer' }}:</strong> {{ turn.text }}
          </li>
        </ol>
        <form @submit.prevent="sendResearchTurn">
          <label>Question<textarea
            v-model="chatMessage"
            required
            maxlength="16000"
            rows="3"
          /></label><button :disabled="busy">
            Ask in this scope
          </button>
        </form>
      </div>
    </section>
    <p>Evidence confirms what the source said, not whether it is true. Model priors stay tier 2 and cannot be used as individual facts. Review each item.</p>
    <ul aria-label="Brief proposals">
      <li
        v-for="item in items"
        :key="item.id"
      >
        <strong>T{{ item.tier }} · {{ item.kind }} · {{ item.status }}</strong>
        <p>{{ item.text }}</p>
        <blockquote v-if="item.quote">
          {{ item.quote }}
        </blockquote>
        <p v-if="item.tier === 2">
          Tendency, not rule. Verify: {{ item.check_suggestion }}
        </p>
        <label v-if="item.kind === 'jargon'">Your vocabulary fluency
          <select
            aria-label="Your vocabulary fluency"
            :value="fluency.find((entry) => entry.term.toLowerCase() === item.text.toLowerCase())?.status ?? 'unknown'"
            @change="changeFluency(item.text, ($event.target as HTMLSelectElement).value as VocabularyFluency['status'])"
          >
            <option value="unknown">Unknown</option><option value="learning">Learning</option><option value="fluent">Fluent</option>
          </select>
        </label>
        <template v-if="item.status === 'proposed'">
          <label>Correct wording<input
            v-model="correction[item.id]"
            :placeholder="item.text"
          ></label>
          <button
            type="button"
            :disabled="busy"
            @click="decide(item, 'confirm')"
          >
            Confirm
          </button>
          <button
            type="button"
            :disabled="busy"
            @click="decide(item, 'correct')"
          >
            Correct
          </button>
          <button
            type="button"
            :disabled="busy"
            @click="decide(item, 'dismiss')"
          >
            Dismiss
          </button>
        </template>
      </li>
    </ul>
    <p v-if="!items.length && scopeId">
      No brief items yet. Generate proposals from saved sources.
    </p>
    <p
      v-if="status"
      role="status"
    >
      {{ status }}
    </p><p
      v-if="error"
      role="alert"
    >
      {{ error }}
    </p>
  </section>
</template>
