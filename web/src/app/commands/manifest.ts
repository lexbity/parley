import { commands as defaultCommands, type CommandClient, type CommandContext, type CommandManifest } from '../../api/commands';
import { contextFingerprint } from './context';

/**
 * Cached palette manifest source (UCR-001 §5.2 & NFR-5). Display uses the
 * cached manifest immediately on open; context changes invalidate it on a
 * 120 ms debounce; dispatch revalidates before use. A route or epoch change
 * increments the generation so an in-flight response can never paint stale
 * commands over the current context.
 */

export interface CommandSourceOptions {
  context: () => CommandContext;
  epoch: () => string | undefined;
  client?: CommandClient;
  debounceMs?: number;
}

export const MANIFEST_DEBOUNCE_MS = 120;

export class CommandManifestSource {
  private readonly client: CommandClient;
  private readonly debounceMs: number;
  private cache: CommandManifest | null = null;
  private generation = 0;
  private timer: ReturnType<typeof globalThis.setTimeout> | undefined;
  private inFlight: Promise<CommandManifest> | null = null;
  private lastKey = '';

  constructor(private readonly options: CommandSourceOptions) {
    this.client = options.client ?? defaultCommands;
    this.debounceMs = options.debounceMs ?? MANIFEST_DEBOUNCE_MS;
  }

  private key(): string {
    return `${this.options.epoch() ?? ''}|${contextFingerprint(this.options.context())}`;
  }

  cached(): CommandManifest | null {
    return this.cache;
  }

  isFresh(): boolean {
    return this.cache !== null && this.key() === this.lastKey;
  }

  /** Debounced invalidation used by context watchers. */
  invalidate(): void {
    if (this.timer) globalThis.clearTimeout(this.timer);
    this.timer = globalThis.setTimeout(() => {
      this.timer = undefined;
      void this.refresh();
    }, this.debounceMs);
  }

  /** Immediate, generation-fenced refresh. Stale responses never replace cache. */
  async refresh(): Promise<CommandManifest> {
    const generation = ++this.generation;
    const key = this.key();
    const pending = this.client.query(this.options.context(), 'palette');
    this.inFlight = pending;
    try {
      const manifest = await pending;
      if (generation !== this.generation || key !== this.key()) return this.cache ?? manifest;
      this.cache = manifest;
      this.lastKey = key;
      return manifest;
    } finally {
      if (this.inFlight === pending) this.inFlight = null;
    }
  }

  /** Revalidate immediately before dispatch; falls back to cached display on failure. */
  async revalidate(): Promise<CommandManifest> {
    try {
      return await this.refresh();
    } catch {
      if (this.cache) return this.cache;
      throw new Error('Command catalog is unavailable. Reopen the palette to retry.');
    }
  }

  get loading(): boolean { return this.inFlight !== null; }

  stop(): void {
    if (this.timer) globalThis.clearTimeout(this.timer);
    this.timer = undefined;
    this.generation += 1;
    this.inFlight = null;
  }
}
