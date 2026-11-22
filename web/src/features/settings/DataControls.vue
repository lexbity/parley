<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { parley, type AdminBackup, type AdminJob, type AdminOperation, type AdminOverview, type AdminPreview } from '../../api/client';

defineOptions({ name: 'WorkspaceDataControls' });
const emit = defineEmits<{ workspaceChanged: [operation: AdminOperation, epoch: string] }>();

const overview = ref<AdminOverview | null>(null);
const operation = ref<AdminOperation>('restore');
const selectedBackup = ref('');
const selectedParty = ref('');
const preview = ref<AdminPreview | null>(null);
const confirmation = ref('');
const job = ref<AdminJob | null>(null);
const busy = ref(false);
const error = ref('');
const status = ref('');
const clock = ref(Date.now());
const previewDeadline = ref(0);
let expiryTimer: ReturnType<typeof globalThis.setInterval> | undefined;
let pollTimer: ReturnType<typeof globalThis.setTimeout> | undefined;
const expired = computed(() => !preview.value || !Number.isFinite(previewDeadline.value) || clock.value >= previewDeadline.value);
const verifiedBackups = computed(() => overview.value?.backups.filter((backup) => backup.integrity === 'verified') ?? []);
const confirmationToken = computed(() => preview.value?.digest.slice(-12) ?? '');
const canCommit = computed(() => !!preview.value && !expired.value && confirmation.value === confirmationToken.value && !busy.value);
const operationLabel = computed(() => operation.value === 'restore' ? 'restore this backup' : operation.value === 'erase_party' ? 'erase this party' : 'erase the whole workspace');

async function load() {
  await parley.connect();
  overview.value = await parley.getAdminOverview();
  selectedBackup.value = overview.value.last_verified_backup?.id ?? verifiedBackups.value[0]?.id ?? '';
  selectedParty.value = overview.value.parties[0]?.id ?? '';
}

function changeOperation(next: AdminOperation) {
  operation.value = next;
  preview.value = null;
  confirmation.value = '';
  job.value = null;
  error.value = '';
  status.value = '';
}

function invalidatePreview() {
  preview.value = null;
  confirmation.value = '';
}

async function createBackup() {
  busy.value = true; error.value = ''; status.value = '';
  try {
    const backup: AdminBackup = await parley.createAdminBackup();
    await load();
    status.value = `Verified backup created at ${backup.created_at}. Schema ${backup.schema_version}; ${backup.size_bytes} bytes.`;
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Backup could not be verified.'; }
  finally { busy.value = false; }
}

async function preparePreview() {
  busy.value = true; error.value = ''; status.value = ''; preview.value = null; confirmation.value = ''; job.value = null;
  try {
    const input = operation.value === 'restore'
      ? { operation: operation.value, backup_id: selectedBackup.value }
      : operation.value === 'erase_party'
        ? { operation: operation.value, party_id: selectedParty.value }
        : { operation: operation.value };
    preview.value = await parley.createAdminPreview(input);
    previewDeadline.value = Date.now() + Math.max(0, Date.parse(preview.value.expires_at) - Date.parse(overview.value?.as_of ?? preview.value.expires_at));
    status.value = `Impact preview ready. It expires at ${preview.value.expires_at}.`;
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'A current impact preview could not be created.'; }
  finally { busy.value = false; }
}

async function commit() {
  if (!preview.value || !canCommit.value) return;
  busy.value = true; error.value = ''; status.value = '';
  try {
    const receipt = await parley.commitAdminPreview({ operation: preview.value.operation, preview_id: preview.value.id, digest: preview.value.digest, expected_workspace_epoch: preview.value.workspace_epoch, expected_event_cursor: preview.value.event_cursor, confirmation: confirmation.value });
    status.value = `${operationLabel.value[0]!.toUpperCase()}${operationLabel.value.slice(1)} was accepted for processing. No recipient delivery was created.`;
    await watchJob(receipt.job_id);
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'The administrative action was refused. The preview remains available for review.'; }
  finally { busy.value = false; }
}

async function watchJob(jobId: string) {
  try {
    job.value = await parley.getAdminJob(jobId);
    if (job.value.status === 'succeeded') {
      status.value = job.value.summary ?? 'Administrative operation completed.';
      preview.value = null; confirmation.value = '';
      if (job.value.operation === 'restore') {
        await load();
        status.value = 'Workspace restored and verified. All previous previews and grants are invalidated; reload other open sessions before editing.';
      } else await load();
      emit('workspaceChanged', job.value.operation, overview.value?.workspace_epoch ?? '');
      return;
    }
    if (['failed', 'cancelled'].includes(job.value.status)) {
      error.value = job.value.last_error ?? `Administrative operation ended as ${job.value.status}.`;
      return;
    }
    pollTimer = globalThis.setTimeout(() => { void watchJob(jobId); }, 750);
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Administrative job status is unavailable. Refresh Settings to reconcile it.'; }
}

function remainingTime(): string {
  if (!preview.value || expired.value) return 'Expired';
  return `${Math.max(0, Math.ceil((previewDeadline.value - clock.value) / 1000))} seconds`;
}

onMounted(() => {
  expiryTimer = globalThis.setInterval(() => { clock.value = Date.now(); }, 1000);
  void load().catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Administrative workspace data is unavailable.'; });
});
onBeforeUnmount(() => { if (expiryTimer) globalThis.clearInterval(expiryTimer); if (pollTimer) globalThis.clearTimeout(pollTimer); });
</script>

<template>
  <section aria-labelledby="data-controls-heading">
    <h3 id="data-controls-heading">
      Workspace backup, restore and erasure
    </h3>
    <p v-if="overview">
      Workspace epoch {{ overview.workspace_epoch }} · event cursor {{ overview.event_cursor }} · {{ overview.parties.length }} parties
    </p>
    <p>Backups preserve workspace history and files. Personal data export is a separate operation. Restores replace the workspace only after server verification.</p>
    <div class="actions">
      <button
        type="button"
        :disabled="busy"
        @click="createBackup"
      >
        Create and verify backup
      </button>
      <button
        type="button"
        :disabled="busy"
        @click="load"
      >
        Refresh data status
      </button>
    </div>
    <p
      v-if="overview?.last_verified_backup"
      role="status"
    >
      Last verified backup: {{ overview.last_verified_backup.created_at }} · schema {{ overview.last_verified_backup.schema_version }} · {{ overview.last_verified_backup.size_bytes }} bytes
    </p>
    <p v-else>
      No verified backup exists yet. Create one before a restore or workspace wipe.
    </p>

    <fieldset :disabled="busy">
      <legend>Review an administrative change</legend>
      <label>Operation<select
        :value="operation"
        @change="changeOperation(($event.target as HTMLSelectElement).value as AdminOperation)"
      ><option value="restore">Restore verified backup</option><option value="erase_party">Erase one party</option><option value="erase_workspace">Erase entire workspace</option></select></label>
      <label v-if="operation === 'restore'">Verified backup<select
        v-model="selectedBackup"
        @change="invalidatePreview"
      ><option
        v-for="backup in verifiedBackups"
        :key="backup.id"
        :value="backup.id"
      >{{ backup.created_at }} · schema {{ backup.schema_version }} · {{ backup.integrity }}</option></select></label>
      <label v-if="operation === 'erase_party'">Party to erase<select
        v-model="selectedParty"
        @change="invalidatePreview"
      ><option
        v-for="party in overview?.parties ?? []"
        :key="party.id"
        :value="party.id"
      >{{ party.name }} · {{ party.kind }}<template v-if="party.descriptor"> · {{ party.descriptor }}</template></option></select></label>
      <p v-if="operation === 'erase_workspace'">
        This removes all workspace records, histories, attachments, layouts and output versions. Create a verified backup before continuing.
      </p>
      <button
        type="button"
        :disabled="busy || (operation === 'restore' && !selectedBackup) || (operation === 'erase_party' && !selectedParty) || (operation === 'erase_workspace' && verifiedBackups.length === 0)"
        @click="preparePreview"
      >
        Preview exact impact
      </button>
    </fieldset>

    <article
      v-if="preview"
      class="preview"
      aria-labelledby="preview-heading"
    >
      <h4 id="preview-heading">
        Review before {{ operationLabel }}
      </h4>
      <p>Preview {{ preview.id }} · state digest <code>{{ preview.digest }}</code></p>
      <p>Workspace epoch {{ preview.workspace_epoch }} · cursor {{ preview.event_cursor }} · expires in {{ remainingTime() }}</p>
      <p v-if="preview.backup">
        Restore source: {{ preview.backup.created_at }} · schema {{ preview.backup.schema_version }} · {{ preview.backup.integrity }}
      </p>
      <p v-if="preview.party">
        Party: {{ preview.party.name }}
      </p>
      <h5>Records affected</h5>
      <ul>
        <li
          v-for="(count, kind) in preview.impact"
          :key="kind"
        >
          {{ kind.replaceAll('_', ' ') }}: {{ count }}
        </li>
      </ul>
      <p v-if="Object.keys(preview.preserved_shared).length">
        Shared evidence preserved for other records:
      </p>
      <ul v-if="Object.keys(preview.preserved_shared).length">
        <li
          v-for="(count, kind) in preview.preserved_shared"
          :key="kind"
        >
          {{ kind.replaceAll('_', ' ') }}: {{ count }} retained
        </li>
      </ul>
      <label>Confirm this exact preview. Type {{ confirmationToken }}<input
        v-model="confirmation"
        autocomplete="off"
        :aria-label="`Confirm digest ${confirmationToken}`"
      ></label>
      <button
        type="button"
        :disabled="!canCommit"
        @click="commit"
      >
        Confirm {{ operationLabel }}
      </button>
      <button
        type="button"
        :disabled="busy"
        @click="preview = null; confirmation = ''"
      >
        Cancel preview
      </button>
      <p
        v-if="expired"
        role="alert"
      >
        This preview expired. Create a fresh impact preview before continuing.
      </p>
    </article>
    <p
      v-if="job && ['queued', 'running'].includes(job.status)"
      role="status"
    >
      {{ job.operation }} is {{ job.status }}. This administrative job uses a session-bound status check.
    </p>
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
.preview { border: 2px solid #80552a; border-radius: .4rem; margin-block: 1rem; padding: 1rem; }
code { overflow-wrap: anywhere; }
</style>
