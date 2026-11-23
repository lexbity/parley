import { mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushPromises } from '@vue/test-utils';
import { parley, type Dossier, type EntityNotes, type GraphEntity } from '../../api/client';
import PartyDesk from './PartyDesk.vue';
import MarkdownEditor from '../../shared/MarkdownEditor.vue';

afterEach(() => vi.restoreAllMocks());

const fieldwork: GraphEntity = { id: 'p1', kind: 'org', name: 'Fieldwork', focus: 'CSV tools', revision: 1, etag: '"p1.epoch.entity.p1.1"' };
const mira: GraphEntity = { id: 'p2', kind: 'person', name: 'Mira Chen', focus: 'Maintenance', revision: 1, etag: '"p1.epoch.entity.p2.1"' };

const emptyDossier: Dossier = { entity: fieldwork, direct_links: [], files: [], opportunities: [], associated_resources: [] };
const notes: EntityNotes = { entity_id: 'p1', markdown: 'Welcomes **fixtures**.\n', revision: 1, etag: '"p1.epoch.notes.p1.1"' };

function stubDesk(parties: GraphEntity[] = [fieldwork, mira]) {
  vi.spyOn(parley, 'getGraph').mockResolvedValue({ nodes: [], edges: [], omitted_nodes: 0, omitted_edges: 0 });
  vi.spyOn(parley, 'listSources').mockResolvedValue([]);
  vi.spyOn(parley, 'listAgentChats').mockResolvedValue([]);
  vi.spyOn(parley, 'listSegments').mockResolvedValue([]);
  vi.spyOn(parley, 'listFluency').mockResolvedValue([]);
  vi.spyOn(parley, 'getDossier').mockResolvedValue(emptyDossier);
  vi.spyOn(parley, 'getEntityNotes').mockResolvedValue(notes);
  return parties;
}

describe('PartyDesk', () => {
  it('keeps the sheet root and the inspected selection distinct until a row click', async () => {
    stubDesk();
    const getDossier = vi.spyOn(parley, 'getDossier').mockResolvedValue({ ...emptyDossier, entity: mira });
    const wrapper = mount(PartyDesk, { props: { parties: [fieldwork, mira], sources: [], opportunities: [] } });
    await flushPromises();
    // Row click selects the row as both root and inspection target.
    await wrapper.findAll('.row-party')[0]!.trigger('click');
    await flushPromises();
    expect(getDossier).toHaveBeenCalledWith('p1');
    expect(wrapper.text()).toContain('Inspector');
    expect(wrapper.text()).toContain('Fieldwork');
    // A different row replaces root and selection.
    await wrapper.findAll('.row-party')[1]!.trigger('click');
    await flushPromises();
    expect(getDossier).toHaveBeenCalledWith('p2');
    expect(wrapper.text()).toContain('Mira Chen');
    wrapper.unmount();
  });

  it('edits a sheet focus cell through the typed party PATCH and refreshes the dossier', async () => {
    stubDesk();
    const updateParty = vi.spyOn(parley, 'updateParty').mockResolvedValue({ ...fieldwork, focus: 'CSV importer', revision: 2, etag: '"p1.epoch.entity.p1.2"' });
    const wrapper = mount(PartyDesk, { props: { parties: [fieldwork, mira], sources: [], opportunities: [] } });
    await flushPromises();
    await wrapper.findAll('.focus-cell')[0]!.trigger('click');
    const input = wrapper.get('.focus-input');
    await input.setValue('CSV importer');
    await wrapper.findAll('.focus-input')[0]!.element.parentElement!.querySelectorAll('button')[0]!.dispatchEvent(new Event('click'));
    await flushPromises();
    expect(updateParty).toHaveBeenCalledWith('p1', { focus: 'CSV importer' }, fieldwork.etag, expect.any(String));
    wrapper.unmount();
  });

  it('opens a card, loads notes, and reports a 412 notes conflict without overwriting newer input', async () => {
    stubDesk();
    const updateNotes = vi.spyOn(parley, 'updateEntityNotes')
      .mockRejectedValue(Object.assign(new Error('The notes changed in another session.'), { status: 412, code: 'REVISION_CONFLICT' }));
    const wrapper = mount(PartyDesk, { props: { parties: [fieldwork, mira], sources: [], opportunities: [] } });
    await flushPromises();
    await wrapper.findAll('.row-party')[0]!.trigger('click');
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Open card')!.trigger('click');
    await flushPromises();
    expect(parley.getEntityNotes).toHaveBeenCalledWith('p1');
    expect(wrapper.text()).toContain('Edit card · Fieldwork');
    // Drive a deterministic document change through the CodeMirror view so the
    // draft differs from the loaded revision and Save is enabled.
    const editor = wrapper.findComponent(MarkdownEditor);
    const view = (editor.vm as { getView(): { state: { doc: { toString(): string } }; dispatch(options: object): void } }).getView();
    view.dispatch({ changes: { from: 0, to: view.state.doc.toString().length, insert: 'New owner note\n' } });
    await flushPromises();
    const saveNotes = wrapper.findAll('button').find((button) => button.text() === 'Save notes')!;
    await saveNotes.trigger('click');
    await flushPromises();
    expect(updateNotes).toHaveBeenCalled();
    expect(wrapper.text()).toContain('changed in another session');
    wrapper.unmount();
  });

  it('promotes the inspected node to the network root via the inspector action', async () => {
    stubDesk();
    const wrapper = mount(PartyDesk, { props: { parties: [fieldwork, mira], sources: [], opportunities: [] } });
    await flushPromises();
    await wrapper.findAll('.row-party')[0]!.trigger('click');
    await flushPromises();
    // A graph node selection changes the inspected record without changing the
    // sheet root; the inspector then offers to open that party's network.
    wrapper.findComponent({ name: 'RelationshipGraph' }).vm.$emit('select-node', 'p2');
    await flushPromises();
    expect(wrapper.find('.outside-root').exists()).toBe(true);
    const promote = wrapper.findAll('.outside-root button')[0]!;
    await promote.trigger('click');
    expect(wrapper.text()).toContain('Network root is now Mira Chen');
    wrapper.unmount();
  });

  it('creates a party from the add form and selects it as the root', async () => {
    stubDesk();
    const createEntity = vi.spyOn(parley, 'createEntity').mockResolvedValue({ id: 'p9', kind: 'org', name: 'Alder', focus: '', revision: 1, etag: '"e"' });
    const wrapper = mount(PartyDesk, { props: { parties: [fieldwork, mira], sources: [], opportunities: [] } });
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Add party')!.trigger('click');
    await wrapper.get('form.add-party select').setValue('org');
    await wrapper.get('form.add-party input[required]').setValue('Alder');
    await wrapper.get('form.add-party').trigger('submit');
    await flushPromises();
    expect(createEntity).toHaveBeenCalledWith({ kind: 'org', name: 'Alder', focus: '' }, expect.any(String));
    expect(wrapper.text()).toContain('Party created. No opportunity, edge or outreach task was created');
    wrapper.unmount();
  });

  it('autosaves notes after composition ends and surfaces dossier load failure', async () => {
    vi.useFakeTimers();
    stubDesk();
    vi.spyOn(parley, 'getDossier').mockRejectedValue(new Error('dossier unavailable'));
    const updateNotes = vi.spyOn(parley, 'updateEntityNotes').mockResolvedValue({ entity_id: 'p1', markdown: 'New owner note\n', revision: 2, etag: '"p1.epoch.notes.p1.2"' });
    const wrapper = mount(PartyDesk, { props: { parties: [fieldwork, mira], sources: [], opportunities: [] } });
    await flushPromises();
    await wrapper.findAll('.row-party')[0]!.trigger('click');
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Open card')!.trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('dossier unavailable');
    const editor = wrapper.findComponent(MarkdownEditor);
    const view = (editor.vm as { getView(): { state: { doc: { toString(): string } }; dispatch(options: object): void } }).getView();
    view.dispatch({ changes: { from: 0, to: view.state.doc.toString().length, insert: 'New owner note\n' } });
    await vi.advanceTimersByTimeAsync(800);
    await flushPromises();
    expect(updateNotes).toHaveBeenCalledWith('p1', 'New owner note\n', 1, '"p1.epoch.notes.p1.1"', expect.any(String));
    expect(wrapper.text()).toContain('Notes saved.');
    wrapper.unmount();
    vi.useRealTimers();
  });

  it('surfaces decision and extraction paths for captures', async () => {
    stubDesk();
    const job = {
      id: 'job1', role: 'extractor', status: 'queued', current_attempt: 0, max_attempts: 3,
      progress_current: 0, progress_total: 1, injection_suspected: false, last_error_code: null, last_error: null,
      source_occurrence_id: 'o1', proposal_batch_id: 'batch1', proposals: [
        { id: 'p', kind: 'signal', status: 'pending', candidate: { statement: 'A claim.', quote: 'A claim.', mention_name: null, mention_kind: null } },
      ],
    };
    vi.spyOn(parley, 'saveSource').mockResolvedValue({ content_id: 'c1', variant_id: 'v1', occurrence_id: 'o1', normalized_sha256: 'a', raw_sha256: 'b', job_id: 'job1', job_status: 'queued' });
    vi.spyOn(parley, 'getExtractorJob').mockResolvedValue(job);
    vi.spyOn(parley, 'resolveProposals').mockRejectedValue(new Error('decision denied'));
    const wrapper = mount(PartyDesk, { props: { parties: [fieldwork, mira], sources: [], opportunities: [] } });
    await flushPromises();
    await wrapper.get('[role="navigation"][aria-label="Lead workspaces"] button').trigger('click');
    await wrapper.get('form.capture textarea').setValue('A captured sentence.');
    await wrapper.get('form.capture').trigger('submit');
    await flushPromises();
    // Confirm without an explicit subject is refused before the API call.
    await wrapper.get('section[aria-label="Capture review"] article button').trigger('click');
    expect(wrapper.text()).toContain('Choose the person or organization this claim is about');
    // Dismiss passes through and reports the decision failure.
    await wrapper.get('section[aria-label="Capture review"] article button:nth-of-type(4)').trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('decision denied');
    wrapper.unmount();
  });

  it('reports focus save when the row lacks an editable etag', async () => {
    stubDesk();
    const noEtag = { ...fieldwork, etag: undefined };
    const wrapper = mount(PartyDesk, { props: { parties: [noEtag, mira], sources: [], opportunities: [] } });
    await flushPromises();
    await wrapper.findAll('.focus-cell')[0]!.trigger('click');
    await wrapper.get('.focus-input').setValue('x');
    await wrapper.get('.focus-input').trigger('keydown.enter');
    await flushPromises();
    expect(wrapper.text()).toContain('The party row is not editable in this session.');
    wrapper.unmount();
  });

  it('reports save errors and guards capture review decisions', async () => {
    stubDesk();
    vi.spyOn(parley, 'updateEntityNotes').mockRejectedValue(new Error('notes offline'));
    vi.spyOn(parley, 'saveSource').mockRejectedValue(new Error('capture denied'));
    const wrapper = mount(PartyDesk, { props: { parties: [fieldwork, mira], sources: [], opportunities: [] } });
    await flushPromises();
    await wrapper.findAll('.row-party')[0]!.trigger('click');
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Open card')!.trigger('click');
    await flushPromises();
    const editor = wrapper.findComponent(MarkdownEditor);
    const view = (editor.vm as { getView(): { state: { doc: { toString(): string } }; dispatch(options: object): void } }).getView();
    view.dispatch({ changes: { from: 0, to: view.state.doc.toString().length, insert: 'New note\n' } });
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Save notes')!.trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('notes offline');
    // Capture save failure is reported on the desk.
    await wrapper.get('[role="navigation"][aria-label="Lead workspaces"] button').trigger('click');
    await wrapper.get('form.capture textarea').setValue('Source text');
    await wrapper.get('form.capture').trigger('submit');
    await flushPromises();
    expect(wrapper.text()).toContain('capture denied');
    wrapper.unmount();
  });
});
