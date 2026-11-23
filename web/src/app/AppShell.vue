<script setup lang="ts">
/* global MouseEvent, HTMLElement, EventTarget, KeyboardEvent */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { parley, type AdminOperation, type ContactThread, type GraphEntity, type Material, type Opportunity, type ProviderConfig, type ReleasedMaterial, type Resource, type SavedSource } from '../api/client';
import type { CommandDescriptor, CommandManifest, CommandSuccess } from '../api/commands';
import { tabs } from './tabs';
import { resolveRoute, routeForTab } from './router';
import CommandPalette from './CommandPalette.vue';
import { deriveContext, type QueuePaletteBinding } from './commands/context';
import { CommandManifestSource } from './commands/manifest';
import { sessionEventReader } from './commands/events';
import { builtInNavigationCommands, createCommandRegistry, installCommandRegistry, mergeDescriptors, registerScoped } from './commands/registry';
import ResourcesWorkspace from '../features/resources/ResourcesWorkspace.vue';
import InterviewerWorkspace from '../features/resources/InterviewerWorkspace.vue';
import QueueWorkspace from '../features/queue/QueueWorkspace.vue';
import PartyDesk from '../features/parties/PartyDesk.vue';
import AgentPolicySettings from '../features/settings/AgentPolicySettings.vue';
import DelegatedRuns from '../features/settings/DelegatedRuns.vue';
import MaintenanceSettings from '../features/settings/MaintenanceSettings.vue';
import CapacitySettings from '../features/settings/CapacitySettings.vue';
import Packs from '../features/settings/Packs.vue';
import ContactThreadPanel from '../features/queue/ContactThreadPanel.vue';
import WritingWorkspace from '../features/output/WritingWorkspace.vue';
import MaterialActions from '../features/output/MaterialActions.vue';

/**
 * Tabs whose owner journey is qualified stay discoverable (RFC-0003 S01);
 * the mandated order and the full destination set come from tabs.ts (APP-01).
 */
const qualifiedTabs = tabs;
const route = ref(resolveRoute(globalThis.location.pathname));
const active = computed(() => route.value.tab);
const writingOpen = ref(false);
const outputStatus = ref('');
const openedMaterials = ref(new Set<string>());
const parties = ref<GraphEntity[]>([]);
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
const sources = ref<SavedSource[]>([]);
const opportunities = ref<Opportunity[]>([]);
const outputs = ref<Material[]>([]);
const materialQuery = ref('');
const visibleOutputs = computed(() => {
  const query = materialQuery.value.trim().toLocaleLowerCase();
  return outputs.value.filter(material => {
    if (outputStatus.value && outputStatus.value !== material.status) return false;
    const opportunity = opportunities.value.find(item => item.id === material.opportunity_id);
    return [material.recipient_name, opportunity?.title, material.kind, material.status]
      .some(field => field?.toLocaleLowerCase().includes(query));
  });
});
const resources = ref<Resource[]>([]);
const threads = ref<ContactThread[]>([]);
const deliveredReleases = ref<Record<string, ReleasedMaterial>>({});
let outputThreadTimer: ReturnType<typeof globalThis.setInterval> | undefined;
let outputThreadReadInFlight = false;
const status = ref('');
const error = ref('');
const busy = ref(false);
const connected = ref(false);
const provider = ref<ProviderConfig>({ endpoint: 'http://127.0.0.1:11434', model: '', num_ctx: 8192, num_predict: 1024, locale: 'en' });
const providerHealth = ref('');
const workspaceRefreshRequired = ref(false);
let adminChannel: InstanceType<typeof globalThis.BroadcastChannel> | null = null;

// --- Command palette (UCR-001 U01). The shell owns one registry and one
// cached manifest source; selection and phase facts come from the active view.
const paletteOpen = ref(false);
const queueSelection = ref<string | null>(null);
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
const selection = computed(() => ({
  opportunity: queueSelection.value ?? (route.value.tab === 'Queue' ? route.value.recordId : null),
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
].join('|'));
const pickerOptions = computed(() => ({
  planning_stage: (queueBinding.value?.stages ?? []).map((stage) => ({ value: stage.value, label: stage.label })),
  // The same authoritative directory the owner create form uses; the palette
  // never accepts a party ID typed by the user or supplied by an agent.
  party: parties.value.map((party) => ({ value: party.id, label: party.name })),
}));

function syncPaletteCommands() {
  paletteCommands.value = mergeDescriptors(
    serverManifest.value?.commands ?? [],
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
  void refreshTab();
  sessionEventReader.hint();
  globalThis.dispatchEvent(new globalThis.CustomEvent('parley:command-committed', { detail: result }));
}

function onQueueBinding(binding: QueuePaletteBinding) {
  const previous = queueBinding.value;
  queueBinding.value = binding;
  queueSelection.value = binding.opportunityId;
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

function onGlobalKeydown(event: KeyboardEvent) {
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
    // Editable chord ownership: a focused editor keeps mod+k for itself.
    if (isEditable(event.target) || hasDialogPrecedence()) return;
    event.preventDefault();
    paletteOpen.value = !paletteOpen.value;
    if (paletteOpen.value) {
      void manifestSource.refresh().then(applyManifest)
        .catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Command catalog is unavailable.'; });
    }
    return;
  }
  if (event.key === 'Escape' && interviewerOpen.value && !paletteOpen.value) toggleInterviewer();
}

let tabRequest = 0;
async function refreshTab(tab = active.value) {
  const request = ++tabRequest;
  try {
    if (tab === 'Parties') {
      const result = await Promise.all([parley.listParties(), parley.listSources(), parley.listOpportunities()]);
      if (request === tabRequest) [parties.value, sources.value, opportunities.value] = result;
    } else if (tab === 'Queue') {
      const result = await Promise.all([parley.listParties(), parley.listOutputs()]);
      if (request === tabRequest) [parties.value, outputs.value] = result;
    } else if (tab === 'Output') {
      const result = await Promise.all([parley.listParties(), parley.listOpportunities(), parley.listOutputs(), parley.listResources(), parley.listThreads()]);
      if (request === tabRequest) [parties.value, opportunities.value, outputs.value, resources.value, threads.value] = result;
    } else if (tab === 'Settings') {
      const [directory] = await Promise.all([parley.listParties(), loadProvider()]);
      if (request === tabRequest) parties.value = directory;
    }
  } catch (reason) {
    if (request === tabRequest) error.value = reason instanceof Error ? reason.message : `Could not load ${tab}.`;
  }
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
  if (!resourcesWorkspace.value?.hasDraft || route.value.tab !== 'Resources') { apply(); return; }
  if (delta) {
    returningFromPop = true;
    globalThis.history.go(-delta);
    resourcesWorkspace.value.requestSwitch(() => {
      allowPop = true;
      globalThis.history.go(delta);
    });
  } else {
    globalThis.history.replaceState({ parleyIndex: historyIndex }, '', route.value.path);
    resourcesWorkspace.value.requestSwitch(() => commitNavigation(target.path, true));
  }
}

async function focusRouteLocation() {
  if (route.value.location === 'templates') writingOpen.value = true;
  if (route.value.tab === 'Output' && route.value.recordId) {
    openedMaterials.value.add(route.value.recordId);
    if (!visibleOutputs.value.some(item => item.id === route.value.recordId)) { materialQuery.value = ''; outputStatus.value = ''; }
  }
  await nextTick();
  const targetId = route.value.location === 'inbox' ? 'leads-inbox'
    : route.value.location === 'templates' ? 'output-templates'
      : route.value.tab === 'Output' && route.value.recordId ? `output-material-${route.value.recordId}` : null;
  if (!targetId) return;
  const target = globalThis.document.getElementById(targetId);
  target?.scrollIntoView({ block: 'center' });
  target?.focus();
}

async function loadProvider() {
  try { provider.value = await parley.getProvider(); } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not load provider settings.'; }
}

async function saveProvider() {
  busy.value = true; error.value = ''; providerHealth.value = '';
  try { await parley.updateProvider(provider.value); status.value = 'Local Extractor provider settings saved. Check model health before using extraction.'; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not save provider settings.'; }
  finally { busy.value = false; }
}

async function checkProvider() {
  busy.value = true; error.value = '';
  try { const result = await parley.checkProvider(); providerHealth.value = result.message; if (!result.available) error.value = result.message; }
  catch (reason) { error.value = reason instanceof Error ? reason.message : 'Could not check the local model.'; }
  finally { busy.value = false; }
}

function installMaterial(updated: Material) {
  const index = outputs.value.findIndex((item) => item.id === updated.id);
  if (index >= 0) outputs.value[index] = updated;
  if (deliveredReleases.value[updated.id]?.version !== updated.version) delete deliveredReleases.value[updated.id];
}

async function applyWorkspaceChange(operation: AdminOperation, epoch: string, announce: boolean) {
  if (announce) adminChannel?.postMessage({ type: 'workspace-changed', operation, epoch });
  if (operation === 'restore' || operation === 'erase_workspace') workspaceRefreshRequired.value = true;
  try {
    if (operation === 'restore' || operation === 'erase_workspace') await parley.refreshSession();
    globalThis.dispatchEvent(new globalThis.CustomEvent('parley:workspace-changed', { detail: { operation, epoch } }));
    await refreshTab();
  } catch (reason) { error.value = reason instanceof Error ? reason.message : 'Workspace records changed; reload the current workspace before continuing.'; }
}

async function pollOutputThreads() {
  if (active.value !== 'Output' || outputThreadReadInFlight || globalThis.document.visibilityState !== 'visible') return;
  outputThreadReadInFlight = true;
  try { threads.value = await parley.listThreads(); }
  catch { /* Keep the latest server-confirmed thread state available during transport recovery. */ }
  finally { outputThreadReadInFlight = false; }
}

function onOutputVisibilityChange() {
  if (globalThis.document.visibilityState === 'visible') void pollOutputThreads();
}

function onWorkspaceQuarantined() { workspaceRefreshRequired.value = true; }
function onWorkspaceReloaded() { workspaceRefreshRequired.value = false; }

onMounted(() => {
  const connection = parley.connect();
  globalThis.history.replaceState({ parleyIndex: historyIndex }, '', route.value.path);
  globalThis.addEventListener('popstate', handlePopState);
  globalThis.addEventListener('keydown', onGlobalKeydown);
  outputThreadTimer = globalThis.setInterval(() => { void pollOutputThreads(); }, 2_000);
  globalThis.document.addEventListener('visibilitychange', onOutputVisibilityChange);
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
  globalThis.document.removeEventListener('visibilitychange', onOutputVisibilityChange);
  globalThis.removeEventListener('parley:workspace-quarantined', onWorkspaceQuarantined);
  globalThis.removeEventListener('parley:workspace-reloaded', onWorkspaceReloaded);
  if (outputThreadTimer) globalThis.clearInterval(outputThreadTimer);
  adminChannel?.close();
});
watch([connected, active], async ([ready, tab]) => {
  if (!ready) return;
  error.value = '';
  await refreshTab(tab);
  if (active.value === tab) void focusRouteLocation();
});
watch(() => route.value.recordId, () => { void focusRouteLocation(); });
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
        @changed="refreshTab"
        @navigate="navigate"
      />
      <section
        v-if="active === 'Settings'"
        aria-labelledby="settings-heading"
      >
        <h2 id="settings-heading">
          Local agent settings
        </h2>
        <p>Extractor runs only through a loopback provider. Manual capture, review, and other workspace actions remain available when the model is unavailable.</p>
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
          <button
            type="submit"
            :disabled="busy"
          >
            Save provider
          </button>
          <button
            type="button"
            :disabled="busy"
            @click="checkProvider"
          >
            Check local model
          </button>
          <p
            v-if="providerHealth"
            role="status"
          >
            {{ providerHealth }}
          </p>
        </form>
        <AgentPolicySettings />
        <DelegatedRuns />
        <CapacitySettings />
        <Packs />
        <MaintenanceSettings
          :parties="parties"
          @erased="refreshTab"
        />
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
          @material-updated="installMaterial"
          @binding="onQueueBinding"
          @navigate="navigate"
        />
      </div>
      <section
        v-if="active === 'Output'"
        aria-labelledby="output-heading"
      >
        <h2
          id="output-heading"
          class="sr-only"
        >
          Approved materials
        </h2>
        <div class="office-tools">
          <button
            class="primary"
            :aria-expanded="writingOpen"
            @click="writingOpen = !writingOpen"
          >
            Create output
          </button>
          <button @click="writingOpen = true; navigate('/output/templates')">
            Manage templates
          </button>
          <input
            v-model="materialQuery"
            aria-label="Find materials"
            type="search"
            placeholder="Find material, opportunity or contact"
          >
          <select
            v-model="outputStatus"
            aria-label="Filter material status"
          >
            <option value="">
              All statuses
            </option><option
              v-for="state in ['draft','approved','released']"
              :key="state"
            >
              {{ state }}
            </option>
          </select>
        </div>
        <div
          class="registry"
          tabindex="0"
          role="region"
          aria-label="Material registry"
        >
          <table>
            <thead><tr><th>Material</th><th>Opportunity</th><th>Contact</th><th>Status</th><th>Release</th></tr></thead><tbody>
              <template
                v-for="material in visibleOutputs"
                :key="material.id"
              >
                <tr
                  :id="`output-material-${material.id}`"
                  tabindex="-1"
                >
                  <td>
                    <button @click="navigate('/output/' + encodeURIComponent(material.id))">
                      {{ material.kind }} · v{{ material.version }}
                    </button>
                  </td><td>
                    <button @click="navigate('/queue/' + encodeURIComponent(material.opportunity_id))">
                      {{ opportunities.find(item => item.id === material.opportunity_id)?.title || material.opportunity_id }}
                    </button>
                  </td><td>{{ material.recipient_name }}</td><td>{{ material.status }}</td><td>
                    <button @click="navigate('/output/' + encodeURIComponent(material.id))">
                      Review
                    </button>
                  </td>
                </tr>
                <tr
                  v-if="openedMaterials.has(material.id)"
                  v-show="route.recordId === material.id"
                >
                  <td colspan="5">
                    <button @click="navigate('/output')">
                      Close material
                    </button>
                    <MaterialActions
                      :material="material"
                      @updated="installMaterial"
                      @delivered="deliveredReleases[material.id] = $event"
                    />
                    <ContactThreadPanel
                      :opportunity-id="material.opportunity_id"
                      :material="material"
                      :delivered="deliveredReleases[material.id] ?? null"
                      :threads="threads"
                      @changed="refreshTab"
                    />
                  </td>
                </tr>
              </template>
              <tr v-if="!visibleOutputs.length">
                <td colspan="5">
                  {{ outputs.length ? 'No matching materials.' : 'No materials yet.' }}
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <WritingWorkspace
          v-show="writingOpen"
          :opportunities="opportunities"
          :parties="parties"
          :resources="resources"
          :outputs="outputs"
        />
      </section>
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
.workspace-grid, .lead-desk { display: grid; gap: .85rem; align-items: start; }
.workspace-grid { grid-template-columns: repeat(auto-fit,minmax(min(100%, 19rem),1fr)); }
.lead-desk { grid-template-columns: minmax(14rem, 1fr) minmax(0, 3fr); }
.lead-desk > section, .lead-desk aside, form, section section, .record-card { min-width: 0; background: white; border: 1px solid #d1dbd2; border-radius: .45rem; padding: .8rem; }
.directory-rows, .output-list { list-style: none; padding: 0; }
.directory-rows { max-height: 40vh; overflow: auto; }
.directory-rows button { width: 100%; text-align: left; }
label { display: grid; gap: .2rem; margin: .48rem 0; }
input, select, textarea { min-width: 0; width: 100%; padding: .38rem .5rem; border: 1px solid #95a59a; border-radius: .32rem; background: white; }
input:is([type="checkbox"],[type="radio"]) { width: auto; justify-self: start; accent-color: #276348; }
small { color: #58665f; } .record-card { margin: .55rem 0; overflow-wrap: anywhere; }
@media (prefers-reduced-motion: no-preference) {
  button, .app-header a, input, textarea, select { transition: background-color 120ms ease-out, border-color 120ms ease-out, box-shadow 120ms ease-out; }
}
@media (max-width: 54rem) { .lead-desk { grid-template-columns: 1fr; } }
@media (max-width: 38rem) { .app-header { flex-wrap: wrap; gap: 0; } .app-header nav { flex-basis: 100%; } .app-header > strong { margin-top: .35rem; } }

:root { background:#f8f7f3; }
.app-header { min-height:54px; }
.workspace-header { display: flex; align-items: start; justify-content: space-between; gap: .75rem; }
.workspace-header > button { min-height: 2.5rem; }
.resource-workspace { position: relative; }
.resource-agent { position: absolute; z-index: 10; inset-inline-end: 0; inset-block-start: 0; width: min(31rem, 100%); max-height: min(46rem, 80dvh); overflow: auto; background: #fffdf7; border: 1px solid #abbcac; border-radius: .55rem; box-shadow: 0 9px 36px #173b2940; }
.resource-agent > .close-agent { display: block; margin: .65rem .75rem .25rem auto; }
.resource-agent .chat-pane { padding: 0; border: 0; background: transparent; }
.lead-desk { grid-template-columns:minmax(12rem,.8fr) minmax(0,2.2fr) minmax(15rem,1fr); align-items:stretch; }
.lead-desk > section, .lead-desk aside, .lead-chat-column { background:white; border:1px solid #ccd5cc; border-radius:.55rem; padding:0; min-width:0; }
.lead-desk aside > section, .lead-chat-column section { border:0; border-radius:0; }
.lead-desk aside > section { padding:0; }
.lead-desk aside label,.lead-desk aside p { margin:.6rem; }
.pane-bar {display:flex;align-items:center;justify-content:space-between;gap:.5rem;padding:.8rem 1rem;border-bottom:1px solid #dce2db;}
.pane-bar h2 {font-size:1rem;margin:0}
.pane-bar select,.office-tools select,.office-tools input,.queue-filter-sort input {width:auto;max-width:100%}
.pane-bar small {text-transform:uppercase;font-size:.7rem;letter-spacing:.08em}
.primary,.queue-views button[aria-pressed=true] {background:#235e51;color:white;border-color:#235e51;}
.primary:not(:disabled):hover {background:#174b40}
.chat-pane {display:flex;flex-direction:column;min-width:0;}
.chat-prompt {margin:1rem;padding:.7rem;background:#f2f4ed;border-radius:.4rem}
.chat-options,.resource-directory,.graph-inspector,.evidence-strip,.lead-chat-column > details {padding:.7rem 1rem;}
.start-chat {align-self:flex-end;margin:auto 1rem 1rem;}
.chat-pane .interviewer-grid {padding:.7rem;}
.directory-rows li button {border:0;border-top:1px solid #e3e9e2;border-radius:0;padding:.55rem .85rem;}
.directory-rows small {display:block;}
.office-tools,.queue-views,.queue-filter-sort {display:flex;flex-wrap:wrap;align-items:center;gap:.45rem;margin:.5rem 0;}
.registry {background:white;border:1px solid #ccd5cc;border-radius:.5rem;padding:.6rem 1rem;overflow-x:auto}
.registry table {border-collapse:collapse;width:100%;text-align:left;}
.registry th,.registry td {padding:.65rem .4rem;border-bottom:1px solid #e0e5df}
.sr-only {position:absolute;clip-path:inset(50%);height:1px;width:1px;overflow:hidden;white-space:nowrap;}
@media(max-width:800px){.lead-desk{grid-template-columns:minmax(0,1fr)}.chat-pane {min-height:18rem}}

.chat-composer {margin-top:auto;padding:.8rem;border:0;border-top:1px solid #dce2db;border-radius:0;}
.chat-composer textarea {resize:vertical;}
.chat-composer > div {display:flex;gap:.5rem;align-items:center}
.chat-composer .primary {margin-left:auto;}
.lead-chat-column {display:flex;flex-direction:column;}
.lead-chat-column > .chat-pane {flex:1;padding:0;}
.lead-desk {min-height:min(73vh,52rem);}
.resource-directory,.graph-inspector {border-top:1px solid #dce2db;}
</style>
