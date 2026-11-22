<script setup lang="ts">
/* global document, URL */
import { computed, onBeforeUnmount, ref } from 'vue';
import { downloadVerifiedAttachment } from '../../api/download';
import { type Material, type ReleasedMaterial } from '../../api/client';

const props = defineProps<{ material: Material; release?: ReleasedMaterial | null }>();

const attachment = computed(() => props.material.attachment);
const safeName = computed(() => (attachment.value?.filename ?? '').replace(/["\\\r\n]/g, '_'));
const busy = ref(false);
const status = ref('');
const error = ref('');
let objectUrl: string | null = null;

function disposeObjectUrl() {
  if (objectUrl) {
    URL.revokeObjectURL(objectUrl);
    objectUrl = null;
  }
}
onBeforeUnmount(disposeObjectUrl);

async function download() {
  const current = attachment.value;
  if (!current || !props.release || busy.value) return;
  busy.value = true;
  error.value = '';
  status.value = 'Verifying bytes in memory…';
  try {
    const verified = await downloadVerifiedAttachment(props.material, props.release.release_id);
    disposeObjectUrl();
    objectUrl = URL.createObjectURL(verified.blob);
    const anchor = document.createElement('a');
    anchor.href = objectUrl;
    anchor.download = safeName.value;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    status.value = 'Bytes were verified in this tab; your browser saves the file. No disk write is claimed here.';
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'The verified download failed.';
    status.value = '';
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <section
    v-if="attachment"
    class="attachment-panel"
    aria-label="Reviewed attachment"
  >
    <h4>Reviewed attachment</h4>
    <p>{{ safeName }} · {{ attachment.content_type }} · {{ attachment.size }} bytes · for {{ material.recipient_name }}</p>
    <button
      type="button"
      :disabled="busy || !release"
      @click="download"
    >
      {{ busy ? 'Verifying…' : release ? 'Download verified bytes' : 'Release the material to enable download' }}
    </button>
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
.attachment-panel { border-top: 1px solid #cbd5ce; margin-top: .5rem; padding-top: .5rem; display: grid; gap: .35rem; }
</style>
