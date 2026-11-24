<script setup lang="ts">
/* global File, URL, document, Event, HTMLInputElement, setTimeout */
import { computed, nextTick, onMounted, ref, watch } from 'vue';
import { ApiError, parley, type BackupRecord, type ErasePreview, type GraphEntity, type MaintenanceOperation, type MaintenanceSettings as Settings, type RestorePreview } from '../../api/client';
const emit=defineEmits<{erased:[]}>();
const parties=ref<GraphEntity[]>([]);
async function loadParties(){ const page=await parley.queryDirectory({limit:200}); parties.value=page.rows; }
const settingsLoading=ref(true); const settingsDirty=ref(false);
const settings=ref<Settings>({backup_directory:'',interval_hours:24,retention_count:14,revision:1,last_successful_at:null}); const backups=ref<BackupRecord[]>([]); const selectedBackup=ref(''); const restorePreview=ref<RestorePreview|null>(null); const importFile=ref<File|null>(null); const importPreview=ref<RestorePreview|null>(null); const emptyTarget=ref(false); const acknowledge=ref(false); const partyId=ref(''); const erasure=ref<ErasePreview|null>(null); const typedName=ref(''); const status=ref(''); const error=ref(''); const busy=ref(false); const partyName=computed(()=>parties.value.find((p)=>p.id===partyId.value)?.name??''); const nameMatches=computed(()=>typedName.value===partyName.value && !!partyName.value);
const eraseNameInput = ref<HTMLInputElement | null>(null);
const restoreConfirmation = ref<HTMLInputElement | null>(null);
const importConfirmation = ref<HTMLInputElement | null>(null);
const previewedParty = ref('');
const previewedBackup = ref('');
function resetRestore() { restorePreview.value = null; previewedBackup.value = ''; acknowledge.value = false; }
function resetErasure() { erasure.value = null; previewedParty.value = ''; typedName.value = ''; }
function resetImport() { importPreview.value = null; emptyTarget.value = false; }
watch(selectedBackup, resetRestore);
watch(partyId, resetErasure);
watch(partyName, resetErasure);
async function load(){try{const [s,b]=await Promise.all([parley.getMaintenanceSettings(),parley.listBackups()]);settings.value=settingsDirty.value?{...settings.value,revision:s.revision,last_successful_at:s.last_successful_at}:s;backups.value=b.items;if(!selectedBackup.value)selectedBackup.value=b.items.find((x)=>x.status==='verified')?.id??'';}catch(e){error.value=e instanceof Error?e.message:'Could not load maintenance settings.';}finally{settingsLoading.value=false;}}
async function action(fn:()=>Promise<void>){if(busy.value)return;busy.value=true;error.value='';status.value='';try{await fn();}catch(e){error.value=e instanceof Error?e.message:'Maintenance action failed.';}finally{busy.value=false;}}
async function save(){await action(async()=>{settings.value=await parley.updateMaintenanceSettings({backup_directory:settings.value.backup_directory.trim(),interval_hours:settings.value.interval_hours,retention_count:settings.value.retention_count,expected_revision:settings.value.revision});settingsDirty.value=false;status.value='Backup destination and schedule saved.';});}
async function backup(){await action(async()=>{const item=await parley.createBackup();backups.value=[item,...backups.value];selectedBackup.value=item.id;status.value=`Verified backup ${item.id} created.`;});}
async function exportData(){await action(async()=>{const blob=await parley.createFullExport();const href=URL.createObjectURL(blob);const a=document.createElement('a');a.href=href;a.download='parley-personal-export.zip';a.click();URL.revokeObjectURL(href);status.value='Personal export downloaded. This archive is owner data, not an approved recipient-facing release.';});}
async function previewRestore() {
  await action(async () => {
    const id = selectedBackup.value;
    if (!id) throw new Error('Choose a verified backup.');
    resetRestore();
    const preview = await parley.previewRestore(id);
    if (selectedBackup.value !== id) return;
    restorePreview.value = preview;
    previewedBackup.value = id;
  });
  void nextTick(() => restoreConfirmation.value?.focus());
}
async function commitRestore() {
  await action(async () => {
    if (!restorePreview.value || previewedBackup.value !== selectedBackup.value || !acknowledge.value) throw new Error('Review the selected backup and acknowledge the replacement.');
    await parley.commitRestore(restorePreview.value, true);
    resetRestore();
    status.value = 'Restore published. The workspace epoch changed. Reopen Parley to sign in again.';
  });
}
async function previewImport() {
  await action(async () => {
    const file = importFile.value;
    if (!file) throw new Error('Choose a Parley export archive.');
    resetImport();
    const preview = await parley.previewFullImport(file);
    if (importFile.value === file) importPreview.value = preview;
  });
  void nextTick(() => importConfirmation.value?.focus());
}
async function commitImport() {
  await action(async () => {
    if (!importPreview.value || !emptyTarget.value) throw new Error('Confirm the target is empty.');
    await parley.commitFullImport(importPreview.value);
    resetImport();
    status.value = 'Personal data import published. A session reset is required.';
  });
}
function chooseFile(event: Event) {
  importFile.value = (event.currentTarget as HTMLInputElement).files?.[0] ?? null;
  resetImport();
}
async function previewErase() {
  await action(async () => {
    const id = partyId.value;
    const name = partyName.value;
    if (!id) throw new Error('Select a person or organization.');
    resetErasure();
    const preview = await parley.previewPartyErasure(id);
    if (partyId.value !== id || partyName.value !== name) return;
    erasure.value = preview;
    previewedParty.value = id;
  });
  void nextTick(() => eraseNameInput.value?.focus());
}
async function waitSession(epoch:string){for(let n=0;n<180;n++){try{await parley.refreshSession(epoch);return;}catch{await new Promise(r=>setTimeout(r,1000));}}throw Error('New workspace session is not available yet. Erasure is not reported complete.');}
async function waitOperation(op:MaintenanceOperation){await waitSession(op.epoch);for(let n=0;n<180;n++){try{const current=await parley.getMaintenanceOperation(op.id);if(current.state==='complete')return current;if(current.state==='failed')throw Error(`Cleanup failed (${current.error_code??'unknown'}).`);}catch(e){if(e instanceof ApiError&&e.status<500)throw e;if(e instanceof Error&&e.message.startsWith('Cleanup failed'))throw e;if(n===179)throw e;}await new Promise(r=>setTimeout(r,1000));}throw Error('Erasure cleanup is still pending.');}
async function erase() {
  await action(async () => {
    if (!erasure.value || previewedParty.value !== partyId.value || !nameMatches.value) throw new Error('Review the selected party and type the exact name.');
    const started = await parley.eraseParty(previewedParty.value, erasure.value);
    resetErasure();
    status.value = 'Erasure committed. Waiting for the new workspace session and cleanup.';
    const done = await waitOperation(started);
    status.value = `Erasure completed after old working data was checked. ${done.external_copies_requiring_disposal} owner-managed copies may need separate disposal.`;
    emit('erased');
    await load();
  });
}
onMounted(()=>{ void load(); void loadParties(); });
</script>
<template>
  <section
    class="maintenance"
    aria-label="Backup, export, restore, and erasure"
  >
    <h2>Backup and data controls</h2><p>Backups and exports contain private workspace data, including third-party information.</p><p
      v-if="error"
      role="alert"
    >
      {{ error }}
    </p><p
      v-if="status"
      role="status"
    >
      {{ status }}
    </p>
    <div class="maintenance-grid">
      <form
        aria-label="Backup schedule"
        @submit.prevent="save"
      >
        <h3>Backup destination and schedule</h3><label>Owner-selected backup directory<input
          v-model="settings.backup_directory"
          required
          maxlength="4096"
          autocomplete="off"
          @input="settingsDirty = true"
        ></label><small>The service validates writability and owner-only access before enabling the schedule.</small><label>Backup interval in hours<input
          v-model.number="settings.interval_hours"
          type="number"
          min="24"
          max="168"
          @input="settingsDirty = true"
        ></label><label>Verified backups to retain<input
          v-model.number="settings.retention_count"
          type="number"
          min="1"
          max="90"
          @input="settingsDirty = true"
        ></label><p>Last verified backup: {{ settings.last_successful_at??'none yet' }}</p><button :disabled="busy||settingsLoading||!settings.backup_directory.trim()">
          Save backup settings
        </button><button
          type="button"
          :disabled="busy||settingsLoading||!settings.backup_directory.trim()"
          @click="backup"
        >
          Create verified backup now
        </button>
      </form>
      <section aria-label="Verified backups">
        <h3>Verified backups</h3><ul>
          <li
            v-for="item in backups"
            :key="item.id"
          >
            {{ item.created_at }} · {{ item.status }} · format {{ item.format_version }} · {{ item.manifest_sha256 }}
          </li>
        </ul><p v-if="!backups.length">
          No verified backup yet.
        </p>
      </section>
      <section aria-label="Personal data export">
        <h3>Full personal data export</h3><p>This is owner data for records and recovery. It does not approve or release recipient-facing content.</p><button
          :disabled="busy"
          @click="exportData"
        >
          Download personal data export
        </button>
      </section>
      <section aria-label="Import personal data">
        <h3>Import personal data</h3><p>Import a full Parley archive into an empty workspace only. This is not a recipient material.</p><label>Parley export archive<input
          type="file"
          accept=".zip,application/zip"
          :disabled="busy"
          @change="chooseFile"
        ></label><button
          :disabled="busy||!importFile"
          @click="previewImport"
        >
          Validate import archive
        </button><article
          v-if="importPreview"
          class="operation-preview"
        >
          <p>Preview {{ importPreview.id }} · {{ importPreview.required_bytes }} bytes · format {{ importPreview.backup.format_version }}</p><label><input
            ref="importConfirmation"
            v-model="emptyTarget"
            type="checkbox"
            :disabled="busy"
          > I confirm this target workspace is empty.</label><button
            :disabled="busy||!emptyTarget"
            @click="commitImport"
          >
            Commit full data import
          </button>
          <button
            type="button"
            :disabled="busy"
            @click="resetImport"
          >
            Cancel
          </button>
        </article>
      </section>
      <section aria-label="Restore workspace">
        <h3>Restore workspace</h3><label>Verified snapshot<select
          v-model="selectedBackup"
          :disabled="busy"
        ><option value="">Choose a backup</option><option
          v-for="b in backups.filter(x=>x.status==='verified')"
          :key="b.id"
          :value="b.id"
        >{{ b.created_at }} · {{ b.id }}</option></select></label><button
          :disabled="busy||!selectedBackup"
          @click="previewRestore"
        >
          Verify restore preview
        </button><article
          v-if="restorePreview"
          class="operation-preview"
        >
          <p>{{ restorePreview.erasure_resurrections }} erased records may return. Snapshot {{ restorePreview.backup.created_at }}; staging {{ restorePreview.required_bytes }} bytes.</p><label><input
            ref="restoreConfirmation"
            v-model="acknowledge"
            type="checkbox"
            :disabled="busy"
          > I understand this replaces current data and may restore erased information.</label><button
            :disabled="busy||!acknowledge"
            @click="commitRestore"
          >
            Commit verified restore
          </button>
          <button
            type="button"
            :disabled="busy"
            @click="resetRestore"
          >
            Cancel
          </button>
        </article>
      </section>
      <section aria-label="Erase a party">
        <h3>Erase a person or organization</h3><p>Erasure is reported complete only after old working-generation cleanup finishes. External owner-managed copies require separate disposal.</p><label>Party<select
          v-model="partyId"
          :disabled="busy"
        ><option value="">Choose a party</option><option
          v-for="p in parties.filter(x=>x.kind==='person'||x.kind==='org')"
          :key="p.id"
          :value="p.id"
        >{{ p.name }} · {{ p.kind }}</option></select></label><button
          :disabled="busy||!partyId"
          @click="previewErase"
        >
          Preview erasure
        </button><article
          v-if="erasure"
          class="operation-preview"
        >
          <h4>Records affected: {{ partyName }}</h4><ul>
            <li
              v-for="(count,kind) in erasure.affected_counts"
              :key="kind"
            >
              {{ kind }}: {{ count }}
            </li>
          </ul><p>{{ erasure.shared_sources }} shared sources · {{ erasure.owner_managed_backups }} owner-managed backups may require separate disposal.</p><label>Type exact party name<input
            ref="eraseNameInput"
            v-model="typedName"
            autocomplete="off"
            :readonly="busy"
          ></label><button
            :disabled="busy||!nameMatches"
            @click="erase"
          >
            Erase {{ partyName }}
          </button>
          <button
            type="button"
            :disabled="busy"
            @click="resetErasure"
          >
            Cancel
          </button>
        </article>
      </section>
    </div>
  </section>
</template>

<style scoped>
.maintenance-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: .65rem; margin-top: .75rem; }
.maintenance-grid > * { min-width: 0; border-color: #c7d1c3; }
.maintenance-grid h3 { margin-top: 0; }
.maintenance-grid input[type="checkbox"] { margin: .2rem 0; }
.maintenance-grid button { margin: .15rem .25rem .15rem 0; }
.operation-preview { margin-top: .55rem; border-top: 1px solid #c7d1c3; padding-top: .4rem; }
@media (max-width: 860px) { .maintenance-grid { grid-template-columns: minmax(0, 1fr); } }
</style>
