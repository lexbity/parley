import { Agent } from "@earendil-works/pi-agent-core"
import { stream } from "@earendil-works/pi-ai/api/openai-completions"
import { Tokenizer } from "@huggingface/tokenizers"
import { RELAY_ORIGIN } from "./relay.mjs"
import { observe } from "./structured.mjs"
import { MAX_PROMPT_SCHEMA, parseJson, sha256, WorkerFailure } from "./protocol.mjs"

const MAX_TASK = 8 * 1024
const MAX_CONTENT = 32 * 1024
const MAX_TRANSCRIPT = 256 * 1024
const MAX_CALLS = 4
const MAX_ARGS = 16 * 1024
const MAX_TOOLS = 20
const MAX_TEXT = 32 * 1024

function reject(code) {
  throw new WorkerFailure(code)
}

function qualifiedTokenizer(bytes, digest) {
  if (sha256(bytes) !== digest) reject("PROFILE_CHANGED")
  try {
    const artifact = parseJson(bytes)
    return new Tokenizer(artifact.tokenizer, artifact.config)
  } catch {
    reject("CONTEXT_BUDGET")
  }
}

function closedTools(tools) {
  if (!Array.isArray(tools) || tools.length === 0 || tools.length > MAX_TOOLS) reject("WORKER_PROTOCOL")
  const names = new Set()
  return tools.map((tool) => {
    if (!tool || typeof tool !== "object" || Array.isArray(tool) ||
        Object.keys(tool).sort().join(",") !== "description,name,parameters" ||
        typeof tool.name !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(tool.name) ||
        typeof tool.description !== "string" || tool.description.length === 0 ||
        tool.description.length > 512 ||
        !tool.parameters || typeof tool.parameters !== "object" || Array.isArray(tool.parameters) ||
        names.has(tool.name)) reject("WORKER_PROTOCOL")
    names.add(tool.name)
    return {
      type: "function",
      function: {
        name: tool.name,
        description: tool.description,
        parameters: tool.parameters,
      },
    }
  })
}

/// Rebuild the canonical host transcript as Pi message shapes. Only the host
/// supplies these messages: a worker-owned session file or SDK cache is never
/// consulted, and the model cannot introduce a tool name of its own.
function canonicalMessages(transcript, continuation) {
  if (!Array.isArray(transcript) || transcript.length > 64) reject("WORKER_PROTOCOL")
  let bytes = 0
  const resolved = new Map()
  if (continuation !== null) {
    if (!continuation || typeof continuation !== "object" ||
        !Number.isSafeInteger(continuation.turn_no) || continuation.turn_no < 1 ||
        !Array.isArray(continuation.results) || continuation.results.length === 0 ||
        continuation.results.length > MAX_CALLS) reject("WORKER_PROTOCOL")
    for (const entry of continuation.results) {
      if (!entry || typeof entry !== "object" ||
          !Number.isSafeInteger(entry.ordinal) || entry.ordinal < 1 || entry.ordinal > MAX_CALLS ||
          !entry.result || typeof entry.result !== "object" || resolved.has(entry.ordinal)) {
        reject("WORKER_PROTOCOL")
      }
      resolved.set(entry.ordinal, entry.result)
    }
  }
  const messages = []
  const pending = []
  for (const item of transcript) {
    if (!item || typeof item !== "object" || typeof item.role !== "string" ||
        typeof item.content !== "string" || Buffer.byteLength(item.content) > MAX_CONTENT) {
      reject("WORKER_PROTOCOL")
    }
    bytes += Buffer.byteLength(item.content)
    if (bytes > MAX_TRANSCRIPT) reject("CONTEXT_BUDGET")
    if (item.role === "system") {
      messages.push({ role: "system", content: item.content, timestamp: Date.now() })
      continue
    }
    if (item.role === "user") {
      messages.push({ role: "user", content: item.content, timestamp: Date.now() })
      continue
    }
    if (item.role === "assistant") {
      const calls = item.tool_calls ?? []
      if (!Array.isArray(calls) || calls.length > MAX_CALLS) reject("WORKER_PROTOCOL")
      const content = [{ type: "text", text: item.content }]
      for (const [index, call] of calls.entries()) {
        if (!call || typeof call.call_ref !== "string" || typeof call.name !== "string" ||
            !call.args || typeof call.args !== "object" || Array.isArray(call.args)) {
          reject("WORKER_PROTOCOL")
        }
        content.push({ type: "toolCall", id: call.call_ref, name: call.name, arguments: call.args })
        if (continuation !== null && !resolved.has(index + 1)) {
          pending.push({ ordinal: index + 1, call_ref: call.call_ref, name: call.name, args: call.args })
        }
      }
      messages.push({ role: "assistant", content, timestamp: Date.now() })
      continue
    }
    if (item.role === "tool") {
      if (typeof item.tool_call_id !== "string" || item.tool_call_id.length === 0) {
        reject("WORKER_PROTOCOL")
      }
      messages.push({
        role: "tool",
        toolCallId: item.tool_call_id,
        toolName: item.tool_call_id,
        content: [{ type: "text", text: item.content }],
        isError: false,
        timestamp: Date.now(),
      })
      continue
    }
    reject("WORKER_PROTOCOL")
  }
  return { messages, pending }
}

/**
 * One bounded Pi Agent Core execution. `control.next()` yields host frames and
 * throws on cancel; `control.send(kind, body)` writes lease-bound frames with
 * the next strictly increasing worker sequence number.
 */
export async function executeDelegated(execution, assets, fetch, signal, control) {
  const { profile, task, transcript, continuation } = execution
  if (typeof task !== "string" || task.length === 0 || Buffer.byteLength(task) > MAX_TASK) {
    reject("WORKER_PROTOCOL")
  }
  const tokenizer = qualifiedTokenizer(assets.tokenizer, profile.tokenizer_digest)
  const tools = closedTools(execution.tools)
  const { messages, pending } = canonicalMessages(transcript, continuation)
  const initialObserved = await observe(fetch, profile)

  const model = {
    id: profile.model_tag,
    name: profile.model_tag,
    provider: "openai",
    api: "openai-completions",
    baseUrl: RELAY_ORIGIN + "/v1",
    reasoning: false,
    input: ["text"],
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    contextWindow: profile.context_tokens,
    maxTokens: profile.max_output_tokens,
    compat: {
      supportsStore: false,
      supportsDeveloperRole: false,
      supportsReasoningEffort: false,
      maxTokensField: "max_tokens",
    },
  }
  let overBudget = false
  let usage = { input: 0, output: 0 }
  if (Buffer.byteLength(JSON.stringify(tools)) + Buffer.byteLength(task) > MAX_PROMPT_SCHEMA) {
    reject("CONTEXT_BUDGET")
  }
  const streamFn = (activeModel, context, options) => stream(activeModel, context, {
    ...options,
    apiKey: "parley-local-relay",
    fetch,
    signal,
    maxRetries: 0,
    cacheRetention: "none",
    temperature: 0,
    maxTokens: profile.max_output_tokens,
  })

  const acceptedCalls = new Map()
  let acceptedTurn = 0
  let lastAssistant = null
  const toolHandlers = new Map()
  for (const descriptor of tools) {
    const name = descriptor.function.name
    toolHandlers.set(name, {
      name,
      label: name,
      description: descriptor.function.description,
      parameters: descriptor.function.parameters,
      executionMode: "sequential",
      async execute(callId, args) {
        const call = acceptedCalls.get(callId)
        if (!call) reject("WORKER_PROTOCOL")
        await control.send("tool_request", {
          turn_no: acceptedTurn,
          ordinal: call.ordinal,
          name,
          args,
        })
        const frame = await control.next()
        if (frame.kind === "tool_result" &&
            frame.body.turn_no === acceptedTurn && frame.body.ordinal === call.ordinal) {
          return {
            content: [{ type: "text", text: JSON.stringify(frame.body.result) }],
            details: {},
          }
        }
        if (frame.kind === "suspend") reject("SUSPENDED")
        reject("WORKER_PROTOCOL")
      },
    })
  }

  const agent = new Agent({
    initialState: { systemPrompt: "A scoped local assistant. Use only the supplied tools.", model, tools: [...toolHandlers.values()], messages },
    streamFn,
    toolExecution: "sequential",
    getApiKey: () => "parley-local-relay",
    onPayload(payload) {
      const request = {
        model: payload.model,
        messages: payload.messages,
        stream: payload.stream,
        stream_options: payload.stream_options,
        max_tokens: payload.max_tokens,
        temperature: payload.temperature,
        tools: Array.isArray(payload.tools) ? payload.tools : tools,
      }
      const count = tokenizer.encode(JSON.stringify(request), { add_special_tokens: false }).ids.length
      if (count + profile.template_tokens + profile.max_output_tokens >
          Math.floor(profile.context_tokens * 0.9)) {
        overBudget = true
        reject("CONTEXT_BUDGET")
      }
      return request
    },
    beforeToolCall(context) {
      const assistant = context.assistantMessage
      const calls = (assistant.content ?? []).filter((item) => item?.type === "toolCall")
      if (calls.length === 0 || calls.length > MAX_CALLS) reject("WORKER_PROTOCOL")
      if (assistant !== lastAssistant) {
        lastAssistant = assistant
        acceptedCalls.clear()
      }
      {
        const projection = calls.map((call, index) => {
          if (!toolHandlers.has(call.name) || typeof call.id !== "string" ||
              !call.arguments || typeof call.arguments !== "object" ||
              Buffer.byteLength(JSON.stringify(call.arguments)) > MAX_ARGS) {
            reject("WORKER_PROTOCOL")
          }
          acceptedCalls.set(call.id, { ordinal: index + 1 })
          return { call_ref: call.id, name: call.name, args: call.arguments }
        })
        const text = assistant.content
          .filter((item) => item?.type === "text")
          .map((item) => item.text)
          .join("")
        if (Buffer.byteLength(text) > MAX_TEXT) reject("RESULT_INVALID")
        return control.send("assistant_turn", { text, calls: projection }).then(async () => {
          const frame = await control.next()
          if (frame.kind !== "turn_accepted" || !Number.isSafeInteger(frame.body.turn_no) ||
              frame.body.turn_no < 1 ||
              !Array.isArray(frame.body.ordinals) ||
              frame.body.ordinals.length !== projection.length ||
              frame.body.ordinals.some((ordinal, index) => ordinal !== index + 1)) {
            reject("WORKER_PROTOCOL")
          }
          acceptedTurn = frame.body.turn_no
          return undefined
        })
      }
    },
  })

  let suspended = false
  try {
    if (continuation !== null) {
      // Reconstruct canonical history from the host's committed turns. Ordinals
      // whose effect is already committed are not replayed; only the durable
      // remainder of the accepted batch is dispatched through the same journal.
      acceptedTurn = continuation.turn_no
      for (const call of pending) {
        await control.send("tool_request", {
          turn_no: acceptedTurn,
          ordinal: call.ordinal,
          name: call.name,
          args: call.args,
        })
        const frame = await control.next()
        if (frame.kind === "suspend") {
          suspended = true
          break
        }
        if (frame.kind !== "tool_result" ||
            frame.body.turn_no !== acceptedTurn || frame.body.ordinal !== call.ordinal) {
          reject("WORKER_PROTOCOL")
        }
        agent.state.messages.push({
          role: "toolResult",
          toolCallId: call.call_ref,
          toolName: call.name,
          content: [{ type: "text", text: JSON.stringify(frame.body.result) }],
          details: {},
          isError: false,
          timestamp: Date.now(),
        })
      }
      if (!suspended) await agent.continue()
    } else {
      await agent.continue()
    }
  } catch (error) {
    if (error instanceof WorkerFailure && error.code === "SUSPENDED") suspended = true
    else throw error
  }
  await agent.waitForIdle().catch((error) => {
    if (error instanceof WorkerFailure && error.code === "SUSPENDED") suspended = true
    else throw error
  })
  if (signal.aborted) reject("CANCELLED")
  if (overBudget) reject("CONTEXT_BUDGET")
  if (suspended) return { suspended: true }
  const final = agent.state.messages.filter((message) => message.role === "assistant").at(-1)
  if (!final || final.stopReason === "error") reject("PROVIDER_FAILED")
  usage = { input: final.usage?.input ?? 0, output: final.usage?.output ?? 0 }
  if (!Number.isInteger(usage.input) || usage.input < 1 || usage.input > profile.context_tokens ||
      !Number.isInteger(usage.output) || usage.output < 1 || usage.output > profile.max_output_tokens) {
    reject("RESULT_INVALID")
  }
  const finalText = (final.content ?? [])
    .filter((item) => item?.type === "text")
    .map((item) => item.text)
    .join("")
  if (Buffer.byteLength(finalText) > MAX_TEXT) reject("RESULT_INVALID")
  const finalObserved = await observe(fetch, profile)
  if (finalObserved.model_digest !== initialObserved.model_digest ||
      finalObserved.provider_version !== initialObserved.provider_version) {
    reject("PROFILE_CHANGED")
  }
  return {
    suspended: false,
    body: {
      mode: "delegated",
      observed: initialObserved,
      usage: { input_tokens: usage.input, output_tokens: usage.output },
      text: finalText,
    },
  }
}
