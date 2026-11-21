import { mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley } from '../api/client';
import AppShell from './AppShell.vue';
import { tabs } from './tabs';

afterEach(() => vi.restoreAllMocks());

describe('AppShell', () => {
  it('opens the working Resources journey without exposing unfinished destinations', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'listResources').mockResolvedValue([]);
    const wrapper = mount(AppShell);
    expect(tabs).toEqual(['Resources', 'Leads', 'Queue', 'Output', 'Settings']);
    expect(wrapper.get('h1').text()).toBe('Resources');
    await wrapper.get('input').setValue('unsent resource title');
    expect(wrapper.find('nav').exists()).toBe(false);
    expect((wrapper.get('input').element as HTMLInputElement).value).toBe('unsent resource title');
    wrapper.unmount();
  });
});
