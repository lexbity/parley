<script setup lang="ts">
/* global MouseEvent, HTMLElement, EventTarget, KeyboardEvent */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from 'vue';
import { ApiError, parley, type AdminOperation, type GraphEntity, type Material, type OpportunitySummary, type ProviderConfig, type ProviderPairing, type ProviderSettings, type StructuredReadiness, type Resource, type SavedSource } from '../api/client';
import type { CommandDescriptor, CommandManifest, CommandSuccess } from '../api/commands';
import { tabs } from './tabs';
import { resolveRoute, routeForTab } from './router';
import CommandPalette from './CommandPalette.vue';
import { deriveContext, type QueuePaletteBinding } from './commands/context';
import { CommandManifestSource } from './commands/manifest';
import { sessionEventReader } from './commands/events';
import { inferenceActivity } from './inference/activity';
import { builtInNavigationCommands, createCommandRegistry, installCommandRegistry, mergeDescriptors, registerScoped } from './commands/registry';
import ResourcesWorkspace from '../features/resources/ResourcesWorkspace.vue';
import InterviewerWorkspace from '../features/resources/InterviewerWorkspace.vue';
import QueueWorkspace from '../features/queue/QueueWorkspace.vue';
import PartyDesk from '../features/parties/PartyDesk.vue';
import AgentPolicySettings from '../features/settings/AgentPolicySettings.vue';
import DelegatedRuns from '../features/settings/DelegatedRuns.vue';
import ApprovalInbox from '../features/settings/ApprovalInbox.vue';
import MaintenanceSettings from '../features/settings/MaintenanceSettings.vue';
import CapacitySettings from '../features/settings/CapacitySettings.vue';
import Packs from '../features/settings/Packs.vue';
import OutputWorkspace from '../features/output/OutputWorkspace.vue';

/**
 * Tabs whose owner journey is qualified stay discoverable (RFC-0003 S01);
 * the mandated order and the full destination set come from tabs.ts (APP-01).
 */
const qualifiedTabs = tabs;
const route = ref(resolveRoute(globalThis.location.pathname));
const active = computed(() => route.value.tab);
// These collections are replaced wholesale after a read, never mutated in
// place. shallowRef keeps Vue from deep-proxying tens of thousands of records
// on every tab activation; children receive new array identities on refresh.
const parties = shallowRef<GraphEntity[]>([]);
const openedTabs = ref(new Set([route.value.tab]));
const interviewerOpen = ref(false);
const interviewerMounted = ref(false);
const interviewerToggle = ref<HTMLElement | null>(null);
const interviewerPanel = ref<HTMLElement | null>(null);
function toggleInterviewer() {
  interviewerOpen.value = !interviewerOpen.value;
  if (interviewerOpen.value) interviewerMounted.value = true;
  void nextTick(() => (interviewerOpen.value ? interviewerPanel.value : interviewerToggle.value)?.focus());
}
const resourcesWorkspace = ref<InstanceType<typeof ResourcesWorkspace> | null>(null);
const partyDesk = ref<InstanceType<typeof PartyDesk> | null>(null);
const lastTabPaths = new Map<string, string>();
const sources = shallowRef<SavedSource[]>([]);
const opportunities = shallowRef<OpportunitySummary[]>([]);
const outputs = shallowRef<Material[]>([]);
const outputError = ref('');
const resources = shallowRef<Resource[]>([]);
const status = ref('');
const error = ref('');
const busy = ref(false);
const connected = ref(false);
const provider = ref<ProviderSettings>({ endpoint: 'http://127.0.0.1:11434', model: '', num_ctx: 8192, num_predict: 1024, locale: 'en' });
const pairing = ref<ProviderPairing>({ provider_version: '', model_digest: '', tokenizer_digest: '', template_tokens: 0 });
const configurePairing = ref(false);
const providerHealth = ref('');
const structuredReadiness = ref<StructuredReadiness | null>(null);
const profileRevision = ref<number | null>(null);
const maxTemplateTokens = computed(() => provider.value.num_ctx - Math.ceil(provider.value.num_ctx / 10) - provider.value.num_predict - 1);
const providerChecking = computed(() => structuredReadiness.value?.status === 'queued' || structuredReadiness.value?.status === 'running');
const providerStatus: Readonly<Record<StructuredReadiness['status'], string>> = {
  unqualified: 'Coach requires a qualified local pairing.',
  queued: 'Structured-output check queued.',
  running: 'Checking structured output.',
  probe_succeeded: 'Structured-output check passed. Follow the setup instructions to finish qualification.',
  failed: 'Structured-output check failed. Review the local installation and pairing.',
  ready: 'Structured output is ready for Coach.',
};
let providerRequest: InstanceType<typeof globalThis.AbortController> | undefined;
let stopProviderProbe: (() => void) | undefined;
const workspaceRefreshRequired = ref(false);
const settingsSections = [
  { id: 'settings-provider', label: 'Local provider' },
  { id: 'settings-agent-policy', label: 'Role policy' },
  { id: 'settings-delegated-runs', label: 'Delegated runs' },
  { id: 'settings-capacity', label: 'Capacity ledger' },
  { id: 'settings-packs', label: 'Packs' },
  { id: 'settings-data', label: 'Backup and data' },
];
let adminChannel: InstanceType<typeof globalThis.BroadcastChannel> | null = null;

// --- Command palette (UCR-001 U01). The shell owns one registry and one
// cached manifest source; selection and phase facts come from the active view.
const paletteOpen = ref(false);
const queueBinding = ref<QueuePaletteBinding | null>(null);
const paletteCommands = ref<CommandDescriptor[]>([]);
const serverManifest = ref<CommandManifest | null>(null);
const registry = createCommandRegistry((path) => navigate(path));
registerScoped(registry, builtInNavigationCommands((path) => navigate(path)));
const disposeRegistry = installCommandRegistry(registry);
const manifestSource = new CommandManifestSource({
  context: () => commandContext.value,
  epoch: () => parley.workspaceEpoch,
});
// Authoritative task index for the selected opportunity (UCR-001 U04). The
// palette never accepts a byte span typed by the user; the picker options
// come exclusively from this bounded notes read.
const queueTasks = ref<Array<{ value: string; label: string; byteStart: number; byteEnd: number; checked: boolean }>>([]);
async function loadQueueTasks() {
  const id = queueBinding.value?.opportunityId;
  if (!id) { queueTasks.value = []; return; }
  try {
    const notes = await parley.getOpportunityNotes(id);
    queueTasks.value = (notes.tasks ?? []).map((task) => ({
      // The line digest is equal for identical task lines; the composite token
      // carries the unique occurrence span and the notes revision so dispatch
      // needs no lookup and can never use a stale revision.
      value: `${task.byte_start}:${task.byte_end}:${task.line_sha256}:${notes.revision}`,
      label: task.text,
      byteStart: task.byte_start,
      byteEnd: task.byte_end,
      lineSha256: task.line_sha256,
      checked: task.checked,
    }));
  } catch {
    queueTasks.value = [];
  }
}
watch(() => queueBinding.value?.opportunityId, () => { void loadQueueTasks(); });
const selection = computed(() => ({
  opportunity: route.value.tab === 'Queue' ? queueBinding.value?.opportunityId : null,
  party: route.value.tab === 'Parties' ? route.value.recordId : null,
  material: route.value.tab === 'Output' ? route.value.recordId : null,
  resource: route.value.tab === 'Resources' ? route.value.recordId : null,
}));
const commandContext = computed(() => deriveContext(route.value, selection.value));
const intentFingerprint = computed(() => [
  parley.workspaceEpoch ?? '',
  queueBinding.value?.opportunityId ?? '',
  queueBinding.value?.phaseRevision ?? 0,
  queueBinding.value?.pack.id ?? '',
  queueBinding.value?.pack.version ?? '',
  queueBinding.value?.originalEventId ?? '',
  queueBinding.value?.notesRevision ?? 0,
].join('|'));
const pickerOptions = computed(() => ({
  planning_stage: (queueBinding.value?.stages ?? []).map((stage) => ({ value: stage.value, label: stage.label })),
  // The same authoritative directory the owner create form uses; the palette
  // never accepts a party ID typed by the user or supplied by an agent.
  party: parties.value.map((party) => ({ value: party.id, label: party.name })),
  // Reviewed resource subtypes (U06): the palette only offers the closed set
  // the boot-validated command schema accepts.
  resource_subtype: [
    { value: 'gift', label: 'Gift' },
    { value: 'proof', label: 'Proof' },
    { value: 'artifact', label: 'Artifact' },
    { value: 'skill', label: 'Skill' },
    { value: 'interest', label: 'Interest' },
    { value: 'note', label: 'Note' },
  ],
  // The task picker reads the indexed tasks of the selected opportunity. The
  // option value is the line digest; the typed span travels with the frozen
  // selection, never as user-typed text.
  opportunity_task: queueTasks.value.map((task) => ({
    value: task.value,
    label: task.checked ? `${task.label} (done)` : task.label,
  })),
}));

function syncPaletteCommands() {
  paletteCommands.value = mergeDescriptors(
    (serverManifest.value?.commands ?? []).filter(command =>
      command.id !== 'opportunity.set_phase' && command.id !== 'opportunity.undo_phase_change'),
    registry.list({ context: commandContext.value, selection: selection.value }),
  );
}

function applyManifest(manifest: CommandManifest) {
  serverManifest.value = manifest;
  syncPaletteCommands();
}

watch(commandContext, () => {
  manifestSource.invalidate();
  syncPaletteCommands();
}, { deep: true });
watch(queueBinding, syncPaletteCommands);

async function revalidatePalette(): Promise<void> {
  applyManifest(await manifestSource.revalidate());
}

function onPaletteCommitted(result: CommandSuccess) {
  status.value = result.dry_run ? 'Command dry run completed.' : 'Command committed and visible records refreshed.';
  void refreshTab(active.value, true);
  sessionEventReader.hint();
  globalThis.dispatchEvent(new globalThis.CustomEvent('parley:command-committed', { detail: result }));
}

function onQueueBinding(binding: QueuePaletteBinding) {
  const previous = queueBinding.value;
  queueBinding.value = binding;
  if (previous?.opportunityId !== binding.opportunityId
    || previous?.phaseRevision !== binding.phaseRevision) manifestSource.invalidate();
}

function isEditable(target: EventTarget | null): boolean {
  const element = target as HTMLElement | null;
  return !!element && (element.isContentEditable === true || ['INPUT', 'TEXTAREA', 'SELECT'].includes(element.tagName));
}

/** A panel or dialog already owns Escape/chords; the palette does not pre-empt it. */
function hasDialogPrecedence(): boolean {
  const document = globalThis.document;
  if (interviewerOpen.value) return true;
  if (!document) return false;
  return [...document.querySelectorAll('dialog[open]:not(.command-palette), [role="alertdialog"]')]
    .some((element) => {
      const node = element as HTMLElement;
      if (node.hidden || element.getAttribute('aria-hidden') === 'true') return false;
      return globalThis.getComputedStyle?.(node).display !== 'none';
    });
}

function togglePalette() {
  if (!connected.value || hasDialogPrecedence()) return;
  paletteOpen.value = !paletteOpen.value;
  if (paletteOpen.value) {
    void manifestSource.refresh().then(applyManifest)
      .catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Command catalog is unavailable.'; });
    void loadQueueTasks();
  }
}

function onGlobalKeydown(event: KeyboardEvent) {
  const key = event.key.toLowerCase();
  const modifier = event.metaKey || event.ctrlKey;
  const quickKey = modifier && event.shiftKey && key === 'p';
  const searchKey = modifier && !event.shiftKey && key === 'k' && !isEditable(event.target);
  if (!event.defaultPrevented && !event.isComposing && !event.repeat && !event.altKey && (quickKey || searchKey)) {
    if (!connected.value || hasDialogPrecedence()) return;
    event.preventDefault();
    togglePalette();
    return;
  }
  if (event.key === 'Escape' && interviewerOpen.value && !paletteOpen.value) toggleInterviewer();
}

// A tab switch is the synchronization point for changes made outside this
// shell (delegated runs, API clients, another browser tab). One bounded event
// read decides whether the large collections are stale; when nothing changed
// we keep the current arrays instead of re-reading two multi-megabyte
// endpoints on every activation.
let lastObservedCursor = '0';
const loadedTabs = new Set<string>();
async function workspaceChanged(): Promise<boolean> {
  try {
    const page = await parley.getEvents(lastObservedCursor);
    lastObservedCursor = page.observed_cursor || page.next_cursor || lastObservedCursor;
    return page.items.length > 0;
  } catch {
    return true; // fail open: an unreadable event stream must not hide records
  }
}

let tabRequest = 0;
async function refreshTab(tab = active.value, force = false) {
  const request = ++tabRequest;
  if (tab === 'Output') outputError.value = '';
  try {
    if (!force && loadedTabs.has(tab) && !(await workspaceChanged())) return;
    if (tab === 'Parties') {
      const result = await Promise.all([parley.listParties(), parley.listSources(), parley.listOpportunities()]);
      if (request === tabRequest) [parties.value, sources.value, opportunities.value] = result;
    } else if (tab === 'Queue') {
      const result = await Promise.all([parley.listParties(), parley.listOutputs()]);
      if (request === tabRequest) [parties.value, outputs.value] = result;
    } else if (tab === 'Output') {
      const result = await Promise.all([parley.listOpportunities(), parley.listOutputs(), parley.listResources()]);
      if (request === tabRequest) [opportunities.value, outputs.value, resources.value] = result;
    } else if (tab === 'Settings') {
      const [directory] = await Promise.all([parley.listParties(), loadProvider()]);
      if (request === tabRequest) parties.value = directory;
    }
    if (request === tabRequest) loadedTabs.add(tab);
  } catch (reason) {
    if (request === tabRequest) {
      const message = reason instanceof Error ? reason.message : `Could not load ${tab}.`;
      if (tab === 'Output') outputError.value = message;
      else error.value = message;
    }
  }
}

function onCollectionChanged() {
  void refreshTab(active.value, true);
}
function onOutputChanged() {
  void refreshTab('Output', true);
}

let historyIndex = Number(globalThis.history.state?.parleyIndex ?? 0);
let returningFromPop = false;
let allowPop = false;

function navigate(path: string, replace = false) {
  if (path === route.value.path && !replace) return;
  if (route.value.tab === 'Resources' && resourcesWorkspace.value) resourcesWorkspace.value.requestSwitch(() => commitNavigation(path, replace));
  else if (route.value.tab === 'Parties' && partyDesk.value && path !== route.value.path) partyDesk.value.requestSwitch(() => commitNavigation(path, replace));
  else commitNavigation(path, replace);
}
function applyNavigation(path: string) {
  lastTabPaths.set(route.value.tab, route.value.path);
  const next = resolveRoute(path);
  openedTabs.value.add(next.tab);
  route.value = next;
}
function commitNavigation(path: string, replace: boolean) {
  const next = resolveRoute(path);
  if (replace) globalThis.history.replaceState({ parleyIndex: historyIndex }, '', next.path);
  else if (next.path !== route.value.path) globalThis.history.pushState({ parleyIndex: ++historyIndex }, '', next.path);
  applyNavigation(next.path);
}

function selectTab(tab: (typeof tabs)[number], event: MouseEvent) {
  if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
  event.preventDefault();
  if (tab === route.value.tab) return;
  navigate(lastTabPaths.get(tab) ?? routeForTab(tab));
}

function handlePopState() {
  if (returningFromPop) { returningFromPop = false; return; }
  const target = resolveRoute(globalThis.location.pathname);
  const targetIndex = Number(globalThis.history.state?.parleyIndex ?? historyIndex);
  const delta = targetIndex - historyIndex;
  const apply = () => {
    historyIndex = targetIndex;
    applyNavigation(target.path);
    if (target.path !== globalThis.location.pathname) globalThis.history.replaceState({ parleyIndex: historyIndex }, '', target.path);
  };
  if (allowPop) { allowPop = false; apply(); return; }
  const guarded = route.value.tab === 'Resources' ? resourcesWorkspace.value : route.value.tab === 'Parties' ? partyDesk.value : null;
  if (!guarded?.hasDraft) { apply(); return; }
  if (delta) {
    returningFromPop = true;
    globalThis.history.go(-delta);
    guarded.requestSwitch(() => {
      allowPop = true;
      globalThis.history.go(delta);
    });
  } else {
    globalThis.history.replaceState({ parleyIndex: historyIndex }, '', route.value.path);
    guarded.requestSwitch(() => commitNavigation(target.path, true));
  }
}

async function focusRouteLocation() {
  if (route.value.tab !== 'Queue') return;
  await nextTick();
  const targetId = route.value.location === 'inbox' ? 'leads-inbox' : null;
  if (!targetId) return;
  const target = globalThis.document.getElementById(targetId);
  target?.focus();
}

function goToSettings(id: string) {
  globalThis.document.getElementById(id)?.focus();
}

function observeProvider(profile: Pick<ProviderConfig, 'profile_revision' | 'structured'>) {
  stopProviderProbe?.();
  structuredReadiness.value = profile.structured;
  if (!providerChecking.value) return;
  stopProviderProbe = inferenceActivity.watchStructuredProbe(
    profile.profile_revision,
    next => {
      structuredReadiness.value = next;
      providerHealth.value = '';
    },
    () => {
      structuredReadiness.value = null;
      error.value = 'The saved profile changed in another session. Reload the saved profile to review it.';
    },
    () => { providerHealth.value = 'The probe status is temporarily unavailable.'; },
  );
}

async function loadProvider() {
  providerRequest?.abort();
  const request = new globalThis.AbortController();
  providerRequest = request;
  const epoch = parley.workspaceEpoch;
  try {
    const profile = await parley.getProvider(request.signal);
    if (request.signal.aborted || epoch !== parley.workspaceEpoch || active.value !== 'Settings') return;
    const { profile_revision, structured, pairing: selectedPairing, ...fields } = profile;
    provider.value = fields;
    pairing.value = selectedPairing ?? { provider_version: '', model_digest: '', tokenizer_digest: '', template_tokens: 0 };
    configurePairing.value = selectedPairing !== null;
    profileRevision.value = profile_revision;
    providerHealth.value = '';
    observeProvider({ profile_revision, structured });
  } catch (reason) {
    if (!request.signal.aborted) error.value = reason instanceof Error ? reason.message : 'Could not load provider settings.';
  }
}

async function saveProvider() {
  if (profileRevision.value === null || busy.value) return;
  busy.value = true; error.value = ''; providerHealth.value = '';
  try {
    await parley.updateProvider({
      ...provider.value,
      expected_profile_revision: profileRevision.value,
      ...(configurePairing.value ? { pairing: pairing.value } : {}),
    });
    await loadProvider();
    status.value = 'Local provider settings saved. Check model readiness before starting inference.';
  } catch (reason) {
    error.value = reason instanceof ApiError && reason.status === 412
      ? 'The saved profile changed in another session. Reload the saved profile to review it.'
      : reason instanceof Error ? reason.message : 'Could not save provider settings.';
  } finally { busy.value = false; }
}

async function checkProvider() {
  if (profileRevision.value === null || busy.value || providerChecking.value) return;
  busy.value = true; error.value = ''; providerHealth.value = '';
  const revision = profileRevision.value;
  const epoch = parley.workspaceEpoch;
  try {
    const result = await parley.checkProvider();
    if (active.value !== 'Settings' || revision !== profileRevision.value || epoch !== parley.workspaceEpoch) return;
    if (result.structured) {
      observeProvider({ profile_revision: revision, structured: result.structured });
    } else {
      providerHealth.value = result.message;
      if (!result.available) error.value = result.message;
    }
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not check the local model.'; }
  finally { busy.value = false; }
}

async function applyWorkspaceChange(operation: AdminOperation, epoch: string, announce: boolean) {
  if (announce) adminChannel?.postMessage({ type: 'workspace-changed', operation, epoch });
  if (operation === 'restore' || operation === 'erase_workspace') workspaceRefreshRequired.value = true;
  try {
    if (operation === 'restore' || operation === 'erase_workspace') await parley.refreshSession();
    globalThis.dispatchEvent(new globalThis.CustomEvent('parley:workspace-changed', { detail: { operation, epoch } }));
    await refreshTab(active.value, true);
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Workspace records changed; reload the current workspace before continuing.'; }
}

function onWorkspaceQuarantined() { workspaceRefreshRequired.value = true; }
function onWorkspaceReloaded() { workspaceRefreshRequired.value = false; }

onMounted(() => {
  const connection = parley.connect();
  globalThis.history.replaceState({ parleyIndex: historyIndex }, '', route.value.path);
  globalThis.addEventListener('popstate', handlePopState);
  globalThis.addEventListener('keydown', onGlobalKeydown);
  globalThis.addEventListener('parley:workspace-quarantined', onWorkspaceQuarantined);
  globalThis.addEventListener('parley:workspace-reloaded', onWorkspaceReloaded);
  if ('BroadcastChannel' in globalThis) {
    adminChannel = new globalThis.BroadcastChannel('parley-workspace-admin');
    adminChannel.onmessage = (message) => {
      const value = message.data as { type?: unknown; operation?: unknown; epoch?: unknown } | null;
      if (value?.type === 'workspace-changed' && ['restore', 'erase_party', 'erase_workspace'].includes(String(value.operation))) {
        void applyWorkspaceChange(value.operation as AdminOperation, typeof value.epoch === 'string' ? value.epoch : '', false);
      }
    };
  }
  void connection
    .then(() => {
      connected.value = true;
      void manifestSource.refresh().then(applyManifest)
        .catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Command catalog is unavailable.'; });
    })
    .catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Workspace is unavailable.'; });
});
onBeforeUnmount(() => {
  globalThis.removeEventListener('popstate', handlePopState);
  globalThis.removeEventListener('keydown', onGlobalKeydown);
  manifestSource.stop();
  disposeRegistry();
  globalThis.removeEventListener('parley:workspace-quarantined', onWorkspaceQuarantined);
  globalThis.removeEventListener('parley:workspace-reloaded', onWorkspaceReloaded);
  adminChannel?.close();
  stopProviderProbe?.();
  providerRequest?.abort();
});
watch([connected, active], async ([ready, tab]) => {
  if (!ready) return;
  if (tab !== 'Settings') {
    stopProviderProbe?.();
    providerRequest?.abort();
  }
  error.value = '';
  await refreshTab(tab);
  if (active.value === tab) void focusRouteLocation();
});
watch(route, () => {
  void focusRouteLocation();
});
</script>

<template>
  <header class="app-header">
    <strong>Parley</strong>
    <nav aria-label="Main navigation">
      <a
        v-for="tab in qualifiedTabs"
        :key="tab"
        :href="lastTabPaths.get(tab) ?? routeForTab(tab)"
        :aria-current="active === tab ? 'page' : undefined"
        @click="selectTab(tab, $event)"
      >
        {{ tab }}
      </a>
    </nav>
    <button
      v-if="connected"
      class="open-palette"
      type="button"
      aria-haspopup="dialog"
      aria-keyshortcuts="Control+Shift+P Meta+Shift+P Control+K Meta+K"
      @click="togglePalette"
    >
      Commands
    </button>
    <p
      v-if="workspaceRefreshRequired"
      role="alert"
    >
      The workspace was replaced in another administrative operation. Dirty Queue notes are quarantined; other pending edits must be reviewed before reloading this session.
    </p>
  </header>
  <main>
    <div class="workspace-header">
      <h1>{{ active }}</h1>
      <button
        v-if="connected && active === 'Resources'"
        ref="interviewerToggle"
        type="button"
        :aria-expanded="interviewerOpen"
        aria-controls="resource-interviewer"
        @click="toggleInterviewer"
      >
        Interviewer
      </button>
    </div>
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
    <template v-if="connected">
      <div
        v-if="openedTabs.has('Resources')"
        v-show="active === 'Resources'"
        class="resource-workspace"
      >
        <ResourcesWorkspace
          ref="resourcesWorkspace"
          :route-id="route.tab === 'Resources' ? route.recordId : null"
          :active="active === 'Resources'"
          @navigate="navigate"
        />
        <aside
          v-if="interviewerMounted"
          v-show="interviewerOpen"
          id="resource-interviewer"
          ref="interviewerPanel"
          class="resource-agent"
          tabindex="-1"
          aria-label="Resource Interviewer"
          @keydown.esc.stop="toggleInterviewer"
        >
          <button
            type="button"
            class="close-agent"
            @click="toggleInterviewer"
          >
            Close Interviewer
          </button>
          <InterviewerWorkspace
            :columns="resourcesWorkspace?.columns"
            @saved="resourcesWorkspace?.refresh()"
          />
        </aside>
      </div>
      <PartyDesk
        v-if="openedTabs.has('Parties')"
        v-show="active === 'Parties'"
        ref="partyDesk"
        :parties="parties"
        :sources="sources"
        :opportunities="opportunities"
        :active="active === 'Parties'"
        :inbox="route.tab === 'Parties' && route.location === 'inbox'"
        :party-id="route.tab === 'Parties' ? route.recordId ?? undefined : undefined"
        @changed="onCollectionChanged"
        @navigate="navigate"
      />
      <section
        v-if="active === 'Settings'"
        class="settings-workspace"
        aria-label="Workspace settings"
      >
        <nav
          class="settings-index"
          aria-label="Settings sections"
        >
          <a
            v-for="section in settingsSections"
            :key="section.id"
            :href="'#' + section.id"
            @click.prevent="goToSettings(section.id)"
          >
            {{ section.label }}
          </a>
        </nav>
        <div class="settings-panels">
          <section
            id="settings-provider"
            tabindex="-1"
            aria-labelledby="settings-heading"
          >
            <h2 id="settings-heading">
              Local provider
            </h2>
            <p>Configure the local model. Coach requires a qualified structured-output pairing.</p>
            <form @submit.prevent="saveProvider">
              <label>Local endpoint<input
                v-model="provider.endpoint"
                required
                autocomplete="off"
              ></label>
              <label>Installed model tag<input
                v-model="provider.model"
                required
                autocomplete="off"
              ></label>
              <label>Context budget<input
                v-model.number="provider.num_ctx"
                type="number"
                min="2048"
                max="131072"
                required
              ></label>
              <label>Output budget<input
                v-model.number="provider.num_predict"
                type="number"
                min="64"
                max="8192"
                required
              ></label>
              <label>Evaluated language tag<input
                v-model="provider.locale"
                required
                maxlength="40"
              ></label>
              <fieldset class="provider-pairing">
                <legend>Coach pairing</legend>
                <label><input
                  v-model="configurePairing"
                  type="checkbox"
                > Configure structured output</label>
                <template v-if="configurePairing">
                  <p>Use the provisioned model and tokenizer identities from local setup.</p>
                  <label>Ollama version<input
                    v-model.trim="pairing.provider_version"
                    required
                    autocomplete="off"
                  ></label>
                  <label>Model digest<input
                    v-model.trim="pairing.model_digest"
                    pattern="[a-f0-9]{64}"
                    required
                    autocomplete="off"
                  ></label>
                  <label>Tokenizer digest<input
                    v-model.trim="pairing.tokenizer_digest"
                    pattern="[a-f0-9]{64}"
                    required
                    autocomplete="off"
                  ></label>
                  <label>Chat template token allowance<input
                    v-model.number="pairing.template_tokens"
                    type="number"
                    min="0"
                    :max="maxTemplateTokens"
                    required
                  ></label>
                </template>
              </fieldset>
              <div class="provider-actions">
                <button
                  class="primary"
                  type="submit"
                  :disabled="busy || profileRevision === null"
                >
                  Save provider
                </button>
                <button
                  type="button"
                  :disabled="busy || profileRevision === null || providerChecking"
                  @click="checkProvider"
                >
                  Check local model readiness
                </button>
                <button
                  type="button"
                  :disabled="busy"
                  @click="loadProvider"
                >
                  Reload saved profile
                </button>
              </div>
              <p
                v-if="structuredReadiness"
                class="provider-health"
                role="status"
              >
                {{ providerStatus[structuredReadiness.status] }}
                <span v-if="structuredReadiness.diagnostic"> {{ structuredReadiness.diagnostic }}</span>
                <span v-if="structuredReadiness.checked_at"> Checked {{ structuredReadiness.checked_at }}.</span>
              </p>
              <p
                v-if="providerHealth"
                class="provider-health"
                role="status"
              >
                {{ providerHealth }}
              </p>
            </form>
          </section>
          <AgentPolicySettings
            id="settings-agent-policy"
            tabindex="-1"
          />
          <DelegatedRuns
            id="settings-delegated-runs"
            tabindex="-1"
            :navigate="navigate"
          />
          <ApprovalInbox
            id="settings-approval-inbox"
            tabindex="-1"
          />
          <CapacitySettings
            id="settings-capacity"
            tabindex="-1"
          />
          <Packs
            id="settings-packs"
            tabindex="-1"
          />
          <MaintenanceSettings
            id="settings-data"
            tabindex="-1"
            :parties="parties"
            @erased="onCollectionChanged"
          />
        </div>
      </section>
      <div
        v-if="openedTabs.has('Queue')"
        v-show="active === 'Queue'"
      >
        <QueueWorkspace
          :parties="parties"
          :outputs="outputs"
          :active="active === 'Queue'"
          :route-id="route.tab === 'Queue' ? route.recordId : null"
          @binding="onQueueBinding"
          @navigate="navigate"
        />
      </div>
      <div
        v-if="openedTabs.has('Output')"
        v-show="active === 'Output'"
        class="output-workspace"
      >
        <p
          v-if="outputError"
          role="alert"
          class="output-notice"
        >
          {{ outputError }}
          <button
            type="button"
            @click="refreshTab('Output', true)"
          >
            Retry
          </button>
        </p>
        <!-- The workspace stays mounted across registry refreshes so unsaved
             working drafts and open reviews survive a committed change. -->
        <OutputWorkspace
          :opportunities="opportunities"
          :resources="resources"
          :outputs="outputs"
          :active="active === 'Output'"
          :record-id="route.tab === 'Output' ? route.recordId : null"
          :location="route.tab === 'Output' ? route.location : null"
          @changed="onOutputChanged"
          @navigate="navigate"
        />
      </div>
    </template>
    <p
      v-else-if="!error"
      role="status"
    >
      Opening workspace…
    </p>
  </main>
  <CommandPalette
    :open="paletteOpen"
    :descriptors="paletteCommands"
    :context="commandContext"
    :selection="selection"
    :picker-options="pickerOptions"
    :intent-fingerprint="intentFingerprint"
    :epoch="parley.workspaceEpoch ?? ''"
    :revalidate="revalidatePalette"
    @close="paletteOpen = false"
    @committed="onPaletteCommitted"
  />
</template>

<style>
:root { color: #182d25; background: #f4f6f2; font: 14px/1.45 system-ui, sans-serif; }
* { box-sizing: border-box; }
body { margin: 0; }
.app-header { display: flex; align-items: stretch; gap: clamp(1rem, 4vw, 4rem); padding-inline: 3vw; background: white; border-bottom: 1px solid #cdd7cf; }
.app-header > strong { align-self: center; font: italic 1.8rem Georgia, serif; }
.app-header nav { display: flex; overflow-x: auto; }
.open-palette { align-self: center; margin-inline-start: auto; min-height: 2.5rem; }
.app-header a { display: grid; align-items: center; padding: .95rem clamp(.45rem, 1.3vw, 1.4rem); text-decoration: none; color: #4c5b53; border-bottom: 3px solid transparent; }
.app-header a[aria-current="page"] { border-color: #28664f; color: #194e39; font-weight: 650; }
main { max-width: 1640px; margin: auto; padding: 1rem 3vw 3rem; }
h1 { margin: 0 0 .75rem; font: normal clamp(2rem, 3vw, 3rem)/1.15 Georgia, serif; letter-spacing: -.035em; }
h2, h3, h4 { margin: .15em 0 .5em; line-height: 1.25; }
h2 { font-size: 1.18rem; } h3 { font-size: 1.04rem; } p { margin: .55rem 0; }
button, input, select, textarea { font: inherit; color: inherit; }
button, summary { cursor: pointer; }
button { padding: .35rem .65rem; border-radius: .36rem; border: 1px solid #abbcb0; background: #fff; }
button:disabled { cursor: default; opacity: .58; }
button:not(:disabled):hover, .app-header a:hover { background: #eaf0eb; }
button:not(:disabled):active { background: #d6e5da; }
button[aria-pressed="true"], button[aria-current="true"] { background: #deebe2; box-shadow: inset 3px 0 #367757; }
:focus-visible { outline: 2px solid #32735b; outline-offset: 2px; }
.workspace-grid { display: grid; gap: .85rem; align-items: start; }
.workspace-grid { grid-template-columns: repeat(auto-fit,minmax(min(100%, 19rem),1fr)); }
form, section section, .record-card { min-width: 0; background: white; border: 1px solid #d1dbd2; border-radius: .45rem; padding: .8rem; }
label { display: grid; gap: .2rem; margin: .48rem 0; }
input, select, textarea { min-width: 0; width: 100%; padding: .38rem .5rem; border: 1px solid #95a59a; border-radius: .32rem; background: white; }
input:is([type="checkbox"],[type="radio"]) { width: auto; justify-self: start; accent-color: #276348; }
small { color: #58665f; } .record-card { margin: .55rem 0; overflow-wrap: anywhere; }
@media (prefers-reduced-motion: no-preference) {
  button, .app-header a, input, textarea, select { transition: background-color 120ms ease-out, border-color 120ms ease-out, box-shadow 120ms ease-out; }
}
@media (max-width: 38rem) {
  .app-header { flex-wrap: wrap; gap: 0; }
  .app-header nav { flex-basis: 100%; min-width: 0; order: 1; }
  .app-header a { justify-content: center; flex: 1 0 auto; padding-inline: .42rem; }
  .app-header > strong { margin-top: .35rem; }
}

:root { background:#f8f7f3; }
.app-header { min-height:54px; }
.workspace-header { display: flex; align-items: start; justify-content: space-between; gap: .75rem; }
.workspace-header > button { min-height: 2.5rem; }
.resource-workspace { position: relative; }
.resource-agent { position: absolute; z-index: 10; inset-inline-end: 0; inset-block-start: 0; width: min(31rem, 100%); max-height: min(46rem, 80dvh); overflow: auto; background: #fffdf7; border: 1px solid #abbcac; border-radius: .55rem; box-shadow: 0 9px 36px #173b2940; }
.resource-agent > .close-agent { display: block; margin: .65rem .75rem .25rem auto; }
.resource-agent .chat-pane { padding: 0; border: 0; background: transparent; }
.pane-bar {display:flex;align-items:center;justify-content:space-between;gap:.5rem;padding:.8rem 1rem;border-bottom:1px solid #dce2db;}
.pane-bar h2 {font-size:1rem;margin:0}
.pane-bar select {width:auto;max-width:100%}
.pane-bar small {text-transform:uppercase;font-size:.7rem;letter-spacing:.08em}
.primary,.queue-views button[aria-pressed=true] {background:#235e51;color:white;border-color:#235e51;}
.primary:not(:disabled):hover {background:#174b40}
.chat-pane {display:flex;flex-direction:column;min-width:0;}
.chat-prompt {margin:1rem;padding:.7rem;background:#f2f4ed;border-radius:.4rem}
.chat-options,.resource-directory,.graph-inspector {padding:.7rem 1rem;}
.start-chat {align-self:flex-end;margin:auto 1rem 1rem;}
.chat-pane .interviewer-grid {padding:.7rem;}
.sr-only {position:absolute;clip-path:inset(50%);height:1px;width:1px;overflow:hidden;white-space:nowrap;}
@media(max-width:800px){.chat-pane {min-height:18rem}}

.chat-composer {margin-top:auto;padding:.8rem;border:0;border-top:1px solid #dce2db;border-radius:0;}
.chat-composer textarea {resize:vertical;}
.chat-composer > div {display:flex;gap:.5rem;align-items:center}
.chat-composer .primary {margin-left:auto;}
.resource-directory,.graph-inspector {border-top:1px solid #dce2db;}
</style>

<style scoped>
.output-workspace, .settings-workspace { --line: #c7d1c3; --panel: #fffef9; --muted: #53665b; }
.output-notice { padding: .7rem 1rem; margin: 0; overflow-wrap: anywhere; color: var(--muted); }
.output-notice h3 { color: #182d25; }
.settings-workspace { display: grid; grid-template-columns: minmax(11rem, 13rem) minmax(0, 1fr); align-items: start; gap: 1rem; }
.settings-index { position: sticky; top: .75rem; display: grid; gap: .15rem; padding: .45rem; background: var(--panel); border: 1px solid var(--line); border-radius: .55rem; }
.settings-index a { display: block; padding: .6rem .7rem; color: #225744; border-radius: .3rem; text-decoration: none; }
.settings-index a:hover { background: #edf2e8; text-decoration: underline; }
.settings-panels { min-width: 0; display: grid; gap: .85rem; }
.settings-panels > section { margin: 0; padding: clamp(.85rem, 2vw, 1.3rem); border: 1px solid var(--line); border-radius: .55rem; background: var(--panel); scroll-margin-top: .75rem; overflow-wrap: anywhere; }
.settings-panels > section > :is(h2, h3):first-child { margin: 0 0 .6rem; font: 1.65rem/1.2 Georgia, serif; }
#settings-provider form { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: .6rem .75rem; border: 0; background: transparent; padding: 0; }
#settings-provider label { margin: 0; }
#settings-provider label:first-child { grid-column: 1 / -1; }
#settings-provider input, #settings-provider button { min-height: 2.5rem; }
.provider-actions { grid-column: 1 / -1; display: flex; flex-wrap: wrap; gap: .45rem; }
.provider-health, .provider-pairing { grid-column: 1 / -1; }
.provider-pairing { display: grid; gap: .5rem; min-width: 0; }
.provider-pairing input[type="checkbox"] { min-height: auto; }
@media (max-width: 680px) {
  .settings-workspace { grid-template-columns: minmax(0, 1fr); }
  .settings-index { position: static; display: flex; flex-wrap: wrap; }
}
@media (max-width: 430px) {
  #settings-provider form { grid-template-columns: minmax(0, 1fr); }
}
</style>
