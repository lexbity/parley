<script setup lang="ts">
/* global setInterval */
/**
 * Owner approval inbox (UCR-001 U08 / FR-8).
 *
 * Every destructive registry command stops here for exact human review.
 * The inbox lists the owner-authorized pending approvals — its own or a
 * delegated run's — and lets the owner approve or deny with keyboard-first
 * controls. Decisions carry their own idempotency key; an exact replay
 * returns the original outcome. The list polls authoritative state and
 * refreshes immediately on `approvals_changed` SSE signals when available.
 */
import { onBeforeUnmount, onMounted, ref } from 'vue';
import { newOperationKey } from '../../api/client';
import {
  CommandApiError, commands, type ApprovalDetail, type ApprovalListItem,
} from '../../api/commands';

const items = ref<ApprovalListItem[]>([]);
const loading = ref(false);
const error = ref('');
const status = ref('');
const busyId = ref<string | null>(null);
const expanded = ref<string | null>(null);
const detail = ref<ApprovalDetail | null>(null);
const stale = ref(false);
const staleReason = ref('');
let pollTimer: ReturnType<typeof setInterval> | null = null;
let stopNotify: (() => void) | null = null;

function sessionInvalid(reason: unknown): boolean {
  return reason instanceof CommandApiError && (reason.status === 401 || reason.status === 403);
}

function markStale(message: string) {
  stale.value = true;
  staleReason.value = message;
  stopPolling();
}

async function load() {
  if (stale.value) return;
  loading.value = true;
  error.value = '';
  try {
    const page = await commands.listApprovals({ status: 'pending', limit: 50 });
    items.value = page.items;
  } catch (reason) {
    if (sessionInvalid(reason)) markStale('The owner session expired. Reopen Approvals with the current session.');
    else error.value = reason instanceof Error ? reason.message : 'Could not read approvals.';
  } finally {
    loading.value = false;
  }
}

function startPolling() {
  stopPolling();
  pollTimer = globalThis.setInterval(() => { void load(); }, 2000);
}

function stopPolling() {
  if (pollTimer !== null) {
    globalThis.clearInterval(pollTimer);
    pollTimer = null;
  }
}

async function decide(row: ApprovalListItem, action: 'grant' | 'deny') {
  if (busyId.value) return;
  busyId.value = row.id;
  error.value = '';
  status.value = '';
  try {
    const fresh = await commands.getApproval(row.id);
    const input = {
      expected_revision: fresh.revision,
      preview_digest: fresh.preview_digest,
    };
    const result = action === 'grant'
      ? await commands.grantApproval(row.id, input, newOperationKey())
      : await commands.denyApproval(row.id, input, newOperationKey());
    void result;
    status.value = action === 'grant'
      ? `${fresh.title ?? row.command_id} approved.`
      : `${fresh.title ?? row.command_id} denied.`;
    if (expanded.value === row.id) {
      expanded.value = null;
      detail.value = null;
    }
    await load();
  } catch (reason) {
    if (sessionInvalid(reason)) markStale('The owner session expired. Reopen Approvals with the current session.');
    else if (reason instanceof CommandApiError && reason.code === 'approval_stale') {
      await load();
      error.value = 'This approval changed while you were reviewing it. Refresh and decide again.';
    } else {
      error.value = reason instanceof Error ? reason.message : 'Could not record the decision.';
    }
  } finally {
    busyId.value = null;
  }
}

async function toggleDetail(row: ApprovalListItem) {
  if (expanded.value === row.id) {
    expanded.value = null;
    detail.value = null;
    return;
  }
  expanded.value = row.id;
  detail.value = null;
  try {
    detail.value = await commands.getApproval(row.id);
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : 'Could not read the approval detail.';
  }
}

function onWorkspaceChanged() {
  markStale('The workspace changed. Reopen Approvals with the current workspace.');
}

async function startNotifications() {
  try {
    stopNotify = await commands.notify({
      onSignal: (signal) => {
        if (signal === 'approvals_changed' || signal === 'epoch_changed') void load();
      },
    });
  } catch {
    // polling remains the authoritative fallback
  }
}

onMounted(() => {
  void load();
  startPolling();
  void startNotifications();
  globalThis.addEventListener('parley:workspace-changed', onWorkspaceChanged);
  globalThis.addEventListener('parley:workspace-quarantined', onWorkspaceChanged);
});

onBeforeUnmount(() => {
  stopPolling();
  stopNotify?.();
  globalThis.removeEventListener('parley:workspace-changed', onWorkspaceChanged);
  globalThis.removeEventListener('parley:workspace-quarantined', onWorkspaceChanged);
});
</script>

<template>
  <section
    aria-labelledby="approval-inbox-heading"
    class="approval-inbox"
  >
    <h2 id="approval-inbox-heading">
      Approvals
    </h2>
    <p>Destructive commands stop here for your exact review. Approving commits the frozen intent; denying or ignoring it changes nothing.</p>
    <p
      v-if="stale"
      role="alert"
      data-stale
    >
      {{ staleReason }}
      <button
        type="button"
        aria-label="Reopen Approvals with current session"
        @click="() => { stale = false; staleReason = ''; void load(); startPolling(); }"
      >
        Reopen with current session
      </button>
    </p>
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

    <div
      v-if="loading && !items.length"
      class="inbox-state"
    >
      Loading approvals…
    </div>
    <div
      v-else-if="!items.length"
      class="inbox-state"
    >
      No pending approvals.
    </div>
    <ul
      v-else
      class="inbox-list"
      aria-label="Pending approvals"
    >
      <li
        v-for="row in items"
        :key="row.id"
        :data-approval-id="row.id"
        :data-status="row.status"
        class="inbox-row"
      >
        <div class="inbox-row-head">
          <strong>{{ row.title ?? row.command_id }}</strong>
          <span class="inbox-run" v-if="row.run_id">delegated run</span>
          <span class="inbox-expires">expires {{ row.expires_at }}</span>
        </div>
        <dl
          v-if="row.preview"
          class="inbox-preview"
        >
          <dt>Placement</dt>
          <dd>{{ row.preview.placement_removed ? 'Removed from the board' : 'Unchanged' }}</dd>
          <dt>Affected material versions</dt>
          <dd>{{ row.preview.impacted_versions_count ?? 0 }}</dd>
        </dl>
        <div class="inbox-actions">
          <button
            type="button"
            :disabled="busyId !== null"
            @click="toggleDetail(row)"
          >
            {{ expanded === row.id ? 'Hide details' : 'Details' }}
          </button>
          <button
            type="button"
            class="primary"
            :disabled="busyId !== null"
            :aria-label="`Approve approval for ${row.title ?? row.command_id}`"
            @click="decide(row, 'grant')"
          >
            Approve
          </button>
          <button
            type="button"
            :disabled="busyId !== null"
            :aria-label="`Deny approval for ${row.title ?? row.command_id}`"
            @click="decide(row, 'deny')"
          >
            Deny
          </button>
        </div>
        <div
          v-if="expanded === row.id"
          class="inbox-detail"
        >
          <p v-if="!detail">Loading detail…</p>
          <template v-else>
            <p v-if="detail.args"><strong>Arguments</strong><code>{{ JSON.stringify(detail.args) }}</code></p>
            <p v-if="detail.preview"><strong>Preview</strong><code>{{ JSON.stringify(detail.preview) }}</code></p>
          </template>
        </div>
      </li>
    </ul>
  </section>
</template>

<style scoped>
.approval-inbox { display: grid; gap: .75rem; }
.inbox-state { color: #56675d; padding: .5rem 0; }
.inbox-list { list-style: none; margin: 0; padding: 0; display: grid; gap: .6rem; }
.inbox-row { border: 1px solid #9db2a4; border-radius: .45rem; padding: .7rem; display: grid; gap: .5rem; }
.inbox-row-head { display: flex; align-items: baseline; gap: .7rem; flex-wrap: wrap; }
.inbox-expires { color: #56675d; font-size: 12px; }
.inbox-run { color: #56675d; font-size: 12px; border: 1px solid #9db2a4; border-radius: 999px; padding: 0 .5rem; }
.inbox-preview { display: grid; grid-template-columns: max-content 1fr; gap: .2rem .8rem; font-size: 13px; margin: .25rem 0; }
.inbox-preview dt { color: #56675d; }
.inbox-preview dd { margin: 0; }
.inbox-actions { display: flex; gap: .5rem; }
.inbox-detail { border-top: 1px solid #e4eee7; padding-top: .5rem; display: grid; gap: .5rem; }
.inbox-detail code { white-space: pre-wrap; word-break: break-word; font-size: 12px; }
</style>
