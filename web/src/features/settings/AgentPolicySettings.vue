<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { parley } from '../../api/client';

const role = ref<'interviewer' | 'briefer'>('interviewer');
const versionId = ref('');
const prompt = ref('');
const bankVersion = ref('');
const bankText = ref('{}');
const busy = ref(false);
const error = ref('');
const status = ref('');

async function loadPrompt() {
  const active = await parley.getAgentPrompt(role.value);
  versionId.value = active.version_id;
  prompt.value = active.content;
}
async function savePrompt() {
  busy.value = true; error.value = ''; status.value = '';
  try { await parley.updateAgentPrompt(role.value, versionId.value, prompt.value); await loadPrompt(); status.value = `A new ${role.value} prompt version is active. Previous runs retain their original digest.`; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not activate this prompt version.'; }
  finally { busy.value = false; }
}
async function loadBank() {
  const bank = await parley.getQuestionBank(); bankVersion.value = bank.version_id; bankText.value = JSON.stringify(bank.content, null, 2);
}
async function saveBank() {
  busy.value = true; error.value = ''; status.value = '';
  try { const content = JSON.parse(bankText.value) as Record<string, unknown>; await parley.updateQuestionBank(bankVersion.value, content); await loadBank(); status.value = 'A new Interviewer question bank version is active.'; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Question bank must be valid JSON.'; }
  finally { busy.value = false; }
}
onMounted(() => { void Promise.all([loadPrompt(), loadBank()]).catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Could not load role settings.'; }); });
</script>

<template>
  <section aria-labelledby="agent-policy-heading">
    <h3 id="agent-policy-heading">
      Versioned role policy
    </h3>
    <label>Role<select
      v-model="role"
      @change="loadPrompt"
    ><option value="interviewer">Interviewer</option><option value="briefer">Briefer</option></select></label>
    <p>Active version {{ versionId }}. Changes create an immutable version; prior jobs keep their saved prompt digest.</p>
    <label>Prompt<textarea
      v-model="prompt"
      rows="10"
    /></label>
    <button
      type="button"
      :disabled="busy || !prompt.trim()"
      @click="savePrompt"
    >
      Activate new prompt version
    </button>
    <h3>Interviewer question bank</h3>
    <p>Version {{ bankVersion }}. Keep one clear question per turn; the owner can skip, defer or mark answers unknown.</p>
    <label>Question bank JSON<textarea
      v-model="bankText"
      rows="10"
      spellcheck="false"
    /></label>
    <button
      type="button"
      :disabled="busy"
      @click="saveBank"
    >
      Activate question bank version
    </button>
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
