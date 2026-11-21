<script setup lang="ts">
import { tabs } from './tabs';
</script>

<template>
  <header>
    <strong>Parley</strong>
    <nav aria-label="Main navigation">
      <button
        v-for="tab in tabs"
        :key="tab"
        type="button"
      >
        {{ tab }}
      </button>
    </nav>
  </header>
  <main />
</template>
