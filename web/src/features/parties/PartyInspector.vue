<script setup lang="ts">
import type { Dossier, GraphEntity } from '../../api/client';
import MarkdownPreview from '../../shared/MarkdownPreview.vue';

const props = defineProps<{
  party: GraphEntity | null;
  dossier: Dossier | null;
  loading: boolean;
  rootPartyId: string | null;
  notesError: string;
}>();

const emit = defineEmits<{ 'open-card': []; 'make-root': [id: string] }>();

function typeLabel(kind: string): string {
  return kind === 'org' ? 'Organization' : kind === 'person' ? 'Person' : 'Project';
}
</script>

<template>
  <section
    class="party-inspector"
    aria-labelledby="inspector-heading"
  >
    <header class="pane-bar">
      <h2 id="inspector-heading">
        Inspector
      </h2>
    </header>
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
      Select a party row or graph node to inspect it. Opening a node never marks facts reviewed.
    </p>
    <template v-else>
      <div class="inspector-head">
        <h3>{{ props.party.name }}</h3>
        <small>{{ typeLabel(props.party.kind) }} · {{ props.party.focus || 'no focus' }}</small>
        <p
          v-if="props.rootPartyId && props.rootPartyId !== props.party.id"
          class="outside-root"
        >
          Sheet root is {{ props.rootPartyId === props.party.id ? 'this party' : 'a different root' }}.
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
      <details
        class="inspector-section"
        :open="true"
      >
        <summary>Notes</summary>
        <div class="notes-preview">
          <MarkdownPreview
            v-if="props.dossier?.entity"
            :source="''"
          />
          <p class="pad-line">
            Load the card to edit owner notes.
          </p>
        </div>
        <button
          type="button"
          class="primary open-card"
          @click="emit('open-card')"
        >
          Open card
        </button>
      </details>
      <details
        class="inspector-section"
        :open="!(props.dossier?.direct_links?.length)"
      >
        <summary>Direct links</summary>
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
            <strong>{{ link.verb }}</strong>
            <small> · T{{ link.tier ?? '—' }}{{ link.wording ? ` · “${link.wording}”` : '' }}</small>
          </li>
        </ul>
      </details>
      <details class="inspector-section">
        <summary>Files</summary>
        <p
          v-if="!props.dossier?.files?.length"
          class="pad-line"
        >
          No files attached to this card.
        </p>
        <ul
          v-else
          class="inspector-list"
        >
          <li
            v-for="file in props.dossier.files"
            :key="file.id"
          >
            {{ file.filename }} <small> · {{ file.content_type }} · {{ file.size }} B · {{ file.share_permission ? 'shareable' : 'private' }}</small>
          </li>
        </ul>
      </details>
      <details class="inspector-section">
        <summary>Open opportunities</summary>
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
            {{ opportunity.title }} <small> · role {{ opportunity.role }}</small>
          </li>
        </ul>
      </details>
      <details class="inspector-section">
        <summary>Associated resources</summary>
        <p
          v-if="!props.dossier?.associated_resources?.length"
          class="pad-line"
        >
          No resources are used by visible opportunities with this party.
        </p>
        <ul
          v-else
          class="inspector-list"
        >
          <li
            v-for="resource in props.dossier.associated_resources"
            :key="resource.id"
          >
            {{ resource.title }} <small> · {{ resource.kind }} · via {{ resource.opportunity_title }}</small>
          </li>
        </ul>
      </details>
    </template>
  </section>
</template>

<style scoped>
.party-inspector { display: flex; flex-direction: column; min-width: 0; }
.inspector-head { padding: .7rem .9rem; border-bottom: 1px solid #dce2db; }
.inspector-head h3 { margin: 0 0 .15rem; }
.inspector-section { border-top: 1px solid #e3e9e2; padding: .35rem .9rem; }
.inspector-section summary { cursor: pointer; font-weight: 600; padding: .3rem 0; }
.inspector-list { margin: .3rem 0 .6rem; padding-left: 1.1rem; }
.inspector-list li { margin: .3rem 0; }
.pad-line { margin: .5rem .9rem; }
.outside-root { background: #fbf6e8; border: 1px solid #e5d9ae; border-radius: .35rem; padding: .45rem .6rem; }
.notes-preview { max-height: 14rem; overflow: auto; border: 1px solid #e0e5df; border-radius: .35rem; padding: 0 .6rem; }
.open-card { margin: .5rem 0 .7rem; }
</style>