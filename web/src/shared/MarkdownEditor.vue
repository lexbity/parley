<script setup lang="ts">
/* global HTMLDivElement */
import { onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';
import { markdown as markdownLanguage } from '@codemirror/lang-markdown';
import { defaultHighlightStyle, syntaxHighlighting } from '@codemirror/language';
import { Annotation, Compartment, EditorState, Transaction } from '@codemirror/state';
import { EditorView, keymap, lineNumbers } from '@codemirror/view';

const props = defineProps<{ modelValue: string; label: string; readonly?: boolean; compact?: boolean }>();
const emit = defineEmits<{ 'update:modelValue': [value: string]; composition: [active: boolean] }>();
const host = ref<HTMLDivElement | null>(null);
let editor: EditorView | null = null;
const input = new Compartment();
const fromModel = Annotation.define<boolean>();

function inputOptions() {
  return [
    EditorState.readOnly.of(props.readonly ?? false),
    EditorView.editable.of(!props.readonly),
    EditorView.contentAttributes.of({ 'aria-label': props.label, spellcheck: 'true', ...(props.readonly ? { tabindex: '0' } : {}) }),
  ];
}

onMounted(() => {
  if (!host.value) return;
  // CodeMirror mounts constructed stylesheets in a shadow root. The packaged
  // app serves its HTML unchanged under a policy that blocks inline styles.
  const root = host.value.attachShadow({ mode: 'open' });
  const state = EditorState.create({
    doc: props.modelValue,
    extensions: [
      lineNumbers(), history(), keymap.of([...defaultKeymap, ...historyKeymap]),
      markdownLanguage(), syntaxHighlighting(defaultHighlightStyle, { fallback: true }), EditorView.lineWrapping,
      input.of(inputOptions()),
      EditorView.updateListener.of((update) => {
        if (update.transactions.some(change => change.docChanged && !change.annotation(fromModel))) {
          emit('update:modelValue', update.state.doc.toString());
        }
      }),
      EditorView.domEventHandlers({
        compositionstart: () => { emit('composition', true); return false; },
        compositionend: () => { emit('composition', false); return false; },
      }),
      EditorView.theme({
        '&': { border: '1px solid #82918a', borderRadius: '.25rem', backgroundColor: 'white' },
        '.cm-scroller': { minHeight: 'var(--markdown-editor-height, 18rem)', maxHeight: 'var(--markdown-editor-max-height, 50dvh)', overflow: 'auto' },
        '.cm-content': { padding: '.7rem', fontFamily: 'ui-monospace, monospace' },
        '.cm-gutters': { backgroundColor: '#f4f6f3', borderRight: '1px solid #d7dfd9' },
        '&.cm-focused': { outline: '3px solid #32735b', outlineOffset: '2px' },
      }),
    ],
  });
  editor = new EditorView({ state, parent: root, root });
});

watch(() => props.modelValue, (value) => {
  if (!editor || editor.state.doc.toString() === value) return;
  editor.dispatch({
    changes: { from: 0, to: editor.state.doc.length, insert: value },
    annotations: [fromModel.of(true), Transaction.addToHistory.of(false)],
  });
});
watch(() => [props.readonly, props.label], () => { editor?.dispatch({ effects: input.reconfigure(inputOptions()) }); });

onBeforeUnmount(() => { editor?.destroy(); editor = null; });
</script>

<template>
  <div
    ref="host"
    class="markdown-editor"
    :class="{ compact }"
  />
</template>

<style scoped>
.markdown-editor { min-width: 0; }
.compact { --markdown-editor-height: 6rem; --markdown-editor-max-height: 13rem; }
</style>
