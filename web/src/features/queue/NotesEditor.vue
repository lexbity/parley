<script setup lang="ts">
/* global setTimeout, clearTimeout, TextEncoder, HTMLDivElement, HTMLInputElement, Event, BeforeUnloadEvent */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { newOperationKey } from '../../api/client';
import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';
import { markdown as markdownLanguage } from '@codemirror/lang-markdown';
import { bracketMatching, defaultHighlightStyle, syntaxHighlighting } from '@codemirror/language';
import { EditorState } from '@codemirror/state';
import { crosshairCursor, dropCursor, EditorView, highlightActiveLine, highlightActiveLineGutter, keymap, lineNumbers, rectangularSelection } from '@codemirror/view';

defineOptions({ name: 'NotesEditor' });

export type NotesSnapshot = { opportunity_id: string; markdown: string; revision: number };
export type NotesSaveInput = { expected_revision: number; markdown: string };
export type TaskToggleInput = {
  expected_revision: number;
  start_byte: number;
  end_byte: number;
  line_hash: string;
  checked: boolean;
};
export interface NotesService {
  getNotes(id: string): Promise<NotesSnapshot>;
  saveNotes(id: string, input: NotesSaveInput, key: string): Promise<{ markdown: string; revision: number }>;
  toggleTask(id: string, input: TaskToggleInput, key: string): Promise<{ markdown: string; revision: number }>;
}
export type ParsedTask = {
  lineIndex: number;
  line: string;
  label: string;
  checked: boolean;
  ordered: boolean;
  depth: number;
  startByte: number;
  endByte: number;
  lineStartByte: number;
  lineEndByte: number;
};

type LocalDraft = { markdown: string; baseMarkdown: string; revision: number };
const localDrafts = new Map<string, LocalDraft>();

const props = defineProps<{ opportunityId: string; service: NotesService; workspaceEpoch: string }>();
const activeId = ref(props.opportunityId);
const activeEpoch = ref(props.workspaceEpoch);
const markdown = ref('');
const baseMarkdown = ref('');
const revision = ref(0);
const loading = ref(true);
const busy = ref(false);
const composing = ref(false);
const errorText = ref('');
const notice = ref('');
const conflict = ref<NotesSnapshot | null>(null);
const editorHost = ref<HTMLDivElement | null>(null);
let editorView: EditorView | null = null;
let debounce: ReturnType<typeof setTimeout> | undefined;
let loadGeneration = 0;
let sequence = 0;
let writing: Promise<void> | null = null;
let pendingSave: { key: string; input: NotesSaveInput; sequence: number; body: string } | null = null;

const dirty = computed(() => markdown.value !== baseMarkdown.value);
const tasks = computed(() => parseTasks(markdown.value));
const previewLines = computed(() => preview(markdown.value));
function draftKey(id: string, epoch = activeEpoch.value): string { return `${epoch}\u0000${id}`; }

function byteLength(value: string): number { return new TextEncoder().encode(value).length; }

function parseTasks(source: string): ParsedTask[] {
  const result: ParsedTask[] = [];
  let byteOffset = 0;
  let fenced: { marker: string; length: number } | null = null;
  const lines = source.split('\n');
  lines.forEach((line, lineIndex) => {
    const fence = /^\s{0,3}(`{3,}|~{3,})/.exec(line);
    if (fence) {
      const token = fence[1] ?? '';
      if (!fenced) fenced = { marker: token[0] ?? '`', length: token.length };
      else if (token[0] === fenced.marker && token.length >= fenced.length) fenced = null;
    } else if (!fenced) {
      const match = /^(\s*)(?:(?:[-+*])|(?:\d+[.)]))(\s+)\[([ xX])\](?=\s|$)(.*)$/.exec(line);
      if (match) {
        const prefix = `${match[1] ?? ''}${line.slice((match[1] ?? '').length, line.length).match(/^(?:[-+*]|\d+[.)])\s+/)?.[0] ?? ''}`;
        const markerChar = line.indexOf('[', prefix.length);
        if (markerChar >= 0) {
          const startByte = byteOffset + byteLength(line.slice(0, markerChar));
          result.push({
            lineIndex,
            line, label: (match[4] ?? '').trim(), checked: (match[3] ?? '').toLowerCase() === 'x',
            ordered: /^\s*\d/.test(line), depth: Math.floor((match[1] ?? '').replace(/\t/g, '  ').length / 2),
            startByte, endByte: startByte + 3, lineStartByte: byteOffset, lineEndByte: byteOffset + byteLength(line),
          });
        }
      }
    }
    byteOffset += byteLength(line) + (lineIndex < lines.length - 1 ? 1 : 0);
  });
  return result;
}

async function sha256Line(line: string): Promise<string> {
  if (!globalThis.crypto?.subtle) throw new Error('This browser cannot verify task source spans.');
  const digest = await globalThis.crypto.subtle.digest('SHA-256', new TextEncoder().encode(line));
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
}

type PreviewLine = { kind: 'text' | 'task' | 'fence' | 'code' | 'heading' | 'list' | 'quote'; text: string; checked?: boolean; ordered?: boolean; depth?: number; level?: number };

function preview(source: string): PreviewLine[] {
  const parsed = parseTasks(source);
  const taskByLine = new Map(parsed.map((task) => [task.lineIndex, task]));
  let fence: { marker: string; length: number } | null = null;
  return source.split('\n').map((line, lineIndex) => {
    const fenceMatch = /^\s{0,3}(`{3,}|~{3,})/.exec(line);
    if (fenceMatch) {
      const marker = fenceMatch[1] ?? '';
      if (!fence) fence = { marker: marker[0] ?? '`', length: marker.length };
      else if (marker[0] === fence.marker && marker.length >= fence.length) fence = null;
      return { kind: 'fence' as const, text: line };
    }
    if (fence) return { kind: 'code' as const, text: line };
    const task = taskByLine.get(lineIndex);
    if (task) return { kind: 'task' as const, text: task.label, checked: task.checked, ordered: task.ordered, depth: task.depth };
    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) return { kind: 'heading' as const, text: heading[2] ?? '', level: heading[1]?.length ?? 1 };
    const list = /^(\s*)(?:([-+*])|(\d+)[.)])\s+(.*)$/.exec(line);
    if (list) return { kind: 'list' as const, text: list[4] ?? '', ordered: !!list[3], depth: Math.floor((list[1] ?? '').replace(/\t/g, '  ').length / 2) };
    if (/^\s*>/.test(line)) return { kind: 'quote' as const, text: line.replace(/^\s*>\s?/, '') };
    return { kind: 'text' as const, text: line };
  });
}

function newKey(): string {
  return newOperationKey();
}

async function load(id: string): Promise<void> {
  const generation = ++loadGeneration;
  activeId.value = id;
  loading.value = true;
  errorText.value = '';
  conflict.value = null;
  try {
    const result = await props.service.getNotes(id);
    if (generation !== loadGeneration || result.opportunity_id !== id) return;
    const draft = localDrafts.get(draftKey(id));
    if (draft) {
      markdown.value = draft.markdown;
      baseMarkdown.value = draft.baseMarkdown;
      revision.value = draft.revision;
      if (draft.baseMarkdown !== result.markdown || draft.revision !== result.revision) conflict.value = result;
    } else {
      markdown.value = result.markdown;
      baseMarkdown.value = result.markdown;
      revision.value = result.revision;
    }
  } catch (error) {
    if (generation === loadGeneration) errorText.value = error instanceof Error ? error.message : 'Notes could not be loaded.';
  } finally {
    if (generation === loadGeneration) loading.value = false;
  }
}

watch(() => [props.opportunityId, props.workspaceEpoch] as const, ([id, epoch]) => {
  if (dirty.value) localDrafts.set(draftKey(activeId.value, activeEpoch.value), { markdown: markdown.value, baseMarkdown: baseMarkdown.value, revision: revision.value });
  activeEpoch.value = epoch;
  editorView?.destroy();
  editorView = null;
  void load(id);
});
watch([editorHost, loading], mountCodeMirror, { flush: 'post' });
watch(markdown, syncCodeMirror);
watch(markdown, () => {
  sequence += 1;
  if (loading.value) return;
  if (dirty.value) localDrafts.set(draftKey(activeId.value), { markdown: markdown.value, baseMarkdown: baseMarkdown.value, revision: revision.value });
  else localDrafts.delete(draftKey(activeId.value));
  notice.value = '';
  if (!dirty.value || composing.value || conflict.value || loading.value) return;
  clearTimeout(debounce);
  debounce = setTimeout(() => { void save(); }, 750);
});

function mountCodeMirror(): void {
  if (!editorHost.value || editorView || loading.value) return;
  editorView = new EditorView({
    parent: editorHost.value,
    state: EditorState.create({
      doc: markdown.value,
      extensions: [
        markdownLanguage(), history(), lineNumbers(), highlightActiveLineGutter(), highlightActiveLine(),
        bracketMatching(), rectangularSelection(), crosshairCursor(), dropCursor(),
        syntaxHighlighting(defaultHighlightStyle, { fallback: true }), EditorView.lineWrapping,
        keymap.of([
          ...defaultKeymap,
          ...historyKeymap,
          { key: 'Mod-s', preventDefault: true, run: () => { void save(); return true; } },
        ]),
        EditorView.domEventHandlers({
          compositionstart: () => { compositionStart(); return false; },
          compositionend: () => { compositionEnd(); return false; },
        }),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) markdown.value = update.state.doc.toString();
        }),
      ],
    }),
  });
}

function syncCodeMirror(): void {
  if (!editorView || editorView.state.doc.toString() === markdown.value) return;
  const selection = editorView.state.selection;
  editorView.dispatch({ changes: { from: 0, to: editorView.state.doc.length, insert: markdown.value }, selection });
}

async function save(retryKey?: string): Promise<void> {
  if (writing) { await writing; return; }
  if (!dirty.value || conflict.value || composing.value || loading.value) return;
  clearTimeout(debounce);
  const intent = pendingSave ?? {
    key: retryKey ?? newKey(),
    input: { expected_revision: revision.value, markdown: markdown.value },
    sequence,
    body: markdown.value,
  };
  const opportunityId = activeId.value;
  pendingSave = intent;
  busy.value = true;
  errorText.value = '';
  const task = (async () => {
    try {
      const saved = await props.service.saveNotes(opportunityId, intent.input, intent.key);
      localDrafts.set(draftKey(opportunityId), { markdown: activeId.value === opportunityId ? markdown.value : intent.body, baseMarkdown: saved.markdown, revision: saved.revision });
      if (activeId.value !== opportunityId) {
        pendingSave = null;
        return;
      }
      baseMarkdown.value = saved.markdown;
      revision.value = saved.revision;
      pendingSave = null;
      if (sequence === intent.sequence && markdown.value === intent.body) {
        markdown.value = saved.markdown;
        localDrafts.delete(draftKey(opportunityId));
        notice.value = 'Notes saved.';
      } else {
        notice.value = 'Earlier notes saved. Newer edits are still being saved.';
      }
    } catch (error) {
      if (error instanceof Error && (error as Error & { status?: number }).status === 412) {
        pendingSave = null;
        try { conflict.value = await props.service.getNotes(opportunityId); }
        catch { errorText.value = 'Notes changed elsewhere. Your text is preserved; current notes could not be fetched.'; }
      } else {
        errorText.value = 'Save status unknown. Your text is still here; retry will reconcile this same operation.';
      }
    }
  })();
  writing = task;
  await task;
  writing = null;
  busy.value = false;
  if (dirty.value && !conflict.value && !errorText.value && !pendingSave) {
    clearTimeout(debounce);
    debounce = setTimeout(() => { void save(); }, 0);
  }
}

async function retrySave(): Promise<void> { if (pendingSave) await save(pendingSave.key); }

function acceptCurrent(): void {
  if (!conflict.value) return;
  markdown.value = conflict.value.markdown;
  baseMarkdown.value = conflict.value.markdown;
  revision.value = conflict.value.revision;
  conflict.value = null;
  localDrafts.delete(draftKey(activeId.value));
  errorText.value = '';
}

function reapplyMine(): void {
  if (!conflict.value) return;
  baseMarkdown.value = conflict.value.markdown;
  revision.value = conflict.value.revision;
  conflict.value = null;
  localDrafts.set(draftKey(activeId.value), { markdown: markdown.value, baseMarkdown: baseMarkdown.value, revision: revision.value });
  errorText.value = '';
  void save();
}

async function toggle(task: ParsedTask, checked: boolean): Promise<void> {
  const opportunityId = activeId.value;
  if (dirty.value) await save();
  if (dirty.value || conflict.value || busy.value) {
    errorText.value = 'Save or resolve the notes conflict before changing a task.';
    return;
  }
  busy.value = true;
  errorText.value = '';
  try {
    const updated = await props.service.toggleTask(opportunityId, {
      expected_revision: revision.value,
      start_byte: task.startByte,
      end_byte: task.endByte,
      line_hash: await sha256Line(task.line),
      checked,
    }, newKey());
    markdown.value = updated.markdown;
    baseMarkdown.value = updated.markdown;
    revision.value = updated.revision;
    localDrafts.delete(draftKey(opportunityId));
    notice.value = 'Task updated in the canonical notes.';
  } catch (error) {
    if ((error as Error & { status?: number }).status === 412) {
      try { conflict.value = await props.service.getNotes(opportunityId); }
      catch { errorText.value = 'This task changed elsewhere; your original notes remain visible.'; }
    } else errorText.value = error instanceof Error ? error.message : 'Task could not be updated.';
  } finally { busy.value = false; }
}

function schedule(): void {
  sequence += 1;
  clearTimeout(debounce);
  if (dirty.value && !conflict.value) debounce = setTimeout(() => { void save(); }, 750);
}
function compositionStart(): void { composing.value = true; clearTimeout(debounce); }
function compositionEnd(): void { composing.value = false; schedule(); }
function toggleAt(index: number, event: Event): void {
  const task = tasks.value.find((candidate) => candidate.lineIndex === index);
  if (task) void toggle(task, (event.currentTarget as HTMLInputElement).checked);
}
function beforeUnload(event: BeforeUnloadEvent): void {
  if (!dirty.value && !writing) return;
  event.preventDefault();
  event.returnValue = '';
}

onMounted(() => { globalThis.addEventListener('beforeunload', beforeUnload); void load(props.opportunityId); });
onBeforeUnmount(() => { clearTimeout(debounce); globalThis.removeEventListener('beforeunload', beforeUnload); loadGeneration += 1; editorView?.destroy(); editorView = null; });

defineExpose({ parseTasks });
</script>

<template>
  <section
    class="notes-editor"
    aria-label="Opportunity notes"
  >
    <div class="notes-columns">
      <div class="source-pane">
        <div class="pane-heading">
          <h3>Notes · Markdown</h3><span role="status">{{ busy ? 'Saving…' : dirty ? 'Unsaved edits' : notice || 'Saved' }}</span>
        </div>
        <p v-if="loading">
          Loading canonical notes…
        </p>
        <div
          v-if="!loading"
          :id="`notes-${opportunityId}`"
          ref="editorHost"
          class="codemirror-host"
          role="textbox"
          aria-multiline="true"
          aria-label="Canonical Markdown notes"
          aria-describedby="notes-status"
        />
        <p
          id="notes-status"
          class="muted"
        >
          One canonical Markdown document. Edits save after typing pauses; task changes update this source.
        </p>
      </div>
      <div
        class="preview-pane"
        aria-label="Rendered notes preview"
      >
        <h3>Preview and tasks</h3>
        <ul
          v-if="!loading"
          class="task-list"
          aria-label="Notes tasks"
        >
          <li
            v-for="(row, index) in previewLines"
            :key="`${index}-${row.text}`"
            :style="{ '--depth': row.depth ?? 0 }"
          >
            <template v-if="row.kind === 'task'">
              <input
                :id="`task-${opportunityId}-${index}`"
                type="checkbox"
                :checked="row.checked"
                :disabled="busy || !!conflict"
                @change="toggleAt(index, $event)"
              >
              <label :for="`task-${opportunityId}-${index}`">{{ row.text }}</label>
            </template>
            <code v-else-if="row.kind === 'fence' || row.kind === 'code'">{{ row.text }}</code>
            <component
              :is="`h${row.level}`"
              v-else-if="row.kind === 'heading'"
            >
              {{ row.text }}
            </component>
            <span
              v-else-if="row.kind === 'list'"
              class="preview-list"
              :style="{ '--depth': row.depth ?? 0 }"
            >{{ row.ordered ? '1. ' : '• ' }}{{ row.text }}</span>
            <blockquote v-else-if="row.kind === 'quote'">
              {{ row.text }}
            </blockquote>
            <span v-else>{{ row.text || ' ' }}</span>
          </li>
        </ul>
      </div>
    </div>
    <div
      v-if="conflict"
      role="alert"
      class="conflict"
    >
      <h3>Notes changed in another session</h3>
      <p>Compare your text with the current canonical notes. No text was overwritten.</p>
      <div class="compare">
        <section><h4>Your edits</h4><pre>{{ markdown }}</pre></section><section><h4>Current saved notes</h4><pre>{{ conflict.markdown }}</pre></section>
      </div>
      <button
        type="button"
        @click="acceptCurrent"
      >
        Use current saved notes
      </button>
      <button
        type="button"
        @click="reapplyMine"
      >
        Review and reapply my text
      </button>
    </div>
    <div
      v-if="errorText"
      role="alert"
    >
      <p>{{ errorText }}</p><button
        v-if="pendingSave"
        type="button"
        @click="retrySave"
      >
        Retry same save
      </button>
    </div>
  </section>
</template>

<style scoped>
.notes-columns { display:grid; grid-template-columns:minmax(0,1fr) minmax(0,1fr); gap:1rem; }
.source-pane,.preview-pane { min-width:0; }
.pane-heading { display:flex; justify-content:space-between; align-items:center; gap:1rem; }
.codemirror-host { min-height:22rem; resize:vertical; overflow:auto; border:1px solid #82918a; border-radius:.25rem; }
.codemirror-host :deep(.cm-editor) { min-height:22rem; font:.95rem/1.55 ui-monospace,monospace; }
.codemirror-host :deep(.cm-content) { white-space:pre-wrap; overflow-wrap:anywhere; }
.task-list { list-style:none; padding:0; white-space:pre-wrap; overflow-wrap:anywhere; }
.task-list li { margin-inline-start:calc(var(--depth) * 1rem); min-height:1.7em; }
.task-list input { width:auto; margin-right:.5rem; }
.preview-list { display:block; margin-inline-start:calc(var(--depth) * 1rem); }
.task-list code, pre { white-space:pre-wrap; overflow-wrap:anywhere; }
.compare { display:grid; grid-template-columns:1fr 1fr; gap:1rem; }
.muted { color:#58665f; }
.visually-hidden { position:absolute; width:1px; height:1px; padding:0; margin:-1px; overflow:hidden; clip:rect(0,0,0,0); white-space:nowrap; border:0; }
@media(max-width:760px) { .notes-columns,.compare { grid-template-columns:1fr; } }
</style>
