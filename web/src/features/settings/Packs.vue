<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { parley, type PackSettings, type PackValidation } from '../../api/client';

defineOptions({ name: 'PackSettingsView' });

const settings = ref<PackSettings | null>(null);
const selectedKey = ref('');
const documentText = ref('');
const stageMappingText = ref('{}');
const validation = ref<PackValidation | null>(null);
const busy = ref(false);
const error = ref('');
const status = ref('');
const selectedPack = computed(() => settings.value?.available_packs.find((pack) => `${pack.id}@${pack.version}` === selectedKey.value) ?? null);
const candidatePackId = computed(() => validation.value?.pack_id ?? selectedPack.value?.id ?? null);
const candidatePackVersion = computed(() => validation.value?.version ?? selectedPack.value?.version ?? null);
const changesActiveVersion = computed(() => !!candidatePackId.value && !!candidatePackVersion.value && (candidatePackId.value !== settings.value?.active_pack_id || candidatePackVersion.value !== settings.value?.active_pack_version));
const canActivate = computed(() => !!settings.value && !!validation.value?.valid && validation.value.pack_id === selectedPack.value?.id && validation.value.normalized_document === documentText.value && (!changesActiveVersion.value || readStageMapping() !== null));

function readStageMapping(): Record<string, string> | null {
  try {
    const value: unknown = JSON.parse(stageMappingText.value);
    if (typeof value !== 'object' || value === null || Array.isArray(value) || Object.keys(value).length === 0) return null;
    const entries = Object.entries(value);
    if (entries.some(([key, target]) => !key.trim() || typeof target !== 'string' || !target.trim())) return null;
    return Object.fromEntries(entries) as Record<string, string>;
  } catch { return null; }
}

async function load() {
  await parley.connect();
  settings.value = await parley.getPackSettings();
  selectedKey.value = `${settings.value.active_pack_id}@${settings.value.active_pack_version}`;
  documentText.value = settings.value.active_document;
}

function selectPack(key: string) {
  selectedKey.value = key;
  const pack = settings.value?.available_packs.find((item) => `${item.id}@${item.version}` === key);
  if (pack) documentText.value = pack.document;
  validation.value = null; error.value = ''; status.value = '';
}

async function validate() {
  busy.value = true; error.value = ''; status.value = ''; validation.value = null;
  try {
    validation.value = await parley.validatePack(documentText.value);
    if (validation.value.valid) {
      if (validation.value.normalized_document !== null) documentText.value = validation.value.normalized_document;
      status.value = `Pack ${validation.value.pack_id} v${validation.value.version} passed server validation. It is still inactive.`;
    }
    else status.value = 'Pack validation found errors. The previously active pack remains in use.';
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not validate this pack.'; }
  finally { busy.value = false; }
}

async function activate() {
  if (!settings.value || !selectedPack.value || !canActivate.value) return;
  busy.value = true; error.value = ''; status.value = '';
  try {
    const mapping = readStageMapping();
    if (!mapping) return;
    const next = await parley.activatePack({ pack_id: validation.value!.pack_id!, version: validation.value!.version!, document: validation.value!.normalized_document!, expected_settings_revision: settings.value.revision, stage_mapping: mapping });
    settings.value = next;
    selectedKey.value = `${next.active_pack_id}@${next.active_pack_version}`;
    documentText.value = next.active_document;
    validation.value = null;
    status.value = `Pack ${next.active_pack_id} v${next.active_pack_version} is active for future work. Historical sends retain their original pack and gate snapshot.`;
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Pack activation was refused. The previous valid pack remains active.'; }
  finally { busy.value = false; }
}

onMounted(() => { void load().catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Could not load pack settings.'; }); });
</script>

<template>
  <section aria-labelledby="packs-heading">
    <h3 id="packs-heading">
      Config packs
    </h3>
    <p v-if="settings">
      Active: {{ settings.active_pack_id }} v{{ settings.active_pack_version }} · settings revision {{ settings.revision }}
    </p>
    <p>Validate a complete pack before activation. Invalid input leaves the current pack active; activation changes future policy and never rewrites historical work.</p>
    <label>Pack to review<select
      :value="selectedKey"
      :disabled="busy"
      @change="selectPack(($event.target as HTMLSelectElement).value)"
    ><option
      v-for="pack in settings?.available_packs ?? []"
      :key="`${pack.id}:${pack.version}`"
      :value="`${pack.id}@${pack.version}`"
    >{{ pack.name }} · {{ pack.id }} v{{ pack.version }}</option></select></label>
    <p v-if="selectedPack">
      Selected version {{ selectedPack.version }}. Edit its full YAML below, then validate.
    </p>
    <label>Pack YAML<textarea
      v-model="documentText"
      rows="16"
      spellcheck="false"
      @input="validation = null"
    /></label>
    <label v-if="changesActiveVersion">Stage mapping JSON<textarea
      v-model="stageMappingText"
      rows="4"
      spellcheck="false"
      aria-label="Stage mapping JSON"
    /><small>Map existing stage names explicitly when changing pack/version. Example: {"Researched":"prepared"}.</small></label>
    <div class="actions">
      <button
        type="button"
        :disabled="busy || !documentText.trim()"
        @click="validate"
      >
        Validate pack
      </button><button
        type="button"
        :disabled="busy || !canActivate"
        @click="activate"
      >
        Activate validated version
      </button>
    </div>
    <ul
      v-if="validation && !validation.valid"
      aria-label="Pack validation errors"
    >
      <li
        v-for="(item, index) in validation.errors"
        :key="`${item.line}-${item.field}-${index}`"
      >
        {{ item.line === null ? 'Pack' : `Line ${item.line}` }}<span v-if="item.field"> · {{ item.field }}</span>: {{ item.message }}
      </li>
    </ul>
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
  </section>
</template>

<style scoped>
.actions { display: flex; gap: .5rem; flex-wrap: wrap; }
textarea { font-family: ui-monospace, monospace; }
</style>
