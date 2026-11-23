import { createServer } from 'vite';
import { createMockApi } from '../e2e/mock-api.ts';
import { webRoot } from '../e2e/paths.ts';

/**
 * Development mock (RFC-0003 FE-13.2): serves the UI from Vite with the same
 * in-process fixture API the Playwright mock project uses. Prints two one-time
 * bootstrap URLs so two independent owner tabs can be opened.
 */
const mock = createMockApi();
const server = await createServer({
  root: webRoot,
  plugins: [mock.plugin],
  server: { host: '127.0.0.1' },
});
await server.listen();
const origin = `http://127.0.0.1:${server.config.server.port ?? 5173}`;
for (let index = 0; index < 2; index += 1) {
  console.log(`${origin}/#bootstrap=${mock.issueBootstrap()}`);
}
console.log(`Parley development mock ready at ${origin}`);
