import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError, parley, type EntityAttachment, type EntityCard, type ResourceColumn } from '../../api/client';
import ResourceEditorForm from './ResourceEditorForm.vue';

const columns: ResourceColumn[] = [
  { id: 'inbox', title: 'Inbox', position: 1024, revision: 1, kind: 'system' },
  { id: 'developing', title: 'Developing', position: 2048, revision: 1, kind: 'owner' },
  { id: 'available', title: 'Available', position: 3072, revision: 1, kind: 'owner' },
];

const card: EntityCard = {
  id: 'card-1', kind: 'resource', title: 'Importer checklist', subtype: 'artifact',
  body: {}, readiness: 'draft', revision: 1, content_revision: 1,
  notes: { markdown: 'Checklist notes', revision: 1 }, etag: '"p1.e.card-1.1"',
};

const commitResult = {
  entity: { ...card, title: 'Saved card' },
  placement: { column_id: 'developing', position: 1024, revision: 1 },
  created_related: null,
  event_cursor: 4,
};

const attachment: EntityAttachment = {
  id: 'att-1', entity_id: 'card-1', upload_id: 'u1', filename: 'cases.csv',
  content_type: 'text/csv', size: 2048, sha256: 'a'.repeat(64),
  share_permission: false, reviewed_by_owner: true, created_at: '2026-11-22T10:00:00Z',
};

afterEach(() => vi.restoreAllMocks());

function mountForm(props: Record<string, unknown> = {}) {
  return mount(ResourceEditorForm, {
    props: {
      columns, mode: 'create', card: null, cardSummary: null,
      initialColumnId: 'developing', graphMode: false, availableEndpoints: [],
      ...props,
    },
  });
}

describe('ResourceEditorForm', () => {
  it('retries a save with the same idempotency key after an unknown outcome', async () => {
    const commit = vi.spyOn(parley, 'commitResourceCard')
      .mockRejectedValueOnce(new TypeError('connection lost'))
      .mockResolvedValueOnce(commitResult as never);
    const wrapper = mountForm();
    await wrapper.get('input').setValue('Workshop outline');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.text()).toContain('Save status unknown');
    const originalKey = commit.mock.calls[0]?.[1];

    await wrapper.get('form').trigger('submit');
    await flushPromises();

    expect(commit).toHaveBeenCalledTimes(2);
    expect(commit.mock.calls[1]?.[1]).toBe(originalKey);
    wrapper.unmount();
  });

  it('freezes the short committing step and keeps post-acknowledgement edits dirty', async () => {
    let acknowledge!: (value: typeof commitResult) => void;
    const commit = vi.spyOn(parley, 'commitResourceCard').mockImplementation(() => new Promise((resolve) => {
      acknowledge = resolve;
    }));
    const wrapper = mountForm();
    await wrapper.get('input').setValue('First version');
    await wrapper.get('form').trigger('submit');
    expect(commit).toHaveBeenCalledOnce();
    // The committing step freezes input (spec §4 allows freeze or edit sequence).
    expect(wrapper.get('input').attributes('disabled')).toBeDefined();

    acknowledge(commitResult);
    await flushPromises();
    expect(wrapper.text()).toContain('Card saved.');

    await wrapper.get('input').setValue('Newer local version');
    expect(wrapper.text()).toContain('Unsaved edits are kept in this draft.');
    expect((wrapper.get('input').element as HTMLInputElement).value).toBe('Newer local version');
    expect(commit).toHaveBeenCalledOnce();
    wrapper.unmount();
  });

  it('reports malformed save failures without clearing the local draft', async () => {
    vi.spyOn(parley, 'commitResourceCard').mockRejectedValue('invalid response');
    const wrapper = mountForm();
    await wrapper.get('input').setValue('Keep this draft');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(wrapper.get('[role="alert"]').text()).toContain('Save failed. Your edits are still here.');
    expect((wrapper.get('input').element as HTMLInputElement).value).toBe('Keep this draft');
    wrapper.unmount();
  });

  it('quarantines a dirty draft on workspace replacement with copy-out and discard', async () => {
    let clipboardDescriptor: PropertyDescriptor | undefined;
    try {
      vi.spyOn(parley, 'commitResourceCard').mockRejectedValue(new ApiError('workspace replaced', 409, 'WORKSPACE_REPLACED'));
      const wrapper = mountForm();
      await wrapper.get('input').setValue('Unsaved resource draft');
      await wrapper.get('form').trigger('submit');
      await flushPromises();

      expect(wrapper.get('[role="alertdialog"]').text()).toContain('Unsaved draft quarantined');
      expect(wrapper.get('textarea[aria-label="Quarantined draft text"]').element).toMatchObject({ value: expect.stringContaining('Unsaved resource draft') });

      const clipboardWrite = vi.fn().mockResolvedValue(undefined);
      clipboardDescriptor = Object.getOwnPropertyDescriptor(navigator, 'clipboard');
      Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: clipboardWrite } });
      await wrapper.findAll('button').find((button) => button.text() === 'Copy draft')!.trigger('click');
      await flushPromises();
      expect(clipboardWrite).toHaveBeenCalledWith(expect.stringContaining('Unsaved resource draft'));

      await wrapper.findAll('button').find((button) => button.text() === 'Discard draft')!.trigger('click');
      await flushPromises();
      expect(wrapper.text()).not.toContain('Unsaved draft quarantined');
      wrapper.unmount();
    } finally {
      if (clipboardDescriptor) Object.defineProperty(navigator, 'clipboard', clipboardDescriptor);
      else Reflect.deleteProperty(navigator, 'clipboard');
    }
  });

  it('lists bound files on an existing card and sends unbind ids on save', async () => {
    const list = vi.spyOn(parley, 'listAttachments').mockResolvedValue([attachment]);
    vi.spyOn(parley, 'getAttachmentContent').mockResolvedValue({
      bytes: new ArrayBuffer(4), filename: 'cases.csv', contentType: 'text/csv', size: 4,
    });
    const commit = vi.spyOn(parley, 'commitResourceCard').mockResolvedValue(commitResult as never);
    const wrapper = mountForm({ mode: 'edit', card });
    await flushPromises();
    expect(list).toHaveBeenCalledWith('card-1');
    const files = wrapper.get('[aria-label="Bound files"]');
    expect(files.text()).toContain('cases.csv');
    expect(files.text()).toContain('private');

    // Cancel restores the binding: removing is local until Save.
    await files.findAll('button').find((button) => button.text() === 'Remove')!.trigger('click');
    expect(files.findAll('button').some((button) => button.text() === 'Keep')).toBe(true);
    await files.findAll('button').find((button) => button.text() === 'Keep')!.trigger('click');

    await files.findAll('button').find((button) => button.text() === 'Remove')!.trigger('click');
    await wrapper.get('input').setValue('Importer checklist v2');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(commit).toHaveBeenCalledWith(
      expect.objectContaining({
        edit: expect.objectContaining({ resource_id: 'card-1', unbind_ids: ['att-1'] }),
      }),
      expect.any(String),
    );
    wrapper.unmount();
  });

  it('offers only owner-authored relationship verbs in graph mode', async () => {
    const commit = vi.spyOn(parley, 'commitResourceCard').mockResolvedValue(commitResult as never);
    const wrapper = mountForm({
      mode: 'edit', card, graphMode: true,
      availableEndpoints: [{ id: 'p1', label: 'Mira Chen', kind: 'person' }],
    });
    await wrapper.get('input[maxlength="240"]').setValue('Importer checklist');
    const box = wrapper.get('details.relation-box');
    await box.get('summary').trigger('click');
    await box.get('input[type="checkbox"]').setValue(true);
    const selectWith = (text: string) => box.findAll('select').find((select) => select.findAll('option').some((option) => option.text() === text))!;
    const verbSelect = selectWith('includes');
    expect(verbSelect.findAll('option').map((option) => option.text())).toEqual(['includes', 'supports', 'relates to', 'considered for']);
    await verbSelect.setValue('supports');
    await selectWith('Link an existing card').setValue('link');
    await box.findAll('input').find((input) => input.attributes('type') !== 'checkbox')!.setValue('Owner · linked test case');
    await selectWith('Mira Chen · person').setValue('p1');
    await wrapper.get('form').trigger('submit');
    await flushPromises();
    expect(commit).toHaveBeenCalledWith(
      expect.objectContaining({
        relation_add: expect.objectContaining({ verb: 'supports', target_id: 'p1' }),
      }),
      expect.any(String),
    );
    wrapper.unmount();
  });
});
