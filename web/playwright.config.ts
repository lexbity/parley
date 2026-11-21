import { defineConfig, devices } from '@playwright/test';

// RFC-0004 INT-06: Playwright runs from user-facing entry points against the
// mock first and against packaged Axum + real SQLite second. Both projects run
// the same spec file and report case IDs; the servers themselves are started
// by per-slice project fixtures, so no webServer is defined here. `prove`
// sets PARLEY_* env vars; the defaults below serve ad-hoc runs.
const jsonReport =
  process.env.PARLEY_PLAYWRIGHT_JSON ?? 'artifacts/playwright-report.json';

export default defineConfig({
  testDir: 'e2e',
  outputDir: process.env.PARLEY_PLAYWRIGHT_OUTPUT ?? 'artifacts/playwright-artifacts',
  fullyParallel: true,
  retries: 0,
  workers: 1,
  reporter: [['list'], ['json', { outputFile: jsonReport }]],
  use: {
    trace: 'retain-on-failure',
  },
  projects: [
    {
      name: 'mock',
      use: {
        ...devices['Desktop Chrome'],
        baseURL: process.env.PARLEY_MOCK_BASE_URL ?? 'http://127.0.0.1:8931',
      },
    },
    {
      name: 'rust',
      use: {
        ...devices['Desktop Chrome'],
        baseURL: process.env.PARLEY_RUST_BASE_URL ?? 'http://127.0.0.1:8932',
      },
    },
  ],
});
