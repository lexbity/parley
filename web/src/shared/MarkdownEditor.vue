<script setup lang="ts">
/* global HTMLDivElement */
import { onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';
import { markdown as markdownLanguage } from '@codemirror/lang-markdown';
import { defaultHighlightStyle, syntaxHighlighting } from '@codemirror/language';
import { EditorState } from '@codemirror/state';
import { EditorView, keymap, lineNumbers } from '@codemirror/view';

const props = defineProps<{ modelValue: string; label: string; readonly?: boolean }>();
const emit = defineEmits<{ 'update:modelValue': [value: string]; composition: [active: boolean] }>();
const host = ref<HTMLDivElement | null>(null);
let editor: EditorView | null = null;
let composing = false;

onMounted(() => {
  if (!host.value) return;
  const state = EditorState.create({
    doc: props.modelValue,
    extensions: [
      lineNumbers(), history(), keymap.of([...defaultKeymap, ...historyKeymap]),
      markdownLanguage(), syntaxHighlighting(defaultHighlightStyle, { fallback: true }), EditorView.lineWrapping,
      EditorState.readOnly.of(props.readonly ?? false),
      EditorView.updateListener.of((update) => {
        if (update.docChanged) emit('update:modelValue', update.state.doc.toString());
      }),
      EditorView.domEventHandlers({
        compositionstart: () => { composing = true; emit('composition', true); return false; },
        compositionend: () => { composing = false; emit('composition', false); return false; },
      }),
      EditorView.contentAttributes.of({ 'aria-label': props.label, spellcheck: 'true' }),
      EditorView.theme({ '&': { minHeight: '18rem', border: '1px solid #82918a', borderRadius: '.25rem', backgroundColor: 'white' }, '.cm-content': { padding: '.7rem', fontFamily: 'ui-monospace, monospace' }, '.cm-gutters': { backgroundColor: '#f4f6f3', borderRight: '1px solid #d7dfd9' }, '&.cm-focused': { outline: '3px solid #32735b', outlineOffset: '2px' } }),
    ],
  });
  editor = new EditorView({ state, parent: host.value });
});

watch(() => props.modelValue, (value) => {
  if (!editor || editor.state.doc.toString() === value) return;
  editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: value } });
});

onBeforeUnmount(() => { editor?.destroy(); editor = null; });

// Tests drive deterministic document changes through the CodeMirror view;
// typing simulation in jsdom cannot reproduce CodeMirror's input pipeline.
defineExpose({ getView: () => editor });
</script>

<template>
  <div
    ref="host"
    class="markdown-editor"
    :data-composing="composing"
  />
</template>
