<script setup lang="ts">
/* global HTMLDetailsElement, Event, HTMLInputElement, HTMLSelectElement, Blob, URL, document, File, navigator */
import { computed, nextTick, reactive, ref, watch } from 'vue';
import { ApiError, newOperationKey, parley, type EntityAttachment, type EntityCard, type PlacementView, type ResourceColumn, type ResourcePlacementInput } from '../../api/client';
import type { components } from '../../api/generated';
import MarkdownEditor from '../../shared/MarkdownEditor.vue';
import MarkdownPreview from '../../shared/MarkdownPreview.vue';
import ResourceTypedFields from './ResourceTypedFields.vue';
import type { PendingRelationship } from './ResourceGraph.vue';

/**
 * Shared content editor for board and graph cards (amendment §4). One draft
 * owns title, subtype, typed body fields, Markdown notes, file changes and
 * placement. Explicit Save card commits everything atomically; Cancel drops
 * uncommitted changes. In a graph node the form additionally offers Add
 * relationship / Link existing / Create related card.
 */

const SUBTYPES = ['gift', 'proof', 'artifact', 'skill', 'interest', 'note'] as const;
export type ResourceSubtype = (typeof SUBTYPES)[number];

/** Owner-authored resource relationship verbs (prototype set). Delivery-
 * asserting verbs are deliberately absent: recorded sends, not edges,
 * establish that something was shared. */
const RELATIONSHIP_VERBS = ['includes', 'supports', 'relates to', 'considered for'] as const;

const props = defineProps<{
  columns: ResourceColumn[];
  mode: 'create' | 'edit';
  card?: EntityCard | null;
  cardSummary?: { resource_id: string; title: string; subtype: string; readiness: string; placement: PlacementView | null; etag: string } | null;
  initialColumnId?: string | null;
  initialTitle?: string;
  graphMode?: boolean;
  endpointSearchError?: string;
  availableEndpoints?: Array<{ id: string; label: string; kind: string; revision?: number }>;
}>();

const emit = defineEmits<{
  saved: [result: { entity: EntityCard; placement: PlacementView | null; createdRelated: { edge_id: string; to_id: string } | null }];
  'update:dirty': [dirty: boolean];
  cancel: [];
  'search-endpoints': [query: string];
  'preview-relationship': [preview: PendingRelationship | null];
}>();

const title = ref('');
const relationshipSection = ref<HTMLDetailsElement | null>(null);
const relationshipTarget = ref<HTMLSelectElement | null>(null);
const subtype = ref<ResourceSubtype>('artifact');
const body = ref<Record<string, unknown>>({});
const relatedBody = ref<Record<string, unknown>>({});
const notes = ref('');
const placementColumn = ref<string>('__off__');
const attachments = ref<EntityAttachment[]>([]);
type StagedFile = { id: string; filename: string; size: number; ready: boolean; percent: number; error: string; file: File; related: boolean };
const stagedUploads = ref<StagedFile[]>([]);
const cardUploads = computed(() => stagedUploads.value.filter(file => !file.related));
const relatedUploads = computed(() => stagedUploads.value.filter(file => file.related));
const markedRemovals = ref<string[]>([]);
const saving = ref(false);
const unknownOutcome = ref(false);
const draftEpoch = parley.workspaceEpoch;
let initializing = false;
let attachmentGeneration = 0;
const error = ref('');
const status = ref('');
type ConflictView = {
  base: { title: string; notes: string };
  mine: { title: string; notes: string };
  current: { title: string; notes: string } | null;
  currentCard: EntityCard | null;
};
const conflict = ref<ConflictView | null>(null);
const epochQuarantined = ref<{ title: string; notes: string } | null>(null);
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
  createSubtype: ResourceSubtype | 'person' | 'org';
  focus: string;
  createTitle: string;
  createNotes: string;
  placement: string;
  incoming: boolean;
}>({
  enabled: false, verb: 'includes', basis: '', mode: 'none', targetId: '', targetQuery: '',
  createSubtype: 'artifact', createTitle: '', createNotes: '', focus: '', placement: '', incoming: false,
});

const dirty = ref(false);
watch([title, subtype, notes, placementColumn, body, relatedBody, stagedUploads, markedRemovals, () => relation.mode, () => relation.verb, () => relation.basis, () => relation.targetId, () => relation.createTitle, () => relation.createSubtype, () => relation.createNotes, () => relation.focus, () => relation.incoming, () => relation.placement], () => {
  if (!initializing) dirty.value = true;
}, { deep: true, flush: 'sync' });
watch(dirty, (value) => emit('update:dirty', value), { flush: 'sync' });

const KIND_LABELS: Record<string, string> = {
  gift: 'Gift or offer', proof: 'Proof or result', artifact: 'Artifact', person: 'Person', org: 'Organization',
  skill: 'Skill', interest: 'Interest', note: 'Note',
};

function subtypeLabel(kind: string): string {
  return KIND_LABELS[kind] ?? kind;
}

const typedGaps = computed<string[]>(() => {
  const gaps: string[] = [];
  if (subtype.value === 'proof') {
    if (!String(body.value.result ?? '').trim()) gaps.push('Result');
    if (!String(body.value.context ?? '').trim()) gaps.push('Context');
    if (!String(body.value.time ?? '').trim()) gaps.push('Time');
    if (!String(body.value.owner_role ?? '').trim()) gaps.push('Owner role');
  } else if (subtype.value === 'gift') {
    if (!String(body.value.effort ?? '').trim()) gaps.push('Effort estimate');
  } else if (subtype.value === 'artifact') {
    if (!String(body.value.reference ?? '').trim()) gaps.push('Available reference');
    if (body.value.share_permission !== true) gaps.push('Permission to share');
  }
  return gaps;
});

const filteredEndpoints = computed(() => {
  const query = relation.targetQuery.trim().toLocaleLowerCase();
  const items = (props.availableEndpoints ?? []).filter(item => item.id !== props.card?.id && (!query || `${item.label} ${item.kind}`.toLocaleLowerCase().includes(query) || item.id === relation.targetId));
  return items.slice(0, 50);
});

watch(() => relation.targetQuery, (value, _old, clear) => {
  const timer = globalThis.setTimeout(() => emit('search-endpoints', value), 180);
  clear(() => globalThis.clearTimeout(timer));
});

const placementIntent = computed<ResourcePlacementInput | null>(() => {
  if (placementColumn.value === '__off__') return { column_id: null };
  return { column_id: placementColumn.value, before_resource_id: null };
});

// A lost acknowledgement must be retried with the exact same request and
// operation key (spec §6 Concurrency); a changed intent gets a fresh key.
// Declared before refreshModel, which the immediate card watcher invokes.
let lastIntent = '';
let intentKey = '';

function refreshModel() {
  const card = liveCard.value;
  initializing = true;
  const ownGeneration = ++attachmentGeneration;
  title.value = card?.title ?? props.initialTitle ?? '';
  subtype.value = (card?.subtype as ResourceSubtype) ?? 'artifact';
  body.value = { ...card?.body };
  relatedBody.value = {};
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
  relation.focus = '';
  relation.placement = '';
  relation.incoming = false;
  dirty.value = props.mode === 'create' && !!props.initialTitle?.trim();
  unknownOutcome.value = false;
  initializing = false;
  lastIntent = '';
  intentKey = '';
  if (props.mode === 'edit' && card) {
    // Bound files must be inspectable and removable (spec §4 Files). A late
    // response for a previous card never populates this draft.
    const cardId = card.id;
    void parley.listAttachments(cardId).then((rows) => {
      if (ownGeneration !== attachmentGeneration) return;
      attachments.value = rows;
    }).catch((reason: unknown) => {
      if (ownGeneration === attachmentGeneration) status.value = reason instanceof Error ? reason.message : 'Could not load the bound files; retry by reopening the card.';
    });
  }
}

watch(() => props.card, () => {
  if (props.mode === 'edit' && props.card) {
    liveCard.value = props.card;
    refreshModel();
  }
}, { immediate: true });
watch(() => props.mode, () => { if (props.mode === 'create') refreshModel(); }, { immediate: true });

async function uploadEntry(entry: StagedFile) {
  entry.error = '';
  entry.percent = 0;
  try {
    entry.id = await parley.stageEntityAttachment(entry.file, percent => { entry.percent = percent; });
    entry.ready = true;
  } catch (reason) {
    entry.error = reason instanceof Error ? reason.message : 'Upload failed.';
  }
}

async function addFiles(event: Event, related = false) {
  const input = event.target as HTMLInputElement;
  const entries = Array.from(input.files ?? [], file => {
    const entry = reactive({ id: newOperationKey(), filename: file.name, size: file.size, ready: false, percent: 0, error: '', file, related });
    stagedUploads.value.push(entry);
    return entry;
  });
  input.value = '';
  await Promise.all(entries.map(uploadEntry));
}

function retryUpload(staged: StagedFile) {
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

function relationIntent(): components['schemas']['RelationAdd'] | undefined {
  if (!relation.enabled) return;
  const base = { verb: relation.verb.trim(), basis: relation.basis.trim(), incoming: relation.incoming };
  if (!base.basis) throw new Error('Enter the basis for this relationship.');
  if (relation.mode === 'link') {
    const target = props.availableEndpoints?.find(item => item.id === relation.targetId);
    if (!target) throw new Error('Choose a card to link.');
    return { ...base, target_id: target.id, expected_target_revision: target.revision };
  }
  if (relation.mode === 'create' && relation.createTitle.trim()) {
    if (relation.createSubtype === 'person' || relation.createSubtype === 'org') {
      return { ...base, create_party: { kind: relation.createSubtype, name: relation.createTitle.trim(), focus: relation.focus.trim(), notes_md: relation.createNotes } };
    }
    return {
      ...base,
      create: {
        title: relation.createTitle.trim(), subtype: relation.createSubtype, body: { ...relatedBody.value }, notes_md: relation.createNotes,
        staged_uploads: relatedUploads.value.map(item => item.id),
        placement: relation.placement ? { column_id: relation.placement } : null,
      },
    };
  }
  throw new Error('Enter a title for the related card or choose an existing card.');
}

function quarantine() {
  epochQuarantined.value = { title: title.value, notes: notes.value };
  status.value = 'The workspace changed. Copy or discard this draft before editing the restored workspace.';
}

/** Commits the whole draft atomically. Resolves true only on a server
 * acknowledgement so the Save/Discard/Stay switch can react to failures. */
async function save(): Promise<boolean> {
  if (saving.value || epochQuarantined.value) return false;
  if (draftEpoch !== parley.workspaceEpoch) { quarantine(); return false; }
  const trimmedTitle = title.value.trim();
  if (!trimmedTitle) { error.value = 'A card needs a title.'; return false; }
  const includesRelatedFiles = relation.enabled && relation.mode === 'create' && relation.createSubtype !== 'person' && relation.createSubtype !== 'org';
  if (stagedUploads.value.some(file => (!file.related || includesRelatedFiles) && !file.ready)) {
    error.value = 'Wait for files to finish uploading. Retry or remove failed files before saving.';
    return false;
  }
  const staged = cardUploads.value.map(item => item.id);
  const payload: components['schemas']['ResourceCardCommitInput'] = {};
  const notesMd = notes.value.replace(/\r\n?/g, '\n');
  if (props.mode === 'create') {
    payload.create = { title: trimmedTitle, subtype: subtype.value, body: { ...body.value }, notes_md: notesMd, placement: placementIntent.value, staged_uploads: staged };
  } else {
    const target = liveCard.value;
    if (!target) return false;
    payload.edit = {
      resource_id: target.id,
      expected_revision: target.revision,
      expected_notes_rev: target.notes.revision,
      ...(trimmedTitle !== target.title ? { title: trimmedTitle } : {}),
      ...(subtype.value !== target.subtype ? { subtype: subtype.value } : {}),
      ...(JSON.stringify(body.value) !== JSON.stringify(target.body) ? { body: { ...body.value } } : {}),
      ...(notesMd !== target.notes.markdown ? { notes_md: notesMd } : {}),
      staged_uploads: staged,
      unbind_ids: [...markedRemovals.value],
    };
    const original = props.cardSummary?.placement;
    const destination = placementIntent.value?.column_id ?? null;
    if (destination !== (original?.column_id ?? null)) {
      payload.placement = placementIntent.value;
      payload.expected_placement_revision = original?.revision ?? 0;
      payload.expected_source_column_revision = props.columns.find(item => item.id === original?.column_id)?.revision ?? null;
      payload.expected_destination_column_revision = props.columns.find(item => item.id === destination)?.revision ?? null;
    }
  }
  try {
    const relationship = relationIntent();
    if (relationship) payload.relation_add = relationship;
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'Check the relationship details.';
    return false;
  }
  const serialized = JSON.stringify(payload);
  if (serialized !== lastIntent) { lastIntent = serialized; intentKey = newOperationKey(); }
  const key = intentKey;
  saving.value = true;
  error.value = '';
  status.value = 'Saving card…';
  try {
    const result = await parley.commitResourceCard(payload, key);
    conflict.value = null;
    dirty.value = false;
    unknownOutcome.value = false;
    status.value = 'Card saved.';
    emit('saved', { entity: result.entity, placement: result.placement, createdRelated: result.created_related });
    return true;
  } catch (reason) {
    if (reason instanceof ApiError && reason.status === 409 && reason.code === 'WORKSPACE_REPLACED') {
      quarantine();
    } else if (reason instanceof ApiError && reason.status === 412) {
      const base = { title: liveCard.value?.title ?? '', notes: liveCard.value?.notes.markdown ?? '' };
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
    } else if (reason instanceof TypeError || reason instanceof Error && reason.message.startsWith('Invalid response:')) {
      unknownOutcome.value = true;
      status.value = 'Save status unknown. Your edits are still here; retry the same save to reconcile.';
    } else {
      error.value = reason instanceof Error ? reason.message : 'Save failed. Your edits are still here.';
    }
    return false;
  } finally {
    saving.value = false;
  }
}

function focusTitle() { void nextTick(() => titleInput.value?.focus()); }
const titleInput = ref<HTMLInputElement | null>(null);
async function focusRelationship() {
  if (!relationshipSection.value) return;
  relationshipSection.value.open = true;
  relation.enabled = true;
  await nextTick();
  relationshipTarget.value?.focus();
}
defineExpose({ save, saving, focusTitle, focusRelationship, quarantine });

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
  const previous = liveCard.value;
  if (!current || !previous) return;
  if (title.value.trim() === previous.title) title.value = current.title;
  if (notes.value === previous.notes.markdown) notes.value = current.notes.markdown;
  if (subtype.value === previous.subtype) subtype.value = current.subtype as ResourceSubtype;
  const changedFields = Object.fromEntries(Object.entries(body.value).filter(([key, value]) => JSON.stringify(value) !== JSON.stringify(previous.body[key])));
  body.value = { ...current.body, ...changedFields };
  liveCard.value = current;
  conflict.value = null;
  status.value = 'Your changed fields are kept against the latest revision. Save again to submit.';
}

watch(() => [relation.enabled, relation.mode, relation.createTitle, relation.createSubtype, relation.verb, relation.incoming] as const,
  ([enabled, mode, child, kind, verb, incoming]) => {
    const parentId = props.card?.id;
    emit('preview-relationship', enabled && mode === 'create' && parentId && child.trim()
      ? { parentId, title: child.trim(), kind: subtypeLabel(kind), verb, incoming }
      : null);
  });
function changeType(event: Event, related = false) {
  const select = event.target as HTMLSelectElement;
  const fields = related ? relatedBody : body;
  const previous = related ? relation.createSubtype : subtype.value;
  if (Object.keys(fields.value).length && !globalThis.confirm('Changing type clears the typed details entered for this card. Continue?')) {
    select.value = previous;
    return;
  }
  fields.value = {};
  if (related) relation.createSubtype = select.value as typeof relation.createSubtype;
  else subtype.value = select.value as ResourceSubtype;
}

function cancel() {
  emit('cancel');
}

async function copyQuarantined() {
  const text = `${epochQuarantined.value?.title ?? ''}\n\n${epochQuarantined.value?.notes ?? ''}`;
  try {
    await navigator.clipboard.writeText(text);
    status.value = 'Quarantined draft copied. It remains isolated until you discard it.';
  } catch {
    status.value = 'Clipboard access was denied; select and copy the text manually.';
  }
}

function discardQuarantined() {
  epochQuarantined.value = null;
  dirty.value = false;
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
      v-if="epochQuarantined"
      class="quarantine-box"
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="quarantine-heading"
    >
      <strong id="quarantine-heading">Unsaved draft quarantined</strong>
      <p>The workspace was replaced while this card was unsaved. It is not replayed into the restored data.</p>
      <textarea
        readonly
        aria-label="Quarantined draft text"
        :value="`${epochQuarantined.title}\n\n${epochQuarantined.notes}`"
        rows="6"
      />
      <button
        type="button"
        @click="copyQuarantined"
      >
        Copy draft
      </button>
      <button
        type="button"
        @click="discardQuarantined"
      >
        Discard draft
      </button>
    </div>
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

    <div
      class="editor-fields"
      :inert="saving || unknownOutcome"
    >
      <label>
        Title
        <input
          ref="titleInput"
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
          :value="subtype"
          :disabled="saving || (mode === 'edit' && (Object.keys(card?.body ?? {}).length > 0 || card?.readiness === 'ready'))"
          @change="changeType($event)"
        >
          <option
            v-for="kind in SUBTYPES"
            :key="kind"
            :value="kind"
          >{{ subtypeLabel(kind) }}</option>
        </select>
      </label>

      <ResourceTypedFields
        v-model="body"
        :subtype="subtype"
        :disabled="saving"
      />
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
          v-if="cardUploads.length"
          class="file-list"
          aria-label="Staged uploads"
        >
          <li
            v-for="staged in cardUploads"
            :key="staged.id"
          >
            <span>{{ staged.filename }} · {{ Math.ceil(staged.size / 1024) }} KB · {{ staged.ready ? 'Ready to attach' : staged.error ? 'Upload failed' : `Uploading ${staged.percent}%` }}</span>
            <button
              type="button"
              :disabled="saving"
              @click="removeStaged(staged.id)"
            >
              Remove
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
          @change="addFiles($event)"
        ></label>
        <p class="file-note">
          Files attach when you save the card.
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
        ref="relationshipSection"
        class="relation-box"
      >
        <summary>Add relationship</summary>
        <label class="check-row"><input
          v-model="relation.enabled"
          type="checkbox"
          :disabled="saving"
        > Include a relationship in this save</label>
        <template v-if="relation.enabled">
          <label>Direction<select
            v-model="relation.incoming"
            :disabled="saving"
          >
            <option :value="false">From this card</option>
            <option :value="true">To this card</option>
          </select></label>
          <p class="verb-preview">
            {{ relation.incoming
              ? [relation.mode === 'link' ? availableEndpoints?.find(item => item.id === relation.targetId)?.label : relation.createTitle, relation.verb, title].map(part => part || '…').join(' ')
              : [title, relation.verb, relation.mode === 'link' ? availableEndpoints?.find(item => item.id === relation.targetId)?.label : relation.createTitle].map(part => part || '…').join(' ') }}
          </p>
          <label>Relationship verb<select
            v-model="relation.verb"
            :disabled="saving"
          ><option
            v-for="verb in RELATIONSHIP_VERBS"
            :key="verb"
            :value="verb"
          >{{ verb }}</option></select></label>
          <label>Basis<input
            v-model="relation.basis"
            maxlength="2000"
            :disabled="saving"
            autocomplete="off"
          ></label>
          <label>Relationship target<select
            ref="relationshipTarget"
            v-model="relation.mode"
            :disabled="saving"
          ><option
            value="none"
            disabled
          >Choose how to connect</option><option value="link">Link an existing card</option><option value="create">Create a related card</option></select></label>
          <template v-if="relation.mode === 'link'">
            <label>Search cards<input
              v-model="relation.targetQuery"
              :disabled="saving"
              autocomplete="off"
              placeholder="Search resources and parties…"
            ></label>
            <p
              v-if="endpointSearchError"
              role="alert"
            >
              {{ endpointSearchError }}
            </p>
            <label>Existing card<select
              v-model="relation.targetId"
              aria-label="Existing card"
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
            </select></label>
          </template>
          <template v-else-if="relation.mode === 'create'">
            <label>New card title<input
              v-model="relation.createTitle"
              maxlength="240"
              :disabled="saving"
              autocomplete="off"
            ></label>
            <label>New card type<select
              :value="relation.createSubtype"
              :disabled="saving"
              @change="changeType($event, true)"
            ><option
               v-for="kind in SUBTYPES"
               :key="kind"
               :value="kind"
             >{{ subtypeLabel(kind) }}</option>
              <option value="person">Person</option>
              <option value="org">Organization</option>
            </select></label>
            <label v-if="relation.createSubtype === 'person' || relation.createSubtype === 'org'">Focus<input
              v-model="relation.focus"
              maxlength="2000"
            ></label>
            <ResourceTypedFields
              v-model="relatedBody"
              :subtype="relation.createSubtype"
              :disabled="saving"
            />
            <div class="notes-block">
              <h4>Related card notes · Markdown</h4>
              <MarkdownEditor
                v-model="relation.createNotes"
                label="Related card notes"
                :readonly="saving"
                compact
              />
              <details v-if="relation.createNotes">
                <summary>Preview Markdown</summary>
                <MarkdownPreview :source="relation.createNotes" />
              </details>
            </div>
            <div v-if="relation.createSubtype !== 'person' && relation.createSubtype !== 'org'">
              <label>Add files to related card<input
                type="file"
                multiple
                :disabled="saving"
                @change="addFiles($event, true)"
              ></label>
              <ul
                v-if="relatedUploads.length"
                class="file-list"
                aria-label="Related card files"
              >
                <li
                  v-for="file in relatedUploads"
                  :key="file.id"
                >
                  <span>{{ file.filename }} · {{ file.ready ? 'Ready to attach' : file.error || `Uploading ${file.percent}%` }}</span>
                  <button
                    v-if="file.error"
                    type="button"
                    :disabled="saving"
                    @click="retryUpload(file)"
                  >
                    Retry
                  </button>
                  <button
                    type="button"
                    :disabled="saving"
                    @click="removeStaged(file.id)"
                  >
                    Remove
                  </button>
                </li>
              </ul>
            </div>
            <label v-if="relation.createSubtype !== 'person' && relation.createSubtype !== 'org'">Related card column<select
              v-model="relation.placement"
              :disabled="saving"
            >
              <option value="">Off board</option>
              <option
                v-for="column in columns"
                :key="column.id"
                :value="column.id"
              >{{ column.title }}</option>
            </select></label>
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
        {{ unknownOutcome ? 'Retry save' : 'Save card' }}
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
.quarantine-box { background: #fff8e6; border: 1px solid #dcc98f; border-radius: .35rem; margin: .6rem .9rem; padding: .5rem .7rem; display: grid; gap: .4rem; }
.quarantine-box textarea { font-family: ui-monospace, monospace; }
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
