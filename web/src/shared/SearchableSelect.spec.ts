import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import SearchableSelect from './SearchableSelect.vue';
import type { SelectOption } from './select-options';

const options: SelectOption[] = Array.from({ length: 200 }, (_, index) => ({
  value: `o${index}`,
  label: `Opportunity ${String(index).padStart(3, '0')}`,
}));

describe('SearchableSelect', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('mounts only a bounded window of options', () => {
    const wrapper = mount(SearchableSelect, {
      props: { modelValue: '', options, label: 'Opportunity', id: 'pick' },
    });
    expect(wrapper.findAll('option')).toHaveLength(50);
    wrapper.unmount();
  });

  it('filters after a debounce and keeps the selected option visible', async () => {
    const wrapper = mount(SearchableSelect, {
      props: { modelValue: 'o150', options, label: 'Opportunity', id: 'pick' },
    });
    // The selected option is outside the first window and is pinned into it.
    expect(wrapper.findAll('option').map(option => option.element.value)).toContain('o150');

    await wrapper.get('input[type="search"]').setValue('Opportunity 15');
    expect(wrapper.findAll('option').map(option => option.element.value)).toContain('o150');
    vi.advanceTimersByTime(120);
    await wrapper.vm.$nextTick();
    const values = wrapper.findAll('option').map(option => option.element.value);
    expect(values).toContain('o150');
    expect(values.length).toBeLessThanOrEqual(50);
    expect(values.every(value => options.find(option => option.value === value)?.label.includes('15'))).toBe(true);
    wrapper.unmount();
  });

  it('queries the server per debounced term in async mode', async () => {
    const search = vi.fn(async (term: string) =>
      options.filter(option => option.label.toLocaleLowerCase().includes(term.toLocaleLowerCase())),
    );
    const wrapper = mount(SearchableSelect, {
      props: { modelValue: '', options: options.slice(0, 50), label: 'Opportunity', id: 'pick', search },
    });
    expect(search).toHaveBeenCalledWith('');
    search.mockClear();
    await wrapper.get('input[type="search"]').setValue('Opportunity 15');
    expect(search).not.toHaveBeenCalled();
    vi.advanceTimersByTime(120);
    await flushPromises();
    expect(search).toHaveBeenCalledTimes(1);
    expect(search).toHaveBeenCalledWith('Opportunity 15');
    const values = wrapper.findAll('option').map(option => option.element.value);
    expect(values).toContain('o150');
    expect(values.length).toBeLessThanOrEqual(50);
    wrapper.unmount();
  });

  it('emits native select changes', async () => {
    const wrapper = mount(SearchableSelect, {
      props: { modelValue: '', options, label: 'Opportunity', id: 'pick' },
    });
    await wrapper.get('select').setValue('o7');
    expect(wrapper.emitted('update:modelValue')).toEqual([['o7']]);
    wrapper.unmount();
  });
});
