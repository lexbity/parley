import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { expect, test, type Page } from '@playwright/test';

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

async function launchMock(): Promise<[string, string]> {
  const child = spawn(process.execPath, ['mock/server.mjs'], { cwd: webDirectory, stdio: 'pipe' });
  processes.push(child);
  const readLine = lineReader(child);
  const first = await readLine((line) => line.startsWith('http://'));
  const second = await readLine((line) => line.startsWith('http://'));
  return [first, second];
}

async function launchRust(): Promise<[string, string]> {
  const directory = await mkdtemp(resolve(tmpdir(), 'parley-s01-'));
  temporaryDirectories.push(directory);
  const workspace = resolve(directory, 'owner', 'workspace.sqlite');
  const executable = resolve(repositoryDirectory, 'target/debug/parley');
  const webDist = resolve(webDirectory, 'dist');
  const environment = { ...process.env, PARLEY_WEB_DIST: webDist };
  const first = spawn(executable, ['--workspace', workspace, '--headless'], { cwd: repositoryDirectory, env: environment, stdio: 'pipe' });
  processes.push(first);
  const readFirstLine = lineReader(first);
  const firstLink = await readFirstLine((line) => line.startsWith('Bootstrap URL: '));
  const second = spawn(executable, ['--workspace', workspace, '--headless'], { cwd: repositoryDirectory, env: environment, stdio: 'pipe' });
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

async function openOwnerPage(page: Page, bootstrapUrl: string): Promise<void> {
  await page.goto(bootstrapUrl);
  await expect(page.getByRole('heading', { name: 'Resources' })).toBeVisible();
  await expect(page.getByRole('status')).toContainText('Workspace ready');
}

test.afterEach(async () => {
  for (const child of processes.splice(0)) {
    if (child.exitCode !== null) continue;
    child.kill('SIGTERM');
    await new Promise<void>((resolveExit) => child.once('exit', () => resolveExit()));
  }
  for (const directory of temporaryDirectories.splice(0)) await rm(directory, { recursive: true, force: true });
});

test('s01_resource_first_launch_retry_and_cross_session_conflict', async ({ browser }, testInfo) => {
  const links = testInfo.project.name === 'mock' ? await launchMock() : await launchRust();
  const first = await browser.newPage();
  const second = await browser.newPage();

  await openOwnerPage(first, links[0]);
  await first.getByRole('button', { name: 'Add resource' }).focus();
  await first.keyboard.press('Enter');
  await first.getByLabel('Name').pressSequentially('Onboarding checklist');
  await first.getByLabel('What is it?').pressSequentially('A short review guide for onboarding teams.');
  await first.getByRole('button', { name: 'Save draft' }).focus();
  await first.keyboard.press('Enter');
  await expect(first.getByRole('status')).toContainText('Draft saved to this workspace.');

  await first.reload();
  await expect(first.getByRole('button', { name: /Onboarding checklist/ })).toBeVisible();
  await openOwnerPage(second, links[1]);
  await second.getByRole('button', { name: /Onboarding checklist/ }).click();
  await second.getByLabel('Name').fill('Shared onboarding checklist');
  await second.getByRole('button', { name: 'Save draft' }).click();
  await expect(second.getByRole('status')).toContainText('Draft saved to this workspace.');

  await first.getByRole('button', { name: /Onboarding checklist/ }).click();
  await first.getByLabel('Name').fill('My onboarding checklist');
  await first.getByRole('button', { name: 'Save draft' }).click();
  await expect(first.getByRole('alert')).toContainText('Review the conflict');
  await expect(first.getByRole('alert')).toContainText('Shared onboarding checklist');
  await expect(first.getByRole('alert')).toContainText('My onboarding checklist');
  expect(await first.evaluate(() => Object.keys(localStorage))).toEqual([]);
  expect(await second.evaluate(() => Object.keys(localStorage))).toEqual([]);
});
