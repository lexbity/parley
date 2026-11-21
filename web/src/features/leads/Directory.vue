<script setup lang="ts">
import { onMounted, ref, watch } from 'vue';
import { newOperationKey, parley, type DirectoryEntity, type Neighborhood as NeighborhoodData, type SavedSource, type SourceDetail } from '../../api/client';
import Neighborhood from '../../shared/graph/Neighborhood.vue';
defineOptions({ name: 'LeadsDirectory' });

const q=ref(''); const kind=ref('');const tag=ref(''); const directory=ref<DirectoryEntity[]>([]); const sources=ref<SavedSource[]>([]);
const selected=ref(''); const graph=ref<NeighborhoodData|null>(null); const detail=ref<SourceDetail|null>(null);
const page=ref(1);const total=ref(0);const hasMore=ref(false);
const name=ref(''); const newKind=ref<'person'|'org'>('person'); const capture=ref(''); const captureTitle=ref(''); const captureUrl=ref('');
const sourceSpeaker=ref('unknown');
const relatedName=ref(''); const relationSource=ref(''); const relationQuote=ref('');
const existingRelated=ref('');const existingSource=ref('');const existingQuote=ref('');
const profileName=ref('');const profileDescriptor=ref('');const profileAliases=ref('');const profileTags=ref('');
const profileDrafts=new Map<string,{name:string;descriptor:string;aliases:string;tags:string}>();
const status=ref(''); const error=ref(''); const busy=ref(false); let directoryGeneration=0; let graphGeneration=0; let detailGeneration=0;let directoryRead:InstanceType<typeof globalThis.AbortController>|undefined;let graphRead:InstanceType<typeof globalThis.AbortController>|undefined;

async function loadDirectory(){directoryRead?.abort();directoryRead=new globalThis.AbortController();const current=++directoryGeneration;error.value='';try{const [result,sourceRows]=await Promise.all([parley.searchDirectory({q:q.value,kind:kind.value||undefined,tag:tag.value||undefined},directoryRead.signal,page.value,50),parley.listSources()]);if(current!==directoryGeneration)return;directory.value=result.items;total.value=result.total;hasMore.value=result.has_more;sources.value=sourceRows;if(selected.value&&!result.items.some(r=>r.id===selected.value))selected.value='';}catch(e){if(current===directoryGeneration&&!(e instanceof Error&&e.name==='AbortError'))error.value=e instanceof Error?e.message:'Directory could not load.';}}
async function loadGraph(){graphRead?.abort();graphRead=new globalThis.AbortController();const id=selected.value;const current=++graphGeneration;if(!id){graph.value=null;return;}graph.value=null;try{const result=await parley.getNeighborhood(id,'party',id,graphRead.signal);if(current!==graphGeneration||selected.value!==id)return;graph.value=result;}catch(e){if(current===graphGeneration&&!(e instanceof Error&&e.name==='AbortError'))error.value=e instanceof Error?e.message:'Neighborhood could not load.';}}
async function openSource(id:string){const current=++detailGeneration;detail.value=null;try{const value=await parley.getSource(id);if(current===detailGeneration)detail.value=value;}catch(e){if(current===detailGeneration)error.value=e instanceof Error?e.message:'Source could not load.';}}
watch([q,kind,tag],()=>{if(page.value!==1)page.value=1;else void loadDirectory();});watch(page,()=>{void loadDirectory();});watch(selected,(next,previous)=>{if(previous)profileDrafts.set(previous,{name:profileName.value,descriptor:profileDescriptor.value,aliases:profileAliases.value,tags:profileTags.value});const entity=directory.value.find(row=>row.id===next);const draft=next?profileDrafts.get(next):undefined;profileName.value=draft?.name??entity?.name??'';profileDescriptor.value=draft?.descriptor??entity?.descriptor??'';profileAliases.value=draft?.aliases??entity?.aliases.join(', ')??'';profileTags.value=draft?.tags??entity?.tags.join(', ')??'';void loadGraph();});
async function addParty(){busy.value=true;error.value='';status.value='';try{const item=await parley.createEntity({kind:newKind.value,name:name.value.trim()},newOperationKey());name.value='';status.value=`${item.kind==='org'?'Organization':'Person'} added. No opportunity or outreach was created.`;page.value=1;await loadDirectory();selected.value=item.id;}catch(e){error.value=e instanceof Error?e.message:'Lead could not be saved.';}finally{busy.value=false;}}
async function saveSource(){busy.value=true;error.value='';try{const partyId=sourceSpeaker.value.startsWith('party:')?sourceSpeaker.value.slice(6):null;const actorKind=sourceSpeaker.value==='owner'?'owner':partyId?'party':'unknown';const row=await parley.saveSource({text:capture.value,title:captureTitle.value||null,url:captureUrl.value||null,actor_kind:actorKind,actor_entity_id:partyId});capture.value='';status.value='Source saved. Speaker attribution is explicit; identity links remain separate.';await loadDirectory();await openSource(row.occurrence_id);}catch(e){error.value=e instanceof Error?e.message:'Source could not be saved.';}finally{busy.value=false;}}
async function addRelated(){if(!selected.value||!relationSource.value)return;busy.value=true;error.value='';try{const node=await parley.createRelated({kind:'person',name:relatedName.value.trim(),related_entity_id:selected.value,verb:'member_of',occurrence_id:relationSource.value,quote:relationQuote.value,view_scope:'party',view_id:selected.value,x:360,y:200});relatedName.value='';status.value='Related person and attributed relationship saved together.';await loadDirectory();selected.value=node.id;}catch(e){error.value=e instanceof Error?e.message:'Related record was not saved; check the cited quote and relationship.';}finally{busy.value=false;}}
async function linkExisting(){if(!selected.value||!existingRelated.value||!existingSource.value)return;busy.value=true;error.value='';try{await parley.linkExisting({from_id:existingRelated.value,to_id:selected.value,verb:'member_of',occurrence_id:existingSource.value,quote:existingQuote.value});status.value='Existing records linked with source evidence; no identity merge occurred.';existingQuote.value='';await loadGraph();}catch(e){error.value=e instanceof Error?e.message:'Relationship was not saved; inspect endpoints and cited passage.';}finally{busy.value=false;}}
async function saveProfile(){const entity=directory.value.find(row=>row.id===selected.value);if(!entity)return;busy.value=true;error.value='';try{await parley.updateDirectory(entity,{name:profileName.value,descriptor:profileDescriptor.value,aliases:profileAliases.value.split(',').map(v=>v.trim()).filter(Boolean),tags:profileTags.value},newOperationKey());profileDrafts.delete(entity.id);status.value='Directory profile saved.';await loadDirectory();}catch(e){error.value=e instanceof Error?e.message:'Profile was not saved; compare the current directory row.';}finally{busy.value=false;}}
onMounted(()=>{void parley.connect().then(loadDirectory).catch(e=>error.value=e instanceof Error?e.message:'Workspace is unavailable.');});
</script>

<template>
  <section
    aria-labelledby="leads-heading"
    aria-label="Lead directory and sources"
  >
    <div
      v-if="status"
      role="status"
    >
      {{ status }}
    </div><div
      v-if="error"
      role="alert"
    >
      {{ error }}
    </div>
    <div class="leads-layout">
      <aside
        class="panel"
        aria-label="Directory"
      >
        <h2 id="leads-heading">
          Leads
        </h2>
        <label>Search name or alias<input
          v-model="q"
          type="search"
          autocomplete="off"
        ></label>
        <label>Type<select v-model="kind"><option value="">People and organizations</option><option value="person">People</option><option value="org">Organizations</option></select></label>
        <label>Topic or tag<input
          v-model="tag"
          type="search"
          autocomplete="off"
        ></label>
        <ul class="directory-list">
          <li
            v-for="entity in directory"
            :key="entity.id"
          >
            <button
              type="button"
              :aria-current="selected===entity.id?'true':undefined"
              @click="selected=entity.id"
            >
              {{ entity.name }} <small>{{ entity.kind }}<span v-if="entity.descriptor"> · {{ entity.descriptor }}</span><span v-else> · {{ entity.id.slice(0,8) }}</span></small>
            </button>
          </li>
        </ul>
        <nav aria-label="Directory pages">
          <button
            type="button"
            :disabled="page<=1"
            @click="page--"
          >
            Previous
          </button>
          <span>Page {{ page }} · {{ total }} matches</span>
          <button
            type="button"
            :disabled="!hasMore"
            @click="page++"
          >
            Next
          </button>
        </nav>
        <p v-if="!directory.length">
          No leads match. Add a person or organization; a person can stand alone.
        </p>
        <form @submit.prevent="addParty">
          <h3>Add lead</h3><label>Type<select v-model="newKind"><option value="person">Person</option><option value="org">Organization</option></select></label><label>Name<input
            v-model="name"
            required
            maxlength="500"
          ></label><button
            :disabled="busy"
            type="submit"
          >
            Add lead
          </button>
        </form>
        <form
          v-if="selected && directory.find(x=>x.id===selected)?.kind==='org'"
          class="panel"
          @submit.prevent="linkExisting"
        >
          <h2>Link an existing person</h2>
          <p>Links two chosen record IDs. It never merges identities.</p>
          <label>Person<select
            v-model="existingRelated"
            required
          ><option
            value=""
            disabled
          >Choose a person</option><option
            v-for="entity in directory.filter(x=>x.kind==='person')"
            :key="entity.id"
            :value="entity.id"
          >{{ entity.name }} · {{ entity.id.slice(0,8) }}</option></select></label>
          <label>Source occurrence<select
            v-model="existingSource"
            required
          ><option
            value=""
            disabled
          >Choose a saved source</option><option
            v-for="source in sources"
            :key="source.occurrence_id"
            :value="source.occurrence_id"
          >{{ source.content_id.slice(0,12) }} · {{ source.occurrence_id.slice(0,8) }}</option></select></label>
          <label>Exact cited passage<textarea
            v-model="existingQuote"
            required
            rows="2"
          /></label>
          <button
            :disabled="busy"
            type="submit"
          >
            Link existing person to organization
          </button>
        </form>
      </aside>
      <div class="detail-column">
        <Neighborhood
          v-if="selected"
          :focus-id="selected"
          :graph="graph"
          @refresh="loadGraph"
          @source="openSource"
        />
        <section
          v-else
          class="panel"
        >
          <h2>Choose a lead</h2><p>Select a directory row to open its scoped graph and evidence.</p>
        </section>
        <form
          v-if="selected"
          class="panel"
          @submit.prevent="addRelated"
        >
          <h2>Add a person to this organization</h2><p>The related person and source-backed edge are committed atomically. No name matching is performed.</p><label>Person name<input
            v-model="relatedName"
            required
          ></label><label>Source occurrence<select
            v-model="relationSource"
            required
          ><option
            value=""
            disabled
          >Choose a saved source</option><option
            v-for="source in sources"
            :key="source.occurrence_id"
            :value="source.occurrence_id"
          >{{ source.content_id.slice(0,12) }} · {{ source.occurrence_id.slice(0,8) }}</option></select></label><label>Exact cited passage<textarea
            v-model="relationQuote"
            required
            rows="2"
          /></label><button
            :disabled="busy || directory.find(x=>x.id===selected)?.kind!=='org'"
            type="submit"
          >
            Save related person and relationship
          </button>
        </form>
        <form
          v-if="selected"
          class="panel"
          @submit.prevent="saveProfile"
        >
          <h2>Edit selected directory entry</h2>
          <label>Name<input
            v-model="profileName"
            required
            maxlength="500"
          ></label>
          <label>Disambiguating descriptor<input
            v-model="profileDescriptor"
            maxlength="1000"
          ></label>
          <label>Aliases, comma separated<input v-model="profileAliases"></label>
          <label>Tags, comma separated<input v-model="profileTags"></label>
          <button
            :disabled="busy"
            type="submit"
          >
            Save profile
          </button>
        </form>
        <section class="panel">
          <h2>Sources inbox</h2><p>Paste is always available. Speaker and identity links are separate owner decisions.</p><form @submit.prevent="saveSource">
            <label>Speaker attribution<select v-model="sourceSpeaker"><option value="unknown">Unknown</option><option value="owner">Owner</option><option
              v-for="entity in directory"
              :key="entity.id"
              :value="`party:${entity.id}`"
            >{{ entity.name }} · {{ entity.kind }} · {{ entity.id.slice(0,8) }}</option></select></label>
            <label>Title<input v-model="captureTitle"></label><label>URL<input
              v-model="captureUrl"
              type="url"
            ></label><label>Captured text<textarea
              v-model="capture"
              required
              rows="5"
            /></label><button
              :disabled="busy"
              type="submit"
            >
              Save source
            </button>
          </form><ul>
            <li
              v-for="source in sources"
              :key="source.occurrence_id"
            >
              <button
                type="button"
                @click="openSource(source.occurrence_id)"
              >
                Open source {{ source.content_id.slice(0,12) }} · {{ source.occurrence_id.slice(0,8) }}
              </button>
            </li>
          </ul>
          <article
            v-if="detail"
            aria-label="Original source"
          >
            <h3>{{ detail.title || 'Untitled source' }}</h3><p v-if="detail.url">
              {{ detail.url }}
            </p><p>Captured {{ detail.captured_at }} · raw SHA-256 {{ detail.raw_sha256 }} · normalization v{{ detail.normalization_version }}</p><pre>{{ detail.text }}</pre>
          </article>
        </section>
      </div>
    </div>
  </section>
</template>

<style scoped>
.leads-layout{display:grid;grid-template-columns:minmax(250px,300px) minmax(0,1fr);gap:1rem;align-items:start}.panel,.detail-column>section{background:#fff;border:1px solid #d7dfd9;border-radius:.5rem;padding:1rem;margin-bottom:1rem}.directory-list{list-style:none;padding:0}.directory-list button{width:100%;text-align:left;min-height:48px}.directory-list button[aria-current=true]{outline:2px solid #173e32}label{display:grid;gap:.25rem;margin:.7rem 0}input,select,textarea{font:inherit;width:100%;box-sizing:border-box;padding:.5rem}pre{white-space:pre-wrap;overflow-wrap:anywhere;max-height:24rem;overflow:auto}@media(max-width:767px){.leads-layout{grid-template-columns:1fr}}
</style>
