import assert from "node:assert/strict"
import { mkdtemp, rm } from "node:fs/promises"
import { createServer } from "node:http"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { once } from "node:events"
import test from "node:test"
import { createRelay } from "../src/relay.mjs"
import { sha256 } from "../src/protocol.mjs"
import { executeDelegated } from "../src/delegated.mjs"

const tokenizer = Buffer.from(JSON.stringify({
  tokenizer: {
    version: "1.0", truncation: null, padding: null, added_tokens: [],
    normalizer: null, pre_tokenizer: { type: "Whitespace" },
    post_processor: null, decoder: null,
    model: { type: "WordLevel", vocab: { "<unk>": 0 }, unk_token: "<unk>" },
  },
  config: {},
}))
const modelTag = "parley-test:latest"
const modelDigest = "b".repeat(64)
const providerVersion = "0.17.0"
const profile = {
  revision: 1, provider_version: providerVersion, model_tag: modelTag,
  model_digest: modelDigest, tokenizer_digest: sha256(tokenizer),
  context_tokens: 8192, template_tokens: 64, max_output_tokens: 256,
}
const tools = [{
  name: "commands__run",
  description: "Run one registered command",
  parameters: { type: "object", additionalProperties: false, properties: { id: { type: "string" } }, required: ["id"] },
}]

function chunk(choice, usage) {
  return "data: " + JSON.stringify({
    id: "chatcmpl-test", object: "chat.completion.chunk", created: 42,
    model: modelTag, choices: choice ? [choice] : [], usage,
  }) + "\n\n"
}

async function provider(t, replies) {
  const dir = await mkdtemp(join(tmpdir(), "parley-delegated-test-"))
  const socket = join(dir, "worker.sock")
  const paths = []
  const bodies = []
  let chat = 0
  const server = createServer(async (request, response) => {
    paths.push(request.url)
    try {
      assert.equal(request.headers.host, "parley-provider.invalid")
      if (request.method === "GET" && request.url === "/api/version") {
        response.setHeader("Content-Type", "application/json")
        response.end(JSON.stringify({ version: providerVersion }))
        return
      }
      if (request.method === "GET" && request.url === "/api/tags") {
        response.setHeader("Content-Type", "application/json")
        response.end(JSON.stringify({ models: [{ name: modelTag, digest: modelDigest }] }))
        return
      }
      assert.equal(request.method, "POST")
      const parts = []
      for await (const data of request) parts.push(data)
      const payload = JSON.parse(Buffer.concat(parts))
      bodies.push(payload)
      assert.equal(payload.model, modelTag)
      assert.equal(payload.stream, true)
      assert.equal(payload.temperature, 0)
      assert.deepEqual(payload.tools.map((tool) => tool.function.name), ["commands__run"])
      response.setHeader("Content-Type", "text/event-stream")
      const reply = replies[chat++] ?? { text: "Done." }
      if (reply.tool) {
        response.write(chunk({ index: 0, delta: { role: "assistant", content: reply.text ?? "" }, finish_reason: null }))
        response.write(chunk({
          index: 0,
          delta: { tool_calls: [{ index: 0, id: "sdk-1", type: "function", function: { name: reply.tool, arguments: JSON.stringify(reply.args ?? {}) } }] },
          finish_reason: null,
        }))
        response.write(chunk({ index: 0, delta: {}, finish_reason: "tool_calls" }))
      } else {
        response.write(chunk({ index: 0, delta: { role: "assistant", content: reply.text }, finish_reason: null }))
        response.write(chunk({ index: 0, delta: {}, finish_reason: "stop" }))
      }
      response.write(chunk(null, { prompt_tokens: 40, completion_tokens: 8, total_tokens: 48 }))
      response.end("data: [DONE]\n\n")
    } catch (error) {
      t.diagnostic(String(error))
      response.statusCode = 500
      response.end()
    }
  })
  server.listen(socket)
  await once(server, "listening")
  const signal = AbortSignal.timeout(5000)
  const relay = createRelay(socket, signal, { chatCalls: 16 })
  t.after(async () => {
    await relay.close()
    await new Promise((resolve) => server.close(resolve))
    await rm(dir, { recursive: true })
  })
  return { relay, paths, bodies, signal }
}

function control(frames) {
  return {
    sent: [],
    async send(kind, body) {
      this.sent.push({ kind, body })
    },
    async next() {
      return frames.shift()
    },
  }
}

test("delegated Pi Agent Core journals a turn before executing its tool", async (t) => {
  const { relay, paths, bodies, signal } = await provider(t, [
    { tool: "commands__run", args: { id: "brief.start" }, text: "Starting." },
    { text: "The brief is ready." },
  ])
  const controls = control([
    { kind: "turn_accepted", body: { turn_no: 1, ordinals: [1] } },
    { kind: "tool_result", body: { turn_no: 1, ordinal: 1, result: { ok: true } } },
  ])
  const result = await executeDelegated({
    mode: "delegated",
    profile,
    deadline_ms: 4000,
    task: "Prepare a brief",
    transcript: [
      { role: "system", content: "Scoped assistant." },
      { role: "user", content: "Prepare a brief" },
    ],
    tools,
    continuation: null,
  }, { tokenizer }, relay.fetch, signal, controls)
  t.diagnostic(JSON.stringify({ result, sent: controls.sent, paths, bodies: bodies.map((body) => body.messages.map((message) => message.role)) }))
  assert.equal(result.suspended, false)
  assert.equal(result.body.mode, "delegated")
  assert.deepEqual(result.body.observed, { provider_version: providerVersion, model_tag: modelTag, model_digest: modelDigest })
  assert.deepEqual(result.body.usage, { input_tokens: 40, output_tokens: 8 })
  assert.equal(result.body.text, "The brief is ready.")
  assert.deepEqual(controls.sent[0].kind, "assistant_turn")
  assert.equal(controls.sent[0].body.calls[0].name, "commands__run")
  assert.deepEqual(controls.sent[1].kind, "tool_request")
  assert.deepEqual(controls.sent[1].body.args, { id: "brief.start" })
  assert.ok(paths.filter((path) => path === "/v1/chat/completions").length >= 2)
})
