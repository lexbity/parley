import { mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushPromises } from '@vue/test-utils';
import { parley, type GraphEntity, type Material, type Opportunity, type SavedSource } from '../api/client';
import AppShell from './AppShell.vue';
import { tabs } from './tabs';

afterEach(() => vi.restoreAllMocks());

function stubWorkspace(parties: GraphEntity[] = [], sources: SavedSource[] = []): void {
  vi.spyOn(parley, 'connect').mockResolvedValue();
  vi.spyOn(parley, 'listResources').mockResolvedValue([]);
  vi.spyOn(parley, 'listParties').mockResolvedValue(parties);
  vi.spyOn(parley, 'listSources').mockResolvedValue(sources);
  vi.spyOn(parley, 'listOpportunities').mockResolvedValue([]);
  vi.spyOn(parley, 'listOutputs').mockResolvedValue([]);
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
    expect(labels).toEqual(['Resources', 'Leads', 'Queue', 'Output']);
    expect(labels).not.toContain('Settings');
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

  it('creates a selected-participant opportunity and presents shared Output records', async () => {
    stubWorkspace([{ id: 'p1', kind: 'person', name: 'Dana Lee', revision: 1 }]);
    const created: Opportunity = { id: 'o1', title: 'Onboarding exchange', state: 'open', revision: 1, participant_ids: ['p1'], source_variant_ids: [], material_ids: ['m1'] };
    const queued: Material = { id: 'm1', opportunity_id: 'o1', recipient_id: 'p1', recipient_name: 'Dana Lee', kind: 'outreach', version: 1, body: 'Hello Dana.', content_sha256: 'a'.repeat(64), status: 'draft', gate: null };
    vi.spyOn(parley, 'listOutputs').mockResolvedValue([queued]);
    const createOpportunity = vi.spyOn(parley, 'createOpportunity').mockResolvedValue(created);
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] button:nth-child(3)').trigger('click');
    await wrapper.get('input[maxlength="500"]').setValue('Onboarding exchange');
    await wrapper.get('select[required]').setValue('p1');
    await wrapper.get('section[aria-labelledby="queue-heading"] form').trigger('submit');
    await flushPromises();
    expect(createOpportunity).toHaveBeenCalledWith('Onboarding exchange', 'p1');
    expect(wrapper.text()).toContain('No message was created or sent');
    expect(wrapper.text()).toContain('Onboarding exchange');
    expect(wrapper.text()).toContain('Dana Lee · v1 · draft');
    await wrapper.get('nav[aria-label="Main navigation"] button:nth-child(4)').trigger('click');
    expect(wrapper.text()).toContain('Approved materials');
    expect(wrapper.text()).toContain('Dana Lee · outreach · v1 · draft');
    wrapper.unmount();
  });

  it('reports opportunity creation errors and copies a released approved version', async () => {
    const approved: Material = { id: 'm1', opportunity_id: 'o1', recipient_id: 'p1', recipient_name: 'Dana Lee', kind: 'outreach', version: 1, body: 'Hi Dana.', content_sha256: 'a'.repeat(64), status: 'approved', gate: null };
    stubWorkspace([{ id: 'p1', kind: 'person', name: 'Dana Lee', revision: 1 }]);
    vi.spyOn(parley, 'listOutputs').mockResolvedValue([approved]);
    vi.spyOn(parley, 'createOpportunity').mockRejectedValueOnce(new Error('opportunity denied'));
    const release = vi.spyOn(parley, 'releaseMaterial').mockResolvedValue({ release_id: 'r1', material_id: 'm1', version: 1, recipient_id: 'p1', body: 'Hi Dana.', content_sha256: approved.content_sha256, issued_at: '2026-11-21T12:00:00Z' });
    const acknowledge = vi.spyOn(parley, 'acknowledgeRelease').mockResolvedValue();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(globalThis.navigator, 'clipboard', { configurable: true, value: { writeText } });
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] button:nth-child(3)').trigger('click');
    await wrapper.get('input[maxlength="500"]').setValue('Onboarding');
    await wrapper.get('select[required]').setValue('p1');
    await wrapper.get('section[aria-labelledby="queue-heading"] form').trigger('submit');
    await flushPromises();
    expect(wrapper.text()).toContain('opportunity denied');
    await wrapper.get('nav[aria-label="Main navigation"] button:nth-child(4)').trigger('click');
    await wrapper.findAll('button').find((button) => button.text() === 'Copy approved text')!.trigger('click');
    await flushPromises();
    expect(release).toHaveBeenCalledWith('m1');
    expect(writeText).toHaveBeenCalledWith('Hi Dana.');
    expect(acknowledge).toHaveBeenCalledWith('r1', true);
    expect(wrapper.text()).toContain('Marking it sent is a separate action');
    wrapper.unmount();
  });

  it('records failed clipboard handoff after issuance and keeps unapproved copy disabled', async () => {
    const approved: Material = { id: 'm2', opportunity_id: 'o1', recipient_id: 'p1', recipient_name: 'Dana Lee', kind: 'outreach', version: 2, body: 'Hi Dana.', content_sha256: 'b'.repeat(64), status: 'approved', gate: null };
    const draft: Material = { ...approved, id: 'm3', version: 1, status: 'draft', gate: null };
    stubWorkspace();
    vi.spyOn(parley, 'listOutputs').mockResolvedValue([approved, draft]);
    vi.spyOn(parley, 'releaseMaterial').mockResolvedValue({ release_id: 'r2', material_id: 'm2', version: 2, recipient_id: 'p1', body: 'Hi Dana.', content_sha256: approved.content_sha256, issued_at: '2026-11-21T12:00:00Z' });
    const acknowledge = vi.spyOn(parley, 'acknowledgeRelease').mockResolvedValue();
    Object.defineProperty(globalThis.navigator, 'clipboard', { configurable: true, value: { writeText: vi.fn().mockRejectedValue(new Error('clipboard unavailable')) } });
    const wrapper = mount(AppShell);
    await flushPromises();
    await wrapper.get('nav[aria-label="Main navigation"] button:nth-child(4)').trigger('click');
    const copyButtons = wrapper.findAll('button').filter((button) => button.text() === 'Copy approved text');
    expect(copyButtons[1]?.attributes('disabled')).toBeDefined();
    await copyButtons[0]!.trigger('click');
    await flushPromises();
    expect(acknowledge).toHaveBeenCalledWith('r2', false);
    expect(wrapper.text()).toContain('clipboard unavailable');
    wrapper.unmount();
  });

  it('reports a failed workspace connection instead of fetching directory data', async () => {
    const connect = vi.spyOn(parley, 'connect').mockRejectedValue(new Error('session refused'));
    const listParties = vi.spyOn(parley, 'listParties');
    vi.spyOn(parley, 'listResources').mockResolvedValue([]);
    vi.spyOn(parley, 'listParties').mockResolvedValue([]);
    vi.spyOn(parley, 'listSources').mockResolvedValue([]);
    vi.spyOn(parley, 'listOpportunities').mockResolvedValue([]);
    vi.spyOn(parley, 'listOutputs').mockResolvedValue([]);
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
    expect(labels).toEqual(['Resources', 'Leads', 'Queue', 'Output']);
    wrapper.unmount();
  });
});
