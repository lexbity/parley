import { createHash } from "node:crypto"
import Ajv2020 from "ajv/dist/2020.js"

export const MAX_FRAME = 512 * 1024
export const MAX_ROLE_INPUT = 256 * 1024
export const MAX_ROLE_OUTPUT = 128 * 1024
export const MAX_PROMPT_SCHEMA = 96 * 1024

export function sha256(value) {
  return createHash("sha256").update(value).digest("hex")
}

export class WorkerFailure extends Error {
  constructor(code) {
    super(code)
    this.code = code
  }
}

function invalid() {
  throw new WorkerFailure("WORKER_PROTOCOL")
}

// Parse once for I-JSON invariants before JSON.parse can collapse duplicate keys.
export function parseJson(bytes) {
  const json = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes)
  const token = /[ \t\r\n]*("(?:[^"\\\u0000-\u001f]|\\(?:["\\/bfnrt]|u[\da-fA-F]{4}))*"|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?|true|false|null|[{}\[\]:,])/y
  let next
  function advance() {
    const before = token.lastIndex
    const match = token.exec(json)
    if (!match) {
      if (!/^[ \t\r\n]*$/.test(json.slice(before))) invalid()
      next = undefined
      return
    }
    next = match[1]
  }
  function consume(delimiter) {
    if (next !== delimiter) invalid()
    advance()
  }
  function string() {
    const value = JSON.parse(next)
    if (typeof value !== "string" || !value.isWellFormed()) invalid()
    return value
  }
  function object(depth) {
    const keys = new Set()
    advance()
    if (next !== "}") {
      while (true) {
        const key = string()
        if (keys.has(key)) invalid()
        keys.add(key)
        advance()
        consume(":")
        value(depth + 1)
        if (next !== ",") break
        advance()
      }
    }
    consume("}")
  }
  function array(depth) {
    advance()
    if (next !== "]") {
      while (true) {
        value(depth + 1)
        if (next !== ",") break
        advance()
      }
    }
    consume("]")
  }
  function value(depth) {
    if (depth > 32 || next === undefined) invalid()
    if (next === "{") return object(depth)
    if (next === "[") return array(depth)
    if (next.startsWith('"')) string()
    else if (/^-?\d/.test(next)) {
      const number = Number(next)
      if (!Number.isFinite(number) || (Number.isInteger(number) && !Number.isSafeInteger(number))) invalid()
    } else if (!["true", "false", "null"].includes(next)) invalid()
    advance()
  }
  advance()
  value(1)
  if (next !== undefined) invalid()
  return JSON.parse(json)
}

export function compileSchema(schema) {
  const ajv = new Ajv2020({ ownProperties: true })
  return ajv.compile(schema)
}

export async function* readFrames(stream, validate) {
  const frame = Buffer.alloc(MAX_FRAME - 1)
  let used = 0
  let expected = 1
  for await (const chunk of stream) {
    let start = 0
    while (start < chunk.length) {
      const lf = chunk.indexOf(10, start)
      const end = lf < 0 ? chunk.length : lf
      const count = end - start
      if (used + count > frame.length) invalid()
      chunk.copy(frame, used, start, end)
      used += count
      if (lf < 0) break
      if (used && frame[used - 1] === 13) invalid()
      const decoded = parseJson(frame.subarray(0, used))
      if (!validate(decoded) || decoded.seq !== expected++) invalid()
      used = 0
      start = lf + 1
      yield decoded
    }
  }
  if (used) invalid()
}

export function writeFrame(stream, validate, frame) {
  if (!validate(frame)) invalid()
  const bytes = Buffer.from(JSON.stringify(frame) + "\n")
  if (bytes.length > MAX_FRAME) invalid()
  return new Promise((resolve, reject) => {
    stream.write(bytes, (error) => error ? reject(error) : resolve())
  })
}
