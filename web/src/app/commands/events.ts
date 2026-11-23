import { parley } from '../../api/client';

/**
 * One session-owned event reader (UCR-001 §5.2). All visible Queue views
 * subscribe to this reader: at most one poll is in flight, hidden tabs suspend
 * polling, BroadcastChannel carries cross-tab hints (authoritative reads stay
 * per tab), and an epoch replacement rebases the cursor before the next poll.
 */

export type EventPage = Awaited<ReturnType<typeof parley.getEvents>>;

export interface EventReaderHooks {
  epoch: () => string | undefined;
  onEvents: (page: EventPage) => void;
}

export class SessionEventReader {
  private hooks: EventReaderHooks | null = null;
  private cursor = '0';
  private epoch: string | undefined;
  private inFlight: Promise<void> | null = null;
  private channel: InstanceType<typeof globalThis.BroadcastChannel> | null = null;
  private readonly onVisibility = () => {
    if (globalThis.document?.visibilityState === 'visible') void this.poll();
  };
  private readonly onFocus = () => { void this.poll(); };

  start(hooks: EventReaderHooks): void {
    if (this.hooks === hooks) return;
    this.stop();
    this.hooks = hooks;
    this.epoch = hooks.epoch();
    this.cursor = parley.eventCursor;
    globalThis.document?.addEventListener('visibilitychange', this.onVisibility);
    globalThis.addEventListener('focus', this.onFocus);
    if ('BroadcastChannel' in globalThis) {
      this.channel = new globalThis.BroadcastChannel('parley-command-events');
      this.channel.onmessage = (message) => {
        if ((message.data as { type?: unknown } | null)?.type === 'changed') void this.poll();
      };
    }
  }

  stop(): void {
    this.hooks = null;
    globalThis.document?.removeEventListener('visibilitychange', this.onVisibility);
    globalThis.removeEventListener('focus', this.onFocus);
    this.channel?.close();
    this.channel = null;
    this.inFlight = null;
  }

  /** Rebase after an epoch replacement; the old cursor is no longer meaningful. */
  rebase(epoch = this.hooks?.epoch()): void {
    if (!epoch || epoch === this.epoch) return;
    this.epoch = epoch;
    this.cursor = parley.eventCursor;
    this.inFlight = null;
  }

  /** Broadcast a hint and poll this tab immediately. */
  hint(): void {
    this.channel?.postMessage({ type: 'changed' });
    void this.poll();
  }

  /** Authoritative poll with one in-flight request and hidden-tab suspension. */
  poll(): Promise<void> {
    if (!this.hooks || this.inFlight) return this.inFlight ?? Promise.resolve();
    if (globalThis.document?.visibilityState === 'hidden') return Promise.resolve();
    const hooks = this.hooks;
    const epoch = hooks.epoch();
    if (epoch && epoch !== this.epoch) this.rebase(epoch);
    const cursor = this.cursor;
    const pending = parley.getEvents(cursor)
      .then((page) => {
        if (this.hooks !== hooks) return;
        if (epoch && epoch !== hooks.epoch()) { this.rebase(hooks.epoch()); return; }
        if (page.items.length) {
          this.cursor = page.observed_cursor || cursor;
          hooks.onEvents(page);
        } else if (page.observed_cursor && page.observed_cursor !== cursor) {
          this.cursor = page.observed_cursor;
        }
      })
      .catch(() => { /* Keep the last authoritative view during transport recovery. */ })
      .finally(() => { if (this.inFlight === pending) this.inFlight = null; });
    this.inFlight = pending;
    return pending;
  }

  get currentCursor(): string { return this.cursor; }
  get polling(): boolean { return this.inFlight !== null; }
}

export const sessionEventReader = new SessionEventReader();
