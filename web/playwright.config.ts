import { defineConfig, devices } from '@playwright/test';
const e2eWorkspace = '/tmp/parley-rfc0002-s01-s02-e2e';

// RFC-0004 INT-06: Playwright runs from user-facing entry points against the
// mock first and against packaged Axum + real SQLite second. Both projects run
// the same spec file and report case IDs; the servers themselves are started
// by per-slice project fixtures, so no webServer is defined here. `prove`
// sets PARLEY_* env vars; the defaults below serve ad-hoc runs.
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
      metadata: { e2eWorkspace },
      use: {
        ...devices['Desktop Chrome'],
        baseURL: process.env.PARLEY_MOCK_BASE_URL ?? 'http://127.0.0.1:8931',
      },
    },
    {
      name: 'rust',
      metadata: { e2eWorkspace },
      use: {
        ...devices['Desktop Chrome'],
        baseURL: process.env.PARLEY_RUST_BASE_URL ?? 'http://127.0.0.1:8932',
      },
    },
  ],
  webServer: [
    { command: 'node e2e/mock-api.mjs', url: 'http://127.0.0.1:8934/api/v1/health', reuseExistingServer: false, timeout: 30_000 },
    { command: `cargo run --quiet --manifest-path ../Cargo.toml --bin parley -- --workspace ${e2eWorkspace} --port 8933`, url: 'http://127.0.0.1:8933/api/v1/health', reuseExistingServer: false, timeout: 120_000 },
    { command: 'PARLEY_API_TARGET=http://127.0.0.1:8934 pnpm exec vite --host 127.0.0.1 --port 8931 --strictPort', url: 'http://127.0.0.1:8931', reuseExistingServer: false, timeout: 60_000 },
    { command: 'PARLEY_API_TARGET=http://127.0.0.1:8933 pnpm exec vite --host 127.0.0.1 --port 8932 --strictPort', url: 'http://127.0.0.1:8932', reuseExistingServer: false, timeout: 60_000 },
  ],
});
