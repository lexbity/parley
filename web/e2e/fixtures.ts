import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import { Server } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { test as base } from '@playwright/test';
import { createServer } from 'vite';
import { mockApi } from './mock-api';
import { parleyBinary, webRoot } from './paths';

export const test = base.extend({
  // eslint-disable-next-line no-empty-pattern -- Playwright requires a destructured fixture argument.
  baseURL: async ({}, use, testInfo) => {
    if (testInfo.project.name === 'mock') {
      const server = await createServer({
        root: webRoot,
        plugins: [mockApi()],
        // A test owns this server until teardown. Repository changes and
        // concurrent Playwright traces must not trigger browser reloads.
        server: {
          middlewareMode: true,
          watch: { ignored: () => true },
          ws: false,
        },
      });
      const http = new Server(server.middlewares);
      try {
        const listening = once(http, 'listening');
        http.listen(0, '127.0.0.1');
        await listening;
        const address = http.address();
        if (!address || typeof address === 'string') {
          throw new Error('The mock server did not bind a TCP port.');
        }
        await use(`http://127.0.0.1:${address.port}`);
      } finally {
        await new Promise<void>((resolve) => http.close(() => resolve()));
        await server.close();
      }
      return;
    }

    if (testInfo.project.name !== 'rust') {
      throw new Error(`Unknown Parley browser project: ${testInfo.project.name}`);
    }
    const workspace = await mkdtemp(join(tmpdir(), 'parley-e2e-'));
    const child = spawn(parleyBinary, ['--workspace', workspace, '--port', '0'], {
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    const closed = new Promise<void>((resolve) => child.once('close', () => resolve()));
    let stderr = '';
    child.stderr.setEncoding('utf8').on('data', (chunk: string) => {
      stderr = (stderr + chunk).slice(-8192);
    });
    const lines = createInterface({ input: child.stdout });
    try {
      const [origin] = await Promise.race([
        once(lines, 'line', { signal: AbortSignal.timeout(15_000) }) as Promise<[string]>,
        once(child, 'exit').then(([code, signal]) => {
          throw new Error(`Parley exited before reporting its address (${code ?? signal}).`);
        }),
      ]);
      if (!/^http:\/\/127\.0\.0\.1:\d+\/$/.test(origin)) {
        throw new Error('Parley did not report a loopback address.');
      }
      await use(origin);
    } catch (error) {
      if (stderr.trim()) {
        throw new Error(stderr.trim(), { cause: error });
      }
      throw error;
    } finally {
      lines.close();
      child.kill();
      await closed;
      await rm(workspace, { recursive: true, force: true });
    }
  },
});
