import { randomUUID } from 'node:crypto';
import { defineConfig, devices } from '@playwright/test';

// Workers inherit this ID so the configuration resolves to the same paths
// throughout one invocation. Concurrent invocations get separate reports.
const runId = process.env.PARLEY_PLAYWRIGHT_RUN_ID ??= randomUUID();
const runDirectory = `artifacts/playwright-${runId}`;

export default defineConfig({
  testDir: '.',
  outputDir: process.env.PARLEY_PLAYWRIGHT_OUTPUT ?? `${runDirectory}/browser`,
  fullyParallel: true,
  retries: 0,
  workers: 1,
  reporter: [
    ['list'],
    ['json', { outputFile: process.env.PARLEY_PLAYWRIGHT_JSON ?? `${runDirectory}/report.json` }],
  ],
  use: {
    ...devices['Desktop Chrome'],
    trace: 'retain-on-failure',
  },
  projects: [
    { name: 'mock', testMatch: 'e2e/*.spec.ts' },
    { name: 'rust-build', testMatch: 'e2e/build-rust.setup.ts' },
    { name: 'rust', testMatch: 'e2e/*.spec.ts', dependencies: ['rust-build'] },
  ],
});
