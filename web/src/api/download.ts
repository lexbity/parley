//! Owner-triggered verified file download (RFC-0003 S06, F-FR-4).
//!
//! The download grant is one-use and consumed server-side at the first byte.
//! This module performs the authenticated fetch, verifies the complete byte
//! count and the reviewed SHA-256 digest in memory, and hands the caller a
//! Blob. It never claims a completed disk write: the browser is responsible
//! for whatever happens after the object URL is handed to a download anchor.
import { parley, type Material } from './client';

export interface VerifiedAttachmentDownload {
  verified: true;
  blob: Blob;
  filename: string;
  contentType: string;
  size: number;
}

export async function downloadVerifiedAttachment(
  material: Material,
  releaseId: string,
): Promise<VerifiedAttachmentDownload> {
  const attachment = material.attachment;
  if (!attachment) throw new Error('This material has no reviewed attachment.');
  const result = await parley.downloadMaterialBytes(material.id, releaseId);
  if (result.size !== attachment.size) {
    throw new Error('Downloaded byte count does not match the reviewed attachment.');
  }
  if (typeof globalThis.crypto !== 'undefined' && globalThis.crypto.subtle) {
    const digest = await globalThis.crypto.subtle.digest('SHA-256', result.bytes);
    const hex = Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
    if (hex !== attachment.sha256) throw new Error('Downloaded bytes failed the reviewed SHA-256 verification.');
  }
  return {
    verified: true,
    blob: new Blob([result.bytes], { type: result.contentType }),
    filename: attachment.filename,
    contentType: result.contentType,
    size: result.size,
  };
}
