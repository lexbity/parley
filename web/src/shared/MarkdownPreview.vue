<script setup lang="ts">
import { computed } from 'vue';

const props = defineProps<{ source: string }>();
type Block = { kind: 'paragraph' | 'heading' | 'ul' | 'ol' | 'code' | 'blockquote'; text: string; level?: number; items?: string[] };

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
    const quote = /^\s*>\s?(.*)$/.exec(line);
    if (quote) { flushParagraph(); flushList(); result.push({ kind: 'blockquote', text: quote[1]! }); continue; }
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

/** Allowlisted inline Markdown rendered after HTML-escaping, so literal
 * punctuation, code and lists round-trip and raw HTML never executes. */
function inline(text: string): string {
  const escaped = text
    .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');
  return escaped
    .replace(/`([^`]+)`/g, '<code>$1</code>')
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/\*([^*]+)\*/g, '<em>$1</em>')
    .replace(/\[([^\]]+)\]\((https?:\/\/[^)\s]+|\/[^)\s]*|mailto:[^)\s]+)\)/g, (_match, label: string, href: string) => {
      const external = /^https?:\/\//.test(href);
      return `<a href="${href}"${external ? ' target="_blank" rel="noopener noreferrer"' : ''}>${label}</a>`;
    });
}
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
      <!-- eslint-disable vue/no-v-html -->
      <blockquote
        v-else-if="block.kind === 'blockquote'"
        v-html="inline(block.text)"
      />
      <!-- eslint-enable vue/no-v-html -->
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
          <!-- eslint-disable-next-line vue/no-v-html -->
          <span v-html="inline(taskText(item))" />
        </li>
      </component>
      <!-- eslint-disable vue/no-v-html -->
      <p
        v-else
        v-html="inline(block.text)"
      />
      <!-- eslint-enable vue/no-v-html -->
    </template>
  </div>
</template>

<style scoped>
.markdown-preview blockquote { margin: .4rem 0; padding: .35rem .6rem; border-left: 3px solid #32735b; background: #f2f4ec; }
.markdown-preview code { background: #e4e8de; border-radius: .2rem; padding: 0 .25rem; }
.markdown-preview a { color: #225744; }
</style>
