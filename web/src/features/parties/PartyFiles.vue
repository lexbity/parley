<script setup lang="ts">
/* global document, URL, File, FileList */
import { onBeforeUnmount, reactive, ref } from 'vue';
import { ApiError, newOperationKey, parley, type DossierFile } from '../../api/client';
const props = defineProps<{ entityId?: string; files?: DossierFile[]; readonly?: boolean }>();
const emit = defineEmits<{ changed: []; staged: [uploadIds: string[]]; busy: [pending: boolean] }>();
type Staged = { file: File; key: string; uploadId?: string; percent: number; state: 'uploading' | 'staged' | 'binding' | 'failed'; error: string };
const queue = ref<Staged[]>([]);
const fileErrors = reactive<Record<string, string>>({});
const fileBusy = reactive<Record<string, boolean>>({});
const deleteKeys = new Map<string, string>();
const objectUrls = new Map<string,string>();
const epoch = parley.workspaceEpoch;
let active = true;
onBeforeUnmount(() => { active = false; for (const url of objectUrls.values()) URL.revokeObjectURL(url); });
function checkContext() {
  if (epoch !== parley.workspaceEpoch) throw new Error('Workspace changed. Reopen the card to stage files in this workspace.');
}
function update() {
  emit('staged', queue.value.flatMap(item => item.uploadId && item.state === 'staged' ? [item.uploadId] : []));
  emit('busy', queue.value.some(item => item.state === 'uploading' || item.state === 'binding' || item.state === 'failed'));
}
async function stage(item: Staged) {
  item.error = ''; item.state = 'uploading'; item.percent = 0; update();
  try {
    checkContext();
    item.uploadId = await parley.stageEntityAttachment(item.file, percent => { item.percent = percent; });
    if (!active) return;
    item.state = 'staged';
  } catch (reason) { item.state = 'failed'; item.error = reason instanceof Error ? reason.message : 'Upload failed.'; }
  finally { if (active) update(); }
}
function add(files: FileList | null) {
  if (props.readonly) return;
  for (const file of Array.from(files ?? [])) {
    const item = reactive<Staged>({ file, key: newOperationKey(), percent: 0, state: 'uploading', error: '' });
    queue.value.push(item);
    void stage(item);
  }
}
function remove(item: Staged) { queue.value = queue.value.filter(other => other.key !== item.key); update(); }
async function bind(item: Staged) {
  if (!props.entityId || !item.uploadId) return;
  item.state = 'binding'; item.error = ''; update();
  try {
    checkContext();
    await parley.bindPartyFile(props.entityId, item.uploadId, item.key);
    remove(item); emit('changed');
  } catch (reason) {
    item.state = 'failed'; item.error = reason instanceof Error ? reason.message : 'Could not attach this file.';
    if (reason instanceof ApiError && [404, 410].includes(reason.status)) { item.uploadId = undefined; item.key = newOperationKey(); }
  } finally { update(); }
}
async function handleFile(file: DossierFile, unlink: boolean) {
  if (!props.entityId || fileBusy[file.id]) return;
  fileErrors[file.id] = ''; fileBusy[file.id] = true;
  try {
    checkContext();
    if (unlink) {
      const key = deleteKeys.get(file.id) ?? newOperationKey();
      deleteKeys.set(file.id, key);
      await parley.unlinkPartyFile(props.entityId, file.id, key);
      deleteKeys.delete(file.id); emit('changed');
    } else {
      const bytes = await parley.getPartyFile(props.entityId, file.id);
      if (!active) return;
      const previous = objectUrls.get(file.id);
      if (previous) URL.revokeObjectURL(previous);
      const url = URL.createObjectURL(bytes);
      objectUrls.set(file.id, url);
      const link = document.createElement('a');
      link.href = url; link.download = file.filename;
      document.body.appendChild(link); link.click(); link.remove();
    }
  } catch (reason) { fileErrors[file.id] = reason instanceof Error ? reason.message : 'File unavailable.'; }
  finally { fileBusy[file.id] = false; }
}
</script>
<template>
  <section
    v-if="readonly || parley.supports('party.attachments')"
    class="file-controls"
    aria-label="Private card files"
    @dragover.prevent
    @drop.prevent="add($event.dataTransfer?.files ?? null)"
  >
    <h4 v-if="!readonly">
      Private files
    </h4>
    <ul v-if="files?.length">
      <li
        v-for="file in files"
        :key="file.id"
      >
        <strong>{{ file.filename }}</strong><small>{{ file.content_type }} · {{ file.size }} bytes · {{ file.share_permission ? 'Sharing permitted' : 'Owner only' }}</small>
        <button
          v-if="parley.supports('party.attachments')"
          type="button"
          :disabled="fileBusy[file.id]"
          @click="handleFile(file, false)"
        >
          Download {{ file.filename }}
        </button>
        <button
          v-if="!readonly"
          type="button"
          :disabled="fileBusy[file.id]"
          @click="handleFile(file, true)"
        >
          Unlink
        </button>
        <span
          v-if="fileErrors[file.id]"
          role="alert"
        >{{ fileErrors[file.id] }}</span>
      </li>
    </ul>
    <label v-if="!readonly">Attach files<input
      type="file"
      multiple
      @change="add(($event.target as HTMLInputElement).files); ($event.target as HTMLInputElement).value = ''"
    ></label>
    <ul v-if="queue.length">
      <li
        v-for="item in queue"
        :key="item.key"
      >
        <strong>{{ item.file.name }}</strong><small>{{ item.file.size }} bytes</small>
        <progress
          v-if="item.state === 'uploading'"
          :aria-label="'Uploading ' + item.file.name"
          :value="item.percent"
          max="100"
        />
        <span
          v-if="item.state === 'binding'"
          role="status"
        >Attaching…</span>
        <span
          v-if="item.error"
          role="alert"
        >{{ item.error }}</span>
        <button
          v-if="item.state === 'failed'"
          type="button"
          @click="item.uploadId ? bind(item) : stage(item)"
        >
          Retry
        </button>
        <button
          v-if="item.state === 'staged' && entityId"
          type="button"
          @click="bind(item)"
        >
          Attach {{ item.file.name }}
        </button>
        <span v-if="item.state === 'staged' && !entityId">Ready to save</span>
        <button
          type="button"
          :disabled="item.state === 'binding'"
          @click="remove(item)"
        >
          Remove
        </button>
      </li>
    </ul>
  </section>
</template>
<style scoped>
.file-controls { display: grid; gap: .5rem; padding: .65rem .9rem; border-top: 1px solid #d7ded3; }
h4,ul { margin: 0; } ul { list-style: none; padding: 0; }
li { margin: .45rem 0; } li small { display: block; } button { margin-right: .3rem; }
label { display: grid; gap: .3rem; } input { max-width: 100%; }
</style>
