import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type DirectoryEntity, type Neighborhood, type SavedSource, type SourceDetail } from '../../api/client';
import Directory from './Directory.vue';

afterEach(()=>vi.restoreAllMocks());

function deferred<T>(){let resolve!:(value:T)=>void;const promise=new Promise<T>((done)=>{resolve=done;});return{promise,resolve};}
const graph=(focus:string,wording:string):Neighborhood=>({focus_id:focus,nodes:[{id:focus,kind:'person',name:focus==='party-a'?'Alex Dana':'Blair Dana',descriptor:'',aliases:[],tags:[],revision:1},{id:`org-${focus}`,kind:'org',name:'Northline',descriptor:'',aliases:[],tags:[],revision:1}],edges:[{id:`edge-${focus}`,from_id:focus,to_id:`org-${focus}`,verb:'member_of',revision:1,claim_id:`claim-${focus}`,claim_revision:1,wording,approved_wording:null,tier:1,speaker_kind:'unknown',speaker_entity_id:null,occurrence_id:null,source_start:null,source_end:null,normalization_version:null,source_title:null,source_url:null}],positions:[],omitted_nodes:0,omitted_edges:0});
const org:DirectoryEntity={id:'org-1',kind:'org',name:'Northline',descriptor:'Hiring team',aliases:[],tags:['onboarding'],revision:1};
const dana:DirectoryEntity={id:'person-1',kind:'person',name:'Dana Lee',descriptor:'Northline',aliases:[],tags:[],revision:1};
const source:SavedSource={content_id:'content-1',variant_id:'variant-1',occurrence_id:'occurrence-1',normalized_sha256:'a'.repeat(64),raw_sha256:'b'.repeat(64)};
const sourceDetail:SourceDetail={...source,text:'Dana will work with the hiring team.',title:'Hiring update',url:null,captured_at:'2026-01-01T00:00:00Z',actor_kind:'party',actor_entity_id:dana.id,normalization_version:1};

function standardReads(rows:DirectoryEntity[]=[org,dana]){
  vi.spyOn(parley,'connect').mockResolvedValue();
  vi.spyOn(parley,'searchDirectory').mockResolvedValue({items:rows,total:rows.length,page:1,page_size:50,has_more:false});
  vi.spyOn(parley,'listSources').mockResolvedValue([source]);
  vi.spyOn(parley,'getNeighborhood').mockResolvedValue({focus_id:'',nodes:[],edges:[],positions:[],omitted_nodes:0,omitted_edges:0});
}

describe('Leads directory selection',()=>{
  it('discards a delayed neighborhood after focus changes, even if the old promise resolves last',async()=>{
    const first:DirectoryEntity={id:'party-a',kind:'person',name:'Alex Dana',descriptor:'Northline',aliases:[],tags:[],revision:1};
    const second:DirectoryEntity={id:'party-b',kind:'person',name:'Blair Dana',descriptor:'Southline',aliases:[],tags:[],revision:1};
    const delayed=deferred<Neighborhood>();const connect=vi.spyOn(parley,'connect').mockResolvedValue();
    vi.spyOn(parley,'searchDirectory').mockResolvedValue({items:[first,second],total:2,page:1,page_size:50,has_more:false});
    vi.spyOn(parley,'listSources').mockResolvedValue([]);
    vi.spyOn(parley,'getNeighborhood').mockImplementation((id)=>id===first.id?delayed.promise:Promise.resolve(graph(second.id,'B evidence only')));
    const wrapper=mount(Directory,{global:{stubs:{VueFlow:true}}});await connect;await flushPromises();
    await wrapper.findAll('.directory-list button')[0]!.trigger('click');await flushPromises();
    await wrapper.findAll('.directory-list button')[1]!.trigger('click');await flushPromises();
    await wrapper.get('button[aria-pressed="false"]').trigger('click');
    expect(wrapper.text()).toContain('B evidence only');
    delayed.resolve(graph(first.id,'STALE A evidence'));await flushPromises();
    expect(wrapper.text()).toContain('Blair Dana');expect(wrapper.text()).toContain('B evidence only');expect(wrapper.text()).not.toContain('STALE A evidence');
    wrapper.unmount();
  });

  it('filters, paginates, preserves local profile edits and saves the selected entry',async()=>{
    vi.spyOn(parley,'connect').mockResolvedValue();
    const search=vi.spyOn(parley,'searchDirectory').mockImplementation(async(_filters,_signal,page=1)=>({items:page===1?[org,dana]:[dana],total:55,page,page_size:50,has_more:page===1}));
    vi.spyOn(parley,'listSources').mockResolvedValue([]);
    vi.spyOn(parley,'getNeighborhood').mockResolvedValue({focus_id:'',nodes:[],edges:[],positions:[],omitted_nodes:0,omitted_edges:0});
    vi.spyOn(parley,'updateDirectory').mockResolvedValue({...org,revision:2,aliases:['Northline Labs'],tags:['onboarding','research']});
    const wrapper=mount(Directory,{global:{stubs:{Neighborhood:true}}});await flushPromises();
    await wrapper.find('[aria-label="Directory pages"] button:last-child').trigger('click');await flushPromises();
    expect(wrapper.text()).toContain('Page 2 · 55 matches');
    await wrapper.find('[aria-label="Directory pages"] button:first-child').trigger('click');await flushPromises();
    expect(wrapper.text()).toContain('Page 1 · 55 matches');
    await wrapper.get('input[type="search"]').setValue('Northline');await flushPromises();
    expect(search).toHaveBeenLastCalledWith({q:'Northline',kind:undefined,tag:undefined},expect.any(AbortSignal),1,50);
    await wrapper.findAll('input[type="search"]')[1]!.setValue('onboarding');await flushPromises();
    expect(search).toHaveBeenLastCalledWith({q:'Northline',kind:undefined,tag:'onboarding'},expect.any(AbortSignal),1,50);
    await wrapper.findAll('.directory-list button').find(button=>button.text().includes('Northline'))!.trigger('click');await flushPromises();
    await wrapper.findAll('.detail-column input')[2]!.setValue('Product hiring');
    await wrapper.findAll('.directory-list button').find(button=>button.text().includes('Dana'))!.trigger('click');await flushPromises();
    await wrapper.findAll('.directory-list button').find(button=>button.text().includes('Northline'))!.trigger('click');await flushPromises();
    expect((wrapper.findAll('.detail-column input')[2]!.element as HTMLInputElement).value).toBe('Product hiring');
    await wrapper.findAll('.detail-column input')[1]!.setValue('Northline Group');
    await wrapper.findAll('.detail-column input')[3]!.setValue('Northline Labs');
    await wrapper.findAll('.detail-column input')[4]!.setValue('onboarding, research');
    const profileForm=wrapper.findAll('form').find(form=>form.text().includes('Edit selected directory entry'))!;
    await profileForm.trigger('submit');await flushPromises();
    expect(parley.updateDirectory).toHaveBeenCalledWith(org,expect.objectContaining({name:'Northline Group',descriptor:'Product hiring',aliases:['Northline Labs'],tags:'onboarding, research'}),expect.any(String));
    expect(wrapper.text()).toContain('Directory profile saved.');
    wrapper.unmount();
  });

  it('creates a standalone party and reports a failed create without claiming success',async()=>{
    standardReads([]);
    vi.spyOn(parley,'createEntity').mockResolvedValue({id:'new-party',kind:'person',name:'Dana Lee',revision:1});
    vi.spyOn(parley,'getNeighborhood').mockResolvedValue({focus_id:'new-party',nodes:[],edges:[],positions:[],omitted_nodes:0,omitted_edges:0});
    const wrapper=mount(Directory,{global:{stubs:{Neighborhood:true}}});await flushPromises();
    const addForm=wrapper.findAll('form')[0]!;
    await addForm.get('input').setValue('Dana Lee');await addForm.trigger('submit');await flushPromises();
    expect(parley.createEntity).toHaveBeenCalledWith({kind:'person',name:'Dana Lee'},expect.any(String));
    expect(wrapper.text()).toContain('No opportunity or outreach was created.');
    wrapper.unmount();

    standardReads([]);
    vi.spyOn(parley,'createEntity').mockRejectedValue(new Error('write refused'));
    const failed=mount(Directory,{global:{stubs:{Neighborhood:true}}});await flushPromises();
    const failedAddForm=failed.findAll('form')[0]!;
    await failedAddForm.get('input').setValue('Dana Lee');await failedAddForm.trigger('submit');await flushPromises();
    expect(failed.get('[role="alert"]').text()).toBe('write refused');
    expect(failed.text()).not.toContain('was created');
    failed.unmount();

    standardReads([]);
    vi.spyOn(parley,'createEntity').mockResolvedValue({id:'new-org',kind:'org',name:'Northline',revision:1});
    const organization=mount(Directory,{global:{stubs:{Neighborhood:true}}});await flushPromises();
    const organizationForm=organization.findAll('form')[0]!;
    await organizationForm.get('select').setValue('org');await organizationForm.get('input').setValue('Northline');await organizationForm.trigger('submit');await flushPromises();
    expect(parley.createEntity).toHaveBeenCalledWith({kind:'org',name:'Northline'},expect.any(String));
    organization.unmount();
  });

  it('saves an owner-attributed independent capture and opens its raw source',async()=>{
    standardReads();
    vi.spyOn(parley,'saveSource').mockResolvedValue(source);
    vi.spyOn(parley,'getSource').mockResolvedValue(sourceDetail);
    const wrapper=mount(Directory,{global:{stubs:{Neighborhood:true}}});await flushPromises();
    await wrapper.findAll('select').at(-1)!.setValue('owner');
    await wrapper.findAll('textarea').at(-1)!.setValue('Dana will work with the hiring team.');
    const captureForm=wrapper.findAll('form').find(form=>form.text().includes('Speaker attribution'))!;
    await captureForm.findAll('input')[0]!.setValue('Hiring update');
    await captureForm.findAll('input')[1]!.setValue('https://northline.example/jobs');
    await captureForm.trigger('submit');await flushPromises();
    expect(parley.saveSource).toHaveBeenCalledWith({text:'Dana will work with the hiring team.',title:'Hiring update',url:'https://northline.example/jobs',actor_kind:'owner',actor_entity_id:null});
    expect(wrapper.get('[aria-label="Original source"]').text()).toContain('Dana will work with the hiring team.');
    await wrapper.findAll('button').find(button=>button.text().startsWith('Open source'))!.trigger('click');await flushPromises();
    expect(parley.getSource).toHaveBeenCalledWith(source.occurrence_id);
    wrapper.unmount();
  });

  it('links chosen records, adds a related person atomically and saves corrected profile errors',async()=>{
    standardReads();
    vi.spyOn(parley,'linkExisting').mockResolvedValue('edge-1');
    vi.spyOn(parley,'createRelated').mockResolvedValue({id:'person-2',kind:'person',name:'Riley Lee',revision:1});
    vi.spyOn(parley,'getNeighborhood').mockResolvedValue({focus_id:'org-1',nodes:[],edges:[],positions:[],omitted_nodes:0,omitted_edges:0});
    vi.spyOn(parley,'updateDirectory').mockRejectedValue(new Error('stale profile'));
    const wrapper=mount(Directory,{global:{stubs:{Neighborhood:true}}});await flushPromises();
    await wrapper.findAll('.directory-list button').find(button=>button.text().includes('Northline'))!.trigger('click');await flushPromises();
    const linkForm=wrapper.findAll('form').find(form=>form.text().includes('Link an existing person'))!;
    await linkForm.get('select').setValue(dana.id);
    await linkForm.findAll('select')[1]!.setValue(source.occurrence_id);
    await linkForm.get('textarea').setValue('Dana will work with the hiring team.');
    await linkForm.trigger('submit');await flushPromises();
    expect(parley.linkExisting).toHaveBeenCalledWith({from_id:dana.id,to_id:org.id,verb:'member_of',occurrence_id:source.occurrence_id,quote:'Dana will work with the hiring team.'});
    expect(wrapper.text()).toContain('no identity merge occurred');
    const relatedForm=wrapper.findAll('form').find(form=>form.text().includes('Add a person to this organization'))!;
    await relatedForm.get('input').setValue('Riley Lee');
    await relatedForm.get('select').setValue(source.occurrence_id);
    await relatedForm.get('textarea').setValue('Dana will work with the hiring team.');
    await relatedForm.trigger('submit');await flushPromises();
    expect(parley.createRelated).toHaveBeenCalledWith(expect.objectContaining({name:'Riley Lee',related_entity_id:org.id,occurrence_id:source.occurrence_id}));
    expect(wrapper.text()).toContain('Related person and attributed relationship saved together.');
    wrapper.unmount();
  });

  it('keeps manual routes usable and shows provider, search, source and link failures',async()=>{
    vi.spyOn(parley,'connect').mockRejectedValue(new Error('workspace unavailable'));
    const offline=mount(Directory,{global:{stubs:{Neighborhood:true}}});await flushPromises();
    expect(offline.get('[role="alert"]').text()).toBe('workspace unavailable');offline.unmount();

    vi.spyOn(parley,'connect').mockResolvedValue();
    vi.spyOn(parley,'searchDirectory').mockRejectedValue(new Error('directory unavailable'));
    vi.spyOn(parley,'listSources').mockResolvedValue([source]);
    vi.spyOn(parley,'getNeighborhood').mockResolvedValue({focus_id:'',nodes:[],edges:[],positions:[],omitted_nodes:0,omitted_edges:0});
    const searchFailure=mount(Directory,{global:{stubs:{Neighborhood:true}}});await flushPromises();
    expect(searchFailure.get('[role="alert"]').text()).toBe('directory unavailable');searchFailure.unmount();

    standardReads();
    vi.spyOn(parley,'saveSource').mockRejectedValue(new Error('source refused'));
    vi.spyOn(parley,'linkExisting').mockRejectedValue(new Error('link refused'));
    vi.spyOn(parley,'getSource').mockRejectedValue(new Error('source detail unavailable'));
    const wrapper=mount(Directory,{global:{stubs:{Neighborhood:true}}});await flushPromises();
    await wrapper.findAll('button').find(button=>button.text().startsWith('Open source'))!.trigger('click');await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toBe('source detail unavailable');
    await wrapper.findAll('textarea').at(-1)!.setValue('Manual capture.');
    const captureForm=wrapper.findAll('form').find(form=>form.text().includes('Speaker attribution'))!;
    await captureForm.trigger('submit');await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toBe('source refused');
    await wrapper.findAll('.directory-list button').find(button=>button.text().includes('Northline'))!.trigger('click');await flushPromises();
    const linkForm=wrapper.findAll('form').find(form=>form.text().includes('Link an existing person'))!;
    await linkForm.get('select').setValue(dana.id);await linkForm.findAll('select')[1]!.setValue(source.occurrence_id);await linkForm.get('textarea').setValue('A valid quote.');await linkForm.trigger('submit');await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toBe('link refused');
    wrapper.unmount();
  });
});
