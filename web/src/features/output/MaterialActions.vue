<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { parley, type GateCheck, type GateReport, type Material, type ReleasedMaterial } from '../../api/client';

const props = defineProps<{ material: Material }>();
const emit = defineEmits<{ updated: [material: Material] }>();
const reasons = ref<Record<string, string>>({});
const busy = ref(false);
const error = ref('');
const status = ref('');
const pendingAck = ref<ReleasedMaterial | null>(null);
const gateReport = ref<GateReport | null>(props.material.gate);
const reportMatchesReasons = ref(false);
const failedSoft = computed(() => gateReport.value?.checks.filter((check) => check.severity === 'soft' && !check.pass) ?? []);
const hardFailures = computed(() => gateReport.value?.checks.filter((check) => check.severity === 'hard' && !check.pass) ?? []);
const overridesComplete = computed(() => failedSoft.value.every((check) => (reasons.value[check.id] ?? '').trim().length >= 10));
const canApprove = computed(() => props.material.status === 'draft' && gateReport.value !== null && gateReport.value.copy_enabled && reportMatchesReasons.value && hardFailures.value.length === 0 && overridesComplete.value);

async function evaluateGate() {
  busy.value = true;
  error.value = '';
  const validReasons = Object.fromEntries(Object.entries(reasons.value).filter(([, reason]) => reason.trim().length >= 10));
  try {
    gateReport.value = await parley.evaluateMaterialGate(props.material.id, validReasons);
    reportMatchesReasons.value = true;
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'Could not refresh the deterministic gate report.';
  } finally { busy.value = false; }
}

watch(reasons, () => { reportMatchesReasons.value = false; }, { deep: true });
watch(() => [props.material.id, props.material.version, props.material.gate] as const, ([, , report]) => {
  gateReport.value = report;
  reportMatchesReasons.value = false;
  if (props.material.status === 'draft') void evaluateGate();
});
onMounted(() => {
  if (!gateReport.value && props.material.status === 'draft') void evaluateGate();
});

async function approve() {
  busy.value = true; error.value = ''; status.value = '';
  try {
    const updated = await parley.approveMaterial(props.material.id, Object.fromEntries(failedSoft.value.map((check) => [check.id, reasons.value[check.id] ?? ''])));
    emit('updated', updated); status.value = `Version ${updated.version} approved against the current recipient and evidence.`;
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'Approval was refused. Refresh this material and review the current gate report.';
  } finally { busy.value = false; }
}

async function acknowledge(release: ReleasedMaterial, succeeded: boolean) {
  await parley.acknowledgeRelease(release.release_id, succeeded);
  pendingAck.value = null;
}

async function release(mode: 'copy' | 'download') {
  busy.value = true; error.value = ''; status.value = '';
  let issued: ReleasedMaterial | null = null;
  let delivered = false;
  try {
    issued = await parley.releaseMaterial(props.material.id);
    if (issued.material_id !== props.material.id || issued.version !== props.material.version || issued.recipient_id !== props.material.recipient_id) {
      throw new Error('The authorized version or recipient changed. No content was released.');
    }
    const digest = await globalThis.crypto.subtle.digest('SHA-256', new globalThis.TextEncoder().encode(issued.body));
    const actual = Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
    if (actual !== issued.content_sha256 || actual !== props.material.content_sha256) throw new Error('The authorized text failed its SHA-256 integrity check. No content was released.');
    if (mode === 'copy') {
      await globalThis.navigator.clipboard.writeText(issued.body);
      delivered = true;
      try { await acknowledge(issued, true); status.value = `Copy confirmed for ${props.material.recipient_name}, version ${issued.version}. Mark sent remains a separate action.`; }
      catch { pendingAck.value = issued; status.value = 'Clipboard accepted the text; its acknowledgement is pending. Reconcile before marking sent.'; }
    } else {
      const blob = new globalThis.Blob([issued.body], { type: 'text/plain;charset=utf-8' });
      const objectUrl = globalThis.URL.createObjectURL(blob);
      const link = globalThis.document.createElement('a');
      link.href = objectUrl;
      link.download = `parley-${filenamePart(props.material.recipient_name)}-${props.material.kind}-v${issued.version}.txt`;
      link.rel = 'noopener';
      globalThis.document.body.append(link); link.click(); link.remove();
      globalThis.setTimeout(() => globalThis.URL.revokeObjectURL(objectUrl), 0);
      delivered = true;
      try { await acknowledge(issued, true); status.value = `Download issued for ${props.material.recipient_name}, version ${issued.version}. The browser cannot confirm a disk save.`; }
      catch { pendingAck.value = issued; status.value = 'Download was issued; its acknowledgement is pending. Reconcile before marking sent.'; }
    }
  } catch (reason) {
    if (issued && !delivered) await acknowledge(issued, false).catch(() => undefined);
    error.value = reason instanceof Error ? reason.message : `Could not ${mode} this approved version.`;
  } finally { busy.value = false; }
}

async function reconcileAck() {
  if (!pendingAck.value) return;
  busy.value = true; error.value = '';
  try { await acknowledge(pendingAck.value, true); status.value = 'The original release acknowledgement is now recorded.'; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Acknowledgement remains pending. Retry reconciliation.'; }
  finally { busy.value = false; }
}

function filenamePart(value: string): string {
  const cleaned = value.normalize('NFKC').replace(/[^\p{L}\p{N}._-]+/gu, '-').replace(/^-+|-+$/g, '').slice(0, 80);
  return cleaned || 'recipient';
}

function checkLabel(check: GateCheck): string { return `${check.id} · ${check.severity.toUpperCase()}`; }
</script>

<template>
  <section
    class="material-actions"
    :aria-label="`Release controls for ${material.recipient_name}, version ${material.version}`"
  >
    <h4>Deterministic gate · {{ material.recipient_name }} · v{{ material.version }}</h4>
    <p v-if="!gateReport">
      The server has not returned a gate snapshot for this version yet.
    </p>
    <ul
      v-else
      aria-label="Gate report"
    >
      <li
        v-for="check in gateReport.checks"
        :key="check.id"
        :data-severity="check.severity"
      >
        <strong>{{ checkLabel(check) }}: {{ check.pass ? 'pass' : check.overridden ? 'overridden' : 'blocked' }}</strong>
        <p>{{ check.detail }}</p>
        <label v-if="check.severity === 'soft' && !check.pass">
          Override reason for {{ check.id }} (at least 10 characters)
          <textarea
            v-model="reasons[check.id]"
            minlength="10"
            maxlength="1000"
            :aria-label="`Override reason for ${check.id}`"
          />
        </label>
      </li>
    </ul>
    <p
      v-if="hardFailures.length"
      role="alert"
    >
      Hard failures block approval: {{ hardFailures.map((check) => check.id).join(', ') }}. Correct the named evidence or material first.
    </p>
    <button
      v-if="material.status === 'draft'"
      type="button"
      :disabled="busy"
      @click="evaluateGate"
    >
      Recalculate current gate report
    </button>
    <button
      v-if="material.status === 'draft'"
      type="button"
      :disabled="busy || !canApprove"
      @click="approve"
    >
      Approve this exact version
    </button>
    <template v-if="material.status === 'approved'">
      <button
        type="button"
        :disabled="busy"
        @click="release('copy')"
      >
        Copy approved text
      </button>
      <button
        type="button"
        :disabled="busy"
        @click="release('download')"
      >
        Download approved text
      </button>
    </template>
    <button
      v-if="pendingAck"
      type="button"
      :disabled="busy"
      @click="reconcileAck"
    >
      Reconcile original release acknowledgement
    </button>
    <p
      v-if="status"
      role="status"
      aria-live="polite"
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
.material-actions { margin-block: .75rem; padding: .75rem; border: 1px solid #cbd5ce; border-radius: .35rem; }
ul { padding-inline-start: 1.3rem; }
li { margin-block: .5rem; }
li[data-severity='hard'] strong { color: #8a2d22; }
li[data-severity='soft'] strong { color: #775311; }
textarea { display:block; width:100%; min-height:4rem; font:inherit; }
</style>
