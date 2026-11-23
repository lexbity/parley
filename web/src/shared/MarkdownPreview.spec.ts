import { mount } from '@vue/test-utils';
import { describe, expect, it } from 'vitest';
import MarkdownPreview from './MarkdownPreview.vue';

describe('MarkdownPreview', () => {
  it('renders allowlisted inline emphasis, code, links and blockquotes', () => {
    const source = [
      'Welcomes **small, reproducible fixtures** and `quoted commas` with *care*.',
      '',
      '> We welcome small fixtures.',
      '',
      'See [the guide](https://example.org/guide) or [local](/resources/r1).',
    ].join('\n');
    const wrapper = mount(MarkdownPreview, { props: { source } });
    const html = wrapper.html();
    expect(html).toContain('<strong>small, reproducible fixtures</strong>');
    expect(html).toContain('<code>quoted commas</code>');
    expect(html).toContain('<em>care</em>');
    expect(html).toContain('<blockquote');
    expect(html).toContain('We welcome small fixtures.');
    expect(html).toContain('<a href="https://example.org/guide" target="_blank" rel="noopener noreferrer">the guide</a>');
    expect(html).toContain('<a href="/resources/r1">local</a>');
  });

  it('never executes raw HTML and round-trips literal punctuation', () => {
    const source = '<img src=x onerror=alert(1)> and **literal <b>bold</b>** stay text';
    const wrapper = mount(MarkdownPreview, { props: { source } });
    expect(wrapper.find('img').exists()).toBe(false);
    expect(wrapper.html()).not.toContain('<img');
    expect(wrapper.html()).toContain('&lt;img');
    expect(wrapper.html()).toContain('<strong>literal &lt;b&gt;bold&lt;/b&gt;</strong>');
  });

  it('preserves lists, task boxes, fenced code and heading levels', () => {
    const source = ['# Test case', '- Try a multiline field', '- [x] Compare rows', '', '```', 'plain', 'code', '```'].join('\n');
    const wrapper = mount(MarkdownPreview, { props: { source } });
    expect(wrapper.find('h1').text()).toBe('Test case');
    expect(wrapper.findAll('ul li').length).toBe(2);
    expect(wrapper.find('input[type="checkbox"]').attributes('checked')).toBeDefined();
    expect(wrapper.find('pre code').text()).toBe('plain\ncode');
  });
});
