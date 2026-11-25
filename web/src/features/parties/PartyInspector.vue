<script setup lang="ts">
/* global Event, HTMLDetailsElement */
import { ref, watch } from 'vue';
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

type MutualValueInterest = { side: string; statement: string; knowledgeKind: string };
type MutualValueEntry = {
  opportunityId: string;
  title: string;
  theyGet: string;
  iGet: string;
  attested: boolean;
  attestedAt: string | null;
  interests: MutualValueInterest[];
};

const EXCHANGE_FETCH_CAP = 10;
const exchangeEntries = ref<MutualValueEntry[]>([]);
const exchangeState = ref<'idle' | 'loading' | 'ready' | 'error'>('idle');
const exchangeTruncated = ref(0);
const mutualValueOpen = ref(false);
let exchangeEpoch = 0;

function decodeEntry(opportunity: { id: string; title: string }, current: Record<string, unknown>): MutualValueEntry {
  const interests = Array.isArray(current.interests)
    ? current.interests.flatMap((raw) => {
        const item = typeof raw === 'object' && raw !== null ? (raw as Record<string, unknown>) : null;
        if (!item || typeof item.statement !== 'string') return [];
        return [{ side: typeof item.side === 'string' ? item.side : '', statement: item.statement, knowledgeKind: typeof item.knowledge_kind === 'string' ? item.knowledge_kind : '' }];
      })
    : [];
  return {
    opportunityId: opportunity.id,
    title: opportunity.title,
    theyGet: typeof current.they_get === 'string' ? current.they_get : '',
    iGet: typeof current.i_get === 'string' ? current.i_get : '',
    attested: current.fairness_attested === true,
    attestedAt: typeof current.attested_at === 'string' ? current.attested_at : null,
    interests,
  };
}

async function loadMutualValue(): Promise<void> {
  if (!props.party) return;
  const epoch = ++exchangeEpoch;
  const partyId = props.party.id;
  const opportunities = props.dossier?.opportunities ?? [];
  exchangeTruncated.value = Math.max(0, opportunities.length - EXCHANGE_FETCH_CAP);
  if (!opportunities.length) {
    exchangeEntries.value = [];
    exchangeState.value = 'ready';
    return;
  }
  exchangeState.value = 'loading';
  try {
    const results = await Promise.all(opportunities.slice(0, EXCHANGE_FETCH_CAP).map(async (opportunity) => {
      const exchange = await parley.getWritingExchange(opportunity.id, partyId);
      return exchange.current ? decodeEntry(opportunity, exchange.current) : null;
    }));
    if (epoch !== exchangeEpoch) return;
    exchangeEntries.value = results.flatMap((entry) => (entry ? [entry] : []));
    exchangeState.value = 'ready';
  } catch {
    if (epoch !== exchangeEpoch) return;
    exchangeState.value = 'error';
  }
}

function onMutualValueToggle(event: Event): void {
  const details = event.currentTarget as HTMLDetailsElement;
  mutualValueOpen.value = details.open;
  if (details.open && exchangeState.value === 'idle') void loadMutualValue();
}

function shortAttestedDate(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? '' : ' ' + date.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
}

watch(() => props.party?.id ?? null, () => {
  exchangeEpoch += 1;
  exchangeEntries.value = [];
  exchangeTruncated.value = 0;
  exchangeState.value = 'idle';
  if (mutualValueOpen.value) void loadMutualValue();
});
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
      <details
        class="inspector-section"
        data-testid="mutual-value"
        @toggle="onMutualValueToggle"
      >
        <summary>
          Mutual value<template v-if="exchangeState === 'ready'">
            ({{ exchangeEntries.length }})
          </template>
        </summary>
        <p
          v-if="exchangeState === 'loading'"
          role="status"
          class="pad-line"
        >
          Loading the exchanges…
        </p>
        <p
          v-else-if="exchangeState === 'error'"
          role="alert"
          class="pad-line"
        >
          Exchanges could not be loaded.
          <button
            type="button"
            class="path-link"
            @click="loadMutualValue()"
          >
            Retry
          </button>
        </p>
        <p
          v-else-if="exchangeState === 'ready' && !exchangeEntries.length"
          class="pad-line"
        >
          No exchange recorded yet.
          <button
            type="button"
            class="path-link"
            @click="emit('navigate', '/output')"
          >
            Set one in Output
          </button>
        </p>
        <ul
          v-else-if="exchangeEntries.length"
          class="inspector-list mutual-value-list"
        >
          <li
            v-for="entry in exchangeEntries"
            :key="entry.opportunityId"
          >
            <button @click="emit('navigate', '/queue/' + entry.opportunityId)">
              {{ entry.title }}
            </button>
            <small
              class="mv-badge"
              :class="entry.attested ? 'attested' : 'unattested'"
            > · {{ entry.attested ? 'attested' : 'needs attestation' }}<template v-if="entry.attestedAt">
              {{ shortAttestedDate(entry.attestedAt) }}
            </template></small>
            <p>They get: {{ entry.theyGet }}</p>
            <p>You get: {{ entry.iGet }}</p>
            <ul
              v-if="entry.interests.length"
              class="interest-lines"
            >
              <li
                v-for="(interest, index) in entry.interests"
                :key="index"
              >
                <small>· {{ interest.knowledgeKind.replaceAll('_', ' ') }}<template v-if="interest.side === 'them' || interest.side === 'me'"> ({{ interest.side === 'them' ? 'their side' : 'your side' }})</template>: {{ interest.statement }}</small>
              </li>
            </ul>
          </li>
        </ul>
        <p
          v-if="exchangeState === 'ready' && exchangeTruncated > 0"
          class="pad-line"
        >
          <small>and {{ exchangeTruncated }} more — see Output.</small>
        </p>
      </details>
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
.mutual-value-list p { margin: .15rem 0 0; }
.mutual-value-list .mv-badge { display: inline; }
.mv-badge.attested { color: #225744; font-weight: 600; }
.mv-badge.unattested { color: #8a5a19; font-weight: 600; }
.interest-lines { list-style: none; margin: .1rem 0 .35rem; padding-left: .9rem; }
.interest-lines small { color: #58665f; }
</style>
