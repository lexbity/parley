import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const webRoot = fileURLToPath(new URL('../', import.meta.url));
export const repoRoot = resolve(webRoot, '..');
export const cargoTarget = resolve(repoRoot, process.env.CARGO_TARGET_DIR ?? 'target');
export const parleyBinary = resolve(cargoTarget, 'release', 'parley');
