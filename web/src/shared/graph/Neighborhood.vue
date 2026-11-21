<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { VueFlow } from '@xyflow/vue';
defineOptions({ name: 'RelationshipNeighborhood' });
import { parley, type GraphEdge, type Neighborhood as NeighborhoodData } from '../../api/client';

const props=defineProps<{ focusId:string; graph:NeighborhoodData|null }>();
const emit=defineEmits<{ refresh:[]; source:[id:string] }>();
const selected=ref(''); const wording=ref(''); const busy=ref(false); const error=ref('');
watch(()=>props.graph,()=>{ if (props.graph && !props.graph.edges.some(e=>e.id===selected.value)) selected.value=''; },{deep:false});
const nodes=computed(()=>props.graph?.nodes.map((node,index)=>({id:node.id,position:props.graph?.positions.find(p=>p.entity_id===node.id)?{x:props.graph.positions.find(p=>p.entity_id===node.id)!.x,y:props.graph.positions.find(p=>p.entity_id===node.id)!.y}:{x:80+(index%3)*220,y:70+Math.floor(index/3)*140},data:{label:`${node.name} · ${node.kind}`} }))??[]);
const edges=computed(()=>props.graph?.edges.map(edge=>({id:edge.id,source:edge.from_id,target:edge.to_id,label:edge.verb,selectable:true,focusable:true,ariaLabel:`${edge.from_id} ${edge.verb} ${edge.to_id}`}))??[]);
const chosen=computed<GraphEdge|undefined>(()=>props.graph?.edges.find(edge=>edge.id===selected.value));
watch(chosen,(edge)=>wording.value=edge?.approved_wording??edge?.wording??'');
async function saveCorrection(){ if(!chosen.value)return; busy.value=true;error.value='';try{await parley.correctClaim(chosen.value.claim_id,chosen.value.claim_revision,wording.value);emit('refresh');}catch(e){error.value=e instanceof Error?e.message:'Correction failed';}finally{busy.value=false;} }
async function moved(nodesNow:unknown[]){
  if(!props.graph)return;
  const vals=nodesNow as Array<{id:string;position:{x:number;y:number}}>;
  for(const node of vals){const old=props.graph.positions.find(p=>p.entity_id===node.id);if(!old||old.x!==node.position.x||old.y!==node.position.y){try{await parley.saveLayout({view_scope:'party',view_id:props.focusId,entity_id:node.id,expected_revision:old?.revision??0,x:node.position.x,y:node.position.y});}catch(e){error.value=e instanceof Error?e.message:'Could not save this view layout';emit('refresh');return;}}}
}
</script>

<template>
  <section
    class="neighborhood"
    aria-label="Relationship neighborhood"
  >
    <h2>Graph and relationships</h2>
    <p v-if="!graph || !graph.edges.length">
      No saved relationships in this neighborhood. Add a related card from a source to create an attributed edge.
    </p>
    <div
      class="graph-canvas"
      aria-label="Relationship graph canvas"
    >
      <VueFlow
        v-if="graph"
        :nodes="nodes"
        :edges="edges"
        fit-view-on-init
        @node-drag-stop="(_event, node) => moved([node])"
        @edge-click="(_event, edge) => { selected=edge.id; }"
      />
    </div>
    <p
      v-if="graph && (graph.omitted_nodes || graph.omitted_edges)"
      role="status"
    >
      Showing a bounded neighborhood. {{ graph.omitted_nodes }} additional nodes and {{ graph.omitted_edges }} additional relationships are omitted; filter or expand the neighborhood to inspect more.
    </p>
    <h3>Relationship list</h3>
    <p>Keyboard equivalent to the graph. Each row opens the same evidence and correction controls.</p>
    <ul class="relationship-list">
      <li
        v-for="edge in graph?.edges ?? []"
        :key="edge.id"
      >
        <button
          type="button"
          :aria-pressed="selected===edge.id"
          @click="selected=edge.id"
        >
          {{ graph?.nodes.find(n=>n.id===edge.from_id)?.name }} — {{ edge.verb }} → {{ graph?.nodes.find(n=>n.id===edge.to_id)?.name }}
        </button>
      </li>
    </ul>
    <section
      v-if="chosen"
      class="evidence"
      aria-label="Relationship evidence"
    >
      <h3>Evidence and approved wording</h3>
      <p><strong>Direction:</strong> {{ graph?.nodes.find(n=>n.id===chosen?.from_id)?.name }} {{ chosen.verb }} {{ graph?.nodes.find(n=>n.id===chosen?.to_id)?.name }}</p>
      <p><strong>Speaker:</strong> {{ chosen.speaker_kind }}<span v-if="chosen.speaker_entity_id"> · {{ graph?.nodes.find(n=>n.id===chosen?.speaker_entity_id)?.name ?? chosen.speaker_entity_id }}</span> · T{{ chosen.tier }}</p>
      <blockquote>{{ chosen.wording }}</blockquote>
      <button
        v-if="chosen.occurrence_id"
        type="button"
        @click="emit('source',chosen.occurrence_id)"
      >
        Open original source
      </button>
      <p v-if="chosen.source_title">
        {{ chosen.source_title }} <span v-if="chosen.source_url">· {{ chosen.source_url }}</span>
      </p>
      <label>Owner-approved wording<textarea
        v-model="wording"
        rows="3"
      /></label>
      <button
        type="button"
        :disabled="busy || wording.trim().length===0 || wording===chosen.approved_wording"
        @click="saveCorrection"
      >
        Save correction
      </button>
      <p v-if="chosen.approved_wording">
        Current wording was corrected by the owner; original evidence remains above.
      </p>
      <p
        v-if="error"
        role="alert"
      >
        {{ error }}
      </p>
    </section>
  </section>
</template>

<style scoped>
.graph-canvas{height:320px;border:1px solid #82918a;background:#fff}.relationship-list{padding-left:1rem}.relationship-list button{min-height:44px}.evidence{margin-top:1rem;padding:1rem;border:1px solid #82918a;background:white}blockquote{border-left:3px solid #698477;margin-left:0;padding-left:1rem}.evidence textarea{display:block;width:100%;min-height:5rem}
</style>
