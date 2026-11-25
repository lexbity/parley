<script setup lang="ts">
/**
 * A native select fronted by a debounced filter. Only a bounded window of
 * options is mounted, so a list of tens of thousands of records does not turn
 * into tens of thousands of DOM nodes on every render. The selected option is
 * always kept in the window so the control never loses its value while the
 * owner is filtering. Native select semantics are preserved for accessibility
 * and keyboard use.
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import type { SelectOption } from './select-options';

const props = withDefaults(defineProps<{
  modelValue: string;
  options: SelectOption[];
  label: string;
  id: string;
  disabled?: boolean;
  required?: boolean;
  placeholder?: string;
  maxVisible?: number;
  /** Server-backed typeahead. When set, each debounced term is queried. */
  search?: (term: string) => Promise<SelectOption[]>;
  /** Label of the current value when it is outside the loaded window. */
  selectedLabel?: string;
}>(), {
  disabled: false,
  required: false,
  placeholder: 'Choose…',
  maxVisible: 50,
  search: undefined,
  selectedLabel: undefined,
});

const emit = defineEmits<{ 'update:modelValue': [value: string] }>();

const inputText = ref('');
const term = ref('');
const remote = ref<SelectOption[] | null>(null);
const searching = ref(false);
let filterTimer: ReturnType<typeof globalThis.setTimeout> | undefined;
let searchGeneration = 0;
watch(inputText, value => {
  if (filterTimer) globalThis.clearTimeout(filterTimer);
  filterTimer = globalThis.setTimeout(() => {
    term.value = value;
    if (!props.search) return;
    const generation = ++searchGeneration;
    searching.value = true;
    Promise.resolve(props.search(value))
      .then(options => { if (generation === searchGeneration) remote.value = options; })
      .catch(() => { if (generation === searchGeneration) remote.value = []; })
      .finally(() => { if (generation === searchGeneration) searching.value = false; });
  }, 120);
});
onMounted(() => {
  if (props.search) void props.search('').catch(() => { /* empty until retry */ });
});
onBeforeUnmount(() => {
  if (filterTimer) globalThis.clearTimeout(filterTimer);
  searchGeneration += 1;
});

const matches = computed(() => {
  if (props.search) return remote.value ?? props.options;
  const needle = term.value.trim().toLocaleLowerCase();
  return needle
    ? props.options.filter(option => option.label.toLocaleLowerCase().includes(needle))
    : props.options;
});
const visible = computed(() => {
  const selected = matches.value.find(option => option.value === props.modelValue)
    ?? (props.selectedLabel ? { value: props.modelValue, label: props.selectedLabel } : null);
  const window = matches.value.slice(0, props.maxVisible);
  if (selected && !window.some(option => option.value === selected.value)) {
    return [selected, ...window.slice(0, props.maxVisible - 1)];
  }
  return window;
});
</script>

<template>
  <div class="searchable-select">
    <label :for="id">{{ label }}</label>
    <input
      v-model="inputText"
      type="search"
      aria-label="Filter choices"
      :placeholder="placeholder"
      autocomplete="off"
      :disabled="disabled"
    >
    <select
      :id="id"
      :value="modelValue"
      :disabled="disabled"
      :required="required"
      @change="emit('update:modelValue', ($event.target as HTMLSelectElement).value)"
    >
      <option
        v-for="option in visible"
        :key="option.value"
        :value="option.value"
      >
        {{ option.label }}
      </option>
    </select>
  </div>
</template>
