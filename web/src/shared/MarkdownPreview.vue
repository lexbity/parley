<script setup lang="ts">
import { computed } from 'vue';

const props = defineProps<{ source: string }>();
type Block = { kind: 'paragraph' | 'heading' | 'ul' | 'ol' | 'code'; text: string; level?: number; items?: string[] };

const blocks = computed<Block[]>(() => {
  const lines = props.source.replace(/\r\n?/g, '\n').split('\n');
  const result: Block[] = [];
  let paragraph: string[] = [];
  let listKind: 'ul' | 'ol' | null = null;
  let listItems: string[] = [];
  let code: string[] | null = null;
  const flushParagraph = () => { if (paragraph.length) result.push({ kind: 'paragraph', text: paragraph.join(' ') }); paragraph = []; };
  const flushList = () => { if (listKind && listItems.length) result.push({ kind: listKind, text: '', items: [...listItems] }); listKind = null; listItems = []; };
  for (const line of lines) {
    if (code) { if (/^\s*```/.test(line)) { result.push({ kind: 'code', text: code.join('\n') }); code = null; } else code.push(line); continue; }
    if (/^\s*```/.test(line)) { flushParagraph(); flushList(); code = []; continue; }
    if (!line.trim()) { flushParagraph(); flushList(); continue; }
    const heading = /^(#{1,6})\s+(.+)$/.exec(line);
    if (heading) { flushParagraph(); flushList(); result.push({ kind: 'heading', text: heading[2]!, level: heading[1]!.length }); continue; }
    const list = /^\s*(?:[-+*]|\d+[.)])\s+(.+)$/.exec(line);
    if (list) {
      flushParagraph();
      const nextKind = /^\s*\d/.test(line) ? 'ol' : 'ul';
      if (listKind !== nextKind) flushList();
      listKind = nextKind; listItems.push(list[1]!); continue;
    }
    flushList(); paragraph.push(line.trim());
  }
  flushParagraph(); flushList();
  if (code) result.push({ kind: 'code', text: code.join('\n') });
  return result;
});

function taskChecked(item: string): boolean | null {
  const marker = /^\[[ xX]\]\s+/.exec(item);
  return marker ? marker[0].toLowerCase().startsWith('[x]') : null;
}

function taskText(item: string): string { return item.replace(/^\[[ xX]\]\s+/, ''); }

</script>

<template>
  <div class="markdown-preview">
    <template
      v-for="(block, index) in blocks"
      :key="index"
    >
      <pre v-if="block.kind === 'code'"><code>{{ block.text }}</code></pre>
      <component
        :is="`h${block.level}`"
        v-else-if="block.kind === 'heading'"
      >
        {{ block.text }}
      </component>
      <component
        :is="block.kind"
        v-else-if="block.kind === 'ul' || block.kind === 'ol'"
      >
        <li
          v-for="(item, itemIndex) in block.items"
          :key="itemIndex"
        >
          <input
            v-if="taskChecked(item) !== null"
            type="checkbox"
            disabled
            :checked="taskChecked(item) === true"
            :aria-label="`Preview task ${taskText(item)}`"
          >
          {{ taskText(item) }}
        </li>
      </component>
      <p v-else>
        {{ block.text }}
      </p>
    </template>
  </div>
</template>
