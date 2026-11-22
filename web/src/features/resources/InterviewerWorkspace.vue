<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { parley, type AgentChat, type AgentChatSummary, type RoleJob } from '../../api/client';

const kind = ref<'proof' | 'gift' | 'artifact'>('proof');
const chat = ref<AgentChat | null>(null);
const chatId = ref('');
const history = ref<AgentChatSummary[]>([]);
const historyId = ref('');
const message = ref('');
const title = ref('');
const fieldsJson = ref('{}');
const editing = ref(false);
const bank = ref<Record<string, unknown>>({});
const busy = ref(false);
const error = ref('');
const status = ref('');
const questions = computed(() => {
  const values = bank.value.questions;
  return values && typeof values === 'object' && !Array.isArray(values)
    ? ((values as Record<string, unknown>)[kind.value] as Array<{ id: string; text: string }> | undefined) ?? []
    : [];
});
const candidate = computed(() => chat.value?.candidates.filter((item) => item.state === 'proposed').at(-1) ?? null);

async function refreshChat() {
  if (!chatId.value) return;
  chat.value = await parley.getAgentChat(chatId.value);
  const proposed = candidate.value;
  if (proposed && !editing.value) {
    title.value = String(proposed.fields.title ?? '');
    fieldsJson.value = JSON.stringify(proposed.fields, null, 2);
  }
}

async function loadHistory() {
  history.value = await parley.listAgentChats({ role: 'interviewer', scope_kind: 'owner' });
}

async function openHistory() {
  if (!historyId.value) return;
  chatId.value = historyId.value;
  editing.value = false;
  message.value = '';
  chat.value = await parley.getAgentChat(historyId.value);
  kind.value = chat.value.asset_kind ?? 'proof';
  const proposed = candidate.value;
  if (proposed) {
    title.value = String(proposed.fields.title ?? '');
    fieldsJson.value = JSON.stringify(proposed.fields, null, 2);
  }
}

async function start() {
  busy.value = true; error.value = ''; status.value = '';
  try {
    chat.value = await parley.createAgentChat({ role: 'interviewer', scope_kind: 'owner', asset_kind: kind.value });
    chatId.value = chat.value.id;
    editing.value = false;
    message.value = '';
    await loadHistory();
    status.value = `Interviewer started for a ${kind.value} candidate. Nothing is saved until you confirm it.`;
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not start the Interviewer.'; }
  finally { busy.value = false; }
}

async function waitForJob(id: string) {
  const startedChat = chatId.value;
  for (let attempt = 0; attempt < 180; attempt += 1) {
    await new Promise((resolve) => globalThis.setTimeout(resolve, 1000));
    const job: RoleJob = await parley.getRoleJob(id);
    if (!['queued', 'running'].includes(job.status)) {
      // The job belongs to the chat that started it; if the owner switched
      // chats, never attach its outcome or refresh to the current chat.
      if (chatId.value !== startedChat) return;
      await refreshChat();
      if (job.status !== 'succeeded') error.value = job.last_error ?? 'Interviewer could not validate its proposal. Continue manually.';
      return;
    }
  }
  if (chatId.value === startedChat) error.value = 'Interviewer is still running. Your conversation is saved and can be reopened.';
}

async function sendTurn() {
  if (!message.value.trim() || busy.value) return;
  if (!chat.value) {
    const draft = message.value;
    await start();
    message.value = draft;
  }
  if (!chat.value) return;
  busy.value = true; error.value = '';
  try {
    const result = await parley.addAgentTurn(chat.value.id, message.value.trim());
    message.value = '';
    await refreshChat();
    void waitForJob(result.job_id).catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Could not refresh the Interviewer result.'; });
    status.value = 'Your answer is saved; the Interviewer proposal remains pending review.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save your answer.'; }
  finally { busy.value = false; }
}

async function askQuestion(questionId: string, action: 'ask' | 'skip' | 'defer' | 'unknown') {
  if (!chat.value) return;
  busy.value = true; error.value = '';
  try { await parley.setInterviewerQuestion(chat.value.id, questionId, action); await refreshChat(); status.value = action === 'ask' ? 'Question added to the owner chat.' : `Question marked ${action}.`; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not update question state.'; }
  finally { busy.value = false; }
}

const emit = defineEmits<{ saved: [] }>();

async function confirmCandidate() {
  if (!chat.value || !candidate.value) return;
  busy.value = true; error.value = '';
  try {
    const fields = JSON.parse(fieldsJson.value) as Record<string, unknown>;
    await parley.confirmInterviewerCandidate(candidate.value.id, { title: title.value.trim(), fields, expected_chat_revision: chat.value.revision });
    emit('saved');
    await refreshChat(); editing.value = false; status.value = 'Owner-confirmed resource saved. Its readiness follows the typed resource validator.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Candidate fields must be valid JSON.'; }
  finally { busy.value = false; }
}

async function dismissCandidate() {
  if (!candidate.value) return;
  busy.value = true; error.value = '';
  try { await parley.dismissInterviewerCandidate(candidate.value.id); await refreshChat(); editing.value = false; status.value = 'Candidate dismissed; its values were not saved.'; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not dismiss candidate.'; }
  finally { busy.value = false; }
}

onMounted(async () => { try { bank.value = (await parley.getQuestionBank()).content; await loadHistory(); } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not load the Interviewer bank or saved chats.'; } });
</script>

<template>
  <section
    class="chat-pane"
    aria-labelledby="interviewer-heading"
  >
    <header class="pane-bar">
      <h2 id="interviewer-heading">
        Resource chat
      </h2><small>Interviewer</small>
    </header>
    <p
      v-if="!chat"
      class="chat-prompt"
    >
      What would you like to offer or record?
    </p>
    <details class="chat-options">
      <summary>Chat options</summary>
      <label>Resource kind
        <select
          v-model="kind"
          :disabled="Boolean(chat)"
        ><option value="proof">Proof</option><option value="gift">Gift</option><option value="artifact">Artifact</option></select>
      </label>
      <label>Saved owner chats
        <select v-model="historyId"><option value="">Choose a saved chat</option><option
          v-for="item in history"
          :key="item.id"
          :value="item.id"
        >{{ item.asset_kind }} · {{ item.last_turn ?? 'No answer yet' }}</option></select>
      </label>
      <button
        type="button"
        :disabled="busy || !historyId"
        @click="openHistory"
      >
        Open saved chat
      </button>
    </details>
    <div
      v-if="chat"
      class="interviewer-grid"
    >
      <section aria-label="Owner chat">
        <h3>Owner chat</h3>
        <ol>
          <li
            v-for="turn in chat.turns"
            :key="turn.id"
          >
            <strong>{{ turn.speaker === 'owner' ? 'You' : 'Interviewer' }}:</strong> {{ turn.text }}
          </li>
        </ol>
      </section>
      <details aria-label="Fixed question bank">
        <summary>Question bank</summary>

        <ul>
          <li
            v-for="question in questions"
            :key="question.id"
          >
            {{ question.text }}
            <small v-if="chat.questions.find((item) => item.question_id === question.id)">{{ chat.questions.find((item) => item.question_id === question.id)?.state }}</small>
            <button
              type="button"
              :disabled="busy"
              @click="askQuestion(question.id, 'ask')"
            >
              Ask
            </button>
            <button
              type="button"
              :disabled="busy"
              @click="askQuestion(question.id, 'skip')"
            >
              Skip
            </button>
            <button
              type="button"
              :disabled="busy"
              @click="askQuestion(question.id, 'defer')"
            >
              Defer
            </button>
            <button
              type="button"
              :disabled="busy"
              @click="askQuestion(question.id, 'unknown')"
            >
              Unknown
            </button>
          </li>
        </ul>
      </details>
    </div>
    <section
      v-if="candidate"
      aria-label="Candidate review"
    >
      <h3>Proposed candidate · not saved</h3>
      <p v-if="candidate.missing_fields.length">
        Missing: {{ candidate.missing_fields.join(', ') }}
      </p>
      <label>Title<input
        v-model="title"
        maxlength="240"
        @input="editing = true"
      ></label>
      <label>Editable fields JSON<textarea
        v-model="fieldsJson"
        rows="8"
        spellcheck="false"
        @input="editing = true"
      /></label>
      <details><summary>Owner quote evidence</summary><pre>{{ JSON.stringify(candidate.owner_quotes, null, 2) }}</pre></details>
      <button
        type="button"
        :disabled="busy"
        @click="confirmCandidate"
      >
        Confirm and save
      </button>
      <button
        type="button"
        :disabled="busy"
        @click="dismissCandidate"
      >
        Dismiss
      </button>
    </section>
    <form
      class="chat-composer"
      @submit.prevent="sendTurn"
    >
      <textarea
        v-model="message"
        aria-label="Your answer"
        rows="3"
        maxlength="16000"
        :disabled="busy"
        placeholder="Tell me about something you've done…"
      />
      <div>
        <button
          v-if="!chat"
          class="ask-question"
          type="button"
          :disabled="busy || Boolean(chat)"
          @click="start"
        >
          Start Interviewer
        </button>
        <button
          class="primary"
          :disabled="busy || !message.trim()"
        >
          Send
        </button>
      </div>
    </form>
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
