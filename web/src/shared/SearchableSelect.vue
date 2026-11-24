<script setup lang="ts">
/**
 * A native select fronted by a debounced filter. Only a bounded window of
 * options is mounted, so a list of tens of thousands of records does not turn
 * into tens of thousands of DOM nodes on every render. The selected option is
 * always kept in the window so the control never loses its value while the
 * owner is filtering. Native select semantics are preserved for accessibility
 * and keyboard use.
 */
import { computed, onBeforeUnmount, ref, watch } from 'vue';
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
}>(), {
  disabled: false,
  required: false,
  placeholder: 'Choose…',
  maxVisible: 50,
});

const emit = defineEmits<{ 'update:modelValue': [value: string] }>();

const search = ref('');
const term = ref('');
let filterTimer: ReturnType<typeof globalThis.setTimeout> | undefined;
watch(search, value => {
  if (filterTimer) globalThis.clearTimeout(filterTimer);
  filterTimer = globalThis.setTimeout(() => { term.value = value; }, 120);
});
onBeforeUnmount(() => { if (filterTimer) globalThis.clearTimeout(filterTimer); });

const matches = computed(() => {
  const needle = term.value.trim().toLocaleLowerCase();
  return needle
    ? props.options.filter(option => option.label.toLocaleLowerCase().includes(needle))
    : props.options;
});
const visible = computed(() => {
  const selected = props.options.find(option => option.value === props.modelValue);
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
      v-model="search"
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
