<script setup lang="ts">
/* global HTMLDialogElement, HTMLElement, HTMLInputElement, KeyboardEvent, Event, document */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { newOperationKey } from '../api/client';
import {
  CommandApiError, commands, type CommandArgs, type CommandContext, type CommandDescriptor,
  type CommandPrompt, type CommandSuccess,
} from '../api/commands';
import { contextFingerprint, type ContextSelection } from './commands/context';
import { useCommandRegistry, type UiRunContext } from './commands/registry';
import { rankCommands, type Recency } from './commands/search';

/**
 * The one accessible command palette (UCR-001 FR-7, §9.2): a native dialog
 * root holding search, prompt and confirmation steps. Descriptor text is
 * always rendered as text; every visible dataset has a distinct state.
 */
const props = withDefaults(defineProps<{
  open: boolean;
  descriptors: CommandDescriptor[];
  context: CommandContext;
  selection?: ContextSelection;
  pickerOptions?: Record<string, Array<{ value: string; label: string }>>;
  intentFingerprint?: string;
  epoch?: string;
  fallbackFocusSelector?: string;
  revalidate?: () => Promise<unknown>;
}>(), {
  selection: () => ({}),
  pickerOptions: () => ({}),
  intentFingerprint: '',
  epoch: '',
  fallbackFocusSelector: 'main .workspace-header h1',
  revalidate: undefined,
});

const emit = defineEmits<{ close: []; committed: [result: CommandSuccess] }>();

type Step = 'search' | 'prompt' | 'confirm' | 'result';
const dialog = ref<HTMLDialogElement | null>(null);
const input = ref<HTMLInputElement | null>(null);
const step = ref<Step>('search');
const query = ref('');
const activeIndex = ref(0);
const composing = ref(false);
const staleIntent = ref(false);
const selected = ref<CommandDescriptor | null>(null);
const dialogStep = ref<HTMLElement | null>(null);
const promptValues = ref<Record<string, string>>({});
const status = ref('');
const error = ref('');
const busy = ref(false);
const recency = ref<Recency>({});
let opener: HTMLElement | null = null;
let intent: { id: string; version: number; args: CommandArgs; key: string; fingerprint: string } | null = null;

const ranked = computed(() => rankCommands(props.descriptors, query.value, recency.value));
const activeId = computed(() => ranked.value[activeIndex.value] ? `command-palette-option-${activeIndex.value}` : undefined);
const prompts = computed<CommandPrompt[]>(() => {
  if (!selected.value) return [];
  const ui = registry.get(selected.value.id);
  return ui?.prompts ?? selected.value.palette.prompts;
});
const confirmation = computed(() => {
  if (!selected.value) return '';
  const ui = registry.get(selected.value.id);
  if (ui?.confirmation) return ui.confirmation;
  return selected.value.effect === 'write'
    ? `Apply “${selected.value.title}” to the selected record?`
    : `Run “${selected.value.title}”?`;
});
const contextKey = computed(() => contextFingerprint(props.context));
const promptsReady = computed(() => prompts.value.every((prompt) => (promptValues.value[prompt.field] ?? '').trim().length > 0));

const registry = useCommandRegistry();

function reset() {
  step.value = 'search';
  query.value = '';
  activeIndex.value = 0;
  selected.value = null;
  promptValues.value = {};
  status.value = '';
  error.value = '';
  busy.value = false;
  staleIntent.value = false;
  intent = null;
}

function focusables(): HTMLElement[] {
  const root = dialog.value;
  if (!root) return [];
  return [...root.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex="0"]')]
    .filter((element) => element.offsetParent !== null || element === document.activeElement);
}

function restoreFocus() {
  const root = globalThis.document;
  const fallback = root?.querySelector<HTMLElement>(props.fallbackFocusSelector) ?? null;
  const previousFocus = opener !== root?.body && opener?.isConnected ? opener : null;
  previousFocus?.focus();
  if (!previousFocus || root?.activeElement !== previousFocus) {
    if (fallback && !fallback.hasAttribute('tabindex')) fallback.setAttribute('tabindex', '-1');
    fallback?.focus();
  }
  opener = null;
}

function close() {
  if (busy.value) return;
  emit('close');
}

function onCancel(event: Event) {
  event.preventDefault();
  close();
}

function onDialogKeydown(event: KeyboardEvent) {
  if (event.defaultPrevented || event.isComposing || composing.value || dialog.value?.open !== true) return;
  if (event.key === 'Escape' && !busy.value && (step.value === 'prompt' || step.value === 'confirm')) {
    event.preventDefault();
    back();
    return;
  }
  if (event.key !== 'Tab') return;
  const items = focusables();
  if (!items.length) return;
  const first = items[0]!;
  const last = items[items.length - 1]!;
  if (event.shiftKey && globalThis.document.activeElement === first) { event.preventDefault(); last.focus(); }
  else if (!event.shiftKey && globalThis.document.activeElement === last) { event.preventDefault(); first.focus(); }
}

function onInputKeydown(event: KeyboardEvent) {
  if (composing.value || event.isComposing) return;
  if (event.key === 'ArrowDown') {
    event.preventDefault();
    activeIndex.value = ranked.value.length ? (activeIndex.value + 1) % ranked.value.length : 0;
  } else if (event.key === 'ArrowUp') {
    event.preventDefault();
    activeIndex.value = ranked.value.length ? (activeIndex.value - 1 + ranked.value.length) % ranked.value.length : 0;
  } else if (event.key === 'Enter') {
    event.preventDefault();
    const item = ranked.value[activeIndex.value];
    if (item) choose(item);
  } else if (event.key === 'Escape') {
    event.preventDefault();
    close();
  }
}

function onCompositionStart() { composing.value = true; }
function onCompositionEnd() { composing.value = false; }

async function choose(descriptor: CommandDescriptor) {
  selected.value = descriptor;
  error.value = '';
  status.value = '';
  promptValues.value = {};
  activeIndex.value = 0;
  recency.value = { ...recency.value, [descriptor.id]: Date.now() };
  if (prompts.value.length) { step.value = 'prompt'; return; }
  const ui = registry.get(descriptor.id);
  if (ui && !ui.confirmation && ui.effect === 'read') {
    try { await registry.run(ui.id, {}, runContext()); close(); }
    catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not run the command.'; }
    return;
  }
  step.value = 'confirm';
  freezeIntent();
}

function freezeIntent() {
  if (!selected.value) return;
  const args: CommandArgs = { ...promptValues.value };
  staleIntent.value = false;
  intent = {
    id: selected.value.id,
    version: selected.value.version,
    args,
    key: newOperationKey(),
    fingerprint: `${props.intentFingerprint}|${props.epoch}|${contextKey.value}`,
  };
}

function submitPrompt() {
  if (!promptsReady.value) { error.value = 'Complete every prompt before continuing.'; return; }
  error.value = '';
  step.value = 'confirm';
  freezeIntent();
}

function back() {
  if (busy.value) return;
  const prior = step.value;
  error.value = '';
  staleIntent.value = false;
  step.value = prior === 'confirm' && prompts.value.length ? 'prompt' : 'search';
  intent = null;
}

function runContext(): UiRunContext {
  return { context: { ...props.context }, selection: { ...props.selection } };
}

async function confirm() {
  if (!intent || busy.value) return;
  if (staleIntent.value || intent.fingerprint !== `${props.intentFingerprint}|${props.epoch}|${contextKey.value}`) {
    error.value = 'The selection, revision or workspace changed after this command was prepared. Review it again before confirming.';
    return;
  }
  busy.value = true;
  error.value = '';
  status.value = '';
  const current = intent;
  try {
    if (props.revalidate) await props.revalidate();
    if (staleIntent.value || current.fingerprint !== `${props.intentFingerprint}|${props.epoch}|${contextKey.value}`) {
      throw new Error('The selection, revision or workspace changed after this command was prepared. Review it again before confirming.');
    }
    const ui = registry.get(current.id);
    if (ui) {
      await registry.run(current.id, current.args, runContext());
      status.value = `${selected.value?.title ?? 'Command'} completed.`;
    } else {
      const result = await commands.invoke(current.id, {
        version: current.version, args: current.args, dry_run: false,
      }, current.key);
      emit('committed', result);
      status.value = `${selected.value?.title ?? 'Command'} completed.`;
    }
    step.value = 'result';
  } catch (reason) {
    if (reason instanceof CommandApiError && reason.response) {
      error.value = reason.response.error.message;
    } else {
      error.value = reason instanceof Error ? reason.message : 'The command could not be completed.';
    }
  } finally {
    busy.value = false;
  }
}

function syncOpen(open: boolean) {
  const root = dialog.value;
  if (!root) return;
  if (open) {
    opener = globalThis.document?.activeElement instanceof HTMLElement ? globalThis.document.activeElement : null;
    reset();
    if (typeof root.showModal === 'function') root.showModal();
    else root.setAttribute('open', '');
    void nextTick(() => input.value?.focus());
  } else {
    if (typeof root.close === 'function' && root.open) root.close();
    else root.removeAttribute('open');
    void nextTick(restoreFocus);
  }
}

watch(() => props.open, syncOpen);
onMounted(() => syncOpen(props.open));
onBeforeUnmount(() => {
  const root = dialog.value;
  if (root?.open && typeof root.close === 'function') root.close();
});

watch([() => props.intentFingerprint, () => props.epoch, contextKey], () => {
  if (step.value === 'confirm') staleIntent.value = true;
});

watch(step, async (current) => {
  await nextTick();
  if (current === 'search') input.value?.focus();
  else dialogStep.value?.focus();
});

watch(activeIndex, async () => {
  await nextTick();
  if (activeId.value) globalThis.document.getElementById(activeId.value)?.scrollIntoView?.({ block: 'nearest' });
});
watch(query, () => { activeIndex.value = 0; });

watch(ranked, (items) => {
  if (activeIndex.value >= items.length) activeIndex.value = 0;
});

defineExpose({ intentKey: () => intent?.key ?? null, currentStep: () => step.value });
</script>

<template>
  <dialog
    ref="dialog"
    class="command-palette"
    aria-labelledby="command-palette-title"
    @cancel="onCancel"
    @keydown="onDialogKeydown"
  >
    <div class="palette-head">
      <h2 id="command-palette-title">
        Command palette
      </h2>
      <button
        type="button"
        aria-label="Close command palette"
        :disabled="busy"
        @click="close"
      >
        Close
      </button>
    </div>
    <p
      role="status"
      aria-live="polite"
      class="sr-only"
    >
      {{ status }}
    </p>

    <template v-if="step === 'search'">
      <input
        ref="input"
        v-model="query"
        type="text"
        role="combobox"
        aria-label="Search commands"
        aria-autocomplete="list"
        aria-controls="command-palette-listbox"
        :aria-expanded="open"
        :aria-activedescendant="activeId"
        autocomplete="off"
        placeholder="Search commands"
        @keydown="onInputKeydown"
        @compositionstart="onCompositionStart"
        @compositionend="onCompositionEnd"
      >
      <ul
        id="command-palette-listbox"
        role="listbox"
        aria-label="Commands"
      >
        <li
          v-if="!descriptors.length"
          role="presentation"
          class="palette-state"
        >
          No commands are available here.
        </li>
        <li
          v-else-if="query.trim() && !ranked.length"
          role="presentation"
          class="palette-state"
        >
          No matching commands.
        </li>
        <template v-else>
          <li
            v-for="(item, index) in ranked"
            :id="`command-palette-option-${index}`"
            :key="item.id"
            role="option"
            :aria-selected="index === activeIndex"
            :class="{ active: index === activeIndex }"
            @click="choose(item)"
          >
            <strong>{{ item.title }}</strong>
            <small v-if="item.summary">{{ item.summary }}</small>
          </li>
        </template>
      </ul>
    </template>

    <form
      v-else-if="step === 'prompt'"
      class="palette-step"
      @submit.prevent="submitPrompt"
    >
      <h3
        ref="dialogStep"
        tabindex="-1"
      >
        {{ selected?.title }}
      </h3>
      <p
        v-if="selected?.summary"
        class="palette-hint"
      >
        {{ selected.summary }}
      </p>
      <label
        v-for="prompt in prompts"
        :key="prompt.field"
      >
        {{ prompt.title }}
        <select
          v-if="pickerOptions[prompt.picker]?.length"
          :value="promptValues[prompt.field] ?? ''"
          @change="promptValues[prompt.field] = ($event.target as HTMLSelectElement).value"
        >
          <option value="">
            Choose…
          </option>
          <option
            v-for="option in pickerOptions[prompt.picker]"
            :key="option.value"
            :value="option.value"
          >
            {{ option.label }}
          </option>
        </select>
        <input
          v-else
          type="text"
          :value="promptValues[prompt.field] ?? ''"
          :aria-label="prompt.title"
          @input="promptValues[prompt.field] = ($event.target as HTMLInputElement).value"
        >
      </label>
      <div class="palette-actions">
        <button
          type="button"
          @click="back"
        >
          Back
        </button>
        <button
          type="submit"
          class="primary"
          :disabled="!promptsReady"
        >
          Continue
        </button>
      </div>
    </form>

    <form
      v-else-if="step === 'confirm'"
      class="palette-step"
      @submit.prevent="confirm"
    >
      <h3
        ref="dialogStep"
        tabindex="-1"
      >
        Confirm command
      </h3>
      <p>{{ selected?.title }}</p>
      <p class="palette-hint">
        {{ confirmation }}
      </p>
      <p
        v-if="error"
        role="alert"
      >
        {{ error }}
      </p>
      <div class="palette-actions">
        <button
          type="button"
          :disabled="busy"
          @click="back"
        >
          Back
        </button>
        <button
          type="submit"
          class="primary"
          :disabled="busy"
        >
          Confirm
        </button>
      </div>
    </form>

    <template v-else>
      <h3
        ref="dialogStep"
        tabindex="-1"
      >
        Command completed
      </h3>
      <p>{{ status }}</p>
      <div class="palette-actions">
        <button
          type="button"
          class="primary"
          @click="close"
        >
          Done
        </button>
      </div>
    </template>
    <p
      v-if="error && step === 'search'"
      role="alert"
    >
      {{ error }}
    </p>
  </dialog>
</template>

<style scoped>
.command-palette { margin: clamp(1rem, 8vh, 4.5rem) auto auto; width: min(38rem, 94vw); max-height: 80dvh; overflow: auto; border: 1px solid #9db2a4; border-radius: .6rem; padding: 1rem; box-shadow: 0 16px 48px #152b2040; }
.command-palette::backdrop { background: #0d1d1780; }
.palette-step { margin-top: .75rem; border: 0; padding: 0; }
.palette-head { display: flex; align-items: center; justify-content: space-between; gap: 1rem; }
.palette-head h2 { margin: 0; font-size: 1.1rem; }
.command-palette input { margin-top: .8rem; }
.command-palette [role="listbox"] { list-style: none; margin: .5rem 0 0; padding: 0; max-height: 22rem; overflow: auto; }
.command-palette [role="option"] { display: grid; gap: .15rem; padding: .55rem .7rem; border-radius: .35rem; cursor: pointer; }
.command-palette [role="option"].active, .command-palette [role="option"]:hover { background: #e4eee7; }
.command-palette [role="option"] small { color: #56675d; }
.palette-state { padding: .75rem; color: #56675d; }
.palette-hint { color: #56675d; font-size: 13px; }
.palette-actions { display: flex; justify-content: flex-end; gap: .5rem; margin-top: .9rem; }
</style>
