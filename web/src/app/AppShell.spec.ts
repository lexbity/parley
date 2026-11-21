import { mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushPromises } from '@vue/test-utils';
import { parley, type GraphEntity, type SavedSource } from '../api/client';
import AppShell from './AppShell.vue';
import { tabs } from './tabs';

afterEach(() => vi.restoreAllMocks());

function stubWorkspace(parties: GraphEntity[] = [], sources: SavedSource[] = []): void {
  vi.spyOn(parley, 'connect').mockResolvedValue();
  vi.spyOn(parley, 'listResources').mockResolvedValue([]);
  vi.spyOn(parley, 'listParties').mockResolvedValue(parties);
  vi.spyOn(parley, 'listSources').mockResolvedValue(sources);
}

function leadsSection(wrapper: ReturnType<typeof mount>) {
  return wrapper.get('section[aria-labelledby="leads-heading"]');
}

describe('AppShell', () => {
  it('renders the mandated destination set and only the qualified tabs', () => {
    expect([...tabs]).toEqual(['Resources', 'Leads', 'Queue', 'Output', 'Settings']);
    stubWorkspace();
    const wrapper = mount(AppShell);
    const labels = wrapper.findAll('nav[aria-label="Main navigation"] button').map((button) => button.text());
    expect(labels).toEqual(['Resources', 'Leads']);
    for (const destination of ['Queue', 'Output', 'Settings']) {
      expect(labels).not.toContain(destination);
    }
    expect(wrapper.get('h1').text()).toBe('Resources');
    wrapper.unmount();
  });

  it('keeps the resource editor mounted with its unsent buffer across tab switches', async () => {
    stubWorkspace();
    const wrapper = mount(AppShell);
    await flushPromises();
    const editorInput = wrapper.get('section[aria-label="Resources workspace"] input');
    await editorInput.setValue('unsent resource title');
    await wrapper.get('nav[aria-label="Main navigation"] button:nth-child(2)').trigger('click');
    expect(wrapper.get('h1').text()).toBe('Leads');
    await wrapper.get('nav[aria-label="Main navigation"] button:nth-child(1)').trigger('click');
    expect((wrapper.get('section[aria-label="Resources workspace"] input').element as HTMLInputElement).value)
      .toBe('unsent resource title');
    wrapper.unmount();
  });

  it('creates a lead without an opportunity and saves source-only captures', async () => {
    stubWorkspace([], []);
    const createEntity = vi.spyOn(parley, 'createEntity')
      .mockResolvedValue({ id: 'p1', kind: 'person', name: 'Dana Lee', revision: 1 });
    const saveSource = vi.spyOn(parley, 'saveSource').mockResolvedValue({
      content_id: 'c1', variant_id: 'v1', occurrence_id: 'o1', normalized_sha256: 'a', raw_sha256: 'b',
    });
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] button:nth-child(2)').trigger('click');
    const leads = leadsSection(wrapper);
    await leads.get('select').setValue('org');
    await leads.get('select').setValue('person');
    await leads.get('input[required]').setValue('Dana Lee');
    await leads.findAll('form')[0]!.trigger('submit');
    await flushPromises();
    expect(createEntity).toHaveBeenCalledWith({ kind: 'person', name: 'Dana Lee' }, expect.any(String));
    expect(wrapper.text()).toContain('No opportunity or outreach was created');
    const areas = leads.findAll('textarea');
    await areas[0]!.setValue('Hiring update with an unresolved Dana mention.');
    await leads.findAll('form')[1]!.trigger('submit');
    await flushPromises();
    expect(saveSource).toHaveBeenCalledWith({ text: 'Hiring update with an unresolved Dana mention.', title: null, url: null });
    expect(wrapper.text()).toContain('No person was inferred or linked');
    wrapper.unmount();
  });

  it('reports a failed workspace connection instead of fetching directory data', async () => {
    const connect = vi.spyOn(parley, 'connect').mockRejectedValue(new Error('session refused'));
    const listParties = vi.spyOn(parley, 'listParties');
    vi.spyOn(parley, 'listResources').mockResolvedValue([]);
    vi.spyOn(parley, 'listParties').mockResolvedValue([]);
    vi.spyOn(parley, 'listSources').mockResolvedValue([]);
    const wrapper = mount(AppShell);
    await flushPromises();
    expect(connect).toHaveBeenCalled();
    expect(wrapper.text()).toContain('session refused');
    expect(listParties).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('reports directory and capture failures without losing the other destinations', async () => {
    stubWorkspace([], []);
    vi.spyOn(parley, 'createEntity').mockRejectedValueOnce(new Error('write denied'));
    vi.spyOn(parley, 'saveSource').mockRejectedValueOnce(new Error('capture denied'));
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] button:nth-child(2)').trigger('click');
    const leads = leadsSection(wrapper);
    await leads.get('input[required]').setValue('Noah');
    await leads.findAll('form')[0]!.trigger('submit');
    await flushPromises();
    expect(wrapper.text()).toContain('write denied');
    await leads.findAll('textarea')[0]!.setValue('A source');
    await leads.findAll('form')[1]!.trigger('submit');
    await flushPromises();
    expect(wrapper.text()).toContain('capture denied');
    const labels = wrapper.findAll('nav[aria-label="Main navigation"] button').map((button) => button.text());
    expect(labels).toEqual(['Resources', 'Leads']);
    wrapper.unmount();
  });
});
