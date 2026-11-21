import { describe, expect, it } from 'vitest';
import { mount } from '@vue/test-utils';
import AppShell from './AppShell.vue';
import { tabs } from './tabs';

describe('App shell', () => {
  it('renders the five owner tabs in the mandated APP-01 order', () => {
    const wrapper = mount(AppShell);
    const labels = wrapper
      .findAll('nav[aria-label="Main navigation"] button')
      .map((button) => button.text());
    expect(labels).toEqual([...tabs]);
  });

  it('declares exactly the five top-level destinations from APP-01', () => {
    expect([...tabs]).toEqual(['Resources', 'Leads', 'Queue', 'Output', 'Settings']);
  });
});
