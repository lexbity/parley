<script setup lang="ts">
import { parley, type Dossier, type GraphEntity } from '../../api/client';
import MarkdownPreview from '../../shared/MarkdownPreview.vue';
import PartyFiles from './PartyFiles.vue';

const props = defineProps<{
  party: GraphEntity | null;
  dossier: Dossier | null;
  loading: boolean;
  rootPartyId: string | null;
  notesError: string;
  markdown?: string;
  nodes?: GraphEntity[];
}>();

const emit = defineEmits<{ 'open-card': []; 'make-root': [id: string]; navigate: [path: string]; 'select-node': [id: string]; 'start-opportunity': []; 'review-retirement': []; 'return-to-results': []; 'inspect-link': [id: string] }>();

function typeLabel(kind: string): string {
  return kind === 'org' ? 'Organization' : kind === 'person' ? 'Person' : kind === 'resource' ? 'Resource' : kind === 'project' ? 'Project' : kind;
}
</script>

<template>
  <section
    class="party-inspector"
    aria-labelledby="inspector-heading"
  >
    <h2
      id="inspector-heading"
      class="sr-only"
    >
      Selected card
    </h2>
    <p
      v-if="props.loading"
      role="status"
      class="pad-line"
    >
      Loading the selection…
    </p>
    <p
      v-else-if="!props.party"
      class="pad-line"
    >
      {{ props.notesError || 'Select a party row or graph node to inspect it.' }} 
    </p>
    <div
      v-else-if="dossier?.retired_at"
      class="inspector-head"
    >
      <h3>{{ party?.name }}</h3>
      <p>This card was retired.</p>
      <button @click="emit('return-to-results')">
        Return to results
      </button>
    </div>
    <template v-else>
      <div class="inspector-head">
        <h3>{{ props.party.name }}</h3>
        <small>{{ typeLabel(props.party.kind) }}<template v-if="props.party.focus"> · {{ props.party.focus }}</template></small>
        <p
          v-if="['person', 'org'].includes(props.party.kind) && props.rootPartyId && props.rootPartyId !== props.party.id"
          class="outside-root"
        >
          <button
            type="button"
            @click="emit('make-root', props.party.id)"
          >
            Open {{ props.party.name }}’s network
          </button>
        </p>
      </div>
      <p
        v-if="props.notesError"
        role="alert"
        class="pad-line"
      >
        {{ props.notesError }}
      </p>
      <p
        v-if="dossier?.import_origin"
        class="pad-line"
      >
        CSV import · row {{ dossier.import_origin.source_record }}
      </p>
      <div class="inspector-section">
        <div
          v-if="markdown"
          class="notes-preview"
          aria-label="Owner notes"
        >
          <MarkdownPreview
            :source="markdown"
          />
        </div>
        <button
          type="button"
          class="primary open-card"
          @click="emit('open-card')"
        >
          Open card
        </button>
      </div>
      <details class="inspector-section">
        <summary>Direct links ({{ dossier?.direct_links.length ?? 0 }})</summary>
        <p
          v-if="!props.dossier?.direct_links?.length"
          class="pad-line"
        >
          No accepted relationships for this party yet.
        </p>
        <ul
          v-else
          class="inspector-list"
        >
          <li
            v-for="link in props.dossier.direct_links"
            :key="link.id"
          >
            <button @click="emit('inspect-link', link.id)">
              {{ link.from_id === party?.id ? '→' : '←' }} {{ link.verb.replaceAll('_',' ') }} · {{ nodes?.find(n => n.id === (link.from_id === party?.id ? link.to_id : link.from_id))?.name ?? (link.from_id === party?.id ? link.to_id : link.from_id) }}
            </button>
            <small> · T{{ link.tier ?? '—' }}{{ link.wording ? ` · “${link.wording}”` : '' }}</small>
          </li>
        </ul>
      </details>
      <details class="inspector-section">
        <summary>Files ({{ dossier?.files?.length ?? 0 }})</summary>
        <p
          v-if="!props.dossier?.files?.length"
          class="pad-line"
        >
          No files attached to this card.
        </p>
        <PartyFiles
          v-else
          :key="props.party.id"
          :entity-id="props.party.id"
          :files="props.dossier.files"
          readonly
        />
      </details>
      <details class="inspector-section">
        <summary>Open opportunities ({{ dossier?.opportunities?.length ?? 0 }})</summary>
        <p
          v-if="!props.dossier?.opportunities?.length"
          class="pad-line"
        >
          No open opportunities involve this party.
        </p>
        <ul
          v-else
          class="inspector-list"
        >
          <li
            v-for="opportunity in props.dossier.opportunities"
            :key="opportunity.id"
          >
            <button @click="emit('navigate', '/queue/' + opportunity.id)">
              {{ opportunity.title }}
            </button> <small> · role {{ opportunity.role }}</small>
          </li>
        </ul>
        <button
          v-if="['org', 'person'].includes(party?.kind ?? '')"
          @click="emit('start-opportunity')"
        >
          Start opportunity
        </button>
      </details>
      <details class="inspector-section">
        <summary>Associated resources ({{ dossier?.associated_resources?.length ?? 0 }})</summary>
        <p
          v-if="!props.dossier?.associated_resources?.length"
          class="pad-line"
        >
          No associated resources.
        </p>
        <ul
          v-else
          class="inspector-list"
        >
          <li
            v-for="resource in props.dossier.associated_resources"
            :key="resource.id"
          >
            <button @click="emit('navigate', '/resources/' + resource.id)">
              {{ resource.title }}
            </button> <small> · {{ resource.kind }}</small>
            <ul
              v-if="resource.paths?.length"
              class="association-paths"
            >
              <li
                v-for="(path, index) in resource.paths"
                :key="index"
              >
                <small>via
                  <template v-if="path.via === 'opportunity'">
                    <button
                      type="button"
                      class="path-link"
                      @click="emit('navigate', '/queue/' + path.opportunity_id)"
                    >
                      {{ path.opportunity_title }}
                    </button>
                  </template>
                  <template v-else>
                    {{ path.verb?.replaceAll('_', ' ') }}
                  </template>
                </small>
              </li>
            </ul>
          </li>
        </ul>
      </details>
      <button
        v-if="dossier?.entity.etag && parley.supports('party.relations')"
        class="retire"
        @click="emit('review-retirement')"
      >
        Review retirement
      </button>
    </template>
  </section>
</template>

<style scoped>
.party-inspector { display: flex; flex-direction: column; min-width: 0; overflow-wrap: anywhere; }
.inspector-head { padding: .85rem 1rem .5rem; }
.inspector-head h3 { margin: 0 0 .15rem; font: 1.75rem/1.17 Georgia, serif; }
.inspector-section { border-top: 1px solid #e3e9e2; padding: .4rem 1rem; }
.inspector-head + .inspector-section { border: 0; }
.inspector-section summary { cursor: pointer; font-weight: 600; padding: .3rem 0; }
.inspector-list { margin: .3rem 0 .6rem; padding-left: 1.1rem; }
.inspector-list li { margin: .3rem 0; }
.pad-line { margin: .5rem .9rem; }
.outside-root { margin: .45rem 0 0; }
.notes-preview { background: #eff3e9; border-radius: .35rem; padding: .1rem .75rem; }
.notes-preview :deep(pre) { overflow: auto; }
.retire { align-self:start; margin:.55rem .9rem; }
.open-card { margin: .5rem 0 .7rem; }
.association-paths { list-style: none; margin: .15rem 0 0; padding-left: 1.1rem; }
.association-paths small { color: #58665f; }
.path-link { border: 0; background: transparent; padding: 0; color: #225744; font: inherit; text-decoration: underline; cursor: pointer; }
</style>
