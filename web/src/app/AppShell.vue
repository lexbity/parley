<script setup lang="ts">
/* global MouseEvent */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { parley, type AdminOperation, type ContactThread, type GraphEntity, type Material, type Opportunity, type ProviderConfig, type ReleasedMaterial, type Resource, type SavedSource } from '../api/client';
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
const outputStatus = ref('');
const parties = ref<GraphEntity[]>([]);
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

async function refreshLeads() {
  const [directory, savedSources, savedOpportunities, savedOutputs, savedResources, savedThreads] = await Promise.all([parley.listParties(), parley.listSources(), parley.listOpportunities(), parley.listOutputs(), parley.listResources(), parley.listThreads()]);
  parties.value = directory;
  sources.value = savedSources;
  opportunities.value = savedOpportunities;
  outputs.value = savedOutputs;
  resources.value = savedResources;
  threads.value = savedThreads;
}

function navigate(path: string, replace = false) {
  if (route.value.tab === 'Parties' && partyDesk.value && path !== route.value.path) {
    partyDesk.value.requestSwitch(() => commitNavigation(path, replace));
  } else commitNavigation(path, replace);
}
function commitNavigation(path: string, replace: boolean) {
  lastTabPaths.set(route.value.tab, route.value.path);
  const next = resolveRoute(path);
  if (replace) globalThis.history.replaceState(null, '', next.path);
  else if (next.path !== route.value.path) globalThis.history.pushState(null, '', next.path);
  route.value = next;
}

function selectTab(tab: (typeof tabs)[number], event: MouseEvent) {
  if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
  event.preventDefault();
  navigate(lastTabPaths.get(tab) ?? routeForTab(tab));
  if (tab === 'Output') void refreshLeads().catch((reason: unknown) => {
    error.value = reason instanceof Error ? reason.message : 'Could not refresh Output records.';
  });
}

function handlePopState() {
  lastTabPaths.set(route.value.tab, route.value.path);
  route.value = resolveRoute(globalThis.location.pathname);
  if (route.value.path !== globalThis.location.pathname) globalThis.history.replaceState(null, '', route.value.path);
  if (route.value.tab === 'Output') void refreshLeads().catch((reason: unknown) => { error.value = reason instanceof Error ? reason.message : 'Could not refresh Output records.'; });
}

async function focusRouteLocation() {
  if (route.value.location === 'templates') writingOpen.value = true;
  if (route.value.tab === 'Output' && route.value.recordId) {
    await refreshLeads().catch(() => undefined);
    if (!visibleOutputs.value.some(item => item.id === route.value.recordId)) materialQuery.value = '';
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
  globalThis.dispatchEvent(new globalThis.CustomEvent('parley:workspace-changed', { detail: { operation, epoch } }));
  try {
    if (operation === 'restore' || operation === 'erase_workspace') await parley.refreshSession();
    await Promise.all([refreshLeads(), parley.listResources().then((next) => { resources.value = next; })]);
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
  if (route.value.path !== globalThis.location.pathname) navigate(route.value.path, true);
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
    .then(async () => { connected.value = true; await refreshLeads(); await focusRouteLocation(); await loadProvider(); })
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
watch(route, () => { void focusRouteLocation(); });
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
    <h1>{{ active }}</h1>
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
        v-show="active === 'Resources'"
        class="resource-workspace"
      >
        <ResourcesWorkspace
          ref="resourcesWorkspace"
          :route-id="route.tab === 'Resources' ? route.recordId : null"
          @navigate="navigate"
          @changed="refreshLeads"
        />
        <InterviewerWorkspace @saved="resourcesWorkspace?.refresh()" />
      </div>
      <PartyDesk
        v-show="active === 'Parties'"
        ref="partyDesk"
        :parties="parties"
        :sources="sources"
        :opportunities="opportunities"
        :active="active === 'Parties'"
        :inbox="route.tab === 'Parties' && route.location === 'inbox'"
        :party-id="route.tab === 'Parties' ? route.recordId ?? undefined : undefined"
        @changed="refreshLeads"
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
        <CapacitySettings />
        <Packs />
        <MaintenanceSettings
          :parties="parties"
          @erased="refreshLeads"
        />
      </section>
      <div v-show="active === 'Queue'">
        <QueueWorkspace
          :parties="parties"
          :outputs="outputs"
          :active="active === 'Queue'"
          :route-id="route.tab === 'Queue' ? route.recordId : null"
          @material-updated="installMaterial"
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
                <tr v-show="route.recordId === material.id">
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
                      @changed="refreshLeads"
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
.workspace-grid, .lead-desk, .resource-workspace { display: grid; gap: .85rem; align-items: start; }
.workspace-grid { grid-template-columns: repeat(auto-fit,minmax(min(100%, 19rem),1fr)); }
.resource-workspace { grid-template-columns: minmax(0,2fr) minmax(17rem,1fr); }
.lead-desk { grid-template-columns: minmax(14rem, 1fr) minmax(0, 3fr); }
.resource-workspace > section, .lead-desk > section, .lead-desk aside, form, section section, .record-card { min-width: 0; background: white; border: 1px solid #d1dbd2; border-radius: .45rem; padding: .8rem; }
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
@media (max-width: 68rem) { .resource-workspace { grid-template-columns: 1fr; } }
@media (max-width: 54rem) { .lead-desk { grid-template-columns: 1fr; } }
@media (max-width: 38rem) { .app-header { flex-wrap: wrap; gap: 0; } .app-header nav { flex-basis: 100%; } .app-header > strong { margin-top: .35rem; } }

:root { background:#f8f7f3; }
.app-header { min-height:54px; }
.resource-workspace { grid-template-columns:minmax(0,2.2fr) minmax(18rem,1fr); align-items:stretch; }
.lead-desk { grid-template-columns:minmax(12rem,.8fr) minmax(0,2.2fr) minmax(15rem,1fr); align-items:stretch; }
.resource-workspace > section, .lead-desk > section, .lead-desk aside, .lead-chat-column { background:white; border:1px solid #ccd5cc; border-radius:.55rem; padding:0; min-width:0; }
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
@media(max-width:800px){.resource-workspace,.lead-desk{grid-template-columns:minmax(0,1fr)}.chat-pane {min-height:18rem}}

.chat-composer {margin-top:auto;padding:.8rem;border:0;border-top:1px solid #dce2db;border-radius:0;}
.chat-composer textarea {resize:vertical;}
.chat-composer > div {display:flex;gap:.5rem;align-items:center}
.chat-composer .primary {margin-left:auto;}
.lead-chat-column {display:flex;flex-direction:column;}
.lead-chat-column > .chat-pane {flex:1;padding:0;}
.lead-desk {min-height:min(73vh,52rem);}
.resource-directory,.graph-inspector {border-top:1px solid #dce2db;}
</style>
