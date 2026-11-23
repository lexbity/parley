<script setup lang="ts">
/* global MouseEvent, HTMLElement */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { parley, type AdminOperation, type ContactThread, type GraphEntity, type Material, type OpportunitySummary, type ProviderConfig, type ReleasedMaterial, type Resource, type SavedSource } from '../api/client';
import { tabs } from './tabs';
import { resolveRoute, routeForTab } from './router';
import ResourcesWorkspace from '../features/resources/ResourcesWorkspace.vue';
import InterviewerWorkspace from '../features/resources/InterviewerWorkspace.vue';
import QueueWorkspace from '../features/queue/QueueWorkspace.vue';
import PartyDesk from '../features/parties/PartyDesk.vue';
import AgentPolicySettings from '../features/settings/AgentPolicySettings.vue';
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
const writingMounted = ref(false);
const writingToggle = ref<HTMLElement | null>(null);
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
const opportunities = ref<OpportunitySummary[]>([]);
const outputs = ref<Material[]>([]);
const outputLoading = ref(false);
const outputLoaded = ref(false);
const outputError = ref('');
const materialQuery = ref('');
const materialSearch = ref<HTMLElement | null>(null);
const outputStates = ['draft', 'approved', 'released'] satisfies Material['status'][];
const opportunityTitles = computed(() => new Map(opportunities.value.map(item => [item.id, item.title])));
const reviewedMaterials = computed(() => outputs.value.filter(material => openedMaterials.value.has(material.id)));
const visibleOutputs = computed(() => {
  const query = materialQuery.value.trim().toLocaleLowerCase();
  return outputs.value.filter(material => {
    if (outputStatus.value && outputStatus.value !== material.status) return false;
    return [material.recipient_name, opportunityTitles.value.get(material.opportunity_id), material.kind, material.status, `v${material.version}`]
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
const settingsSections = [
  { id: 'settings-provider', label: 'Local provider' },
  { id: 'settings-agent-policy', label: 'Role policy' },
  { id: 'settings-capacity', label: 'Capacity ledger' },
  { id: 'settings-packs', label: 'Packs' },
  { id: 'settings-data', label: 'Backup and data' },
];
let adminChannel: InstanceType<typeof globalThis.BroadcastChannel> | null = null;

let tabRequest = 0;
async function refreshTab(tab = active.value) {
  const request = ++tabRequest;
  if (tab === 'Output') { outputLoading.value = true; outputError.value = ''; }
  try {
    if (tab === 'Parties') {
      const result = await Promise.all([parley.listParties(), parley.listSources(), parley.listOpportunities()]);
      if (request === tabRequest) [parties.value, sources.value, opportunities.value] = result;
    } else if (tab === 'Queue') {
      const result = await Promise.all([parley.listParties(), parley.listOutputs()]);
      if (request === tabRequest) [parties.value, outputs.value] = result;
    } else if (tab === 'Output') {
      const result = await Promise.all([parley.listOpportunities(), parley.listOutputs(), parley.listResources(), parley.listThreads()]);
      if (request === tabRequest) {
        [opportunities.value, outputs.value, resources.value, threads.value] = result;
        outputLoaded.value = true;
      }
    } else if (tab === 'Settings') {
      const [directory] = await Promise.all([parley.listParties(), loadProvider()]);
      if (request === tabRequest) parties.value = directory;
    }
  } catch (reason) {
    if (request === tabRequest) {
      const message = reason instanceof Error ? reason.message : `Could not load ${tab}.`;
      if (tab === 'Output') outputError.value = message;
      else error.value = message;
    }
  } finally {
    if (tab === 'Output' && request === tabRequest) outputLoading.value = false;
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
  if (route.value.location === 'templates') {
    showWriting();
    return;
  }
  if (route.value.tab === 'Output' && route.value.recordId) {
    writingOpen.value = false;
    openedMaterials.value.add(route.value.recordId);
  }
  await nextTick();
  const targetId = route.value.location === 'inbox' ? 'leads-inbox'
    : route.value.tab === 'Output' && route.value.recordId ? `output-review-${route.value.recordId}` : null;
  if (!targetId) return;
  const target = globalThis.document.getElementById(targetId);
  target?.focus();
}

function goToSettings(id: string) {
  globalThis.document.getElementById(id)?.focus();
}
function showWriting() {
  writingMounted.value = true;
  writingOpen.value = true;
  void nextTick(() => {
    const id = route.value.location === 'templates' ? 'output-templates' : 'writing-heading';
    globalThis.document.getElementById(id)?.focus();
  });
}
function closeWriting() {
  writingOpen.value = false;
  if (route.value.location === 'templates') navigate('/output');
  void nextTick(() => writingToggle.value?.focus());
}
function clearMaterialFilters() {
  materialQuery.value = '';
  outputStatus.value = '';
  void nextTick(() => materialSearch.value?.focus());
}
function closeMaterial() {
  const id = route.value.recordId;
  navigate('/output');
  void nextTick(() => {
    const trigger = id ? globalThis.document.getElementById(`output-material-${id}`) : null;
    (trigger ?? materialSearch.value)?.focus();
  });
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
  else outputs.value = [updated, ...outputs.value];
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
    .then(() => { connected.value = true; })
    .catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Workspace is unavailable.'; });
});
onBeforeUnmount(() => {
  globalThis.removeEventListener('popstate', handlePopState);
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
watch(route, (current, previous) => {
  if (current.tab === 'Output' && current.location !== 'templates' && previous.location === 'templates') writingOpen.value = false;
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
            <p>Configure the Extractor’s local model and check its connection.</p>
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
              <div class="provider-actions">
                <button
                  class="primary"
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
              </div>
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
            @erased="refreshTab"
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
          @navigate="navigate"
        />
      </div>
      <div
        v-if="openedTabs.has('Output')"
        v-show="active === 'Output'"
        class="output-workspace"
      >
        <div class="output-tools">
          <button
            ref="writingToggle"
            class="primary"
            type="button"
            :aria-expanded="writingOpen"
            @click="writingOpen ? closeWriting() : showWriting()"
          >
            {{ writingOpen ? 'Back to materials' : 'Create output' }}
          </button>
          <button
            v-if="route.location !== 'templates'"
            type="button"
            @click="navigate('/output/templates')"
          >
            Manage templates
          </button>
        </div>
        <p
          v-if="outputError"
          role="alert"
          class="output-notice"
        >
          {{ outputError }}
          <button
            type="button"
            @click="refreshTab('Output')"
          >
            Retry
          </button>
        </p>
        <div
          v-show="!writingOpen"
          class="output-layout"
          :class="{ 'review-open': route.tab === 'Output' && !!route.recordId }"
        >
          <section
            class="registry"
            aria-label="Material registry"
          >
            <div class="output-filters">
              <label>Find materials
                <input
                  ref="materialSearch"
                  v-model="materialQuery"
                  type="search"
                  placeholder="Material, opportunity or contact"
                >
              </label>
              <label>Status
                <select
                  v-model="outputStatus"
                  aria-label="Filter material status"
                >
                  <option value="">All statuses</option>
                  <option
                    v-for="state in outputStates"
                    :key="state"
                    :value="state"
                  >{{ state }}</option>
                </select>
              </label>
            </div>
            <p
              v-if="outputLoading && !outputLoaded"
              role="status"
              class="output-notice"
            >
              Loading materials…
            </p>
            <div
              v-else-if="outputLoaded"
              class="registry-scroll"
            >
              <table role="table">
                <thead role="rowgroup">
                  <tr role="row">
                    <th
                      scope="col"
                      role="columnheader"
                    >
                      Material
                    </th>
                    <th
                      scope="col"
                      role="columnheader"
                    >
                      Opportunity
                    </th>
                    <th
                      scope="col"
                      role="columnheader"
                    >
                      Contact
                    </th>
                    <th
                      scope="col"
                      role="columnheader"
                    >
                      Status
                    </th>
                    <th
                      scope="col"
                      role="columnheader"
                    >
                      Release
                    </th>
                  </tr>
                </thead>
                <tbody role="rowgroup">
                  <tr
                    v-for="material in visibleOutputs"
                    :key="material.id"
                    role="row"
                    :class="{ selected: route.tab === 'Output' && route.recordId === material.id }"
                  >
                    <th
                      scope="row"
                      role="rowheader"
                    >
                      {{ material.kind }} · v{{ material.version }}
                    </th>
                    <td
                      role="cell"
                      class="material-opportunity"
                    >
                      <button
                        v-if="opportunityTitles.has(material.opportunity_id)"
                        type="button"
                        class="text-action"
                        @click="navigate('/queue/' + encodeURIComponent(material.opportunity_id))"
                      >
                        {{ opportunityTitles.get(material.opportunity_id) }}
                      </button>
                      <span v-else>Unavailable</span>
                    </td>
                    <td
                      role="cell"
                      class="material-recipient"
                    >
                      {{ material.recipient_name }}
                    </td>
                    <td
                      role="cell"
                      class="material-status"
                    >
                      {{ material.status }}
                    </td>
                    <td
                      role="cell"
                      class="material-review"
                    >
                      <button
                        :id="`output-material-${material.id}`"
                        type="button"
                        :aria-label="`Review ${material.kind} v${material.version} for ${material.recipient_name}`"
                        :aria-pressed="route.tab === 'Output' && route.recordId === material.id"
                        @click="navigate('/output/' + encodeURIComponent(material.id))"
                      >
                        Review
                      </button>
                    </td>
                  </tr>
                </tbody>
              </table>
              <div
                v-if="!visibleOutputs.length"
                class="output-notice"
              >
                <p>{{ outputs.length ? 'No matching materials.' : 'No materials yet.' }}</p>
                <button
                  v-if="materialQuery || outputStatus"
                  type="button"
                  @click="clearMaterialFilters"
                >
                  Clear filters
                </button>
              </div>
            </div>
          </section>
          <aside
            class="output-review"
            aria-label="Material review"
          >
            <section
              v-for="material in reviewedMaterials"
              v-show="route.tab === 'Output' && route.recordId === material.id"
              :key="material.id"
              class="material-detail"
              :aria-labelledby="`output-review-${material.id}`"
              @keydown.esc.stop="closeMaterial"
            >
              <header class="material-heading">
                <div>
                  <small>{{ material.recipient_name }}</small>
                  <h3
                    :id="`output-review-${material.id}`"
                    tabindex="-1"
                  >
                    {{ material.kind }} · v{{ material.version }}
                  </h3>
                </div>
                <button
                  type="button"
                  @click="closeMaterial"
                >
                  Close material
                </button>
              </header>
              <div class="material-workflow">
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
                  @changed="refreshTab('Output')"
                />
              </div>
            </section>
            <div
              v-if="route.tab !== 'Output' || !route.recordId"
              class="output-notice"
            >
              <h3>Material review</h3>
              <p>Select a version to review its content, release checks, and contact thread.</p>
            </div>
            <div
              v-else-if="outputLoaded && !outputs.some(material => material.id === route.recordId)"
              class="output-notice"
            >
              <h3>Material unavailable</h3>
              <button
                type="button"
                @click="closeMaterial"
              >
                Back to materials
              </button>
            </div>
          </aside>
        </div>
        <WritingWorkspace
          v-if="writingMounted"
          v-show="writingOpen"
          :opportunities="opportunities"
          :resources="resources"
          :outputs="outputs"
          @saved="refreshTab('Output')"
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
  .app-header nav { flex-basis: 100%; min-width: 0; }
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
.output-tools { display: flex; flex-wrap: wrap; gap: .5rem; margin-bottom: .85rem; }
.output-tools button, .material-heading button, .output-filters input, .output-filters select { min-height: 2.5rem; }
.output-layout { display: grid; grid-template-columns: minmax(0, 1.55fr) minmax(0, 1fr); align-items: start; gap: 1rem; }
.registry, .output-review { min-width: 0; padding: 0; border: 1px solid var(--line); border-radius: .55rem; background: var(--panel); overflow: hidden; }
.output-filters { display: grid; grid-template-columns: minmax(0, 2fr) minmax(0, 1fr); gap: .65rem; padding: .8rem 1rem; }
.output-filters label { margin: 0; }
.registry-scroll { overflow: auto; max-height: min(66dvh, 46rem); }
.registry table { border-collapse: separate; border-spacing: 0; table-layout: fixed; width: 100%; text-align: left; }
.registry th, .registry td { padding: .75rem .6rem; border-top: 1px solid #d7dcd1; vertical-align: top; overflow-wrap: anywhere; }
.registry th:first-child { padding-inline-start: 1rem; }
.registry thead th { position: sticky; top: 0; z-index: 1; color: #445b4d; background: #f0f2e9; font: .83rem/1.5 ui-monospace, monospace; }
.registry thead th:nth-child(2) { width: 29%; }
.registry thead th:nth-child(4) { width: 15%; }
.registry thead th:last-child { width: 17%; }
.registry tbody th { font-weight: 650; }
.registry tr.selected { background: #e8efe6; }
.material-status { text-transform: capitalize; }
.text-action { background: transparent; color: #225744; border: 0; padding: 0; text-align: left; overflow-wrap: anywhere; }
.text-action:hover { text-decoration: underline; }
.output-notice { padding: .7rem 1rem; margin: 0; overflow-wrap: anywhere; color: var(--muted); }
.output-notice h3 { color: #182d25; }
.material-detail { border: 0; padding: 0; border-radius: 0; background: none; }
.material-heading { display: flex; flex-wrap: wrap; justify-content: space-between; align-items: start; gap: .55rem; padding: .9rem 1rem; border-bottom: 1px solid var(--line); background: #edf2e8; overflow-wrap: anywhere; }
.material-heading h3 { margin: 0; font: 1.75rem/1.17 Georgia, serif; }
.material-workflow { display: grid; gap: .65rem; min-width: 0; padding: .8rem; overflow-wrap: anywhere; }
.material-workflow :deep(button) { margin-block: .15rem; }
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
.provider-health { grid-column: 1 / -1; }
@media (max-width: 1020px) {
  .output-layout { grid-template-columns: minmax(0, 1fr); }
  .output-layout:not(.review-open) .output-review, .output-layout.review-open .registry { display: none; }
}
@media (max-width: 680px) {
  .settings-workspace { grid-template-columns: minmax(0, 1fr); }
  .settings-index { position: static; display: flex; flex-wrap: wrap; }
  .registry-scroll { max-height: none; }
  .registry table, .registry tbody { display: block; }
  .registry thead { position: absolute; clip-path: inset(50%); width: 1px; height: 1px; overflow: hidden; white-space: nowrap; }
  .registry tbody tr { display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: .25rem .6rem; padding: .7rem .85rem; border-top: 1px solid #d7dcd1; }
  .registry tbody th, .registry tbody td { padding: 0; border: 0; }
  .material-opportunity, .material-recipient { grid-column: 1 / -1; color: var(--muted); }
  .material-status { grid-column: 2; grid-row: 1; text-align: end; }
  .material-review { grid-column: 2; justify-self: end; }
  .material-review button { min-height: 2.4rem; }
}
@media (max-width: 430px) {
  #settings-provider form { grid-template-columns: minmax(0, 1fr); }
  .output-filters { grid-template-columns: minmax(0, 1fr); }
}
</style>
