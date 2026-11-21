import { execFile } from 'node:child_process';
import { access } from 'node:fs/promises';
import { promisify } from 'node:util';
import { expect, test } from '@playwright/test';
import { build } from 'vite';
import { cargoTarget, parleyBinary, repoRoot, webRoot } from './paths';

test('build the bundled Parley binary', async () => {
  test.setTimeout(240_000);
  await build({ root: webRoot });
  await promisify(execFile)('cargo', [
    'build', '--locked', '--release', '--bin', 'parley', '--target-dir', cargoTarget,
  ], { cwd: repoRoot, timeout: 210_000 });
  await expect(access(parleyBinary)).resolves.toBeUndefined();
});
