<script setup lang="ts">
/* global Event, HTMLInputElement, Blob, URL, document, File */
import { computed, reactive, ref, watch } from 'vue';
import { ApiError, newOperationKey, parley, type EntityAttachment, type EntityCard, type PlacementView, type ResourceColumn, type ResourcePlacementInput } from '../../api/client';
import MarkdownEditor from '../../shared/MarkdownEditor.vue';
import MarkdownPreview from '../../shared/MarkdownPreview.vue';

/**
 * Shared content editor for board and graph cards (amendment §4). One draft
 * owns title, subtype, typed body fields, Markdown notes, file changes and
 * placement. Explicit Save card commits everything atomically; Cancel drops
 * uncommitted changes. In a graph node the form additionally offers Add
 * relationship / Link existing / Create related card.
 */

const SUBTYPES = ['gift', 'proof', 'artifact', 'skill', 'interest', 'note'] as const;
export type ResourceSubtype = (typeof SUBTYPES)[number];

const props = defineProps<{
  columns: ResourceColumn[];
  mode: 'create' | 'edit';
  card?: EntityCard | null;
  cardSummary?: { resource_id: string; title: string; subtype: string; readiness: string; placement: PlacementView | null; etag: string } | null;
  initialColumnId?: string | null;
  graphMode?: boolean;
  availableEndpoints?: Array<{ id: string; label: string; kind: string }>;
}>();

const emit = defineEmits<{
  saved: [result: { entity: EntityCard; placement: PlacementView | null; createdRelated: { edge_id: string; to_id: string } | null }];
  cancel: [];
  navigate: [path: string];
}>();

const title = ref('');
const subtype = ref<ResourceSubtype>('artifact');
const body = reactive<Record<string, unknown>>({});
const notes = ref('');
const placementColumn = ref<string>('__off__');
const attachments = ref<EntityAttachment[]>([]);
const stagedUploads = ref<Array<{ id: string; filename: string; size: number; ready: boolean; error: string; file?: File | null }>>([]);
const markedRemovals = ref<string[]>([]);
const saving = ref(false);
const error = ref('');
const status = ref('');
type ConflictView = {
  base: { title: string; notes: string };
  mine: { title: string; notes: string };
  current: { title: string; notes: string } | null;
  currentCard: EntityCard | null;
};
const conflict = ref<ConflictView | null>(null);
/** The revision target for edits: the card as last confirmed by the server. */
const liveCard = ref<EntityCard | null>(props.card ?? null);

// Relationship intent (graph mode only).
const relation = reactive<{
  enabled: boolean;
  verb: string;
  basis: string;
  mode: 'none' | 'link' | 'create';
  targetId: string;
  targetQuery: string;
  createSubtype: ResourceSubtype;
  createTitle: string;
  createNotes: string;
}>({
  enabled: false, verb: 'includes', basis: '', mode: 'none', targetId: '', targetQuery: '',
  createSubtype: 'artifact', createTitle: '', createNotes: '',
});

const dirty = ref(false);
watch([title, subtype, notes, placementColumn], () => { dirty.value = true; });
watch(body, () => { dirty.value = true; }, { deep: true });
watch([stagedUploads, markedRemovals], () => { dirty.value = true; }, { deep: true });
watch(() => relation.enabled, () => { dirty.value = true; });

const KIND_LABELS: Record<string, string> = {
  gift: 'Gift or offer', proof: 'Proof or result', artifact: 'Artifact',
  skill: 'Skill', interest: 'Interest', note: 'Note',
};

function subtypeLabel(kind: string): string {
  return KIND_LABELS[kind] ?? kind;
}

const typedGaps = computed<string[]>(() => {
  const gaps: string[] = [];
  if (subtype.value === 'proof') {
    if (!String(body.result ?? '').trim()) gaps.push('Result');
    if (!String(body.context ?? '').trim()) gaps.push('Context');
    if (!String(body.time ?? '').trim()) gaps.push('Time');
    if (!String(body.owner_role ?? '').trim()) gaps.push('Owner role');
  } else if (subtype.value === 'gift') {
    if (!String(body.effort ?? '').trim()) gaps.push('Effort estimate');
  } else if (subtype.value === 'artifact') {
    if (!String(body.reference ?? '').trim()) gaps.push('Available reference');
    if (body.share_permission !== true) gaps.push('Permission to share');
  }
  return gaps;
});

const filteredEndpoints = computed(() => {
  const query = relation.targetQuery.trim().toLocaleLowerCase();
  const items = (props.availableEndpoints ?? []).filter((item) => !query || item.label.toLocaleLowerCase().includes(query));
  return items.slice(0, 50);
});

const placementIntent = computed<ResourcePlacementInput | null>(() => {
  if (placementColumn.value === '__off__') return { column_id: null };
  return { column_id: placementColumn.value, before_resource_id: null };
});

function refreshModel() {
  const card = props.card;
  title.value = card?.title ?? '';
  subtype.value = (card?.subtype as ResourceSubtype) ?? 'artifact';
  Object.keys(body).forEach((key) => delete body[key]);
  if (card?.body) Object.assign(body, card.body);
  notes.value = card?.notes?.markdown ?? '';
  const summary = props.cardSummary;
  placementColumn.value = summary?.placement?.column_id ?? (props.mode === 'create' && props.initialColumnId ? props.initialColumnId : '__off__');
  attachments.value = [];
  markedRemovals.value = [];
  stagedUploads.value = [];
  conflict.value = null;
  error.value = '';
  status.value = '';
  relation.enabled = false;
  relation.mode = 'none';
  relation.targetId = '';
  relation.targetQuery = '';
  relation.verb = 'includes';
  relation.basis = '';
  relation.createTitle = '';
  relation.createNotes = '';
  relation.createSubtype = 'artifact';
  dirty.value = false;
}

watch(() => props.card, () => {
  if (props.mode === 'edit' && props.card) {
    liveCard.value = props.card;
    refreshModel();
  }
}, { immediate: true });
watch(() => props.mode, () => { if (props.mode === 'create') refreshModel(); }, { immediate: true });

async function uploadEntry(entry: { id: string; filename: string; size: number; ready: boolean; error: string; file?: File | null }) {
  if (!entry.file) { entry.error = 'The file is no longer available; choose it again.'; return; }
  entry.error = '';
  try {
    const upload = await parley.stageUpload(entry.file, entry.file.name);
    entry.id = upload.id;
    entry.ready = true;
    entry.size = Number(upload.size);
  } catch (reason) {
    entry.error = reason instanceof Error ? reason.message : 'Upload failed.';
  }
}

async function addFiles(event: Event) {
  const input = event.target as HTMLInputElement;
  for (const file of Array.from(input.files ?? [])) {
    const entry = { id: `staging-${Math.random().toString(36).slice(2)}`, filename: file.name, size: file.size, ready: false, error: '', file };
    stagedUploads.value.push(entry);
    await uploadEntry(entry);
  }
  input.value = '';
}

function retryUpload(staged: { id: string; filename: string; size: number; ready: boolean; error: string; file?: File | null }) {
  void uploadEntry(staged);
}

function removeStaged(id: string) {
  stagedUploads.value = stagedUploads.value.filter((item) => item.id !== id);
}

function markRemoval(attachmentId: string) {
  markedRemovals.value.push(attachmentId);
}

function unmarkRemoval(attachmentId: string) {
  markedRemovals.value = markedRemovals.value.filter((item) => item !== attachmentId);
}

async function downloadAttachment(attachment: EntityAttachment) {
  if (!props.card) return;
  try {
    const content = await parley.getAttachmentContent(props.card.id, attachment.id);
    const blob = new Blob([content.bytes], { type: content.contentType });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = content.filename;
    anchor.click();
    URL.revokeObjectURL(url);
  } catch (reason) {
    status.value = reason instanceof Error ? reason.message : 'Could not download the file.';
  }
}

function relationIntent() {
  if (!relation.enabled) return undefined;
  const base = { verb: relation.verb.trim(), basis: relation.basis.trim() };
  if (relation.mode === 'link') return { ...base, target_id: relation.targetId || null };
  if (relation.mode === 'create') {
    return {
      ...base,
      create: { title: relation.createTitle.trim(), subtype: relation.createSubtype, body: {}, notes_md: relation.createNotes },
    };
  }
  return undefined;
}

function freezeTitle(): string {
  return title.value.trim();
}

async function save() {
  if (saving.value) return;
  const trimmedTitle = freezeTitle();
  if (!trimmedTitle) { error.value = 'A card needs a title.'; return; }
  const payload: Record<string, unknown> = {};
  const notesMd = notes.value.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
  if (props.mode === 'create') {
    payload.create = { title: trimmedTitle, subtype: subtype.value, body: { ...body }, notes_md: notesMd, placement: placementIntent.value };
  } else {
    const target = liveCard.value;
    if (!target) return;
    payload.edit = {
      resource_id: target.id,
      expected_revision: target.revision,
      expected_notes_rev: target.notes.revision,
      title: trimmedTitle,
      subtype: subtype.value,
      body: { ...body },
      notes_md: notesMd,
      staged_uploads: stagedUploads.value.filter((item) => item.ready).map((item) => item.id),
      unbind_ids: markedRemovals.value,
    };
    if (placementIntent.value) payload.placement = placementIntent.value;
  }
  const relation = relationIntent();
  if (relation) payload.relation_add = relation;
  const key = newOperationKey();
  saving.value = true;
  error.value = '';
  status.value = 'Saving card…';
  try {
    const result = await parley.commitResourceCard(payload as never, key);
    conflict.value = null;
    dirty.value = false;
    status.value = 'Card saved.';
    emit('saved', { entity: result.entity, placement: result.placement, createdRelated: result.created_related });
  } catch (reason) {
    if (reason instanceof ApiError && reason.status === 412) {
      const base = { title: props.card?.title ?? '', notes: props.card?.notes?.markdown ?? '' };
      const mine = { title: trimmedTitle, notes: notesMd };
      conflict.value = { base, mine, current: null, currentCard: null };
      status.value = 'This card changed in another session. Compare base, yours and current before retrying.';
      if (liveCard.value?.id) {
        try {
          const latest = await parley.getEntityCard(liveCard.value.id);
          conflict.value.current = { title: latest.title, notes: latest.notes.markdown };
          conflict.value.currentCard = latest;
        } catch { /* keep partial conflict view */ }
      }
    } else if (reason instanceof TypeError) {
      status.value = 'Save status unknown. Your edits are still here; retry the same save to reconcile.';
    } else {
      error.value = reason instanceof Error ? reason.message : 'Save failed. Your edits are still here.';
    }
  } finally {
    saving.value = false;
  }
}

function useCurrentVersion() {
  const current = conflict.value?.currentCard;
  if (!current) return;
  liveCard.value = current;
  conflict.value = null;
  refreshModel();
  dirty.value = false;
  status.value = 'Loaded the current version. Review and save when ready.';
}

function keepMineAndRetry() {
  const current = conflict.value?.currentCard;
  if (!current) return;
  // Adopt the current revisions but keep this draft's fields.
  liveCard.value = current;
  conflict.value = null;
  status.value = 'Your edits are kept against the latest revision. Save again to submit.';
}

function cancel() {
  emit('cancel');
}
</script>

<template>
  <form
    class="resource-editor-form"
    aria-label="Resource card editor"
    @submit.prevent="save"
  >
    <header class="pane-bar">
      <h3>{{ mode === 'create' ? 'New card' : `Edit card · ${card?.title ?? ''}` }}</h3>
      <span
        v-if="saving"
        role="status"
      >Saving…</span>
    </header>

    <p
      v-if="status"
      role="status"
      class="form-status"
    >
      {{ status }}
    </p>
    <p
      v-if="error"
      role="alert"
      class="form-error"
    >
      {{ error }}
    </p>

    <div
      v-if="conflict"
      class="conflict-box"
      role="alert"
    >
      <strong>Conflict: this card changed in another session</strong>
      <dl class="conflict-versions">
        <div><dt>Base (saved before your edit)</dt><dd>{{ conflict.base.title }}</dd></div>
        <div><dt>Yours (unsaved draft)</dt><dd>{{ conflict.mine.title }}</dd></div>
        <div><dt>Current (in workspace)</dt><dd>{{ conflict.current?.title ?? 'unknown' }}</dd></div>
      </dl>
      <div class="conflict-actions">
        <button
          type="button"
          :disabled="saving"
          @click="keepMineAndRetry"
        >
          Keep my edits and retry
        </button>
        <button
          type="button"
          :disabled="saving || !conflict.currentCard"
          @click="useCurrentVersion"
        >
          Use current version
        </button>
      </div>
    </div>

    <div class="editor-fields">
      <label>
        Title
        <input
          v-model="title"
          required
          maxlength="240"
          autocomplete="off"
          :disabled="saving"
        >
      </label>
      <label>
        Type
        <select
          v-model="subtype"
          :disabled="saving"
        >
          <option
            v-for="kind in SUBTYPES"
            :key="kind"
            :value="kind"
          >{{ subtypeLabel(kind) }}</option>
        </select>
      </label>

      <template v-if="subtype === 'proof'">
        <label>Result<input
          v-model="body.result"
          :disabled="saving"
          autocomplete="off"
        ></label>
        <label>Context<input
          v-model="body.context"
          :disabled="saving"
          autocomplete="off"
        ></label>
        <label>Time<input
          v-model="body.time"
          :disabled="saving"
          autocomplete="off"
        ></label>
        <label>Owner role<input
          v-model="body.owner_role"
          :disabled="saving"
          autocomplete="off"
        ></label>
      </template>
      <template v-else-if="subtype === 'gift'">
        <label>Effort estimate<input
          v-model="body.effort"
          :disabled="saving"
          autocomplete="off"
        ></label>
        <label>Availability<select
          v-model="body.delivery"
          :disabled="saving"
        ><option value="on_request">I can make this on request</option><option value="exists">It already exists</option></select></label>
      </template>
      <template v-else-if="subtype === 'artifact'">
        <label>Available reference<input
          v-model="body.reference"
          :disabled="saving"
          autocomplete="off"
        ></label>
        <label class="check-row"><input
          v-model="body.share_permission"
          type="checkbox"
          :disabled="saving"
        > Permission to share</label>
      </template>
      <p
        v-if="typedGaps.length"
        class="gap-note"
      >
        Missing typed fields: {{ typedGaps.join(', ') }}. Later labels or prose do not satisfy them.
      </p>

      <div class="notes-block">
        <h4>Owner notes · Markdown</h4>
        <MarkdownEditor
          v-model="notes"
          label="Owner notes"
          :readonly="saving || !!conflict"
        />
        <details>
          <summary>Preview Markdown</summary><div class="notes-preview">
            <MarkdownPreview :source="notes" />
          </div>
        </details>
      </div>

      <div class="files-block">
        <h4>Files</h4>
        <ul
          v-if="attachments.length"
          class="file-list"
          aria-label="Bound files"
        >
          <li
            v-for="attachment in attachments"
            :key="attachment.id"
          >
            <span>{{ attachment.filename }} · {{ Math.ceil(attachment.size / 1024) }} KB · {{ attachment.share_permission ? 'shareable' : 'private' }}</span>
            <button
              type="button"
              :disabled="saving"
              @click="downloadAttachment(attachment)"
            >
              Download
            </button>
            <button
              v-if="!markedRemovals.includes(attachment.id)"
              type="button"
              :disabled="saving"
              @click="markRemoval(attachment.id)"
            >
              Remove
            </button>
            <button
              v-else
              type="button"
              :disabled="saving"
              @click="unmarkRemoval(attachment.id)"
            >
              Keep
            </button>
          </li>
        </ul>
        <ul
          v-if="stagedUploads.length"
          class="file-list"
          aria-label="Staged uploads"
        >
          <li
            v-for="staged in stagedUploads"
            :key="staged.id"
          >
            <span>{{ staged.filename }} · {{ Math.ceil(staged.size / 1024) }} KB · {{ staged.ready ? 'staged' : 'uploading…' }}</span>
            <button
              type="button"
              :disabled="saving || staged.ready"
              @click="removeStaged(staged.id)"
            >
              Drop
            </button>
            <button
              v-if="staged.error"
              type="button"
              :disabled="saving"
              @click="retryUpload(staged)"
            >
              Retry
            </button>
            <span
              v-if="staged.error"
              class="upload-error"
            >{{ staged.error }}</span>
          </li>
        </ul>
        <label class="file-add">Add files<input
          type="file"
          multiple
          :disabled="saving"
          @change="addFiles"
        ></label>
        <p class="file-note">
          An upload alone binds nothing; Save commits the bindings. Cancel leaves no visible binding.
        </p>
      </div>

      <label>
        Resource board column
        <select
          v-model="placementColumn"
          :disabled="saving"
        >
          <option value="__off__">Off board</option>
          <option
            v-for="column in columns"
            :key="column.id"
            :value="column.id"
          >{{ column.title }}</option>
        </select>
      </label>

      <details
        v-if="graphMode"
        class="relation-box"
      >
        <summary>Add relationship …</summary>
        <label class="check-row"><input
          v-model="relation.enabled"
          type="checkbox"
          :disabled="saving"
        > Include a relationship in this save</label>
        <template v-if="relation.enabled">
          <label>From this card<output class="verb-preview">{{ card?.title ?? 'New card' }} → {{ relation.verb || '…' }} → {{ relation.mode === 'link' ? (availableEndpoints?.find((item) => item.id === relation.targetId)?.label ?? 'choose target') : relation.mode === 'create' ? (relation.createTitle || 'new card') : '…' }}</output></label>
          <label>Verb<input
            v-model="relation.verb"
            maxlength="120"
            :disabled="saving"
            autocomplete="off"
          ></label>
          <label>Basis<input
            v-model="relation.basis"
            maxlength="2000"
            :disabled="saving"
            autocomplete="off"
          ></label>
          <label>Link existing<select
            v-model="relation.mode"
            :disabled="saving"
          ><option value="link">Link an existing card</option><option value="create">Create a related card</option></select></label>
          <template v-if="relation.mode === 'link'">
            <label>Search cards<input
              v-model="relation.targetQuery"
              :disabled="saving"
              autocomplete="off"
              placeholder="Search resources and parties…"
            ></label>
            <select
              v-model="relation.targetId"
              :disabled="saving"
            >
              <option value="">
                Choose a target
              </option>
              <option
                v-for="item in filteredEndpoints"
                :key="item.id"
                :value="item.id"
              >
                {{ item.label }} · {{ item.kind }}
              </option>
            </select>
          </template>
          <template v-else>
            <label>New card title<input
              v-model="relation.createTitle"
              maxlength="240"
              :disabled="saving"
              autocomplete="off"
            ></label>
            <label>New card type<select
              v-model="relation.createSubtype"
              :disabled="saving"
            ><option
              v-for="kind in SUBTYPES"
              :key="kind"
              :value="kind"
            >{{ subtypeLabel(kind) }}</option></select></label>
            <label>Notes<textarea
              v-model="relation.createNotes"
              rows="2"
              :disabled="saving"
            /></label>
            <p class="file-note">
              The related card is created off board. You can place it later from the picker.
            </p>
          </template>
        </template>
      </details>
    </div>

    <footer class="editor-actions">
      <button
        type="submit"
        class="primary"
        :disabled="saving || !!conflict"
      >
        Save card
      </button>
      <button
        type="button"
        :disabled="saving"
        @click="cancel"
      >
        Cancel
      </button>
      <span
        v-if="dirty && !saving"
        class="dirty-note"
      >Unsaved edits are kept in this draft.</span>
    </footer>
  </form>
</template>

<style scoped>
.resource-editor-form { background: white; border: 1px solid #ccd5cc; border-radius: .55rem; display: flex; flex-direction: column; min-width: 0; }
.resource-editor-form .pane-bar { border-bottom: 1px solid #dce2db; }
.resource-editor-form h3 { margin: 0; }
.editor-fields { padding: .7rem .9rem; overflow: auto; }
.notes-block, .files-block { border-top: 1px solid #e3e9e2; margin-top: .6rem; padding-top: .4rem; }
.notes-block h4, .files-block h4 { margin: .4rem 0; }
.notes-preview { border: 1px solid #e0e5df; border-radius: .35rem; padding: 0 .6rem; max-height: 14rem; overflow: auto; }
.file-list { list-style: none; padding: 0; }
.file-list li { display: flex; gap: .4rem; align-items: center; flex-wrap: wrap; border-top: 1px solid #edf0ec; padding: .3rem 0; }
.file-list span { flex: 1; min-width: 8rem; }
.file-add { border: 1px dashed #aebdb2; border-radius: .35rem; padding: .4rem .6rem; }
.file-note, .gap-note, .dirty-note, .form-status { color: #58665f; font-size: .85rem; }
.form-error { color: #852521; }
.upload-error { color: #852521; }
.conflict-box { background: #fdf1ef; border: 1px solid #e2b7b0; border-radius: .35rem; margin: .6rem .9rem; padding: .5rem .7rem; }
.conflict-versions { margin: .4rem 0; display: grid; gap: .25rem; }
.conflict-versions > div { display: grid; grid-template-columns: 12rem 1fr; gap: .5rem; }
.conflict-versions dt { color: #852521; font-weight: 600; font-size: .85rem; }
.conflict-versions dd { margin: 0; }
.conflict-actions { display: flex; gap: .4rem; flex-wrap: wrap; }
.check-row { display: flex; align-items: center; gap: .4rem; }
.relation-box { border-top: 1px solid #dce2db; margin-top: .6rem; padding-top: .4rem; }
.verb-preview { font-family: ui-monospace, monospace; font-size: .85rem; }
.editor-actions { display: flex; gap: .5rem; align-items: center; padding: .7rem .9rem; border-top: 1px solid #dce2db; }
.editor-actions .dirty-note { margin-left: auto; }
</style>
