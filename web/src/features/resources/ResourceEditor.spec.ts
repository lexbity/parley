import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError, parley, type Resource } from '../../api/client';
import ResourceEditor from './ResourceEditor.vue';

const resource: Resource = {
  id: '01',
  kind: 'gift',
  title: 'Offer',
  body: { description: '', delivery: 'on_request' },
  readiness: 'draft',
  revision: 1,
  etag: '"p1.epoch.resource.01.1"',
};

afterEach(() => vi.restoreAllMocks());

describe('ResourceEditor', () => {
  it('saves an incomplete gift as an explicit draft and reports its gaps', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'listResources').mockResolvedValue([]);
    const create = vi.spyOn(parley, 'createResource').mockResolvedValue(resource);
    const wrapper = mount(ResourceEditor);
    await flushPromises();

    await wrapper.findAll('button').find((button) => button.text() === 'Add resource')?.trigger('click');
    await wrapper.get('form').trigger('submit');
    await flushPromises();

    expect(create).toHaveBeenCalledOnce();
    expect(create.mock.calls[0]?.[0]).toMatchObject({ kind: 'gift', title: '', body: { description: '' } });
    expect(create.mock.calls[0]?.[1]).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7/);
    expect(wrapper.text()).toContain('Describe the offer before it can be ready.');
    expect(wrapper.text()).toContain('Draft saved. Still missing: Name, What you can offer.');
    wrapper.unmount();
  });

  it('retries a save with the same idempotency key after an unknown outcome', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'listResources').mockResolvedValue([]);
    const create = vi.spyOn(parley, 'createResource')
      .mockRejectedValueOnce(new TypeError('connection lost'))
      .mockResolvedValueOnce(resource);
    const wrapper = mount(ResourceEditor);
    await flushPromises();
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.text()).toContain('Save status unknown');
    const originalKey = create.mock.calls[0]?.[1];

    await wrapper.get('form').trigger('submit');
    await flushPromises();

    expect(create).toHaveBeenCalledTimes(2);
    expect(create.mock.calls[1]?.[1]).toBe(originalKey);
    expect(wrapper.text()).toContain('Draft saved. Still missing: Name, What you can offer.');
    wrapper.unmount();
  });

  it('keeps a newer edit dirty while the frozen save is in flight', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'listResources').mockResolvedValue([]);
    let acknowledge!: (value: Resource) => void;
    const create = vi.spyOn(parley, 'createResource').mockImplementation(() => new Promise((resolve) => {
      acknowledge = resolve;
    }));
    const wrapper = mount(ResourceEditor);
    await flushPromises();
    await wrapper.get('input').setValue('First version');
    await wrapper.get('textarea').setValue('First offer');
    await wrapper.get('form').trigger('submit');
    expect(create).toHaveBeenCalledOnce();

    await wrapper.get('input').setValue('Newer local version');
    await wrapper.get('textarea').setValue('Newer offer');
    expect(wrapper.get('input').attributes('disabled')).toBeUndefined();
    acknowledge(resource);
    await flushPromises();

    expect(wrapper.text()).toContain('Earlier revision saved. Your newer edits are still unsaved.');
    expect(wrapper.text()).toContain('Unsaved edits are kept in this tab.');
    expect((wrapper.get('input').element as HTMLInputElement).value).toBe('Newer local version');
    expect(create.mock.calls[0]?.[0]).toMatchObject({ title: 'First version' });
    wrapper.unmount();
  });

  it('handles service availability and list failures with visible recovery status', async () => {
    vi.spyOn(parley, 'connect').mockRejectedValueOnce(new TypeError('Workspace offline'));
    const disconnected = mount(ResourceEditor);
    await flushPromises();
    expect(disconnected.get('[role="status"]').text()).toBe('Workspace offline');
    disconnected.unmount();

    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'listResources').mockRejectedValue('offline');
    const unavailable = mount(ResourceEditor);
    await flushPromises();
    expect(unavailable.get('[role="status"]').text()).toBe('Could not load the local workspace.');
    await unavailable.findAll('button').find((button) => button.text() === 'Reload')?.trigger('click');
    expect(unavailable.get('[role="status"]').text()).toBe('Could not load the local workspace.');
    unavailable.unmount();
  });

  it('supports completed proof drafts and truthful readiness', async () => {
    const ready: Resource = { ...resource, kind: 'proof', title: 'Measured outcome', readiness: 'ready' };
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'listResources').mockResolvedValue([]);
    const create = vi.spyOn(parley, 'createResource').mockResolvedValue(ready);
    const wrapper = mount(ResourceEditor);
    await flushPromises();
    await wrapper.get('select').setValue('proof');
    await wrapper.get('input').setValue(ready.title);
    await wrapper.get('textarea').setValue('Documented evidence');
    expect(wrapper.find('label').text()).not.toContain('Availability');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(create.mock.calls[0]?.[0]).toMatchObject({ kind: 'proof', title: ready.title });
    expect(wrapper.get('[role="status"]').text()).toBe('Resource saved.');
    wrapper.unmount();
  });

  it('compares a 412 conflict and allows the owner to reapply or select current text', async () => {
    const latest: Resource = { ...resource, title: 'Workspace version', revision: 2, etag: '"p2"' };
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'listResources')
      .mockResolvedValueOnce([resource])
      .mockResolvedValueOnce([latest]);
    const update = vi.spyOn(parley, 'updateResource')
      .mockRejectedValueOnce(new ApiError('Stale resource', 412, 'STALE_REVISION'))
      .mockResolvedValueOnce({ ...latest, title: 'Owner re-applied version', revision: 3, etag: '"p3"' });
    const wrapper = mount(ResourceEditor);
    await flushPromises();
    await wrapper.findAll('li button').find((button) => button.text().includes('Offer'))?.trigger('click');
    await wrapper.get('input').setValue('Owner re-applied version');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('Workspace version');
    expect(wrapper.get('[role="alert"]').text()).toContain('Owner re-applied version');
    await wrapper.findAll('button').find((button) => button.text() === 'Keep my current edits and review against latest')?.trigger('click');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(update).toHaveBeenCalledTimes(2);
    expect(update.mock.calls[1]?.[0].etag).toBe(latest.etag);
    expect(wrapper.get('[role="status"]').text()).toBe('Draft saved. Still missing: What you can offer.');
    wrapper.unmount();
  });

  it('keeps a newer conflicting buffer and supports taking the current version', async () => {
    const latest: Resource = { ...resource, title: 'Other session', revision: 2, etag: '"p2"' };
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'listResources').mockResolvedValueOnce([resource]).mockResolvedValueOnce([latest]);
    vi.spyOn(parley, 'updateResource').mockRejectedValueOnce(new ApiError('Stale', 412, 'STALE_REVISION'));
    const wrapper = mount(ResourceEditor);
    await flushPromises();
    await wrapper.findAll('li button').find((button) => button.text().includes('Offer'))?.trigger('click');
    await wrapper.get('input').setValue('Submitted edit');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    await wrapper.findAll('button').find((button) => button.text() === 'Use current version')?.trigger('click');
    expect((wrapper.get('input').element as HTMLInputElement).value).toBe(latest.title);
    expect(wrapper.find('[role="alert"]').exists()).toBe(false);
    expect(wrapper.findAll('button').find((button) => button.text() === 'Save draft')?.attributes('disabled')).toBeDefined();
    wrapper.unmount();
  });

  it('honors dirty navigation prompts and keeps the tab-local unload warning', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'listResources').mockResolvedValue([resource]);
    const confirm = vi.spyOn(globalThis, 'confirm').mockReturnValueOnce(false).mockReturnValueOnce(false).mockReturnValueOnce(true);
    const wrapper = mount(ResourceEditor);
    await flushPromises();
    await wrapper.findAll('li button').find((button) => button.text().includes('Offer'))?.trigger('click');
    await wrapper.get('input').setValue('Uncommitted local edit');
    await wrapper.findAll('button').find((button) => button.text() === 'Add resource')?.trigger('click');
    expect((wrapper.get('input').element as HTMLInputElement).value).toBe('Uncommitted local edit');
    await wrapper.findAll('button').find((button) => button.text() === 'Reload')?.trigger('click');
    expect(confirm).toHaveBeenCalledTimes(2);
    const event = new Event('beforeunload', { cancelable: true }) as BeforeUnloadEvent;
    window.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
    await wrapper.findAll('button').find((button) => button.text() === 'Cancel edits')?.trigger('click');
    expect((wrapper.get('input').element as HTMLInputElement).value).toBe(resource.title);
    wrapper.unmount();
  });

  it('reloads after explicit dirty confirmation and clears a selection removed elsewhere', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'listResources').mockResolvedValueOnce([resource]).mockResolvedValueOnce([]);
    vi.spyOn(globalThis, 'confirm').mockReturnValue(true);
    const wrapper = mount(ResourceEditor);
    await flushPromises();
    await wrapper.findAll('li button').find((button) => button.text().includes('Offer'))?.trigger('click');
    await wrapper.get('input').setValue('Dirty before reload');
    await wrapper.findAll('button').find((button) => button.text() === 'Reload')?.trigger('click');
    await flushPromises();
    expect(wrapper.find('li').exists()).toBe(false);
    expect(wrapper.get('h2').text()).toBe('Add a resource');
    wrapper.unmount();

    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'listResources').mockResolvedValue([resource]);
    const availability = mount(ResourceEditor);
    await flushPromises();
    await availability.findAll('li button').find((button) => button.text().includes('Offer'))?.trigger('click');
    await availability.findAll('select').at(1)?.setValue('exists');
    expect(availability.findAll('select').at(1)?.element).toHaveProperty('value', 'exists');
    expect(availability.text()).toContain('Unsaved edits are kept in this tab.');
    availability.unmount();
  });

  it('does not cancel a clean beforeunload event', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'listResources').mockResolvedValue([]);
    const wrapper = mount(ResourceEditor);
    await flushPromises();
    const event = new Event('beforeunload', { cancelable: true }) as BeforeUnloadEvent;
    window.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(false);
    wrapper.unmount();
  });

  it('reports malformed save failures without clearing the local draft', async () => {
    vi.spyOn(parley, 'connect').mockResolvedValue();
    vi.spyOn(parley, 'listResources').mockResolvedValue([]);
    vi.spyOn(parley, 'createResource').mockRejectedValue('invalid response');
    const wrapper = mount(ResourceEditor);
    await flushPromises();
    await wrapper.get('input').setValue('Keep this draft');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.get('[role="status"]').text()).toBe('Save failed. Your text is still here.');
    expect((wrapper.get('input').element as HTMLInputElement).value).toBe('Keep this draft');
    wrapper.unmount();
  });

  it('quarantines a dirty draft on workspace replacement with copy-out and discard', async () => {
    let clipboardDescriptor: PropertyDescriptor | undefined;
    try {
      vi.spyOn(parley, 'connect').mockResolvedValue();
      vi.spyOn(parley, 'listResources').mockResolvedValue([resource]);
      const refreshSession = vi.spyOn(parley, 'refreshSession').mockResolvedValue();
      const wrapper = mount(ResourceEditor);
      await flushPromises();
      await wrapper.findAll('li button').find((button) => button.text().includes('Offer'))?.trigger('click');
      await wrapper.get('input').setValue('Unsaved resource draft');

      const clipboardWrite = vi.fn().mockResolvedValue(undefined);
      clipboardDescriptor = Object.getOwnPropertyDescriptor(navigator, 'clipboard');
      Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: clipboardWrite } });

      globalThis.dispatchEvent(new globalThis.CustomEvent('parley:workspace-changed', { detail: { operation: 'restore' } }));
      await flushPromises();

      expect(wrapper.get('[role="alertdialog"]').text()).toContain('Unsaved resource draft quarantined');
      expect(wrapper.get('textarea[aria-label="Quarantined draft text"]').element).toMatchObject({ value: expect.stringContaining('Unsaved resource draft') });

      await wrapper.findAll('button').find((button) => button.text() === 'Copy quarantined draft')!.trigger('click');
      await flushPromises();
      expect(clipboardWrite).toHaveBeenCalledWith(expect.stringContaining('Unsaved resource draft'));

      await wrapper.findAll('button').find((button) => button.text() === 'Discard local draft and load current workspace')!.trigger('click');
      await flushPromises();
      expect(refreshSession).toHaveBeenCalledOnce();
      expect(wrapper.text()).not.toContain('Unsaved resource draft quarantined');
      expect((wrapper.get('input').element as HTMLInputElement).value).toBe('');
      wrapper.unmount();
    } finally {
      if (clipboardDescriptor) Object.defineProperty(navigator, 'clipboard', clipboardDescriptor);
      else Reflect.deleteProperty(navigator, 'clipboard');
    }
  });
});
