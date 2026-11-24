import assert from "node:assert/strict"
import { mkdtemp, rm } from "node:fs/promises"
import { createServer } from "node:http"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { once } from "node:events"
import test from "node:test"
import { createRelay } from "../src/relay.mjs"
import { sha256 } from "../src/protocol.mjs"
import { executeStructured } from "../src/structured.mjs"

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

function chunk(choice, usage) {
  return "data: " + JSON.stringify({
    id: "chatcmpl-test", object: "chat.completion.chunk", created: 42,
    model: modelTag, choices: choice ? [choice] : [], usage,
  }) + "\n\n"
}

async function provider(t, { drift = false, malformed = false } = {}) {
  const dir = await mkdtemp(join(tmpdir(), "parley-worker-test-"))
  const socket = join(dir, "worker.sock")
  const paths = []
  let catalogReads = 0
  const server = createServer(async (request, response) => {
    paths.push(request.url)
    try {
      assert.equal(request.headers.host, "parley-provider.invalid")
      assert.equal(request.headers.authorization, undefined)
      if (request.method === "GET" && request.url === "/api/version") {
        response.setHeader("Content-Type", "application/json")
        response.end(JSON.stringify({ version: providerVersion }))
        return
      }
      if (request.method === "GET" && request.url === "/api/tags") {
        const digest = ++catalogReads === 2 && drift ? "c".repeat(64) : modelDigest
        response.setHeader("Content-Type", "application/json")
        response.end(JSON.stringify({ models: [{ name: modelTag, digest }] }))
        return
      }
      assert.equal(request.method, "POST")
      assert.equal(request.url, "/v1/chat/completions")
      const bytes = []
      for await (const data of request) bytes.push(data)
      const payload = JSON.parse(Buffer.concat(bytes))
      assert.equal(payload.model, modelTag)
      assert.deepEqual(payload.messages.map((message) => message.role), ["system", "user"])
      assert.equal(typeof payload.messages[1].content, "string")
      assert.equal(payload.temperature, 0)
      assert.equal(payload.max_tokens, 256)
      assert.equal(payload.stream, true)
      assert.equal(payload.stream_options.include_usage, true)
      assert.equal(payload.response_format.type, "json_schema")
      assert.equal(payload.response_format.json_schema.strict, true)
      assert.equal(payload.response_format.json_schema.schema.additionalProperties, false)
      assert.equal(payload.tools, undefined)
      response.setHeader("Content-Type", "text/event-stream")
      response.write(chunk({ index: 0, delta: { role: "assistant", content: malformed ? '{"witness":"wrong"}' : '{"witness":"parley-structured-ready"}' }, finish_reason: null }))
      response.write(chunk({ index: 0, delta: {}, finish_reason: "stop" }))
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
  const signal = AbortSignal.timeout(3000)
  const relay = createRelay(socket, signal)
  t.after(async () => {
    await relay.close()
    await new Promise((resolve) => server.close(resolve))
    await rm(dir, { recursive: true })
  })
  return { relay, paths, signal }
}

test("pinned Pi sends actual schema payload through the Unix connector, without ambient authority", async (t) => {
  const { relay, paths, signal } = await provider(t)
  const result = await executeStructured({ profile, mode: "probe" }, { tokenizer }, relay.fetch, signal)
  assert.deepEqual(result, {
    mode: "probe", observed: { model_tag: modelTag, model_digest: modelDigest, provider_version: providerVersion },
    usage: { input_tokens: 40, output_tokens: 8 },
  })
  assert.deepEqual(paths, ["/api/version", "/api/tags", "/v1/chat/completions", "/api/version", "/api/tags"])
})

test("post-inference catalog drift fences the result and a second chat cannot reach the relay", async (t) => {
  const { relay, paths, signal } = await provider(t, { drift: true })
  await assert.rejects(executeStructured({ profile, mode: "probe" }, { tokenizer }, relay.fetch, signal), { code: "PROFILE_CHANGED" })
  await assert.rejects(relay.fetch("http://parley-provider.invalid/v1/chat/completions", { method: "POST", body: "{}" }))
  assert.equal(paths.filter((path) => path === "/v1/chat/completions").length, 1)
})

test("schema-invalid output cannot pass as a capability witness", async (t) => {
  const { relay, paths, signal } = await provider(t, { malformed: true })
  await assert.rejects(executeStructured({ profile, mode: "probe" }, { tokenizer }, relay.fetch, signal), { code: "RESULT_INVALID" })
  assert.deepEqual(paths, ["/api/version", "/api/tags", "/v1/chat/completions"])
})
