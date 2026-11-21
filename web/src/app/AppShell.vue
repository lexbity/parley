<script setup lang="ts">
import { ref } from 'vue';
import { tabs } from './tabs';
import ResourceEditor from '../features/resources/ResourceEditor.vue';
import Directory from '../features/leads/Directory.vue';

/**
 * Tabs whose owner journey is qualified stay discoverable (RFC-0003 S01);
 * the mandated order and the full destination set come from tabs.ts (APP-01).
 */
const qualifiedTabs = tabs.filter((tab) => tab === 'Resources' || tab === 'Leads');
const active = ref<(typeof tabs)[number]>('Resources');
</script>

<template>
  <header>
    <strong>Parley</strong>
    <nav aria-label="Main navigation">
      <button
        v-for="tab in qualifiedTabs"
        :key="tab"
        type="button"
        :aria-current="active === tab ? 'page' : undefined"
        @click="active = tab"
      >
        {{ tab }}
      </button>
    </nav>
  </header>
  <main>
    <h1>{{ active }}</h1>
    <div v-show="active === 'Resources'">
      <ResourceEditor />
    </div>
    <Directory v-if="active === 'Leads'" />
  </main>
</template>

<style>
:root { color: #18221f; background: #f4f6f3; font: 16px/1.5 system-ui, sans-serif; }
body { margin: 0; } header { display: flex; align-items: center; gap: 2rem; padding: .75rem 2rem; background: white; border-bottom: 1px solid #d7dfd9; }
nav { display: flex; gap: .5rem; } button, input, select, textarea { font: inherit; } button { cursor: pointer; padding: .45rem .8rem; }
button[aria-current="page"] { background: #173e32; color: white; } main { max-width: 1100px; margin: 2rem auto; padding: 0 1rem; }
.workspace-grid { display: grid; grid-template-columns: repeat(auto-fit,minmax(300px,1fr)); gap: 1.5rem; align-items: start; }
form, section section { background: white; border: 1px solid #d7dfd9; border-radius: .5rem; padding: 1rem; }
label { display: grid; gap: .3rem; margin: .8rem 0; } input, select, textarea { width: 100%; box-sizing: border-box; padding: .5rem; border: 1px solid #82918a; border-radius: .25rem; }
small { color: #58665f; } .capture { grid-column: 1 / -1; } @media(max-width:650px) { header { align-items: start; flex-direction: column; gap: .5rem; padding: 1rem; } nav { flex-wrap: wrap; } }
</style>
