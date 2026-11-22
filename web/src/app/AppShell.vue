<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue';
import { tabs } from './tabs';
import ResourceEditor from '../features/resources/ResourceEditor.vue';
import Directory from '../features/leads/Directory.vue';
import OpportunityWorkspace from '../features/queue/OpportunityWorkspace.vue';
import Board, { type BoardService } from '../features/queue/Board.vue';
import { ApiError, parley } from '../api/client';

/**
 * Tabs whose owner journey is qualified stay discoverable (RFC-0003 S01);
 * the mandated order and the full destination set come from tabs.ts (APP-01).
 */
const qualifiedTabs = tabs.filter((tab) => tab === 'Resources' || tab === 'Leads' || tab === 'Queue' || tab === 'Output');
const active = ref<(typeof tabs)[number]>('Resources');
const workspaceEpoch = ref('');
const workspaceError = ref('');
const participants = ref<Array<{id:string;kind:'person'|'org';name:string}>>([]);
const queueRefresh = ref(0);
const outputRefresh = ref(0);
let eventCursor = '';
let eventPollTimer: ReturnType<typeof globalThis.setTimeout> | undefined;
let eventPollInFlight = false;
const boardService: BoardService = {
  listOpportunities: () => parley.getBoard(),
  createOpportunity: (input, key) => parley.createOpportunity(input, key),
  createList: (input, key) => parley.createBoardList(input, key),
  renameList: (id, input, key) => parley.renameBoardList(id, input, key),
  reorderLists: (input, key) => parley.reorderBoardLists(input, key),
  archiveList: (id, input, key) => parley.archiveBoardList(id, input, key),
  moveCard: (input, key) => parley.moveBoardCard(input.opportunity_id, { destination_list_id:input.destination_list_id, anchor_opportunity_id:input.anchor_opportunity_id, expected_board_revision:input.expected_board_revision, expected_source_revision:input.expected_source_revision, expected_destination_revision:input.expected_destination_revision }, key),
  reorderCard: (input, key) => parley.reorderBoardCard(input.list_id, { opportunity_id:input.opportunity_id, anchor_opportunity_id:input.anchor_opportunity_id, expected_revision:input.expected_revision }, key),
  getNotes: (id) => parley.getOpportunityNotes(id),
  saveNotes: (id, input, key) => parley.saveOpportunityNotes(id, input, key),
  toggleTask: (id, input, key) => parley.toggleOpportunityTask(id, input, key),
};

function scheduleEventPoll(delay = 2000):void {
  if(eventPollTimer) globalThis.clearTimeout(eventPollTimer);
  eventPollTimer=undefined;
  if(globalThis.document.visibilityState==='hidden'||!workspaceEpoch.value)return;
  eventPollTimer=globalThis.setTimeout(()=>{void pollEvents();},delay);
}
async function pollEvents():Promise<void>{
  if(eventPollInFlight||globalThis.document.visibilityState==='hidden'||!workspaceEpoch.value)return;
  eventPollInFlight=true;
  try{
    const page=await parley.getEvents(eventCursor);
    workspaceError.value='';
    if(page.items.length){
      queueRefresh.value++;
      outputRefresh.value++;
      if(active.value==='Queue'){
        const directory=await parley.searchDirectory({},undefined,1,200);
        participants.value=directory.items.filter((item)=>item.kind==='person'||item.kind==='org').map(({id,kind,name})=>({id,kind:kind as 'person'|'org',name}));
      }
    }
    eventCursor=page.next_cursor;
  }catch(error){
    if(error instanceof ApiError&&error.status===410){
      eventCursor='0';
      queueRefresh.value++;
      outputRefresh.value++;
    }
    workspaceError.value=error instanceof Error?`Workspace updates are delayed: ${error.message}`:'Workspace updates are delayed.';
  }finally{
    eventPollInFlight=false;
    scheduleEventPoll();
  }
}
function onVisibilityChange():void{if(globalThis.document.visibilityState==='visible')scheduleEventPoll(0);else if(eventPollTimer){globalThis.clearTimeout(eventPollTimer);eventPollTimer=undefined;}}
onMounted(() => { void parley.connect().then(() => { workspaceEpoch.value = parley.workspaceEpoch;eventCursor=parley.eventCursor;globalThis.document.addEventListener('visibilitychange',onVisibilityChange);scheduleEventPoll(0); }).catch((error: unknown) => { workspaceError.value = error instanceof Error ? error.message : 'Workspace session is unavailable.'; }); });
async function selectTab(tab:(typeof tabs)[number]):Promise<void>{
  if(tab==='Queue'){
    try{await parley.connect();const [page]=await Promise.all([parley.searchDirectory({},undefined,1,200)]);participants.value=page.items.filter((item)=>item.kind==='person'||item.kind==='org').map(({id,kind,name})=>({id,kind:kind as 'person'|'org',name}));queueRefresh.value++;}
    catch(error){workspaceError.value=error instanceof Error?error.message:'Participants could not be loaded.';}
  }
  if(tab==='Output') outputRefresh.value++;
  active.value=tab;
}
onUnmounted(()=>{if(eventPollTimer)globalThis.clearTimeout(eventPollTimer);globalThis.document.removeEventListener('visibilitychange',onVisibilityChange);});
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
        @click="selectTab(tab)"
      >
        {{ tab }}
      </button>
    </nav>
  </header>
  <main>
    <h1>{{ active }}</h1>
    <p
      v-if="workspaceError"
      role="alert"
    >
      {{ workspaceError }}
    </p>
    <div v-show="active === 'Resources'">
      <ResourceEditor />
    </div>
    <Directory v-show="active === 'Leads'" />
    <Board
      v-if="workspaceEpoch"
      v-show="active === 'Queue'"
      :service="boardService"
      :workspace-epoch="workspaceEpoch"
      :participant-options="participants"
      :refresh-key="queueRefresh"
    >
      <template #materials="{ opportunity }">
        <OpportunityWorkspace
          surface="queue"
          :opportunity-id="opportunity.id"
          embedded
        />
      </template>
    </Board>
    <OpportunityWorkspace
      v-if="workspaceEpoch"
      v-show="active === 'Output'"
      surface="output"
      :refresh-key="outputRefresh"
    />
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
