<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { parley, type CapacityRecord } from '../../api/client';

const rows = ref<CapacityRecord[]>([]);
const period = ref(currentQuarter());
const unit = ref('project slots');
const total = ref(1);
const busy = ref(false);
const error = ref('');
const status = ref('');

function currentQuarter(): string {
  const now = new Date();
  return `${now.getFullYear()}-Q${Math.floor(now.getMonth() / 3) + 1}`;
}

async function load() {
  rows.value = await parley.listCapacity();
}

async function provision() {
  busy.value = true; error.value = ''; status.value = '';
  try {
    const normalizedPeriod = period.value.trim();
    const normalizedUnit = unit.value.trim();
    if (!normalizedPeriod || !normalizedUnit || !Number.isFinite(total.value) || total.value < 0) throw new Error('Enter a period, unit and non-negative capacity total.');
    if (rows.value.some((row) => row.period === normalizedPeriod && row.unit.toLocaleLowerCase() === normalizedUnit.toLocaleLowerCase())) throw new Error('That capacity period already exists. Provision a new period; replacing it could reset the committed ledger.');
    const saved = await parley.updateCapacity({ period: normalizedPeriod, unit: normalizedUnit, total: total.value });
    rows.value = [...rows.value.filter((row) => row.period !== saved.period || row.unit !== saved.unit), saved];
    status.value = `Capacity provisioned: ${saved.remaining} ${saved.unit} remain in ${saved.period}. Sends charge the ledger once when the approved material uses this capacity.`;
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not provision capacity.'; }
  finally { busy.value = false; }
}

onMounted(() => { void load().catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Could not load capacity ledgers.'; }); });
</script>

<template>
  <section aria-labelledby="capacity-heading">
    <h3 id="capacity-heading">
      Capacity ledger
    </h3>
    <p>Capacity is owner-reported. The deterministic send operation charges an applicable ledger once; editing the remaining amount here never happens implicitly.</p>
    <ul v-if="rows.length">
      <li
        v-for="row in rows"
        :key="`${row.period}:${row.unit}`"
      >
        {{ row.period }} · {{ row.committed }} committed of {{ row.total }} {{ row.unit }} · {{ row.remaining }} remaining
      </li>
    </ul>
    <p v-else>
      No capacity ledger is configured. Scarcity claims stay unavailable until a real ledger exists.
    </p>
    <form @submit.prevent="provision">
      <label>New period<input
        v-model="period"
        required
        maxlength="80"
      ></label>
      <label>Unit<input
        v-model="unit"
        required
        maxlength="80"
      ></label>
      <label>Total available<input
        v-model.number="total"
        type="number"
        min="0"
        step="any"
        required
      ></label>
      <button
        type="submit"
        :disabled="busy"
      >
        Provision new capacity period
      </button>
    </form>
    <p
      v-if="status"
      role="status"
    >
      {{ status }}
    </p>
    <p
      v-if="error"
      role="alert"
    >
      {{ error }}
    </p>
  </section>
</template>
