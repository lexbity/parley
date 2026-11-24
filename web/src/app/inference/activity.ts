import { ApiError, parley, type CoachJob, type CoachSnapshot, type StructuredReadiness } from '../../api/client';

interface WeeklyCoachRead {
  snapshot: CoachSnapshot;
  job: CoachJob | null;
}

interface WeeklyCoachListener {
  update(value: WeeklyCoachRead): void;
  error(reason: unknown): void;
}

export function jobIsActive(job: CoachJob | null): boolean {
  return job?.status === 'queued' || job?.status === 'running';
}

class InferenceActivity {
  private readonly weeklyListeners = new Set<WeeklyCoachListener>();
  private timer: ReturnType<typeof globalThis.setTimeout> | undefined;
  private request: AbortController | undefined;
  private weekly: WeeklyCoachRead | undefined;
  private epoch: string | undefined;
  private admittedJob: string | undefined;

  watchWeekly(listener: WeeklyCoachListener): () => void {
    this.weeklyListeners.add(listener);
    if (this.weeklyListeners.size === 1) {
      this.epoch = parley.workspaceEpoch;
      globalThis.addEventListener('focus', this.refresh);
      globalThis.document.addEventListener('visibilitychange', this.visibilityChanged);
      void this.read();
    } else if (this.weekly) {
      listener.update(this.weekly);
    }
    return () => {
      this.weeklyListeners.delete(listener);
      if (this.weeklyListeners.size === 0) {
        globalThis.removeEventListener('focus', this.refresh);
        globalThis.document.removeEventListener('visibilitychange', this.visibilityChanged);
        this.stopRead();
        this.weekly = undefined;
        this.admittedJob = undefined;
      }
    };
  }

  observeAdmission(id: string): void {
    this.admittedJob = id;
    this.refresh();
  }

  readonly refresh = (): void => {
    this.stopRead();
    void this.read(true);
  };

  watchStructuredProbe(
    revision: number,
    update: (structured: StructuredReadiness) => void,
    profileChanged: () => void,
    failed: () => void,
  ): () => void {
    const epoch = parley.workspaceEpoch;
    let stopped = false;
    let timer: ReturnType<typeof globalThis.setTimeout> | undefined;
    let request: AbortController | undefined;

    function pause() {
      globalThis.clearTimeout(timer);
      request?.abort();
      request = undefined;
    }

    function stop() {
      stopped = true;
      pause();
      globalThis.removeEventListener('focus', refresh);
      globalThis.document.removeEventListener('visibilitychange', refresh);
    }

    function refresh() {
      pause();
      if (!stopped && globalThis.document.visibilityState !== 'hidden') void read();
    }

    async function read() {
      if (epoch !== parley.workspaceEpoch) { stop(); return; }
      const active = new AbortController();
      request = active;
      try {
        const profile = await parley.getProvider(active.signal);
        if (active.signal.aborted || epoch !== parley.workspaceEpoch) return;
        if (profile.profile_revision !== revision) {
          stop();
          profileChanged();
          return;
        }
        update(profile.structured);
        if (profile.structured.status === 'queued' || profile.structured.status === 'running') {
          timer = globalThis.setTimeout(() => { void read(); }, 2_000);
        } else {
          stop();
        }
      } catch {
        if (active.signal.aborted || epoch !== parley.workspaceEpoch) return;
        failed();
        timer = globalThis.setTimeout(() => { void read(); }, 8_000);
      } finally {
        if (request === active) request = undefined;
      }
    }

    globalThis.addEventListener('focus', refresh);
    globalThis.document.addEventListener('visibilitychange', refresh);
    refresh();
    return stop;
  }

  private readonly visibilityChanged = (): void => {
    if (globalThis.document.visibilityState === 'hidden') {
      this.stopRead();
    } else {
      this.refresh();
    }
  };

  private stopRead(): void {
    globalThis.clearTimeout(this.timer);
    this.timer = undefined;
    this.request?.abort();
    this.request = undefined;
  }

  private async read(forceSnapshot = false): Promise<void> {
    if (!this.weeklyListeners.size || this.request || globalThis.document.visibilityState === 'hidden') return;
    const sessionEpoch = parley.workspaceEpoch;
    if (sessionEpoch !== this.epoch) {
      this.epoch = sessionEpoch;
      this.weekly = undefined;
      this.admittedJob = undefined;
    }
    const request = new AbortController();
    this.request = request;
    const current = () => !request.signal.aborted && sessionEpoch === parley.workspaceEpoch;
    const prior = this.weekly;
    let active = Boolean(this.admittedJob) || jobIsActive(prior?.job ?? null);
    try {
      // Poll a known active job directly. Read the weekly snapshot after a
      // terminal transition, on focus, and when observing idle weekly work.
      const snapshot = !forceSnapshot && prior && jobIsActive(prior.job) && !this.admittedJob
        ? prior.snapshot
        : await parley.getWeeklyCoach(request.signal);
      const id = this.admittedJob ?? snapshot.active_job_id ?? snapshot.latest_job_id;
      const job = id ? await parley.getCoachJob(id, request.signal) : null;
      active = jobIsActive(job);
      const refreshed = snapshot === prior?.snapshot && !active
        ? await parley.getWeeklyCoach(request.signal)
        : snapshot;
      if (!current()) return;
      this.admittedJob = undefined;
      this.weekly = { snapshot: refreshed, job };
      for (const listener of this.weeklyListeners) listener.update(this.weekly);
    } catch (reason) {
      if (!current()) return;
      if (reason instanceof ApiError && reason.status === 404) {
        this.admittedJob = undefined;
        this.weekly = undefined;
      }
      for (const listener of this.weeklyListeners) listener.error(reason);
    } finally {
      if (this.request === request) {
        this.request = undefined;
        if (this.weeklyListeners.size) {
          this.timer = globalThis.setTimeout(() => { void this.read(); }, active ? 2_000 : 8_000);
        }
      }
    }
  }
}

export const inferenceActivity = new InferenceActivity();
