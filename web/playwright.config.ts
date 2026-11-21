import { defineConfig, devices } from '@playwright/test';

// RFC-0004 INT-06: Playwright runs from user-facing entry points against the
// mock first and against the actual Rust service second. Both projects run the
// same spec file and report case IDs; the servers themselves are started by
// per-slice project fixtures inside the spec (a standalone HTTP fixture server
// or the real launcher on an OS-assigned loopback port), so no webServer is
// defined here. `prove` sets PARLEY_* env vars for the JSON report and output.
const jsonReport =
  process.env.PARLEY_PLAYWRIGHT_JSON ?? 'artifacts/playwright-report.json';

export default defineConfig({
  testDir: '.',
  testMatch: 'e2e/*.spec.ts',
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
      use: { ...devices['Desktop Chrome'] },
    },
    {
      name: 'rust',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
});
