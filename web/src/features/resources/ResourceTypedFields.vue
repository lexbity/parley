<script setup lang="ts">
defineProps<{ subtype: string; disabled: boolean }>();
const body = defineModel<Record<string, unknown>>({ required: true });
</script>

<template>
  <template v-if="subtype === 'proof'">
    <label>Result<input
      v-model="body.result"
      :disabled="disabled"
      autocomplete="off"
    ></label>
    <label>Context<input
      v-model="body.context"
      :disabled="disabled"
      autocomplete="off"
    ></label>
    <label>Time<input
      v-model="body.time"
      :disabled="disabled"
      autocomplete="off"
    ></label>
    <label>Owner role<input
      v-model="body.owner_role"
      :disabled="disabled"
      autocomplete="off"
    ></label>
  </template>
  <template v-else-if="subtype === 'gift'">
    <label>Availability
      <select
        v-model="body.delivery"
        :disabled="disabled"
      >
        <option value="on_request">I can make this on request</option>
        <option value="exists">It already exists</option>
      </select>
    </label>
    <label>Effort estimate<input
      v-model="body.effort"
      :disabled="disabled"
      autocomplete="off"
    ></label>
    <label v-if="body.delivery === 'exists'">Available reference<input
      v-model="body.reference"
      :disabled="disabled"
      autocomplete="off"
    ></label>
  </template>
  <template v-else-if="subtype === 'artifact'">
    <label>Available reference<input
      v-model="body.reference"
      :disabled="disabled"
      autocomplete="off"
    ></label>
    <label class="check-row"><input
      v-model="body.share_permission"
      type="checkbox"
      :disabled="disabled"
    > Permission to share</label>
  </template>
</template>

<style scoped>
.check-row { display: flex; align-items: center; gap: .4rem; }
</style>
