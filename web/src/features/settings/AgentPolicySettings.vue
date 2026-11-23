<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from 'vue';
import { parley } from '../../api/client';

type Role = 'interviewer' | 'briefer';
const role = ref<Role>('interviewer');
const prompts = reactive<Record<Role, { version: string; text: string; loading: boolean; error: string }>>({
  interviewer: { version: '', text: '', loading: false, error: '' },
  briefer: { version: '', text: '', loading: false, error: '' },
});
const prompt = computed(() => prompts[role.value]);
const bankVersion = ref('');
const bankText = ref('{}');
const busy = ref(false);
const error = ref('');
const status = ref('');

async function loadPrompt(target: Role) {
  const draft = prompts[target];
  if (draft.loading) return;
  draft.loading = true;
  draft.error = '';
  try {
    const active = await parley.getAgentPrompt(target);
    draft.version = active.version_id;
    draft.text = active.content;
  } catch (reason) {
    draft.error = reason instanceof Error ? reason.message : 'Could not load the role prompt.';
  } finally {
    draft.loading = false;
  }
}
async function savePrompt() {
  const target = role.value;
  const draft = prompts[target];
  if (busy.value || draft.loading || !draft.version || !draft.text.trim()) return;
  busy.value = true; error.value = ''; status.value = '';
  try {
    await parley.updateAgentPrompt(target, draft.version, draft.text);
    draft.version = '';
    await loadPrompt(target);
    status.value = `A new ${target} prompt version is active.`;
  }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not activate this prompt version.'; }
  finally { busy.value = false; }
}
async function loadBank() {
  const bank = await parley.getQuestionBank(); bankVersion.value = bank.version_id; bankText.value = JSON.stringify(bank.content, null, 2);
}
async function saveBank() {
  if (busy.value || !bankVersion.value) return;
  busy.value = true; error.value = ''; status.value = '';
  try { const content = JSON.parse(bankText.value) as Record<string, unknown>; await parley.updateQuestionBank(bankVersion.value, content); await loadBank(); status.value = 'A new Interviewer question bank version is active.'; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Question bank must be valid JSON.'; }
  finally { busy.value = false; }
}
watch(role, target => { if (!prompts[target].version) void loadPrompt(target); }, { immediate: true });
onMounted(() => { void loadBank().catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Could not load the question bank.'; }); });
</script>

<template>
  <section
    class="role-settings"
    aria-labelledby="agent-policy-heading"
  >
    <h3 id="agent-policy-heading">
      Versioned role policy
    </h3>
    <div class="role-panels">
      <div>
        <label>Role<select
          v-model="role"
          :disabled="busy"
        ><option value="interviewer">Interviewer</option><option value="briefer">Briefer</option></select></label>
        <p
          v-if="prompt.loading"
          role="status"
        >
          Loading prompt…
        </p>
        <p
          v-if="prompt.error"
          role="alert"
        >
          {{ prompt.error }}
          <button
            type="button"
            @click="loadPrompt(role)"
          >
            Retry
          </button>
        </p>
        <p v-if="prompt.version">
          Active version {{ prompt.version }}
        </p>
        <label>Prompt<textarea
          v-model="prompt.text"
          rows="10"
          :readonly="busy || prompt.loading || !prompt.version"
        /></label>
        <button
          type="button"
          :disabled="busy || prompt.loading || !prompt.version || !prompt.text.trim()"
          @click="savePrompt"
        >
          Activate new prompt version
        </button>
      </div>
      <div>
        <h4>Interviewer question bank</h4>
        <p v-if="bankVersion">
          Active version {{ bankVersion }}
        </p>
        <label>Question bank JSON<textarea
          v-model="bankText"
          rows="10"
          spellcheck="false"
          :readonly="busy || !bankVersion"
        /></label>
        <button
          type="button"
          :disabled="busy || !bankVersion"
          @click="saveBank"
        >
          Activate question bank version
        </button>
      </div>
    </div>
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

<style scoped>
.role-panels { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: .75rem; }
.role-panels > div { padding: .75rem; border: 1px solid #c7d1c3; border-radius: .45rem; }
.role-panels h4, .role-panels label:first-child { margin-top: 0; }
.role-panels textarea { resize: vertical; }
@media (max-width: 860px) { .role-panels { grid-template-columns: minmax(0, 1fr); } }
</style>
