import { flushPromises, mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { parley, type Material } from '../../api/client';
import AttachmentPanel from './AttachmentPanel.vue';

/* global document, URL */

const sha256 = '2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824';

function materialWithAttachment(): Material {
  return {
    id: 'm1', opportunity_id: 'o1', recipient_id: 'p1', recipient_name: 'Dana Lee', kind: 'outreach', version: 1,
    body: 'See attached.', content_sha256: sha256, status: 'released', gate: null,
    attachment: { upload_id: 'u1', sha256, filename: 'handoff.pdf', content_type: 'application/pdf', size: 5 },
  };
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('AttachmentPanel', () => {
  it('verifies downloaded bytes in memory and never claims a completed disk write', async () => {
    const bytes = new TextEncoder().encode('hello').buffer;
    vi.spyOn(parley, 'downloadMaterialBytes').mockResolvedValue({
      bytes, contentType: 'application/pdf', filename: 'handoff.pdf', size: 5,
    });
    const digest = vi.fn().mockResolvedValue(Uint8Array.from(Buffer.from(sha256, 'hex')).buffer);
    vi.stubGlobal('crypto', { subtle: { digest } });
    const createObjectURL = vi.fn(() => 'blob:mock-download');
    const revokeObjectURL = vi.fn();
    vi.stubGlobal('URL', { ...URL, createObjectURL, revokeObjectURL });
    const downloads: string[] = [];
    const originalCreateElement = document.createElement.bind(document);
    vi.spyOn(document, 'createElement').mockImplementation((tag: string) => {
      if (tag.toLowerCase() === 'a') {
        return { href: '', download: '', setAttribute: vi.fn(), click: vi.fn(() => undefined), remove: vi.fn(() => undefined) } as unknown as HTMLAnchorElement;
      }
      return originalCreateElement(tag);
    });
    vi.spyOn(document.body, 'appendChild').mockImplementation((anchor) => {
      downloads.push((anchor as HTMLAnchorElement).download);
      return null as unknown as Node;
    });

    const wrapper = mount(AttachmentPanel, {
      props: {
        material: materialWithAttachment(),
        release: { release_id: 'r1', material_id: 'm1', version: 1, recipient_id: 'p1', body: '', content_sha256: sha256, issued_at: '2026-11-22T00:00:00Z', attachment: null } as unknown as Awaited<ReturnType<typeof parley.releaseMaterial>>,
      },
    });
    await wrapper.findAll('button').find((button) => button.text() === 'Download verified bytes')!.trigger('click');
    await flushPromises();

    expect(parley.downloadMaterialBytes).toHaveBeenCalledWith('m1', 'r1');
    expect(digest).toHaveBeenCalledWith('SHA-256', bytes);
    expect(createObjectURL).toHaveBeenCalledOnce();
    expect(downloads).toEqual(['handoff.pdf']);
    expect(wrapper.text()).toContain('verified in this tab');
    expect(wrapper.text()).not.toContain('written to disk');
    wrapper.unmount();
  });

  it('does not offer download until a one-use release exists', async () => {
    const wrapper = mount(AttachmentPanel, { props: { material: materialWithAttachment(), release: null } });
    const button = wrapper.findAll('button').find((item) => item.text().includes('Release the material'))!;
    expect(button.attributes('disabled')).toBeDefined();
    expect(wrapper.text()).toContain('handoff.pdf');
    wrapper.unmount();
  });
});
