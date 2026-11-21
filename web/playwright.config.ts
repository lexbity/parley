import { randomUUID } from 'node:crypto';
import { defineConfig, devices } from '@playwright/test';

// RFC-0004 INT-06: Playwright runs from user-facing entry points against the
// mock first and against the actual Rust service second. Both projects run the
// same spec files and report case IDs. The mock project starts only Vite with
// the in-process fixture API; the rust project builds the Vite bundle and the
// release binary once (rust-build dependency), then opens the embedded UI on a
// fresh temporary workspace per test. Workers inherit this ID so the
// configuration resolves to the same paths throughout one invocation;
// concurrent invocations get separate reports.
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
