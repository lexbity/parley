import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { expect, type Page } from '@playwright/test';

const webDirectory = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const repositoryDirectory = resolve(webDirectory, '..');
const processes: ChildProcessWithoutNullStreams[] = [];
const temporaryDirectories: string[] = [];

function lineReader(child: ChildProcessWithoutNullStreams): (matches: (line: string) => boolean) => Promise<string> {
  const lines = createInterface({ input: child.stdout });
  const buffered: string[] = [];
  const waiters: Array<{ matches: (line: string) => boolean; resolve: (line: string) => void }> = [];
  lines.on('line', (line) => {
    const index = waiters.findIndex((waiter) => waiter.matches(line));
    if (index < 0) {
      buffered.push(line);
      return;
    }
    waiters.splice(index, 1)[0]?.resolve(line);
  });
  return (matches) => {
    const index = buffered.findIndex(matches);
    if (index >= 0) return Promise.resolve(buffered.splice(index, 1)[0] ?? '');
    return new Promise((resolveLine, reject) => {
      const timeout = setTimeout(() => reject(new Error('Timed out waiting for the local service to start.')), 10_000);
      waiters.push({
        matches,
        resolve: (line) => {
          clearTimeout(timeout);
          resolveLine(line);
        },
      });
      child.once('error', (error) => {
        clearTimeout(timeout);
        reject(error);
      });
      child.once('exit', (code) => {
        if (code === 0) return;
        clearTimeout(timeout);
        reject(new Error(`Local service exited before readiness (code ${code}).`));
      });
    });
  };
}

/** Start the independent HTTP fixture service; it prints two bootstrap URLs. */
export async function launchMock(): Promise<[string, string]> {
  const child = spawn(process.execPath, ['mock/server.mjs'], { cwd: webDirectory, stdio: 'pipe' });
  processes.push(child);
  const readLine = lineReader(child);
  const first = await readLine((line) => line.startsWith('http://'));
  const second = await readLine((line) => line.startsWith('http://'));
  return [first, second];
}

/**
 * Start the real launcher twice against one workspace: the first process
 * serves it, the second redeems the control channel and exits, proving the
 * single-writer lock and the bootstrap handoff (RFC-0004 S01).
 */
export async function launchRust(): Promise<[string, string]> {
  const directory = await mkdtemp(resolve(tmpdir(), 'parley-e2e-'));
  temporaryDirectories.push(directory);
  const workspace = resolve(directory, 'owner', 'workspace.sqlite');
  const executable = resolve(repositoryDirectory, 'target/debug/parley');
  const first = spawn(executable, ['--workspace', workspace, '--headless'], { cwd: repositoryDirectory, stdio: 'pipe' });
  processes.push(first);
  const readFirstLine = lineReader(first);
  const firstLink = await readFirstLine((line) => line.startsWith('Bootstrap URL: '));
  const second = spawn(executable, ['--workspace', workspace, '--headless'], { cwd: repositoryDirectory, stdio: 'pipe' });
  processes.push(second);
  const readSecondLine = lineReader(second);
  const secondLink = await readSecondLine((line) => line.startsWith('Bootstrap URL: '));
  if (second.exitCode === null) {
    await new Promise<void>((resolveExit, reject) => {
      const timeout = setTimeout(() => reject(new Error('Second launcher did not exit after opening the active workspace.')), 10_000);
      second.once('exit', (code) => {
        clearTimeout(timeout);
        if (code === 0) resolveExit();
        else reject(new Error(`Second launcher exited with ${code}.`));
      });
    });
  }
  return [firstLink.slice('Bootstrap URL: '.length), secondLink.slice('Bootstrap URL: '.length)];
}

export async function openOwnerPage(page: Page, bootstrapUrl: string): Promise<void> {
  await page.goto(bootstrapUrl);
  await expect(page.getByRole('heading', { name: 'Resources' })).toBeVisible();
  await expect(page.getByRole('status')).toContainText('Workspace ready');
}

export async function stopLaunchedServices(): Promise<void> {
  for (const child of processes.splice(0)) {
    if (child.exitCode !== null) continue;
    child.kill('SIGTERM');
    await new Promise<void>((resolveExit) => child.once('exit', () => resolveExit()));
  }
  for (const directory of temporaryDirectories.splice(0)) await rm(directory, { recursive: true, force: true });
}
