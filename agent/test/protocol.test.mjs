import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { Readable } from "node:stream"
import test from "node:test"
import { compileSchema, MAX_FRAME, parseJson, readFrames } from "../src/protocol.mjs"

const runtimeSchema = readFileSync(new URL("../../contract/agent/runtime-v1.schema.json", import.meta.url))
const validate = compileSchema(parseJson(runtimeSchema))
const ack = { v: 1, seq: 1, kind: "hello_ack", body: { capabilities: ["structured_role", "delegated"], max_frame_bytes: MAX_FRAME } }

async function collect(...chunks) {
  const items = []
  for await (const item of readFrames(Readable.from(chunks.map((chunk) => Buffer.from(chunk))), validate)) {
    items.push(item)
  }
  return items
}

test("runtime frame accepts one LF-delimited frame split across arbitrary reads", async () => {
  const text = JSON.stringify(ack)
  assert.deepEqual(await collect(text.slice(0, 19), text.slice(19), "\n"), [ack])
})

test("closed frame rejects unknown fields, duplicate sequence, CRLF and EOF", async () => {
  const text = JSON.stringify(ack)
  for (const input of [
    text,
    text + "\r\n",
    text + "\n" + text + "\n",
    text.replace('"seq":1', '"seq":1,"extra":1') + "\n",
    text.replace('"seq":1', '"seq":1,"seq":1') + "\n",
    " ".repeat(MAX_FRAME) + text + "\n",
    Buffer.from([0xff, 0x0a]),
  ]) {
    await assert.rejects(collect(input))
  }
})

test("I-JSON rejects duplicate escaped keys, nonfinite and oversized numbers and lone surrogates", () => {
  for (const text of [
    '{"a":1,"\\u0061":2}',
    '{"a":9e400}',
    '{"a":9007199254740992}',
    '{"a":"\\ud800"}',
    "[".repeat(33) + "0" + "]".repeat(33),
  ]) assert.throws(() => parseJson(Buffer.from(text)))
  assert.deepEqual(parseJson(Buffer.from('{"a":"\\ud83d\\ude00"}')), { a: "😀" })
})
