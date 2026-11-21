import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import { Server } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { expect, test as base, type Page } from '@playwright/test';
import { createServer } from 'vite';
import { createMockApi, type MockApi } from './mock-api';
import { parleyBinary, webRoot } from './paths';

/**
 * Per-test owner workspace (RFC-0004 INT-06). The mock project serves the
 * production UI from Vite with the in-process fixture API; the rust project
 * launches the real bundled binary on a fresh temporary SQLite workspace and
 * redeems its one-time bootstrap secret so `page.goto('/')` opens a signed-in
 * owner session.
 */

export type OwnerSession = {
  origin: string;
  /** Redeemed HttpOnly session cookie value for the primary owner page. */
  cookie: string;
  /** A one-time bootstrap URL for a second, independent owner session. */
  secondBootstrapUrl(): Promise<string>;
};

type Fixtures = {
  ownerSession: OwnerSession;
};

const BOOTSTRAP_PREFIX = 'Bootstrap URL: ';

async function redeemBootstrap(origin: string, secret: string): Promise<string> {
  const response = await fetch(`${origin}/api/v1/session/bootstrap`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ secret }),
  });
  if (!response.ok) {
    throw new Error(`Bootstrap redemption failed (${response.status}).`);
  }
  const cookie = /parley_session=([^;]+)/.exec(response.headers.get('set-cookie') ?? '')?.[1];
  if (!cookie) throw new Error('Bootstrap redemption did not set a session cookie.');
  return cookie;
}

/** Open a second independent owner page from a bootstrap URL (RFC-0004 S01). */
export async function openOwnerPage(page: Page, bootstrapUrl: string): Promise<void> {
  await page.goto(bootstrapUrl);
  await expect(page.getByRole('heading', { name: 'Resources' })).toBeVisible();
  await expect(page.getByRole('status')).toContainText('Workspace ready');
}

export const test = base.extend<Fixtures>({
  ownerSession: [
    // eslint-disable-next-line no-empty-pattern -- Playwright requires a destructured fixture argument.
    async ({}, use, testInfo) => {
      if (testInfo.project.name === 'mock') {
        const mock: MockApi = createMockApi();
        const server = await createServer({
          root: webRoot,
          plugins: [mock.plugin],
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
          const origin = `http://127.0.0.1:${address.port}`;
          const cookie = await redeemBootstrap(origin, mock.issueBootstrap());
          await use({
            origin,
            cookie,
            secondBootstrapUrl: async () => `${origin}/#bootstrap=${mock.issueBootstrap()}`,
          });
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
      const child = spawn(parleyBinary, ['--workspace', join(workspace, 'owner', 'workspace.sqlite'), '--headless'], {
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      const closed = new Promise<void>((resolve) => child.once('close', () => resolve()));
      let stderr = '';
      child.stderr.setEncoding('utf8').on('data', (chunk: string) => {
        stderr = (stderr + chunk).slice(-8192);
      });
      const lines = createInterface({ input: child.stdout });
      try {
        const line = await Promise.race([
          (async () => {
            for await (const line of lines) {
              if (line.startsWith(BOOTSTRAP_PREFIX)) return line;
            }
            throw new Error('Parley exited without printing its bootstrap URL.');
          })(),
          once(child, 'exit').then(([code, signal]) => {
            throw new Error(`Parley exited before reporting its bootstrap URL (${code ?? signal}).`);
          }),
        ]);
        const bootstrapUrl = new URL(line.slice(BOOTSTRAP_PREFIX.length));
        if (bootstrapUrl.protocol !== 'http:' || bootstrapUrl.hostname !== '127.0.0.1') {
          throw new Error('Parley did not report a loopback bootstrap URL.');
        }
        const origin = bootstrapUrl.origin;
        const cookie = await redeemBootstrap(origin, bootstrapUrl.hash.slice('#bootstrap='.length));
        await use({
          origin,
          cookie,
          secondBootstrapUrl: async () => {
            // A second launcher redeems the control channel of the live
            // workspace, proves the single-writer handoff, prints a fresh
            // one-time bootstrap URL and exits.
            const handedOff = spawn(parleyBinary, ['--workspace', join(workspace, 'owner', 'workspace.sqlite'), '--headless'], {
              stdio: ['ignore', 'pipe', 'pipe'],
            });
            const handoffLines = createInterface({ input: handedOff.stdout });
            const handoffLine = await Promise.race([
              (async () => {
                for await (const line of handoffLines) {
                  if (line.startsWith(BOOTSTRAP_PREFIX)) return line;
                }
                throw new Error('The second launcher exited without a bootstrap URL.');
              })(),
              once(handedOff, 'exit').then(([code, signal]) => {
                throw new Error(`The second launcher exited early (${code ?? signal}).`);
              }),
            ]);
            handoffLines.close();
            await new Promise<void>((resolveExit, reject) => {
              const timeout = setTimeout(
                () => reject(new Error('The second launcher did not exit after the handoff.')),
                10_000,
              );
              handedOff.once('exit', (code) => {
                clearTimeout(timeout);
                if (code === 0) resolveExit();
                else reject(new Error(`The second launcher exited with ${code}.`));
              });
            });
            return handoffLine.slice(BOOTSTRAP_PREFIX.length);
          },
        });
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
    { auto: true },
  ],

  baseURL: async ({ ownerSession }, use) => {
    await use(ownerSession.origin);
  },

  context: async ({ browser, ownerSession }, use) => {
    const context = await browser.newContext();
    context.addCookies([
      {
        name: 'parley_session',
        value: ownerSession.cookie,
        domain: '127.0.0.1',
        path: '/api/v1',
        httpOnly: true,
        sameSite: 'Strict',
      },
    ]);
    await use(context);
    await context.close();
  },
});
