import { Agent, request } from "undici"
import { WorkerFailure } from "./protocol.mjs"

export const RELAY_ORIGIN = "http://parley-provider.invalid"
const MAX_REQUEST = 512 * 1024
const MAX_RESPONSE = 2 * 1024 * 1024
const MAX_HEADERS = 16 * 1024
const CHUNK_BYTES = 32 * 1024

function denied() {
  throw new WorkerFailure("PROVIDER_FAILED")
}

function responseBody(body, release) {
  const iterator = body[Symbol.asyncIterator]()
  let size = 0
  let remaining
  return new ReadableStream({
    async pull(controller) {
      if (!remaining?.length) {
        const next = await iterator.next()
        if (next.done) {
          release()
          controller.close()
          return
        }
        remaining = next.value
        size += remaining.length
        if (size > MAX_RESPONSE) {
          body.destroy()
          denied()
        }
      }
      controller.enqueue(remaining.subarray(0, CHUNK_BYTES))
      remaining = remaining.subarray(CHUNK_BYTES)
    },
    cancel() {
      body.destroy()
      release()
    },
  })
}

export function createRelay(socketPath, signal, { chatCalls = 1 } = {}) {
  const dispatcher = new Agent({
    connect: { socketPath },
    connections: 1,
    pipelining: 0,
    maxHeaderSize: MAX_HEADERS,
    maxResponseSize: MAX_RESPONSE,
  })
  const observedPaths = new Map()
  let busy = false
  let chatUsed = 0
  async function fetchRelay(input, init) {
    const url = new URL(input instanceof Request ? input.url : input)
    const method = init?.method ?? (input instanceof Request ? input.method : "GET")
    if (url.origin !== RELAY_ORIGIN || url.username || url.password || url.search || url.hash || busy) denied()

    const path = url.pathname
    let body
    if (path === "/v1/chat/completions" && method === "POST" && chatUsed < chatCalls) {
      if (typeof init?.body !== "string" || Buffer.byteLength(init.body) > MAX_REQUEST) denied()
      chatUsed += 1
      body = init.body
    } else if ((path === "/api/version" || path === "/api/tags") && method === "GET") {
      const count = observedPaths.get(path) ?? 0
      if (count >= 2) denied()
      observedPaths.set(path, count + 1)
    } else denied()

    busy = true
    try {
      const incoming = await request(url, {
        method,
        dispatcher,
        signal: init?.signal ? AbortSignal.any([signal, init.signal]) : signal,
        headers: {
          host: "parley-provider.invalid",
          "accept-encoding": "identity",
          ...(body === undefined ? {} : { "content-type": "application/json" }),
        },
        body,
        maxRedirections: 0,
      })
      const encoding = incoming.headers["content-encoding"]
      if (incoming.statusCode < 200 || incoming.statusCode >= 300 || (encoding && encoding !== "identity")) {
        incoming.body.destroy()
        denied()
      }
      const type = incoming.headers["content-type"]
      if (typeof type !== "string") {
        incoming.body.destroy()
        denied()
      }
      return new Response(responseBody(incoming.body, () => { busy = false }), {
        status: incoming.statusCode,
        headers: { "content-type": type },
      })
    } catch (error) {
      busy = false
      throw error
    }
  }
  return { fetch: fetchRelay, close: () => dispatcher.destroy() }
}
