import { Tokenizer } from "@huggingface/tokenizers"
import { stream } from "@earendil-works/pi-ai/api/openai-completions"
import { normalizeContext } from "@earendil-works/pi-ai/utils/transcript"
import { RELAY_ORIGIN } from "./relay.mjs"
import {
  compileSchema, MAX_PROMPT_SCHEMA, MAX_ROLE_INPUT, MAX_ROLE_OUTPUT,
  parseJson, sha256, WorkerFailure,
} from "./protocol.mjs"

const PROBE_SCHEMA = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  type: "object",
  required: ["witness"],
  properties: { witness: { const: "parley-structured-ready" } },
  additionalProperties: false,
}

function reject(code) {
  throw new WorkerFailure(code)
}

export async function observe(fetch, profile) {
  const version = await (await fetch(RELAY_ORIGIN + "/api/version")).json()
  if (version?.version !== profile.provider_version) reject("PROFILE_CHANGED")
  const catalog = await (await fetch(RELAY_ORIGIN + "/api/tags")).json()
  if (!Array.isArray(catalog?.models)) reject("MODEL_UNAVAILABLE")
  const matches = catalog.models.filter((item) => item?.name === profile.model_tag)
  if (matches.length !== 1) reject("MODEL_UNAVAILABLE")
  if (matches[0].digest !== profile.model_digest) reject("PROFILE_CHANGED")
  return { provider_version: version.version, model_digest: matches[0].digest, model_tag: matches[0].name }
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

export async function executeStructured(execution, assets, fetch, signal) {
  const { profile, mode, role } = execution
  const probe = mode === "probe"
  const schemaName = role === "template_builder" ? "parley_template_builder"
    : role === "interviewer" ? "parley_interviewer"
    : role === "briefer" ? "parley_briefer"
    : role === "extractor" ? "parley_extractor"
    : "parley_coach"
  const schemaBytes = probe ? Buffer.from(JSON.stringify(PROBE_SCHEMA))
    : role === "template_builder" ? assets.builderSchema
    : role === "interviewer" ? assets.interviewerSchema
    : role === "briefer" ? assets.brieferSchema
    : role === "extractor" ? assets.extractorSchema
    : assets.coachSchema
  if (!probe && sha256(schemaBytes) !== execution.schema_digest) reject("WORKER_PROTOCOL")
  const schema = parseJson(schemaBytes)
  const validate = compileSchema(schema)
  const system = probe ? "Return the JSON object exactly as required by the response schema." : execution.system
  const input = probe ? '{"probe":"structured output capability"}' : JSON.stringify(execution.input)
  if (Buffer.byteLength(system) + schemaBytes.length > MAX_PROMPT_SCHEMA ||
      Buffer.byteLength(input) > MAX_ROLE_INPUT) reject("CONTEXT_BUDGET")

  const tokenizer = qualifiedTokenizer(assets.tokenizer, profile.tokenizer_digest)
  await observe(fetch, profile)
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
  const reply = await stream(model, normalizeContext({
    systemPrompt: system,
    messages: [{ role: "user", content: input, timestamp: Date.now() }],
  }), {
    // The SDK requires a nonempty value. The relay layer never forwards it.
    apiKey: "parley-local-relay",
    fetch,
    signal,
    maxRetries: 0,
    cacheRetention: "none",
    temperature: 0,
    maxTokens: profile.max_output_tokens,
    onPayload(payload) {
      const request = {
        model: payload.model,
        messages: payload.messages,
        stream: payload.stream,
        stream_options: payload.stream_options,
        max_tokens: payload.max_tokens,
        temperature: payload.temperature,
        response_format: {
          type: "json_schema",
          json_schema: { name: probe ? "parley_probe" : schemaName, schema, strict: true },
        },
      }
      const count = tokenizer.encode(JSON.stringify(request), { add_special_tokens: false }).ids.length
      const total = count + profile.template_tokens + profile.max_output_tokens
      if (total > Math.floor(profile.context_tokens * 0.9)) {
        overBudget = true
        reject("CONTEXT_BUDGET")
      }
      return request
    },
  }).result()
  if (signal.aborted) reject("CANCELLED")
  if (overBudget) reject("CONTEXT_BUDGET")
  if (reply.stopReason === "error") reject("PROVIDER_FAILED")
  if (reply.stopReason !== "stop" || reply.content.some((item) => item.type !== "text")) reject("RESULT_INVALID")

  const output = reply.content.map((item) => item.text).join("")
  if (Buffer.byteLength(output) > MAX_ROLE_OUTPUT) reject("RESULT_INVALID")
  let candidate
  try {
    candidate = parseJson(Buffer.from(output))
    if (!validate(candidate)) reject("RESULT_INVALID")
  } catch {
    reject("RESULT_INVALID")
  }
  const usage = { input_tokens: reply.usage.input, output_tokens: reply.usage.output }
  if (!Number.isInteger(usage.input_tokens) || usage.input_tokens < 1 || usage.input_tokens > profile.context_tokens ||
      !Number.isInteger(usage.output_tokens) || usage.output_tokens < 1 || usage.output_tokens > profile.max_output_tokens) {
    reject("RESULT_INVALID")
  }
  const observed = await observe(fetch, profile)
  return probe ? { mode, observed, usage } : { mode, observed, usage, candidate }
}
