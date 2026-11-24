import { createServer, type Server } from 'node:http';
import type { Socket } from 'node:net';

/** Scripted Ollama-compatible upstream for the Rust browser project. The
 * response shapes match the wire that the contained Pi worker drives through
 * the private relay; no browser mock substitutes for this fixture. */
export type ModelReply =
  | { kind: 'final'; text: string }
  | { kind: 'tool'; name: string; args: unknown; text?: string };

export type ModelServer = {
  endpoint: string;
  requests: string[];
  close(): Promise<void>;
};

export async function startModelServer(replies: ModelReply[]): Promise<ModelServer> {
  const sockets = new Set<Socket>();
  const requests: string[] = [];
  let index = 0;
  const server: Server = createServer((request, response) => {
    const parts: Buffer[] = [];
    request.on('data', (chunk: Buffer) => parts.push(chunk));
    request.on('end', () => {
      const body = Buffer.concat(parts).toString('utf8');
      requests.push(`${request.method} ${request.url} ${body}`);
      if (request.url === '/api/version') {
        response.writeHead(200, { 'content-type': 'application/json' });
        response.end(JSON.stringify({ version: '0.17.0' }));
        return;
      }
      if (request.url === '/api/tags') {
        response.writeHead(200, { 'content-type': 'application/json' });
        response.end(JSON.stringify({ models: [{ name: 'parley-fixture:latest', digest: 'b'.repeat(64) }] }));
        return;
      }
      if (request.method !== 'POST' || request.url !== '/v1/chat/completions') {
        response.writeHead(404);
        response.end();
        return;
      }
      const reply: ModelReply | undefined = replies[Math.min(index++, replies.length - 1)];
      if (!reply) {
        response.writeHead(500);
        response.end();
        return;
      }
      const chunk = (delta: unknown, finish: string | null) =>
        `data: ${JSON.stringify({
          id: 'chatcmpl-fixture',
          object: 'chat.completion.chunk',
          created: 42,
          model: 'parley-fixture:latest',
          choices: [{ index: 0, delta, finish_reason: finish }],
          usage: { prompt_tokens: 40, completion_tokens: 8, total_tokens: 48 },
        })}\n\n`;
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      if (reply.kind === 'tool') {
        if (reply.text) response.write(chunk({ role: 'assistant', content: reply.text }, null));
        response.write(chunk({
          tool_calls: [{
            index: 0,
            id: 'sdk-0',
            type: 'function',
            function: { name: reply.name, arguments: JSON.stringify(reply.args) },
          }],
        }, null));
        response.write(chunk({}, 'tool_calls'));
      } else {
        response.write(chunk({ role: 'assistant', content: reply.text }, null));
        response.write(chunk({}, 'stop'));
      }
      response.write(chunk({}, null));
      response.end('data: [DONE]\n\n');
    });
  });
  server.on('connection', (socket) => sockets.add(socket));
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Model server did not bind');
  return {
    endpoint: `http://127.0.0.1:${address.port}`,
    requests,
    close: () => new Promise<void>((resolve) => {
      for (const socket of sockets) socket.destroy();
      server.close(() => resolve());
    }),
  };
}
