import { spawn, type ChildProcess } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { cp, mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium, expect, request, type APIRequestContext } from '@playwright/test';

type Session = { workspace_id: string; epoch: string; mutation_token: string };
type Running = { bootstrap: URL; api: APIRequestContext; session: Session };
type Bundle = { launcher_sha256: string; web_assets: Record<string, string> };

const [launcherArg, manifestArg, previousArg] = process.argv.slice(2);
if (!launcherArg) throw new Error('Usage: node e2e/release-smoke.ts LAUNCHER [RELEASE-MANIFEST.json [PREVIOUS_LAUNCHER]]');
const launcher = resolve(launcherArg);
const previous = previousArg ? resolve(previousArg) : null;
const hash = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');
const bundle: Bundle | null = manifestArg ? JSON.parse(await readFile(manifestArg, 'utf8')) : null;
if (bundle && hash(await readFile(launcher)) !== bundle.launcher_sha256) throw new Error('Packaged launcher digest mismatch');

async function stopLauncher(child: ChildProcess, closed: Promise<number | null>) {
  if (child.exitCode !== null || child.signalCode !== null || !child.pid) return;
  child.kill('SIGINT');
  const abort = new AbortController();
  const outcome = await Promise.race([closed, delay(5000, 'timeout' as const, { signal: abort.signal })]);
  abort.abort();
  if (outcome === 'timeout') {
    child.kill('SIGKILL');
    await closed;
    throw new Error('The launcher did not stop cleanly');
  }
  if (outcome !== 0) throw new Error(`Launcher shutdown exited with ${outcome}`);
}

async function withLauncher<T>(binary: string, workspace: string, use: (running: Running) => Promise<T>): Promise<T> {
  await mkdir(dirname(workspace), { recursive: true, mode: 0o700 });
  const child = spawn(binary, ['--workspace', workspace, '--headless'], {
    cwd: dirname(workspace),
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stderr = '';
  child.stderr.on('data', (part: Buffer) => { stderr = (stderr + part.toString()).slice(-4000); });
  const lines = createInterface({ input: child.stdout });
  const closed = new Promise<number | null>(resolveClose => child.once('close', resolveClose));
  let api: APIRequestContext | null = null;
  try {
    const bootstrap = await new Promise<URL>((resolveReady, rejectReady) => {
      const timer = setTimeout(() => rejectReady(new Error('Timed out waiting for the packaged launcher')), 20_000);
      child.once('error', rejectReady);
      child.once('exit', (code, signal) => rejectReady(new Error(`Launcher exited during startup (${code ?? signal})`)));
      lines.on('line', (line) => {
        if (!line.startsWith('Bootstrap URL: ')) return;
        try { resolveReady(new URL(line.slice('Bootstrap URL: '.length))); }
        catch { rejectReady(new Error('Malformed launcher bootstrap URL')); }
      });
      void closed.then(() => clearTimeout(timer));
    });
    if (bootstrap.protocol !== 'http:' || bootstrap.hostname !== '127.0.0.1' || !bootstrap.hash.startsWith('#bootstrap=')) {
      throw new Error('Launcher did not report its one-time loopback URL');
    }
    api = await request.newContext({ baseURL: bootstrap.origin, timeout: 10_000 });
    expect((await api.get('/api/v1/session')).status()).toBe(401);
    const bootstrapResponse = await api.post('/api/v1/session/bootstrap', { data: { secret: bootstrap.hash.slice('#bootstrap='.length) } });
    expect(bootstrapResponse.status(), 'Redeem the packaged launcher bootstrap').toBe(200);
    const sessionResponse = await api.get('/api/v1/session');
    expect(sessionResponse.status(), 'Read the authenticated packaged session').toBe(200);
    return await use({ api, bootstrap, session: await sessionResponse.json() as Session });
  } catch (error) {
    const safeStderr = stderr.trim().replace(/#bootstrap=[^\s]+/g, '#bootstrap=[redacted]');
    if (safeStderr) throw new Error(safeStderr, { cause: error });
    throw error;
  } finally {
    await api?.dispose();
    await stopLauncher(child, closed);
    lines.close();
  }
}

function privateHeaders(session: Session) {
  return { 'X-Parley-Epoch': session.epoch, 'X-Parley-Token': session.mutation_token };
}

async function addParty({ api, session }: Running, name: string) {
  const response = await api.post('/api/v1/entities', {
    headers: { ...privateHeaders(session), 'Idempotency-Key': randomUUID() },
    data: { name, kind: 'person', focus: 'Software version qualification' },
  });
  expect(response.status(), 'Create a synthetic Party').toBe(201);
}

async function partyNames({ api, session }: Running) {
  const response = await api.get('/api/v1/entities?kind=person', { headers: privateHeaders(session) });
  expect(response.status(), 'Read the synthetic Parties from the Workspace copy').toBe(200);
  const result = await response.json() as { data: Array<{ name: string }> };
  return result.data.map(party => party.name);
}

async function verifyUI({ api, bootstrap }: Running) {
  const health = await api.get('/api/v1/health');
  expect(health.status()).toBe(200);
  expect(await health.json()).toMatchObject({ status: 'ready', model: 'not-probed' });
  for (const [path, digest] of Object.entries(bundle?.web_assets ?? {})) {
    const response = await api.get(path === 'index.html' ? '/' : '/' + path);
    expect(response.status(), `Embedded web file ${path}`).toBe(200);
    expect(hash(await response.body()), `Embedded digest for ${path}`).toBe(digest);
  }
  const browser = await chromium.launch();
  try {
    const context = await browser.newContext({ serviceWorkers: 'block', storageState: await api.storageState() });
    const externalRequests: string[] = [];
    await context.route('**/*', async route => {
      const requested = new URL(route.request().url());
      if (requested.origin === bootstrap.origin) await route.continue();
      else {
        externalRequests.push(requested.origin);
        await route.abort();
      }
    });
    const page = await context.newPage();
    await page.goto(bootstrap.origin + '/resources');
    await expect(page.getByRole('heading', { name: 'Resources', exact: true })).toBeVisible();
    const board = page.getByRole('region', { name: 'Resource board', exact: true });
    await expect(board.getByRole('heading', { name: /Inbox/ })).toBeVisible();
    await board.getByRole('button', { name: 'Add card to Inbox' }).click();
    const editor = page.getByRole('form', { name: 'Resource card editor' });
    await editor.getByLabel('Title').fill('Packaged Resource');
    const notes = editor.getByRole('textbox', { name: 'Owner notes', exact: true });
    await expect(notes).toHaveCSS('font-family', /monospace/);
    await notes.fill('Packaged **Markdown**.');
    await editor.getByRole('button', { name: 'Save card' }).click();
    await expect(board.getByRole('button', { name: /^Artifact Packaged Resource/ })).toBeVisible();
    await page.reload();
    await expect(board.getByRole('button', { name: /^Artifact Packaged Resource/ })).toBeVisible();
    await page.getByRole('link', { name: 'Parties', exact: true }).click();
    await page.getByRole('button', { name: 'Import CSV', exact: true }).click();
    const importer = page.getByRole('dialog', { name: 'Import parties from CSV' });
    await importer.getByLabel('Or paste CSV').fill('Name\nPackaged party\n');
    await importer.getByRole('button', { name: 'Read columns' }).click();
    await importer.getByRole('button', { name: 'Preview mapped rows' }).click();
    const sheet = importer.getByRole('region', { name: 'CSV preview spreadsheet' });
    await expect(sheet.getByText('Packaged party', { exact: true })).toBeVisible();
    await importer.getByRole('button', { name: 'Cancel', exact: true }).click();
    expect(externalRequests).toEqual([]);
  } finally {
    await browser.close();
  }
}

const scratch = await mkdtemp(join(process.platform === 'darwin' ? '/tmp' : tmpdir(), 'parley-release-'));
try {
  await withLauncher(launcher, join(scratch, 'fresh', 'workspace.sqlite'), verifyUI);
  if (!bundle) {
    console.log('Development launcher rendered the embedded UI and saved a Resource.');
  } else {
    const baseline = previous ?? launcher;
    const seed = join(scratch, 'seed');
    const preUpgrade = 'Pre-upgrade Party';
    const postUpgrade = 'Post-upgrade Party';
    const baselineWorkspace = await withLauncher(baseline, join(seed, 'workspace.sqlite'), async (running) => {
      await addParty(running, preUpgrade);
      const backup = await running.api.post('/api/v1/backups', { headers: privateHeaders(running.session) });
      expect(backup.status(), 'Create the verified pre-upgrade backup').toBe(200);
      expect(await backup.json()).toMatchObject({ status: 'verified', workspace_id: running.session.workspace_id });
      return running.session.workspace_id;
    });
    const upgraded = join(scratch, 'upgraded');
    await cp(seed, upgraded, { recursive: true });
    await withLauncher(launcher, join(upgraded, 'workspace.sqlite'), async (running) => {
      expect(running.session.workspace_id).toBe(baselineWorkspace);
      expect(await partyNames(running)).toContain(preUpgrade);
      await addParty(running, postUpgrade);
    });
    await withLauncher(launcher, join(upgraded, 'workspace.sqlite'), async (running) => {
      expect(await partyNames(running)).toEqual(expect.arrayContaining([preUpgrade, postUpgrade]));
    });
    const recovered = join(scratch, 'recovered');
    await cp(seed, recovered, { recursive: true });
    await withLauncher(baseline, join(recovered, 'workspace.sqlite'), async (running) => {
      expect(running.session.workspace_id).toBe(baselineWorkspace);
      expect(await partyNames(running)).toEqual([preUpgrade]);
    });
    console.log(previous ? 'Packaged UI, prior-package upgrade, restart and recovery passed.' : 'Packaged UI, copied-workspace restart and same-version recovery passed.');
  }
} finally {
  await rm(scratch, { recursive: true, force: true });
}
