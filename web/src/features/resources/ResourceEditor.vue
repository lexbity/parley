<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue';
import { VueFlow, Background, type Node } from '@xyflow/vue';
import { ApiError, newOperationKey, parley, type Resource } from '../../api/client';

type ResourceKind = Resource['kind'];
const props = defineProps<{ routeId?: string | null }>();
const emit = defineEmits<{ navigate: [path: string] }>();
const resources = ref<Resource[]>([]);
const editorOpen = ref(false);
const kindFilter = ref('');
const shown = computed(() => resources.value.filter(item => !kindFilter.value || item.kind === kindFilter.value));
const nodes = computed<Node[]>(() => shown.value.map((item, index) => ({
  id: item.id, position: { x: (index % 3) * 230, y: Math.floor(index / 3) * 130 },
  data: { label: item.title || 'Untitled draft', kind: item.kind },
  selected: item.id === selectedId.value, draggable: false, connectable: false,
})));
function closeEditor() {
  if (pending.value || saving.value || (dirty.value && !globalThis.confirm('Discard unsaved changes?'))) return;
  editorOpen.value = false;
  selectedId.value = null;
  dirty.value = false;
  emit('navigate', '/resources');
}
const selectedId = ref<string | null>(null);
const kind = ref<ResourceKind>('gift');
const title = ref('');
const description = ref('');
const delivery = ref<'exists' | 'on_request'>('on_request');
const missing = ref<string[]>([]);
const status = ref('Connecting to the local workspace…');
const loading = ref(true);
const dirty = ref(false);
const saving = ref(false);
const pending = ref<{ key: string; base: Resource | null; input: { kind: ResourceKind; title: string; body: Record<string, unknown> } } | null>(null);
const conflict = ref<{
  base: Resource;
  current: Resource;
  submitted: { title: string; body: Record<string, unknown> };
  local: { title: string; body: Record<string, unknown> };
} | null>(null);
const selected = computed(() => resources.value.find((item) => item.id === selectedId.value) ?? null);
const workspaceQuarantined = ref(false);
const quarantinedTitle = ref('');
const quarantinedDescription = ref('');
const quarantineStatus = ref('');

function bodyValue(): Record<string, unknown> {
  return { description: description.value, delivery: delivery.value };
}

function validate(): string[] {
  const errors: string[] = [];
  if (!title.value.trim()) errors.push('Name');
  if (kind.value === 'gift' && delivery.value === 'on_request' && !bodyValue().description) errors.push('What you can offer');
  return errors;
}

async function refresh(): Promise<void> {
  try {
    resources.value = await parley.listResources();
    if (selectedId.value && !resources.value.some((item) => item.id === selectedId.value)) selectedId.value = null;
    status.value = 'Workspace ready. Incomplete resources can be saved as drafts.';
  } catch (error) {
    status.value = error instanceof Error ? error.message : 'Could not load the local workspace.';
  } finally {
    loading.value = false;
  }
}

function newResource(): void {
  if (workspaceQuarantined.value) return;
  if (dirty.value && !globalThis.confirm('Discard this unsaved resource draft and start another?')) return;
  editorOpen.value = true;
  selectedId.value = null;
  kind.value = 'gift';
  title.value = '';
  description.value = '';
  delivery.value = 'on_request';
  missing.value = [];
  dirty.value = false;
  conflict.value = null;
  pending.value = null;
  status.value = 'New resource. Save a draft now, or complete the fields and mark it ready later.';
  emit('navigate', '/resources');
}

function edit(resource: Resource, reviewedReplacement = false): void {
  if (workspaceQuarantined.value) return;
  if (pending.value) return;
  if (dirty.value && !reviewedReplacement && !globalThis.confirm('Discard this unsaved resource edit?')) return;
  editorOpen.value = true;
  selectedId.value = resource.id;
  kind.value = resource.kind;
  title.value = resource.title;
  description.value = String(resource.body.description ?? '');
  delivery.value = resource.body.delivery === 'exists' ? 'exists' : 'on_request';
  missing.value = [];
  dirty.value = false;
  conflict.value = null;
  emit('navigate', `/resources/${encodeURIComponent(resource.id)}`);
}

function openRoutedResource(): void {
  if (!props.routeId || loading.value) return;
  const resource = resources.value.find((item) => item.id === props.routeId);
  if (!resource || selectedId.value === resource.id) return;
  if (dirty.value && !globalThis.confirm('Keep the route change and discard this unsaved resource edit?')) {
    emit('navigate', selectedId.value ? `/resources/${encodeURIComponent(selectedId.value)}` : '/resources');
    return;
  }
  edit(resource, true);
}

function markDirty(): void {
  dirty.value = true;
  missing.value = [];
}

function reapplyConflict(): void {
  if (!conflict.value) return;
  selectedId.value = conflict.value.current.id;
  conflict.value = null;
  dirty.value = true;
  status.value = 'Your current edits are ready for review against the latest saved revision.';
}

async function save(): Promise<void> {
  if (workspaceQuarantined.value) return;
  missing.value = validate();
  if (missing.value.length) {
    status.value = `Draft has missing fields: ${missing.value.join(', ')}.`;
  }
  const base = selected.value;
  const frozen = { kind: kind.value, title: title.value, body: bodyValue() };
  const intent = pending.value ?? { key: newOperationKey(), base, input: frozen };
  pending.value = intent;
  saving.value = true;
  conflict.value = null;
  status.value = 'Saving…';
  try {
    const saved = intent.base
      ? await parley.updateResource(intent.base, { title: intent.input.title, body: intent.input.body }, intent.key)
      : await parley.createResource(intent.input, intent.key);
    pending.value = null;
    resources.value = base
      ? resources.value.map((item) => item.id === saved.id ? saved : item)
      : [saved, ...resources.value];
    selectedId.value = saved.id;
    if (title.value === intent.input.title && kind.value === intent.input.kind && description.value === intent.input.body.description) dirty.value = false;
    status.value = dirty.value
      ? 'Earlier revision saved. Your newer edits are still unsaved.'
      : missing.value.length
      ? `Draft saved. Still missing: ${missing.value.join(', ')}.`
      : saved.readiness === 'draft' ? 'Draft saved to this workspace.' : 'Resource saved.';
  } catch (error) {
    if (error instanceof ApiError && error.status === 412 && intent.base) {
      pending.value = null;
      const latest = await parley.listResources().then((all) => all.find((item) => item.id === intent.base?.id));
      if (latest) {
        resources.value = resources.value.map((item) => item.id === latest.id ? latest : item);
        conflict.value = {
          base: intent.base,
          current: latest,
          submitted: { title: intent.input.title, body: intent.input.body },
          local: { title: title.value, body: bodyValue() },
        };
        status.value = 'This resource changed in another session. Compare both versions before applying your edit.';
      }
    } else if (error instanceof ApiError && (error.status === 401 || (error.status === 409 && error.code === 'WORKSPACE_REPLACED'))) {
      // The workspace epoch changed underneath this tab: the draft cannot be
      // saved into the restored workspace and must be quarantined.
      pending.value = null;
      quarantinedTitle.value = intent.input.title;
      quarantinedDescription.value = String(intent.input.body.description ?? '');
      workspaceQuarantined.value = true;
      quarantineStatus.value = 'The workspace epoch changed while saving; this draft is isolated and cannot be saved into the current workspace.';
      globalThis.dispatchEvent(new globalThis.CustomEvent('parley:workspace-quarantined'));
    } else if (error instanceof TypeError) {
      status.value = 'Save status unknown. Your text is still here; retry the same save to reconcile it.';
    } else {
      status.value = error instanceof Error ? error.message : 'Save failed. Your text is still here.';
    }
  } finally {
    saving.value = false;
  }
}

async function reload(): Promise<void> {
  if (workspaceQuarantined.value) return;
  if (dirty.value && !globalThis.confirm('Reloading can discard unacknowledged edits. Continue?')) return;
  dirty.value = false;
  await refresh();
}

async function onWorkspaceChanged(event: unknown): Promise<void> {
  const detailEvent = event as { detail?: { operation?: string } };
  if (!['restore', 'erase_workspace', 'erase_party'].includes(detailEvent.detail?.operation ?? '')) return;
  if (dirty.value || pending.value) {
    // Never retry a save into a replaced workspace; isolate the draft.
    pending.value = null;
    quarantinedTitle.value = title.value;
    quarantinedDescription.value = description.value;
    workspaceQuarantined.value = true;
    quarantineStatus.value = 'This workspace was replaced. The draft buffer is isolated and cannot be saved into the restored workspace.';
    globalThis.dispatchEvent(new globalThis.CustomEvent('parley:workspace-quarantined'));
    return;
  }
  await refresh();
}

async function copyQuarantined(): Promise<void> {
  const text = `Title: ${quarantinedTitle.value}\n\nDescription: ${quarantinedDescription.value}`;
  try {
    await globalThis.navigator.clipboard.writeText(text);
    quarantineStatus.value = 'Unsaved draft copied. It remains quarantined until you discard it or close this workspace.';
  } catch {
    quarantineStatus.value = 'Clipboard access was denied. Select and copy the read-only text manually; it remains quarantined.';
  }
}

async function discardQuarantined(): Promise<void> {
  workspaceQuarantined.value = false;
  quarantineStatus.value = '';
  dirty.value = false;
  pending.value = null;
  conflict.value = null;
  editorOpen.value = true;
  selectedId.value = null;
  kind.value = 'gift';
  title.value = '';
  description.value = '';
  delivery.value = 'on_request';
  await parley.refreshSession();
  await refresh();
  globalThis.dispatchEvent(new globalThis.CustomEvent('parley:workspace-reloaded'));
}

onMounted(async () => {
  globalThis.addEventListener('beforeunload', warnAboutDirtyDraft);
  globalThis.addEventListener('parley:workspace-changed', onWorkspaceChanged);
  try {
    await parley.connect();
    await refresh();
    openRoutedResource();
  } catch (error) {
    loading.value = false;
    status.value = error instanceof Error ? error.message : 'The local workspace is unavailable.';
  }
});
watch(() => props.routeId, () => openRoutedResource());

onUnmounted(() => {
  globalThis.removeEventListener('beforeunload', warnAboutDirtyDraft);
  globalThis.removeEventListener('parley:workspace-changed', onWorkspaceChanged);
});

function warnAboutDirtyDraft(event: { preventDefault: () => void }): void {
  if (!dirty.value && !pending.value) return;
  event.preventDefault();
  (event as unknown as { returnValue: string }).returnValue = '';
}
defineExpose({ refresh });
</script>

<template>
  <section
    class="resources"
    aria-label="Resources workspace"
  >
    <section
      v-if="workspaceQuarantined"
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="resource-quarantine-heading"
      class="quarantine-dialog"
    >
      <h2 id="resource-quarantine-heading">
        Unsaved resource draft quarantined
      </h2>
      <p>The workspace changed while this draft was unsaved. It is not being replayed into the restored data.</p>
      <label>Copy out the isolated draft<textarea
        readonly
        aria-label="Quarantined draft text"
        :value="quarantinedTitle + '\n\n' + quarantinedDescription"
        rows="8"
      /></label>
      <button
        type="button"
        @click="copyQuarantined"
      >
        Copy quarantined draft
      </button>
      <button
        type="button"
        @click="discardQuarantined"
      >
        Discard local draft and load current workspace
      </button>
      <p
        v-if="quarantineStatus"
        role="status"
      >
        {{ quarantineStatus }}
      </p>
    </section>
    <div class="pane-bar">
      <button
        class="primary"
        :disabled="!!pending"
        @click="newResource"
      >
        Add resource
      </button>
    </div>
    <header class="pane-bar">
      <h2>Resource network</h2><select
        v-model="kindFilter"
        aria-label="Resource type filter"
      >
        <option value="">
          All types
        </option><option value="gift">
          Offers
        </option><option value="proof">
          Proofs
        </option><option value="artifact">
          Artifacts
        </option>
      </select>
    </header>
    <div class="resource-canvas">
      <VueFlow
        v-if="nodes.length"
        :nodes="nodes"
        :nodes-connectable="false"
        :delete-key-code="null"
        fit-view-on-init
      >
        <template #node-default="{ id, data }">
          <button
            class="resource-node"
            @click="edit(resources.find(item => item.id === id)!)"
          >
            <small>{{ data.kind }}</small><strong>{{ data.label }}</strong>
          </button>
        </template>
        <Background />
      </VueFlow>
      <p
        v-else
        class="canvas-empty"
      >
        {{ loading ? 'Loading resources…' : resources.length ? 'No resources match this type.' : 'Add your first resource' }}
      </p>
    </div>
    <details class="resource-directory">
      <summary>Saved resources · {{ resources.length }}</summary><button @click="reload">
        Reload
      </button><ul aria-label="Saved resources">
        <li
          v-for="item in shown"
          :key="item.id"
        >
          <button @click="edit(item)">
            {{ item.title || 'Untitled draft' }} · {{ item.kind }}
          </button>
        </li>
      </ul>
    </details>
    <form
      v-show="editorOpen"
      class="resource-form"
      @submit.prevent="save"
    >
      <button
        class="close-editor"
        type="button"
        :disabled="saving || !!pending"
        @click="closeEditor"
      >
        Close
      </button>
      <h2>{{ selected ? 'Edit resource' : 'Add a resource' }}</h2>
      <label>
        Resource type
        <select
          v-model="kind"
          :disabled="!!selected"
          @change="markDirty"
        >
          <option value="gift">Gift or offer</option>
          <option value="proof">Proof or result</option>
          <option value="artifact">Artifact</option>
        </select>
      </label>
      <label>
        Name
        <input
          v-model="title"
          autocomplete="off"
          @input="markDirty"
        >
        <span
          v-if="missing.includes('Name')"
          class="field-error"
        >Add a name or save as an incomplete draft.</span>
      </label>
      <label>
        What is it?
        <textarea
          v-model="description"
          rows="4"
          @input="markDirty"
        />
        <span
          v-if="missing.includes('What you can offer')"
          class="field-error"
        >Describe the offer before it can be ready.</span>
      </label>
      <label v-if="kind === 'gift'">
        Availability
        <select
          v-model="delivery"
          @change="markDirty"
        >
          <option value="on_request">I can make this on request</option>
          <option value="exists">It already exists</option>
        </select>
      </label>
      <p
        v-if="selected"
        class="readiness"
      >
        Status: {{ selected.readiness }} · revision {{ selected.revision }}
      </p>
      <div class="resource-actions">
        <button
          type="submit"
          :disabled="saving || (!dirty && !!selected)"
        >
          {{ saving ? 'Saving…' : pending ? 'Retry same save' : 'Save draft' }}
        </button>
        <button
          v-if="selected"
          type="button"
          :disabled="!!pending"
          @click="edit(selected)"
        >
          Cancel edits
        </button>
      </div>
      <p
        v-if="dirty"
        class="dirty-state"
      >
        Unsaved edits are kept in this tab.
      </p>
      <div
        v-if="conflict"
        class="conflict"
        role="alert"
      >
        <h3>Review the conflict</h3>
        <p><strong>Saved before:</strong> {{ conflict.base.title }} — {{ conflict.base.body.description }}</p>
        <p><strong>Current in workspace:</strong> {{ conflict.current.title }} — {{ conflict.current.body.description }}</p>
        <p><strong>Submitted edit:</strong> {{ conflict.submitted.title }} — {{ conflict.submitted.body.description }}</p>
        <p><strong>Current unsaved buffer:</strong> {{ conflict.local.title }} — {{ conflict.local.body.description }}</p>
        <button
          type="button"
          @click="edit(conflict.current, true)"
        >
          Use current version
        </button>
        <button
          type="button"
          @click="reapplyConflict"
        >
          Keep my current edits and review against latest
        </button>
      </div>
    </form>
    <p
      class="status"
      role="status"
      aria-live="polite"
    >
      {{ status }}
    </p>
  </section>
</template>

<style scoped>
.resources { position: relative; padding: 0; overflow: hidden; }
.resource-canvas { position: relative; height: clamp(25rem, 62vh, 48rem); background: radial-gradient(#bbcdc0 .8px, transparent .8px) #eff3ed; background-size: 18px 18px; }
.canvas-empty { position:absolute; inset:46% 1rem auto; text-align:center; color:#526657; }
.resource-node { border:0; background:transparent; width:100%; display:grid; gap:.2rem; }
.resource-form { position:absolute; z-index:4; top:3.4rem; right:.7rem; width:min(24rem,95%); max-height:80%; overflow:auto; box-shadow:0 6px 25px #24362d30; }
.close-editor { float:right; }
.status { padding:.6rem 1rem; margin:0; }
.field-error {color:#852521}
.quarantine-dialog { position:fixed; z-index:1000; inset:10vh max(1rem,calc((100vw - 42rem)/2)); background:white; padding:1rem; overflow:auto; }
</style>
