import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';
import AppShell from './AppShell.vue';
import { tabs } from './tabs';

const api = vi.hoisted(() => ({
  captureSource: vi.fn(), createEntity: vi.fn(), createResource: vi.fn(),
  listEntities: vi.fn(), listResources: vi.fn(), listSources: vi.fn(),
}));
vi.mock('../api/client', () => api);

const entity = { id: 'p1', name: 'Dana Lee', kind: 'person', revision: 1 };
const resource = { id: 'r1', title: 'Walkthrough', kind: 'gift', readiness: 'draft', revision: 1 };

describe('App shell', () => {
  afterEach(() => vi.resetAllMocks());

  it('renders the five owner tabs in the mandated APP-01 order', async () => {
    api.listEntities.mockResolvedValue([entity]);
    api.listResources.mockResolvedValue([resource]);
    api.listSources.mockResolvedValue([]);
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('select').setValue('proof');
    await wrapper.get('select').setValue('artifact');
    const labels = wrapper.findAll('nav[aria-label="Main navigation"] button').map((button) => button.text());
    expect(labels).toEqual([...tabs]);
    expect(wrapper.text()).toContain('Walkthrough');
  });

  it('creates an incomplete resource draft and reports a failed save', async () => {
    api.listEntities.mockResolvedValue([]); api.listResources.mockResolvedValue([]); api.listSources.mockResolvedValue([]);
    api.createResource.mockResolvedValue(resource);
    const wrapper = mount(AppShell); await flushPromises();
    await wrapper.get('input[placeholder*="onboarding"]').setValue('Walkthrough');
    await wrapper.get('textarea').setValue('A real walkthrough offer');
    await wrapper.find('form').trigger('submit'); await flushPromises();
    expect(api.createResource).toHaveBeenCalledWith(expect.objectContaining({ kind: 'gift', title: 'Walkthrough' }));
    expect(wrapper.text()).toContain('Draft saved');
    api.createResource.mockRejectedValueOnce(new Error('disk full'));
    await wrapper.find('form').trigger('submit'); await flushPromises();
    expect(wrapper.text()).toContain('disk full');
  });

  it('creates a lead without an opportunity and saves source-only captures', async () => {
    api.listEntities.mockResolvedValue([]); api.listResources.mockResolvedValue([]); api.listSources.mockResolvedValue([]);
    api.createEntity.mockResolvedValue(entity);
    api.captureSource.mockResolvedValue({ content_id: 'c1', variant_id: 'v1', occurrence_id: 'o1', normalized_sha256: 'a', raw_sha256: 'b' });
    const wrapper = mount(AppShell); await flushPromises();
    await wrapper.get('nav button:nth-child(2)').trigger('click');
    await wrapper.get('select').setValue('org');
    await wrapper.get('select').setValue('person');
    await wrapper.get('input[required]').setValue('Dana Lee');
    await wrapper.findAll('form')[0]!.trigger('submit'); await flushPromises();
    expect(api.createEntity).toHaveBeenCalledWith('person', 'Dana Lee');
    expect(wrapper.text()).toContain('No opportunity or outreach was created');
    const areas = wrapper.findAll('textarea');
    await areas[0]!.setValue('Hiring update with an unresolved Dana mention.');
    await wrapper.findAll('form')[1]!.trigger('submit'); await flushPromises();
    expect(api.captureSource).toHaveBeenCalledWith('Hiring update with an unresolved Dana mention.');
    expect(wrapper.text()).toContain('No person was inferred or linked');
  });

  it('shows source and lead failures and retains the other tab destinations', async () => {
    api.listEntities.mockRejectedValue(new Error('workspace offline'));
    api.listResources.mockResolvedValue([]); api.listSources.mockResolvedValue([]);
    const wrapper = mount(AppShell); await flushPromises();
    expect(wrapper.text()).toContain('workspace offline');
    await wrapper.get('nav button:nth-child(2)').trigger('click');
    api.createEntity.mockRejectedValueOnce(new Error('write denied'));
    await wrapper.get('input[required]').setValue('Noah');
    await wrapper.findAll('form')[0]!.trigger('submit'); await flushPromises();
    expect(wrapper.text()).toContain('write denied');
    api.captureSource.mockRejectedValueOnce(new Error('capture denied'));
    await wrapper.findAll('textarea')[0]!.setValue('A source');
    await wrapper.findAll('form')[1]!.trigger('submit'); await flushPromises();
    expect(wrapper.text()).toContain('capture denied');
    for (const tab of ['Queue', 'Output', 'Settings']) {
      const button = wrapper.findAll('nav button').find((candidate) => candidate.text() === tab);
      expect(button?.attributes('disabled')).toBeDefined();
    }
    expect(wrapper.get('h1').text()).toBe('Leads');
  });

  it('declares exactly the five top-level destinations from APP-01', () => {
    expect([...tabs]).toEqual(['Resources', 'Leads', 'Queue', 'Output', 'Settings']);
  });
});
