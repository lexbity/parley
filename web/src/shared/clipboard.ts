/**
 * Deadline-guarded clipboard write.
 *
 * An unfocused document can leave the clipboard promise pending forever; the
 * deadline hands the flow to the pending-acknowledgement reconciliation. The
 * clipboard is only ever used for the owner's explicit copy action on
 * already-approved release bytes — never for preview text or raw output.
 */
export function writeClipboardWithDeadline(text: string): Promise<void> {
  return Promise.race<void>([
    globalThis.navigator.clipboard.writeText(text),
    new Promise((_, reject) =>
      globalThis.setTimeout(
        () =>
          reject(
            new Error(
              'The clipboard did not respond; the release acknowledgement stays pending.',
            ),
          ),
        3000,
      ),
    ),
  ]);
}
