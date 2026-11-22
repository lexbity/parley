<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { parley, type DailyQueueItem, type DailyQueueSnapshot } from '../../api/client';

defineOptions({ name: 'DailyQueueView' });

const emit = defineEmits<{ navigate: [path: string] }>();
const snapshot = ref<DailyQueueSnapshot | null>(null);
const search = ref('');
const statusFilter = ref('any');
const query = ref('');
const error = ref('');
const loading = ref(false);
const generation = ref(0);
const blocked = computed(() => (snapshot.value?.items ?? []).filter((item) => item.type === 'blocked'));
const actionable = computed(() => (snapshot.value?.items ?? []).filter((item) => item.type !== 'blocked'));
const hasFilter = computed(() => query.value.length > 0 || statusFilter.value !== 'any');

async function load() {
  const current = ++generation.value;
  loading.value = true; error.value = '';
  try {
    await parley.connect();
    const result = await parley.getDailyQueue({ ...(query.value ? { q: query.value } : {}), ...(statusFilter.value !== 'any' ? { status: statusFilter.value } : {}) });
    if (current === generation.value) snapshot.value = result;
  } catch (reason) {
    if (current === generation.value) error.value = reason instanceof Error ? reason.message : 'Could not load deterministic daily work.';
  } finally { if (current === generation.value) loading.value = false; }
}

function applyFilters() {
  query.value = search.value.trim();
  void load();
}

function openItem(item: DailyQueueItem) {
  if (item.remediation_target?.kind === 'question') {
    emit('navigate', `/resources?question=${encodeURIComponent(item.remediation_target.id)}`);
    return;
  }
  emit('navigate', `/queue/${encodeURIComponent(item.opportunity_id)}`);
}

function formatDate(value: string | null): string {
  if (!value) return 'No deadline';
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? value : new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(date);
}

onMounted(() => { void load(); });
</script>

<template>
  <section aria-labelledby="daily-heading">
    <h3 id="daily-heading">
      Daily priorities
    </h3>
    <p v-if="snapshot">
      As of {{ snapshot.as_of }} · pack {{ snapshot.pack_id }} v{{ snapshot.pack_version }} · {{ snapshot.actionable_count }} actionable · {{ snapshot.deferred_count }} deferred · daily cap {{ snapshot.daily_cap }}
    </p>
    <form
      class="filters"
      @submit.prevent="applyFilters"
    >
      <label>Find daily work<input
        v-model="search"
        aria-label="Find daily work"
      ></label>
      <label>Outreach state<select
        v-model="statusFilter"
        aria-label="Daily status filter"
      ><option value="any">Any status</option><option value="open">Open</option><option value="waiting">Waiting</option><option value="conversing">In conversation</option><option value="timed_out">Timed out</option></select></label>
      <button
        type="submit"
        :disabled="loading"
      >
        Apply filters
      </button>
      <button
        type="button"
        :disabled="loading"
        @click="load"
      >
        Refresh daily work
      </button>
    </form>
    <p
      v-if="loading"
      role="status"
    >
      Loading server-ranked daily work…
    </p>
    <p
      v-if="error"
      role="alert"
    >
      Daily work could not be loaded: {{ error }}
    </p>
    <section aria-labelledby="daily-actionable-heading">
      <h4 id="daily-actionable-heading">
        Suggested work
      </h4>
      <p v-if="!loading && !error && actionable.length === 0">
        {{ hasFilter ? 'No daily items match these filters.' : 'No actionable work is due. Queue reads do not start model jobs.' }}
      </p>
      <ol v-else>
        <li
          v-for="item in actionable"
          :key="item.id"
          class="daily-item"
        >
          <div>
            <strong>{{ item.title }}</strong><span v-if="item.party_name"> · {{ item.party_name }}</span>
            <p>{{ item.reason }}</p>
            <small>{{ item.type.replaceAll('_', ' ') }} · {{ formatDate(item.due_at) }}</small>
          </div>
          <button
            type="button"
            :aria-label="`Open ${item.title}`"
            @click="openItem(item)"
          >
            Open work
          </button>
        </li>
      </ol>
    </section>
    <section aria-labelledby="daily-blocked-heading">
      <h4 id="daily-blocked-heading">
        Blocked
      </h4>
      <p v-if="!loading && !error && blocked.length === 0">
        No blocked opportunities in this view.
      </p>
      <ul v-else>
        <li
          v-for="item in blocked"
          :key="item.id"
          class="daily-item blocked-item"
        >
          <div>
            <strong>{{ item.title }}</strong><span v-if="item.party_name"> · {{ item.party_name }}</span>
            <p>{{ item.blocking_reason || item.reason }}</p>
            <small>{{ item.reason }}</small>
          </div>
          <button
            type="button"
            :aria-label="`Resolve ${item.title} block`"
            @click="openItem(item)"
          >
            Review blocker
          </button>
        </li>
      </ul>
    </section>
  </section>
</template>

<style scoped>
.filters { display: flex; flex-wrap: wrap; align-items: end; gap: .75rem; margin-block: 1rem; }
.filters label { min-width: min(17rem, 100%); }
.daily-item { display: flex; justify-content: space-between; align-items: center; gap: 1rem; margin-block: .5rem; padding: .75rem; background: #fff; border: 1px solid #cbd5ce; border-radius: .35rem; }
.blocked-item { border-inline-start: .3rem solid #80552a; }
.daily-item p { margin-block: .25rem; }
@media (max-width: 600px) { .daily-item { align-items: start; flex-direction: column; } }
</style>
