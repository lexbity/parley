/// <reference types="vitest/config" />
import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';

export default defineConfig({
  plugins: [vue()],
  // RFC-0004: package qualification reads the actual Vite manifest and proves
  // the embedded index and asset hashes, so the manifest is always emitted.
  build: {
    manifest: true,
  },
  test: {
    environment: 'jsdom',
    include: ['src/**/*.{test,spec}.ts'],
    coverage: {
      provider: 'v8',
      reporter: ['text', 'json-summary'],
      include: ['src/**'],
      // Entry wiring (createApp mount) is exercised by Playwright e2e, not
      // unit tests; counting it would distort the coverage ratchet.
      exclude: ['src/main.ts'],
    },
  },
});
